use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::materials;

/// Reserve room for decal images (one layer each).
pub const DECAL_LAYERS: u32 = 48;
/// Edge length of every layer.
pub const TEX_SIZE: u32 = 256;

/// Number of per-material layers (one per known Roblox material + a fallback).
pub fn material_layers() -> u32 {
    (materials::MATERIALS.len() as u32) + 1
}

/// First dynamic decal layer.
pub fn decal_base() -> u32 {
    material_layers()
}

/// Back-compat aliases.
pub const MATERIAL_LAYERS: u32 = 0; // use `material_layers()`
pub const DECAL_BASE: u32 = 0; // use `decal_base()`

/// Material name -> texture array layer. Unknown materials map to 0 (fallback).
pub fn material_layer(material: &str) -> u32 {
    if let Some((idx, _)) = materials::MATERIALS
        .iter()
        .enumerate()
        .find(|(_, m)| m.name == material)
    {
        (idx as u32) + 1
    } else {
        match material {
            "Cement" | "Asphalt" => material_layer("Concrete"),
            _ => 0,
        }
    }
}

/// GPU-side texture arrays + the decal/material slot maps.
/// `view` is the color/albedo array, `metal_view` and `rough_view` provide
/// per-material PBR modulation sampled in the shader.
pub struct TextureBank {
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    metal_texture: wgpu::Texture,
    pub metal_view: wgpu::TextureView,
    rough_texture: wgpu::Texture,
    pub rough_view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    mips: u32,
    /// Decal file path -> array layer.
    slots: HashMap<PathBuf, u32>,
    /// Material names whose real color texture has been uploaded.
    uploaded_color: HashSet<String>,
    /// Material names whose real metalness texture has been uploaded.
    uploaded_metal: HashSet<String>,
    /// Material names whose real roughness texture has been uploaded.
    uploaded_rough: HashSet<String>,
    /// Material texture id -> resolved cache path, once found.
    material_cache_paths: HashMap<u64, PathBuf>,
    /// Number of decal layers currently in use.
    pub loaded: usize,
}

impl TextureBank {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let layers = material_layers() + DECAL_LAYERS;
        let mips = TEX_SIZE.ilog2() + 1;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("material-color-textures"),
            size: wgpu::Extent3d {
                width: TEX_SIZE,
                height: TEX_SIZE,
                depth_or_array_layers: layers,
            },
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("material-color-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let metal_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("material-metal-textures"),
            size: wgpu::Extent3d {
                width: TEX_SIZE,
                height: TEX_SIZE,
                depth_or_array_layers: layers,
            },
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let metal_view = metal_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("material-metal-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let rough_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("material-rough-textures"),
            size: wgpu::Extent3d {
                width: TEX_SIZE,
                height: TEX_SIZE,
                depth_or_array_layers: layers,
            },
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let rough_view = rough_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("material-rough-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("material-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let bank = Self {
            texture,
            view,
            metal_texture,
            metal_view,
            rough_texture,
            rough_view,
            sampler,
            mips,
            slots: HashMap::new(),
            uploaded_color: HashSet::new(),
            uploaded_metal: HashSet::new(),
            uploaded_rough: HashSet::new(),
            material_cache_paths: HashMap::new(),
            loaded: 0,
        };
        let neutral = solid_rgba(128, 128, 128, 255);
        // Layer 0: generic fallback. Then one procedural layer per material.
        let fallback = procedural_pattern("SmoothPlastic");
        upload_layer(queue, &bank.texture, bank.mips, 0, &fallback);
        upload_layer(queue, &bank.metal_texture, bank.mips, 0, &neutral);
        upload_layer(queue, &bank.rough_texture, bank.mips, 0, &neutral);
        for (i, m) in materials::MATERIALS.iter().enumerate() {
            let layer = (i as u32) + 1;
            let rgba = procedural_pattern(m.name);
            upload_layer(queue, &bank.texture, bank.mips, layer, &rgba);
            upload_layer(queue, &bank.metal_texture, bank.mips, layer, &neutral);
            upload_layer(queue, &bank.rough_texture, bank.mips, layer, &neutral);
        }
        let _ = device;
        bank
    }

    /// Array layer holding `path`, uploading the image on first use.
    pub fn decal_layer(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        path: &Path,
    ) -> Option<u32> {
        if let Some(layer) = self.slots.get(path) {
            return Some(*layer);
        }
        let layer = decal_base() + self.slots.len() as u32;
        if layer >= decal_base() + DECAL_LAYERS {
            return None;
        }
        let rgba = load_image_layer(path)?;
        upload_layer(queue, &self.texture, self.mips, layer, &rgba);
        let neutral = solid_rgba(128, 128, 128, 255);
        upload_layer(queue, &self.metal_texture, self.mips, layer, &neutral);
        upload_layer(queue, &self.rough_texture, self.mips, layer, &neutral);
        self.slots.insert(path.to_path_buf(), layer);
        self.loaded = self.slots.len();
        Some(layer)
    }

    /// Try to replace a material's procedural layers with cached Roblox texture maps
    /// (color, metalness, roughness). `textures_dir` points at the asset cache `textures/`
    /// folder (files named `<id>.<ext>`). Returns true if any real map was uploaded.
    pub fn ensure_material_texture(
        &mut self,
        queue: &wgpu::Queue,
        material: &str,
        textures_dir: Option<&Path>,
    ) -> bool {
        let Some(dir) = textures_dir else {
            return false;
        };
        let Some(info) = materials::material_info(material) else {
            return false;
        };
        let layer = material_layer(material);
        if layer == 0 {
            return false;
        }
        let mut any = false;
        if !self.uploaded_color.contains(material) {
            if let Some(id) = info.textures.color {
                if let Some(path) = self.resolve_material_cache_path(id, dir) {
                    if let Some(rgba) = load_image_layer(&path) {
                        upload_layer(queue, &self.texture, self.mips, layer, &rgba);
                        self.uploaded_color.insert(material.to_string());
                        any = true;
                    }
                }
            }
        } else {
            any = true;
        }
        if !self.uploaded_metal.contains(material) {
            if let Some(id) = info.textures.metalness {
                if let Some(path) = self.resolve_material_cache_path(id, dir) {
                    if let Some(rgba) = load_image_layer(&path) {
                        upload_layer(queue, &self.metal_texture, self.mips, layer, &rgba);
                        self.uploaded_metal.insert(material.to_string());
                        any = true;
                    }
                }
            }
        } else {
            any = true;
        }
        if !self.uploaded_rough.contains(material) {
            if let Some(id) = info.textures.roughness {
                if let Some(path) = self.resolve_material_cache_path(id, dir) {
                    if let Some(rgba) = load_image_layer(&path) {
                        upload_layer(queue, &self.rough_texture, self.mips, layer, &rgba);
                        self.uploaded_rough.insert(material.to_string());
                        any = true;
                    }
                }
            }
        } else {
            any = true;
        }
        any
    }

    fn resolve_material_cache_path(&mut self, id: u64, dir: &Path) -> Option<PathBuf> {
        if let Some(p) = self.material_cache_paths.get(&id) {
            if p.exists() {
                return Some(p.clone());
            }
        }
        let prefix = format!("{id}.");
        let mut best: Option<(u8, PathBuf)> = None;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with(&prefix) || name.ends_with(".json") {
                    continue;
                }
                let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
                let rank = match ext.as_str() {
                    "png" | "jpg" | "jpeg" | "webp" => 3,
                    "ktx2" => 2,
                    _ => 0,
                };
                if best.as_ref().map(|(r, _)| rank >= *r).unwrap_or(true) {
                    best = Some((rank, entry.path()));
                }
            }
        }
        let path = best.map(|(_, p)| p)?;
        self.material_cache_paths.insert(id, path.clone());
        Some(path)
    }

}

/// Uploads RGBA8 data (TEX_SIZE x TEX_SIZE) for one array layer, with a full mip chain.
fn upload_layer(queue: &wgpu::Queue, target: &wgpu::Texture, mips: u32, layer: u32, base: &[u8]) {
    let mut level_data = base.to_vec();
    let mut size = TEX_SIZE;
    for level in 0..mips {
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: target,
                mip_level: level,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &level_data,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(size * 4),
                rows_per_image: Some(size),
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
        if size == 1 {
            break;
        }
        level_data = downsample(&level_data, size);
        size /= 2;
    }
}

fn downsample(src: &[u8], size: u32) -> Vec<u8> {
    let half = (size / 2).max(1);
    let mut out = Vec::with_capacity((half * half * 4) as usize);
    for y in 0..half {
        for x in 0..half {
            let mut acc = [0u32; 4];
            for dy in 0..2 {
                for dx in 0..2 {
                    let sx = (x * 2 + dx).min(size - 1);
                    let sy = (y * 2 + dy).min(size - 1);
                    let i = ((sy * size + sx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += src[i + c] as u32;
                    }
                }
            }
            for c in 0..4 {
                out.push((acc[c] / 4) as u8);
            }
        }
    }
    out
}

fn solid_rgba(r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
    let n = (TEX_SIZE * TEX_SIZE) as usize;
    let mut out = vec![0u8; n * 4];
    for px in out.chunks_exact_mut(4) {
        px[0] = r;
        px[1] = g;
        px[2] = b;
        px[3] = a;
    }
    out
}

/// Decodes an image file and stretches it into a `TEX_SIZE`^2 RGBA layer.
fn load_image_layer(path: &Path) -> Option<Vec<u8>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[textures] cannot read {}: {e}", path.display());
            return None;
        }
    };
    let img = match image::load_from_memory(&bytes) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("[textures] cannot decode {}: {e}", path.display());
            return None;
        }
    };
    let img = img.resize_exact(TEX_SIZE, TEX_SIZE, image::imageops::FilterType::Triangle);
    Some(img.to_rgba8().into_raw())
}

// ---------------------------------------------------------------------------
// Procedural material patterns (grayscale detail that multiplies part colour)
// ---------------------------------------------------------------------------

fn procedural_pattern(name: &str) -> Vec<u8> {
    let n = TEX_SIZE as usize;
    let mut out = vec![255u8; n * n * 4];
    let mut put = |x: usize, y: usize, v: f32| {
        let i = (y * n + x) * 4;
        let c = (v.clamp(0.0, 1.0) * 255.0) as u8;
        out[i] = c;
        out[i + 1] = c;
        out[i + 2] = c;
        out[i + 3] = 255;
    };
    let cell = 64usize; // 4x4 pattern repeats per tile
    for y in 0..n {
        for x in 0..n {
            let fx = x as f32;
            let fy = y as f32;
            let v = match name {
                "Plastic" => studs(fx, fy, cell),
                "SmoothPlastic" => 0.985 + noise2(x, y, 5) * 0.015,
                "Slate" | "Rock" | "Basalt" | "Limestone" | "Sandstone" => {
                    0.62 + noise2(x, y, 24) * 0.34 + noise2(x, y, 6) * 0.06
                }
                "Concrete" | "Asphalt" | "Pavement" | "Cement" => {
                    0.80 + noise2(x, y, 40) * 0.14 + blotch(x, y) * 0.06
                }
                "Brick" | "ClayRoofTiles" | "RoofShingles" => brick(fx, fy, cell),
                "Wood" | "WoodPlanks" => wood(fx, fy, cell),
                "Grass" | "LeafyGrass" | "Ground" => {
                    0.55 + noise2(x, y, 8) * 0.38 + noise2(x, y, 2) * 0.07
                }
                "Sand" | "Snow" | "Mud" | "Salt" | "Glacier" => 0.86 + noise2(x, y, 60) * 0.12,
                "Metal" | "DiamondPlate" | "CorrodedMetal" | "Foil" => diamond_plate(fx, fy, cell),
                "Fabric" | "Carpet" | "Leather" => fabric(fx, fy),
                "Marble" | "CeramicTiles" | "Plaster" => marble(fx, fy),
                "Cobblestone" | "Pebble" => cobble(fx, fy, cell),
                "Ice" | "Glass" => 0.90 + noise2(x, y, 12) * 0.10,
                "Granite" => 0.68 + noise2(x, y, 20) * 0.22 + noise2(x, y, 5) * 0.10,
                "Neon" | "ForceField" => 1.0,
                "Cardboard" => 0.82 + noise2(x, y, 14) * 0.10 + ((fx * 0.4).sin() * 0.03),
                "Rubber" | "CrackedLava" => 0.78 + noise2(x, y, 18) * 0.18,
                _ => 0.97 + noise2(x, y, 3) * 0.03,
            };
            put(x, y, v);
        }
    }
    out
}

/// Deterministic 0..1 value noise from integer coordinates.
fn hash2(x: i32, y: i32, salt: i32) -> f32 {
    let mut h = (x.wrapping_mul(374761393))
        .wrapping_add(y.wrapping_mul(668265263))
        .wrapping_add(salt.wrapping_mul(2_147_483_647));
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h ^ (h >> 16)) & 0xFFFF) as f32 / 65535.0
}

/// Smooth-interpolated value noise at a given cell size (in pixels).
fn noise2(x: usize, y: usize, scale: i32) -> f32 {
    let scale = scale.max(1);
    let fx = x as f32 / scale as f32;
    let fy = y as f32 / scale as f32;
    let x0 = fx.floor() as i32;
    let y0 = fy.floor() as i32;
    let tx = smooth(fx - x0 as f32);
    let ty = smooth(fy - y0 as f32);
    let a = hash2(x0, y0, scale);
    let b = hash2(x0 + 1, y0, scale);
    let c = hash2(x0, y0 + 1, scale);
    let d = hash2(x0 + 1, y0 + 1, scale);
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    top + (bottom - top) * ty
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn blotch(x: usize, y: usize) -> f32 {
    noise2(x, y, 90) * noise2(x, y, 33)
}

/// Classic plastic studs: beveled square tiles with a raised round stud.
fn studs(fx: f32, fy: f32, cell: usize) -> f32 {
    let c = cell as f32;
    let cx = fx % c - c * 0.5;
    let cy = fy % c - c * 0.5;
    let r = (cx * cx + cy * cy).sqrt() / c;
    let edge = (cx.abs().max(cy.abs())) / (c * 0.5);
    let tile = if edge > 0.94 { 0.86 } else { 0.95 };
    if r < 0.30 {
        let hx = cx / c + 0.16;
        let hy = cy / c + 0.16;
        let shade = 1.0 - (hx * hx + hy * hy) * 3.4;
        (0.93 + shade.clamp(-0.16, 0.07)).max(0.78)
    } else if r < 0.34 {
        0.74
    } else {
        tile
    }
}

fn brick(fx: f32, fy: f32, cell: usize) -> f32 {
    let h = cell as f32 * 0.5;
    let w = cell as f32;
    let row = (fy / h).floor();
    let offset = if (row as i32) % 2 == 0 { 0.0 } else { w * 0.5 };
    let bx = ((fx + offset) % w) / w;
    let by = (fy % h) / h;
    let mortar = bx < 0.04 || bx > 0.96 || by < 0.08 || by > 0.92;
    if mortar {
        0.52 + noise2(fx as usize, fy as usize, 4) * 0.08
    } else {
        0.80 + noise2(fx as usize, fy as usize, 8) * 0.16
    }
}

fn wood(fx: f32, fy: f32, cell: usize) -> f32 {
    let plank = cell as f32;
    let py = (fy % plank) / plank;
    let gap = py < 0.05 || py > 0.95;
    let grain = ((fx * 0.11).sin() * 0.5 + 0.5) * 0.12;
    if gap {
        0.55
    } else {
        0.82 + grain + noise2(fx as usize, fy as usize, 5) * 0.08
    }
}

fn diamond_plate(fx: f32, fy: f32, cell: usize) -> f32 {
    let c = cell as f32 * 0.5;
    let lx = fx % c - c * 0.5;
    let ly = fy % c - c * 0.5;
    let d = (lx.abs() + ly.abs()) / c;
    let brushed = noise2(fx as usize, fy as usize, 2) * 0.06;
    if (0.18..0.30).contains(&d) {
        0.98 - brushed
    } else {
        0.80 + brushed
    }
}

fn fabric(fx: f32, fy: f32) -> f32 {
    let weave_x = ((fx * 1.6).sin() * 0.5 + 0.5) * 0.10;
    let weave_y = ((fy * 1.6).sin() * 0.5 + 0.5) * 0.10;
    0.80 + weave_x + weave_y
}

fn marble(fx: f32, fy: f32) -> f32 {
    let x = fx as usize;
    let y = fy as usize;
    let vein = ((fx * 0.05 + noise2(x, y, 40) * 6.0).sin() * 0.5 + 0.5).powf(6.0);
    0.98 - vein * 0.35
}

fn cobble(fx: f32, fy: f32, cell: usize) -> f32 {
    let c = cell as f32;
    let gx = fx / c;
    let gy = fy / c;
    let jitter_x = hash2(gx as i32, gy as i32, 11) * 0.35;
    let jitter_y = hash2(gx as i32, gy as i32, 17) * 0.35;
    let dx = gx.fract() - 0.5 - jitter_x * 0.2;
    let dy = gy.fract() - 0.5 - jitter_y * 0.2;
    let r = (dx * dx + dy * dy).sqrt();
    if r > 0.42 {
        0.42
    } else {
        0.74 + (0.42 - r) * 0.4 + noise2(fx as usize, fy as usize, 6) * 0.06
    }
}
