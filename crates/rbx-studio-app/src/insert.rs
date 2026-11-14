//! Toolbox asset → Studio instance conversion.
//!
//! Runs on a worker thread: downloads (or reuses the cached copy of) the asset
//! payload, then builds the data model the GUI thread splices into the place:
//!
//! | Toolbox category | Roblox type | Inserted as |
//! |---|---|---|
//! | Models | 10 | parsed `.rbxm`/`.rbxmx` instance tree + `BasePart`s |
//! | Audio | 3 | `Sound` with `SoundId` |
//! | Images | 1 / 13 | thin `Part` with a `Decal` child (`Texture`) |
//! | Meshes | 4 / 40 | `MeshPart` with `MeshId` + cached `.mesh` geometry |
//!
//! Every payload is written to the on-disk cache first, so the next insert (or
//! the next app launch) is instant and the renderer can read the mesh/image
//! straight from disk.

use crate::asset_cache::{self, Kind};
use crate::roblox_api;
use anyhow::{anyhow, Context, Result};
use rbx_core::{InstanceNode, MeshRef, Part, Vector3};

/// Everything the GUI thread needs to place one insert.
pub struct BuiltInsert {
    /// Explorer node for the inserted root.
    pub node: InstanceNode,
    /// Parts to add to the rendered scene (may be empty, e.g. audio).
    pub parts: Vec<Part>,
    /// Short class label used in the status line ("Sound", "MeshPart", …).
    pub label: String,
    /// Bytes of the asset payload.
    pub bytes: usize,
    /// Extra human note for the status/output (cache location, ids, …).
    pub note: String,
}

/// Downloads (or reads from cache) and converts one Toolbox asset.
pub fn build(asset_id: i64, name: &str, category: i32) -> Result<BuiltInsert> {
    let wanted = match category {
        3 => Kind::Audio,
        1 | 13 => Kind::Texture,
        4 | 40 => Kind::Mesh,
        _ => Kind::Model,
    };

    // Cache hit: no network round-trip, the payload is the same bytes Roblox served.
    // A container payload that still needs unwrapping is treated as a miss, unless
    // it is a wrapper model that was stored as the inserted instance last time.
    if let Some(path) = asset_cache::find(wanted, asset_id) {
        if let Some(bytes) = asset_cache::read(wanted, asset_id) {
            if !bytes.is_empty() {
                let direct = decodes(wanted, &bytes);
                let wrapper = matches!(wanted, Kind::Texture | Kind::Mesh)
                    && rbx_core::parse_model(&bytes).is_ok();
                if direct || wrapper {
                    let type_id = if direct { category as i64 } else { 10 };
                    return finish(asset_id, name, type_id, bytes, Some(path), true);
                }
            }
        }
    }
    // A wrapper model (image/mesh entry with no usable inner asset) is stored in
    // the model cache, so look there too before hitting the network again.
    if matches!(wanted, Kind::Texture | Kind::Mesh) {
        if let Some(path) = asset_cache::find(Kind::Model, asset_id) {
            if let Some(bytes) = asset_cache::read(Kind::Model, asset_id) {
                if rbx_core::parse_model(&bytes).is_ok() {
                    let mut built = finish(asset_id, name, 10, bytes, Some(path), true)?;
                    wrapper_note(&mut built, category);
                    return Ok(built);
                }
            }
        }
    }

    let download = roblox_api::download_asset(asset_id)?;
    if download.content_type.starts_with("text/html") {
        return Err(anyhow!(
            "the asset CDN returned an HTML page instead of asset content"
        ));
    }
    let mut type_id = download.asset_type_id;
    let mut bytes = download.bytes;
    let mut content_type = download.content_type;

    // Image and mesh entries are often delivered as a `.rbxm`/`.rbxmx` wrapper
    // around the real `Decal`/`MeshPart`. Unwrap it by fetching the inner asset.
    if matches!(wanted, Kind::Texture | Kind::Mesh) && !decodes(wanted, &bytes) {
        let props: &[&str] = match wanted {
            Kind::Texture => &["Texture", "TextureId", "Image"],
            _ => &["MeshId", "MeshID", "Mesh"],
        };
        if let Some(inner) = rbx_core::find_asset_reference(&bytes, props) {
            if inner != asset_id {
                let inner_dl = roblox_api::download_asset(inner)?;
                type_id = inner_dl.asset_type_id.or(type_id);
                bytes = inner_dl.bytes;
                content_type = inner_dl.content_type;
            }
        }
        if !decodes(wanted, &bytes) {
            // Some entries ship a wrapper model with no fetchable inner asset
            // (a `MeshPart` without `MeshId`, for instance). Insert the wrapper
            // as a model so the user still gets an instance in the place.
            if rbx_core::parse_model(&bytes).is_ok() {
                let ext = asset_cache::ext_for(Kind::Model, &bytes, &content_type);
                let path =
                    asset_cache::store(Kind::Model, asset_id, ext, &bytes, type_id, &content_type)
                        .ok();
                let mut built = finish(asset_id, name, 10, bytes, path, false)?;
                wrapper_note(&mut built, category);
                return Ok(built);
            }
            return Err(anyhow!(
                "the {} payload for asset {asset_id} could not be decoded ({}, {} bytes)",
                match wanted {
                    Kind::Texture => "image",
                    _ => "mesh",
                },
                content_type,
                bytes.len()
            ));
        }
    }

    let kind = match type_id {
        Some(3) => Kind::Audio,
        Some(1) | Some(13) => Kind::Texture,
        Some(4) | Some(40) => Kind::Mesh,
        Some(10) | None => Kind::Model,
        Some(other) => {
            return Err(anyhow!(
                "asset {asset_id} is a {} — Toolbox inserts support Models, Audio, Images and Meshes",
                roblox_api::asset_type_label(other)
            ))
        }
    };
    let ext = asset_cache::ext_for(kind, &bytes, &content_type);
    let path = asset_cache::store(
        kind,
        asset_id,
        ext,
        &bytes,
        type_id,
        &content_type,
    )
    .ok();
    finish(asset_id, name, type_id.unwrap_or(category as i64), bytes, path, false)
}

/// Whether a payload can be consumed directly by the renderer.
fn decodes(kind: Kind, bytes: &[u8]) -> bool {
    match kind {
        Kind::Texture => image::load_from_memory(bytes).is_ok(),
        Kind::Mesh => rbx_core::parse_mesh(bytes).is_ok(),
        _ => true,
    }
}

/// Annotates a wrapper-model insert (Toolbox image/mesh entry whose payload is a
/// model with no fetchable inner asset) with a note explaining the fallback.
fn wrapper_note(built: &mut BuiltInsert, category: i32) {
    let prefix = format!(
        "{} model wrapper (inner asset not available)",
        roblox_api::asset_type_label(category as i64)
    );
    built.note = if built.note.is_empty() {
        prefix
    } else {
        format!("{prefix} — {}", built.note.trim_start_matches(", "))
    };
}

fn finish(
    asset_id: i64,
    name: &str,
    type_id: i64,
    bytes: Vec<u8>,
    cached: Option<std::path::PathBuf>,
    from_cache: bool,
) -> Result<BuiltInsert> {
    let cache_note = match (&cached, from_cache) {
        (Some(path), true) => format!(", from cache {}", shorten(path)),
        (Some(path), false) => format!(", cached to {}", shorten(path)),
        (None, _) => String::new(),
    };
    let size = bytes.len();
    let built = match type_id {
        3 => audio(name, asset_id, size, cache_note),
        1 | 13 => image(name, asset_id, bytes, cached, size, cache_note),
        4 | 40 => mesh(name, asset_id, bytes, cached, size, cache_note),
        10 => model(name, bytes, size, cache_note),
        other => Err(anyhow!(
            "asset type {} cannot be inserted yet",
            roblox_api::asset_type_label(other)
        )),
    }?;
    Ok(built)
}

fn shorten(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    if text.len() <= 64 {
        return text;
    }
    // Keep the tail (folder + file), which is the informative part.
    let tail: String = text.chars().rev().take(56).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…{tail}")
}

fn audio(name: &str, asset_id: i64, bytes: usize, note: String) -> Result<BuiltInsert> {
    let sound_id = format!("rbxassetid://{asset_id}");
    let node = InstanceNode::leaf(name, "Sound")
        .with_prop("SoundId", sound_id.clone())
        .with_prop("Volume", "0.5")
        .with_prop("Looped", "false")
        .with_prop("PlaybackSpeed", "1");
    Ok(BuiltInsert {
        node,
        parts: Vec::new(),
        label: "Sound".to_string(),
        bytes,
        note: format!("SoundId {sound_id}{note}"),
    })
}

fn image(
    name: &str,
    asset_id: i64,
    bytes: Vec<u8>,
    cached: Option<std::path::PathBuf>,
    len: usize,
    note: String,
) -> Result<BuiltInsert> {
    let (w, h) = image::load_from_memory(&bytes)
        .map(|img| {
            let img = img.into_rgba8();
            (img.width().max(1), img.height().max(1))
        })
        .with_context(|| "the downloaded image could not be decoded")?;
    let aspect = h as f32 / w as f32;
    let width = 8.0f32;
    let depth = (width * aspect).clamp(0.5, 32.0);
    let texture_id = format!("rbxassetid://{asset_id}");

    let mut part = Part::default();
    part.name = name.to_string();
    part.class_name = "Part".to_string();
    part.size = Vector3::new(width, 0.1, depth);
    part.color = [1.0, 1.0, 1.0];
    part.material = "SmoothPlastic".to_string();
    part.cast_shadow = false;
    part.decal = cached.as_ref().map(|p| p.display().to_string());

    let node = InstanceNode::leaf(name, "Part")
        .with_prop("Size", format!("{width}, 0.1, {depth:.2}"))
        .with_child(
            InstanceNode::leaf("Decal", "Decal")
                .with_prop("Texture", texture_id.clone())
                .with_prop("Transparency", "0"),
        );
    Ok(BuiltInsert {
        node,
        parts: vec![part],
        label: "Decal".to_string(),
        bytes: len,
        note: format!("Decal.Texture {texture_id}{note}"),
    })
}

fn mesh(
    name: &str,
    asset_id: i64,
    bytes: Vec<u8>,
    cached: Option<std::path::PathBuf>,
    len: usize,
    note: String,
) -> Result<BuiltInsert> {
    let geo = rbx_core::parse_mesh(&bytes)
        .with_context(|| "the downloaded mesh could not be parsed")?;
    let size = geo.size();
    // Roblox mesh coordinates are already in studs; keep 1:1 but guard against
    // degenerate/absurd bounds so the inserted part stays visible.
    let scale = if size.x.max(size.y).max(size.z) > 200.0 {
        200.0 / size.x.max(size.y).max(size.z)
    } else {
        1.0
    };
    let size = Vector3::new(
        (size.x * scale).max(0.05),
        (size.y * scale).max(0.05),
        (size.z * scale).max(0.05),
    );
    let mesh_id = format!("rbxassetid://{asset_id}");

    let mut part = Part::default();
    part.name = name.to_string();
    part.class_name = "MeshPart".to_string();
    part.size = size;
    part.color = [0.639, 0.635, 0.647];
    part.material = "SmoothPlastic".to_string();
    part.mesh = Some(MeshRef {
        mesh_id: mesh_id.clone(),
        texture_id: String::new(),
        mesh_type: Some("MeshPart".to_string()),
        scale: None,
        offset: None,
    });
    part.mesh_file = cached
        .as_ref()
        .map(|p| p.display().to_string())
        .filter(|_| geo.positions.len() > 0);

    let node = InstanceNode::leaf(name, "MeshPart")
        .with_prop("MeshId", mesh_id.clone())
        .with_prop("Size", format!("{:.2}, {:.2}, {:.2}", size.x, size.y, size.z))
        .with_prop("Material", "SmoothPlastic");
    Ok(BuiltInsert {
        node,
        parts: vec![part],
        label: "MeshPart".to_string(),
        bytes: len,
        note: format!(
            "MeshId {mesh_id} ({} verts, {} tris){note}",
            geo.positions.len(),
            geo.indices.len() / 3
        ),
    })
}

fn model(name: &str, bytes: Vec<u8>, len: usize, note: String) -> Result<BuiltInsert> {
    let mut model = rbx_core::import_model(&bytes)?;
    let _ = name;
    let class = model.class_name.clone();
    let (resolved, failed) = resolve_meshes(&mut model.parts);
    let note = if resolved > 0 || failed > 0 {
        let mut extra = Vec::new();
        if resolved > 0 {
            extra.push(format!("{resolved} mesh{} resolved", if resolved == 1 { "" } else { "es" }));
        }
        if failed > 0 {
            extra.push(format!("{failed} mesh{} unavailable", if failed == 1 { "" } else { "es" }));
        }
        format!("{note}, {}", extra.join(", "))
    } else {
        note
    };
    Ok(BuiltInsert {
        node: model.root,
        parts: model.parts,
        label: class,
        bytes: len,
        note,
    })
}

/// Best-effort: downloads and caches Sky cubemap face textures referenced by a
/// place's `Sky` instance. Failures are ignored (gradient/clear fallback remains).
pub fn resolve_sky_textures(sky: &mut rbx_core::SkySettings) -> (usize, usize) {
    fn resolve_one(uri: &str) -> Option<String> {
        let asset_id = parse_asset_id(uri)?;
        if let Some(path) = asset_cache::find(Kind::Texture, asset_id) {
            return Some(path.display().to_string());
        }
        let download = roblox_api::download_asset(asset_id).ok()?;
        let bytes = download.bytes;
        if image::load_from_memory(&bytes).is_err() {
            return None;
        }
        let ext = asset_cache::ext_for(Kind::Texture, &bytes, &download.content_type);
        let path = asset_cache::store(
            Kind::Texture,
            asset_id,
            ext,
            &bytes,
            download.asset_type_id,
            &download.content_type,
        )
        .ok()?;
        Some(path.display().to_string())
    }

    let mut resolved = 0usize;
    let mut failed = 0usize;
    let faces: [(&str, &mut Option<String>); 6] = [
        (sky.skybox_bk.as_str(), &mut sky.skybox_bk_file),
        (sky.skybox_dn.as_str(), &mut sky.skybox_dn_file),
        (sky.skybox_ft.as_str(), &mut sky.skybox_ft_file),
        (sky.skybox_lf.as_str(), &mut sky.skybox_lf_file),
        (sky.skybox_rt.as_str(), &mut sky.skybox_rt_file),
        (sky.skybox_up.as_str(), &mut sky.skybox_up_file),
    ];
    for (uri, slot) in faces {
        if slot.is_some() || uri.trim().is_empty() {
            continue;
        }
        if let Some(path) = resolve_one(uri) {
            *slot = Some(path);
            resolved += 1;
        } else {
            // Only count as failed if there was an asset id we tried to fetch.
            if parse_asset_id(uri).is_some() {
                failed += 1;
            }
        }
    }
    (resolved, failed)
}

/// Best-effort: downloads and caches mesh geometry for parts that reference a
/// mesh asset (`MeshPart`/`SpecialMesh`/CSG) but have no local `mesh_file` yet.
/// Failures are ignored so inserts/places still open with primitive fallbacks.
pub fn resolve_meshes(parts: &mut [rbx_core::Part]) -> (usize, usize) {
    let mut resolved = 0usize;
    let mut failed = 0usize;
    for part in parts.iter_mut() {
        if part.mesh_file.is_some() {
            continue;
        }
        let Some(mesh_ref) = part.mesh.as_ref() else { continue };
        let Some(asset_id) = parse_asset_id(&mesh_ref.mesh_id) else { continue };
        // Cache hit first.
        if let Some(path) = asset_cache::find(Kind::Mesh, asset_id) {
            part.mesh_file = Some(path.display().to_string());
            resolved += 1;
            continue;
        }
        match roblox_api::download_asset(asset_id) {
            Ok(download) => {
                // Unwrap common mesh wrapper payloads (some assets come as .rbxm).
                let mut bytes = download.bytes;
                if rbx_core::parse_mesh(&bytes).is_err() {
                    if let Some(inner) = rbx_core::find_asset_reference(&bytes, &["MeshId", "MeshID", "Mesh"]) {
                        if inner != asset_id {
                            if let Ok(inner_dl) = roblox_api::download_asset(inner) {
                                bytes = inner_dl.bytes;
                            }
                        }
                    }
                }
                if rbx_core::parse_mesh(&bytes).is_ok() {
                    let ext = asset_cache::ext_for(Kind::Mesh, &bytes, &download.content_type);
                    if let Ok(path) = asset_cache::store(Kind::Mesh, asset_id, ext, &bytes, download.asset_type_id, &download.content_type) {
                        part.mesh_file = Some(path.display().to_string());
                        resolved += 1;
                    } else {
                        failed += 1;
                    }
                } else {
                    failed += 1;
                }
            }
            Err(_) => {
                failed += 1;
            }
        }
    }
    (resolved, failed)
}

pub(crate) fn parse_asset_id(uri: &str) -> Option<i64> {
    let uri = uri.trim();
    if uri.is_empty() {
        return None;
    }
    if let Some(pos) = uri.find("id=") {
        let digits: String = uri[pos + 3..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(id) = digits.parse::<i64>() {
            if id > 0 { return Some(id); }
        }
    }
    let tail = uri.rsplit('/').next()?.split(['?', '&']).next()?;
    let id = tail.trim().parse::<i64>().ok()?;
    if id > 0 { Some(id) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_of(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([200, 40, 40, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode png");
        out.into_inner()
    }

    #[test]
    fn audio_inserts_a_sound_with_its_id() {
        let built = finish(42, "Beep", 3, vec![0; 16], None, false).expect("audio");
        assert_eq!(built.label, "Sound");
        assert_eq!(built.node.class_name, "Sound");
        assert_eq!(
            built.node.props.get("SoundId").map(String::as_str),
            Some("rbxassetid://42")
        );
        assert!(built.parts.is_empty(), "audio has no geometry");
    }

    #[test]
    fn image_inserts_a_flat_part_with_a_decal() {
        let png = png_of(64, 32);
        let built = finish(7, "Picture", 1, png, None, false).expect("image");
        assert_eq!(built.label, "Decal");
        assert_eq!(built.parts.len(), 1);
        let part = &built.parts[0];
        // 2:1 image → 8 x 4 studs, lying flat.
        assert!((part.size.x - 8.0).abs() < 1e-3, "{:?}", part.size);
        assert!((part.size.z - 4.0).abs() < 1e-3, "{:?}", part.size);
        assert!(part.decal.is_none(), "no cache path was supplied");
        let decal = &built.node.children[0];
        assert_eq!(decal.class_name, "Decal");
        assert_eq!(
            decal.props.get("Texture").map(String::as_str),
            Some("rbxassetid://7")
        );
    }

    #[test]
    fn mesh_inserts_a_meshpart_with_parsed_geometry() {
        // v1.00 halves coordinates; this is a 4x2x8 quad. v1 keeps every
        // vertex on the single line after the face count.
        let data = "version 1.00\n2\n\
[0,1,0][0,1,0][0,1,0][8,1,0][0,1,0][1,1,0][8,1,16][0,1,0][1,0,0]\
[0,1,0][0,1,0][0,1,0][8,1,16][0,1,0][1,0,0][0,1,16][0,1,0][0,0,0]\n";
        let built = finish(9, "Rock", 4, data.as_bytes().to_vec(), None, false).expect("mesh");
        assert_eq!(built.label, "MeshPart");
        let part = &built.parts[0];
        assert_eq!(part.class_name, "MeshPart");
        assert!((part.size.x - 4.0).abs() < 1e-3, "{:?}", part.size);
        assert!((part.size.z - 8.0).abs() < 1e-3, "{:?}", part.size);
        assert_eq!(
            built.node.props.get("MeshId").map(String::as_str),
            Some("rbxassetid://9")
        );
        assert!(built.note.contains("verts"), "{}", built.note);
    }

    #[test]
    fn unsupported_types_are_rejected() {
        let err = finish(11, "Thing", 24, vec![0; 4], None, false)
            .err()
            .expect("animations are not insertable");
        assert!(err.to_string().contains("cannot be inserted"), "{err}");
    }

    /// Live end-to-end check against the real Toolbox (needs network + the
    /// Open Cloud key linked in Studio Settings). Run with
    /// `cargo test -p rbx-studio-app -- --ignored live_`.
    #[test]
    #[ignore = "network + Open Cloud key"]
    fn live_image_audio_and_mesh_inserts() {
        if roblox_api::load_api_key().ok().flatten().is_none() {
            eprintln!("skipping: no Open Cloud key linked");
            return;
        }
        for (category, what) in [(13i32, "image"), (3, "audio"), (40, "mesh")] {
            let items = roblox_api::search(category as u32, "", 5).expect("toolbox search");
            let ids: Vec<(i64, String)> = items
                .into_iter()
                .filter(|i| i.id != 0)
                .take(5)
                .map(|i| (i.id as i64, i.name))
                .collect();
            if ids.is_empty() {
                eprintln!("no {what} results, skipping");
                continue;
            }
            for (id, name) in ids {
                match build(id, &name, category) {
                    Ok(built) => {
                        let geometry = built
                            .parts
                            .iter()
                            .filter(|p| p.mesh_file.is_some())
                            .count();
                        eprintln!(
                            "{what} {id} \"{name}\": {} parts ({geometry} with .mesh), {} → {}",
                            built.parts.len(),
                            built.label,
                            built.note
                        );
                        if matches!(category, 13 | 40) {
                            assert!(!built.parts.is_empty(), "{what} should render geometry");
                        }
                    }
                    Err(e) => eprintln!("{what} {id} failed: {e}"),
                }
            }
        }
    }
}
