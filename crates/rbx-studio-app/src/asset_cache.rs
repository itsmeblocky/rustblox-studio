//! On-disk cache for downloaded Roblox assets.
//!
//! Toolbox inserts download models, meshes, textures and audio through the
//! Open Cloud asset-delivery API. Re-downloading on every insert (or every
//! app start) is wasteful, so each payload is written to a per-user cache:
//!
//! ```text
//! %LOCALAPPDATA%\RustbloxStudio\cache\assets\{models,meshes,textures,audio}\
//!     <assetId>.<ext>          asset bytes
//!     <assetId>.json           metadata sidecar (type, size, timestamp)
//! ```
//!
//! Nothing secret is cached: the API key and signed CDN URLs never reach the
//! disk. Only the asset payload itself (the same bytes Roblox serves to the
//! client) plus non-sensitive metadata.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Which asset family a cached file belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Model,
    Mesh,
    Texture,
    Audio,
}

impl Kind {
    pub fn dir(self) -> &'static str {
        match self {
            Kind::Model => "models",
            Kind::Mesh => "meshes",
            Kind::Texture => "textures",
            Kind::Audio => "audio",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Model => "model",
            Kind::Mesh => "mesh",
            Kind::Texture => "texture",
            Kind::Audio => "audio",
        }
    }
}

/// Metadata sidecar written next to every cached asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub id: i64,
    pub kind: String,
    pub asset_type_id: Option<i64>,
    pub content_type: String,
    pub bytes: u64,
    pub fetched_unix: u64,
    pub file: String,
}

/// Root of the Rustblox asset cache.
pub fn root() -> PathBuf {
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("RustbloxStudio").join("cache");
    }
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("rustbloxstudio")
}

fn assets_dir() -> PathBuf {
    root().join("assets")
}

pub fn kind_dir(kind: Kind) -> PathBuf {
    assets_dir().join(kind.dir())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Cache rank for a payload extension: higher is more useful. A Toolbox image
/// or mesh asset is sometimes served as a `.rbxm` wrapper, so the decoded
/// payload (`png`, `mesh`) must win over that container on a cache lookup.
fn ext_rank(kind: Kind, ext: &str) -> u8 {
    match kind {
        Kind::Model => match ext {
            "rbxm" | "rbxmx" => 2,
            _ => 0,
        },
        Kind::Mesh => match ext {
            "mesh" => 3,
            "csgphs" => 2,
            _ => 0,
        },
        Kind::Texture => match ext {
            "png" | "jpg" | "jpeg" | "webp" => 3,
            "ktx2" => 2,
            _ => 0,
        },
        Kind::Audio => match ext {
            "ogg" | "mp3" | "wav" | "flac" => 3,
            _ => 0,
        },
    }
}

/// Cached payload for an asset, if one was downloaded before. When several
/// files exist for an id, the most directly usable one is returned.
pub fn find(kind: Kind, id: i64) -> Option<PathBuf> {
    let dir = kind_dir(kind);
    let prefix = format!("{id}.");
    let mut best: Option<(u8, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(&prefix) || name.ends_with(".json") {
            continue;
        }
        let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let rank = ext_rank(kind, &ext);
        if best.as_ref().map(|(r, _)| rank >= *r).unwrap_or(true) {
            best = Some((rank, entry.path()));
        }
    }
    best.map(|(_, path)| path)
}

/// Reads a previously cached asset.
pub fn read(kind: Kind, id: i64) -> Option<Vec<u8>> {
    std::fs::read(find(kind, id)?).ok()
}

/// Writes an asset payload plus its metadata sidecar; returns the file path.
pub fn store(
    kind: Kind,
    id: i64,
    ext: &str,
    bytes: &[u8],
    asset_type_id: Option<i64>,
    content_type: &str,
) -> Result<PathBuf> {
    let dir = kind_dir(kind);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let ext = ext.trim_start_matches('.');
    let ext = if ext.is_empty() { "bin" } else { ext };
    let file = dir.join(format!("{id}.{ext}"));
    std::fs::write(&file, bytes).with_context(|| format!("writing {}", file.display()))?;

    let meta = Meta {
        id,
        kind: kind.label().to_string(),
        asset_type_id,
        content_type: content_type.to_string(),
        bytes: bytes.len() as u64,
        fetched_unix: unix_now(),
        file: file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
    };
    let meta_path = dir.join(format!("{id}.json"));
    if let Ok(json) = serde_json::to_vec_pretty(&meta) {
        let _ = std::fs::write(meta_path, json);
    }
    Ok(file)
}

/// File extension for a payload, sniffed from its magic bytes (content-type as
/// a fallback). Extensions are what the renderer/`image` crate dispatch on.
pub fn ext_for(kind: Kind, bytes: &[u8], content_type: &str) -> &'static str {
    let starts = |magic: &[u8]| bytes.starts_with(magic);
    let ct = content_type.to_ascii_lowercase();
    match kind {
        Kind::Model => {
            if starts(b"<roblox!") {
                "rbxm"
            } else if bytes.first() == Some(&b'<') {
                "rbxmx"
            } else {
                "bin"
            }
        }
        Kind::Mesh => {
            if starts(b"version ") {
                "mesh"
            } else if starts(b"CSGPHS") {
                "csgphs"
            } else if starts(b"<roblox!") {
                "rbxm"
            } else if bytes.first() == Some(&b'<') {
                "rbxmx"
            } else {
                "bin"
            }
        }
        Kind::Texture => {
            if starts(b"\x89PNG\r\n\x1a\n") {
                "png"
            } else if starts(b"<roblox!") {
                "rbxm"
            } else if bytes.first() == Some(&b'<') {
                "rbxmx"
            } else if starts(b"\xFF\xD8\xFF") {
                "jpg"
            } else if starts(b"GIF8") {
                "gif"
            } else if starts(b"BM") {
                "bmp"
            } else if starts(b"RIFF") && bytes.len() > 11 && &bytes[8..12] == b"WEBP" {
                "webp"
            } else if starts(b"KTX2") || starts(b"\xABKTX") {
                "ktx2"
            } else if ct.contains("png") {
                "png"
            } else {
                "img"
            }
        }
        Kind::Audio => {
            if starts(b"OggS") {
                "ogg"
            } else if starts(b"ID3") {
                "mp3"
            } else if starts(b"RIFF") && bytes.len() > 11 && &bytes[8..12] == b"WAVE" {
                "wav"
            } else if starts(b"fLaC") {
                "flac"
            } else if ct.contains("ogg") {
                "ogg"
            } else if ct.contains("mpeg") || ct.contains("mp3") {
                "mp3"
            } else {
                "audio"
            }
        }
    }
}

/// Human-readable cache location, for the Output console.
pub fn describe() -> String {
    root().display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_points_at_the_cache_root() {
        assert!(root().display().to_string().to_lowercase().contains("cache"));
    }

    #[test]
    fn extensions_follow_the_payload() {
        assert_eq!(ext_for(Kind::Model, b"<roblox!", ""), "rbxm");
        assert_eq!(ext_for(Kind::Model, b"<roblox ", ""), "rbxmx");
        assert_eq!(ext_for(Kind::Audio, b"OggS", ""), "ogg");
        assert_eq!(ext_for(Kind::Texture, b"\x89PNG\r\n\x1a\n", ""), "png");
        assert_eq!(ext_for(Kind::Mesh, b"version 1.00\n", ""), "mesh");
    }
}
