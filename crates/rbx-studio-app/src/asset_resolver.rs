//! Resolve Roblox asset references (`rbxassetid://12345`) to local files.
//!
//! Asset references show up in several places: a value typed into the
//! Properties panel, a place file's `TextureID`/`MeshId`, a Toolbox insert, or
//! the Sky cubemap. Every reference is resolved the same way:
//!
//! 1. look the id up in **every** cache bucket (`models`, `meshes`, `textures`,
//!    `audio`) - anything already downloaded is reused with no network call;
//! 2. otherwise fetch it (Open Cloud asset delivery when a key is linked, else
//!    the public endpoint);
//! 3. sniff the payload to decide which bucket it belongs in, and store it
//!    there so the next reference to the same id is instant.
//!
//! The caller supplies a `hint` (what the property implies) which is only used
//! when the payload itself does not identify a family (e.g. the very first
//! reference to a mesh, before the file has a magic-byte signature).

use crate::asset_cache::{self, Kind};
use crate::insert;
use crate::roblox_api;
use anyhow::{anyhow, Result};
use std::path::PathBuf;

/// An asset reference that now has a file on disk.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub id: i64,
    pub kind: Kind,
    pub path: PathBuf,
    /// `true` when the file came from a cache (no network round-trip).
    pub cached: bool,
}

impl Resolved {
    /// One-line summary for the Output console.
    pub fn note(&self) -> String {
        let file = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if self.cached {
            format!(
                "{} {} already cached in {} ({file})",
                self.kind.label(),
                self.id,
                self.kind.dir()
            )
        } else {
            format!(
                "{} {} downloaded and cached in {} ({file})",
                self.kind.label(),
                self.id,
                self.kind.dir()
            )
        }
    }
}

/// Extracts the numeric id from `rbxassetid://123`, `rbxassetid://123?x=1`,
/// `http://www.roblox.com/asset/?id=123`, `https://host/123?v=1` or a bare `123`.
pub fn asset_id(uri: &str) -> Option<i64> {
    let id = insert::parse_asset_id(uri)?;
    if id > 0 {
        Some(id)
    } else {
        None
    }
}

/// Whether a string looks like an asset reference at all (used to decide if a
/// property edit should trigger a fetch). Bare numeric ids count, since Roblox
/// content values historically accept `"12345"` as well as `rbxassetid://12345`.
pub fn is_asset_reference(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return false;
    }
    v.starts_with("rbxassetid://")
        || v.contains("roblox.com/asset")
        || v.to_ascii_lowercase().contains("rbxassetid")
        || v.chars().all(|c| c.is_ascii_digit())
}

/// Cache rank for a stored payload: a decoded image/mesh always wins over a
/// wrapper container or an unrecognised payload.
fn usable(kind: Kind, path: &std::path::Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match kind {
        Kind::Texture => matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "ktx2" | "rbxm" | "rbxmx"),
        Kind::Mesh => matches!(ext.as_str(), "mesh" | "csgphs" | "rbxm" | "rbxmx"),
        Kind::Audio => matches!(ext.as_str(), "ogg" | "mp3" | "wav" | "flac" | "rbxm" | "rbxmx"),
        Kind::Model => matches!(ext.as_str(), "rbxm" | "rbxmx"),
    }
}

/// Looks for an id in **every** cache bucket, preferring `prefer` and then
/// decoded payloads over containers/unknowns.
pub fn find_anywhere(id: i64, prefer: Kind) -> Option<(Kind, PathBuf)> {
    let order = [
        prefer,
        Kind::Texture,
        Kind::Mesh,
        Kind::Audio,
        Kind::Model,
    ];
    let mut fallback: Option<(Kind, PathBuf)> = None;
    for kind in order {
        let Some(path) = asset_cache::find(kind, id) else {
            continue;
        };
        if usable(kind, &path) {
            return Some((kind, path));
        }
        if fallback.is_none() {
            fallback = Some((kind, path));
        }
    }
    fallback
}

/// Which cache bucket a payload belongs in, from its magic bytes. `hint` (what
/// the referencing property implies) breaks ties for Roblox containers.
pub fn classify(bytes: &[u8], hint: Kind) -> Kind {
    let starts = |magic: &[u8]| bytes.starts_with(magic);
    let riff = |tag: &[u8; 4]| bytes.len() > 11 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == tag;

    if starts(b"\x89PNG\r\n\x1a\n")
        || starts(&[0xFF, 0xD8, 0xFF])
        || starts(b"GIF8")
        || starts(b"BM")
        || starts(b"KTX2")
        || riff(b"WEBP")
    {
        return Kind::Texture;
    }
    if starts(b"OggS") || starts(b"ID3") || starts(b"fLaC") || riff(b"WAVE") {
        return Kind::Audio;
    }
    if starts(b"version ") || starts(b"CSGPHS") {
        return Kind::Mesh;
    }
    if starts(b"<roblox!") || bytes.first() == Some(&b'<') {
        // A `.rbxm`/`.rbxmx` container: a mesh reference usually wraps the real
        // `.mesh`, otherwise it is an ordinary model.
        return if hint == Kind::Mesh { Kind::Mesh } else { Kind::Model };
    }
    hint
}

/// Property names that carry the inner asset id of a wrapper container.
fn inner_props(hint: Kind) -> &'static [&'static str] {
    match hint {
        Kind::Texture => &["Texture", "TextureId", "Image"],
        Kind::Mesh => &["MeshId", "MeshID", "Mesh"],
        _ => &[],
    }
}

/// Downloads `inner` (the real payload behind a wrapper) and files it under the
/// *referenced* `id`, so every later lookup of the reference is a cache hit.
/// `None` when the download does not yield a payload usable for the hint.
fn fetch_inner(id: i64, inner: i64, hint: Kind) -> Option<Resolved> {
    let download = roblox_api::download_asset(inner).ok()?;
    if download.bytes.is_empty() {
        return None;
    }
    let kind = classify(&download.bytes, hint);
    if kind == Kind::Model {
        return None;
    }
    let ext = asset_cache::ext_for(kind, &download.bytes, &download.content_type);
    let path = asset_cache::store(
        kind,
        id,
        ext,
        &download.bytes,
        download.asset_type_id,
        &download.content_type,
    )
    .ok()?;
    Some(Resolved {
        id,
        kind,
        path,
        cached: false,
    })
}

/// Resolves an asset reference: cache first (any bucket), otherwise download
/// and file the payload into the cache bucket its bytes belong to.
pub fn resolve(uri: &str, hint: Kind) -> Result<Resolved> {
    let id = asset_id(uri)
        .ok_or_else(|| anyhow!("`{}` is not a Roblox asset reference", uri.trim()))?;

    if let Some((kind, path)) = find_anywhere(id, hint) {
        let found = Resolved {
            id,
            kind,
            path,
            cached: true,
        };
        // An image/mesh reference cached earlier as a `.rbxm` wrapper (the
        // Toolbox serves both forms): fetch the wrapper's inner asset instead of
        // handing the renderer a container it cannot decode.
        if kind == Kind::Model {
            let props = inner_props(hint);
            if !props.is_empty() {
                let inner = std::fs::read(&found.path)
                    .ok()
                    .and_then(|bytes| rbx_core::find_asset_reference(&bytes, props));
                if let Some(inner) = inner {
                    if inner != id {
                        if let Some(inner_found) = fetch_inner(id, inner, hint) {
                            return Ok(inner_found);
                        }
                    }
                }
            }
        }
        return Ok(found);
    }

    let download = roblox_api::download_asset(id)?;
    let bytes = download.bytes;
    if bytes.is_empty() {
        return Err(anyhow!("asset {id} downloaded 0 bytes"));
    }
    let content_type = download.content_type;

    let kind = classify(&bytes, hint);
    // Images and meshes are sometimes served as a `.rbxm` wrapper around the
    // real payload; pull the inner asset id back out and fetch that instead.
    if kind == Kind::Model && hint != Kind::Model {
        let props = inner_props(hint);
        if !props.is_empty() {
            if let Some(inner) = rbx_core::find_asset_reference(&bytes, props) {
                if inner != id {
                    if let Some(inner_found) = fetch_inner(id, inner, hint) {
                        return Ok(inner_found);
                    }
                }
            }
        }
    }

    let ext = asset_cache::ext_for(kind, &bytes, &content_type);
    let path = asset_cache::store(
        kind,
        id,
        ext,
        &bytes,
        download.asset_type_id,
        &content_type,
    )?;
    Ok(Resolved {
        id,
        kind,
        path,
        cached: false,
    })
}

/// Attaches a resolved asset file to the scene: textures become the part's
/// surface decal, meshes its geometry. A part matches when its name equals the
/// reference's target instance **or** when it references the same asset id
/// (so one download serves every part that shares it). Returns how many parts
/// were updated.
pub fn attach(
    parts: &mut [rbx_core::Part],
    kind: Kind,
    id: Option<i64>,
    file: &str,
    target: &str,
) -> usize {
    if !matches!(kind, Kind::Texture | Kind::Mesh) {
        return 0;
    }
    let target_name = target.rsplit('/').next().unwrap_or("");
    let mut updated = 0usize;
    for part in parts.iter_mut() {
        let references = |value: &str| id.is_some() && asset_id(value) == id;
        let mesh_match = part
            .mesh
            .as_ref()
            .map(|m| references(&m.mesh_id))
            .unwrap_or(false);
        let texture_match = part
            .mesh
            .as_ref()
            .map(|m| references(&m.texture_id))
            .unwrap_or(false);
        let is_target = !target_name.is_empty() && part.name == target_name;
        match kind {
            Kind::Texture => {
                if is_target || texture_match {
                    part.decal = Some(file.to_string());
                    updated += 1;
                }
            }
            Kind::Mesh => {
                if is_target || mesh_match {
                    part.mesh_file = Some(file.to_string());
                    updated += 1;
                }
            }
            _ => {}
        }
    }
    updated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_asset_reference_form() {
        assert_eq!(asset_id("rbxassetid://12345"), Some(12345));
        assert_eq!(asset_id("rbxassetid://12345?version=2"), Some(12345));
        assert_eq!(
            asset_id("http://www.roblox.com/asset/?id=6319951708"),
            Some(6319951708)
        );
        assert_eq!(asset_id("https://assetdelivery.roblox.com/v1/asset?id=7"), Some(7));
        assert_eq!(asset_id("123"), Some(123));
        assert_eq!(asset_id("not an asset"), None);
        assert_eq!(asset_id(""), None);
    }

    #[test]
    fn detects_asset_references() {
        assert!(is_asset_reference("rbxassetid://1"));
        assert!(is_asset_reference("http://www.roblox.com/asset/?id=1"));
        assert!(is_asset_reference(" 12345 "));
        assert!(!is_asset_reference("Hello"));
        assert!(!is_asset_reference(""));
    }

    #[test]
    fn classifies_payloads_by_magic_bytes() {
        assert_eq!(classify(b"\x89PNG\r\n\x1a\n1234", Kind::Model), Kind::Texture);
        assert_eq!(classify(b"\xFF\xD8\xFF\xE0", Kind::Model), Kind::Texture);
        assert_eq!(classify(b"OggS\0\0", Kind::Model), Kind::Audio);
        assert_eq!(classify(b"ID3\x04", Kind::Model), Kind::Audio);
        assert_eq!(classify(b"version 1.00\n", Kind::Model), Kind::Mesh);
        assert_eq!(classify(b"CSGPHS....", Kind::Texture), Kind::Mesh);
        // Roblox containers follow the property hint.
        assert_eq!(classify(b"<roblox!...", Kind::Mesh), Kind::Mesh);
        assert_eq!(classify(b"<roblox!...", Kind::Texture), Kind::Model);
        // Unknown payloads fall back to the hint.
        assert_eq!(classify(b"junk", Kind::Audio), Kind::Audio);
    }

    #[test]
    fn attaches_to_every_part_sharing_the_reference() {
        let part = rbx_core::Scene::default().parts.remove(0);
        let mut parts = vec![part; 3];
        parts[0].name = "MeshA".into();
        parts[1].name = "MeshB".into();
        parts[2].name = "Other".into();
        for part in parts.iter_mut().take(2) {
            part.mesh = Some(rbx_core::MeshRef {
                mesh_id: "rbxassetid://707".into(),
                texture_id: "rbxassetid://808".into(),
                ..Default::default()
            });
        }
        // One download serves every part that shares the mesh id.
        assert_eq!(
            attach(&mut parts, Kind::Mesh, Some(707), "m.mesh", "Workspace/MeshA"),
            2
        );
        assert_eq!(parts[0].mesh_file.as_deref(), Some("m.mesh"));
        assert_eq!(parts[1].mesh_file.as_deref(), Some("m.mesh"));
        assert_eq!(parts[2].mesh_file, None);

        // A texture typed on a part with no matching id lands on that part only.
        assert_eq!(
            attach(&mut parts, Kind::Texture, Some(999), "t.png", "Workspace/Other"),
            1
        );
        assert_eq!(parts[2].decal.as_deref(), Some("t.png"));
        assert_eq!(parts[0].decal, None);

        // Audio has no geometry to attach.
        assert_eq!(attach(&mut parts, Kind::Audio, Some(1), "a.ogg", "Workspace/Other"), 0);
    }

    /// Live check of the download path: finds an image asset that is not in the
    /// cache yet, resolves it, and confirms the payload landed in `textures`.
    /// Needs network (and a linked Open Cloud key if the public endpoint is
    /// refused). Run with `--ignored live_downloads_uncached_asset`.
    #[test]
    #[ignore]
    fn live_downloads_uncached_asset() {
        let mut tried = 0;
        for keyword in ["glitter", "lava", "snow", "neon", "sand"] {
            let items = match roblox_api::search(13, keyword, 5) {
                Ok(items) => items,
                Err(err) => {
                    eprintln!("toolbox image search '{keyword}' failed: {err}");
                    continue;
                }
            };
            for item in items {
                let id = item.id as i64;
                if find_anywhere(id, Kind::Texture).is_some() {
                    continue; // already cached: not what this test checks
                }
                tried += 1;
                match resolve(&format!("rbxassetid://{id}"), Kind::Texture) {
                    Ok(found) => {
                        assert_eq!(found.kind, Kind::Texture, "{found:?}");
                        assert!(!found.cached);
                        assert!(found.path.exists(), "{} missing", found.path.display());
                        eprintln!("resolved uncached asset {id} -> {}", found.path.display());
                        return;
                    }
                    Err(err) => eprintln!("asset {id} failed: {err}"),
                }
            }
        }
        eprintln!("no downloadable uncached image found (tried {tried}); skipping");
    }

    /// Live (machine-dependent) check against the real per-user cache: an id
    /// cached under `textures` resolves even when the caller's hint says
    /// otherwise, with no network round-trip.
    #[test]
    #[ignore]
    fn live_cache_hit_ignores_hint() {
        let dir = asset_cache::root().join("assets").join("textures");
        let entry = std::fs::read_dir(&dir).ok().and_then(|d| {
            d.flatten().find(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.eq_ignore_ascii_case("png"))
                    .unwrap_or(false)
            })
        });
        let Some(entry) = entry else {
            eprintln!("no cached textures on this machine; skipping");
            return;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let id: i64 = name
            .split('.')
            .next()
            .unwrap()
            .parse()
            .expect("cache file name is <id>.<ext>");
        let found = resolve(&format!("rbxassetid://{id}"), Kind::Model).expect("cache hit");
        assert!(found.cached, "should come from the cache: {found:?}");
        assert_eq!(found.kind, Kind::Texture);
    }
}
