//! Fetch Roblox Studio API dump and store it in `assets/studio/api/`.
//!
//! Usage:
//!   cargo run -p rbx-studio --bin fetch-api-dump
//!   cargo run -p rbx-studio --bin fetch-api-dump -- --force
//!   cargo run -p rbx-studio --bin fetch-api-dump -- --version version-67c8b1dedbb74fed
//!
//! "Every studio update": re-run this tool (or wire it into your update flow).
//! It checks https://setup.rbxcdn.com/versionQTStudio and re-downloads when the
//! version changes (or when --force is passed).

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".."))
}

fn api_dir() -> PathBuf {
    workspace_root().join("assets").join("studio").join("api")
}

fn read_text_url(url: &str) -> Result<String> {
    let out = Command::new("curl.exe")
        .args(["-fsSL", url])
        .output()
        .with_context(|| format!("running curl for {url}"))?;
    if !out.status.success() {
        bail!(
            "curl failed for {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

fn download_file(url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let status = Command::new("curl.exe")
        .args(["-fSL", url, "-o"])
        .arg(dest)
        .status()
        .with_context(|| format!("running curl download for {url}"))?;
    if !status.success() {
        bail!("curl download failed for {url}");
    }
    Ok(())
}

fn read_meta(path: &Path) -> Option<Value> {
    let s = fs::read_to_string(path).ok()?;
    let s = s.strip_prefix('\u{feff}').unwrap_or(&s);
    serde_json::from_str(s).ok()
}

fn main() -> Result<()> {
    let mut force = false;
    let mut version_override: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--force" | "-f" => force = true,
            "--version" => version_override = args.next(),
            "--help" | "-h" => {
                println!("fetch-api-dump: fetch Roblox Studio API dump to assets/studio/api/");
                println!("  --force        re-download even if version matches");
                println!("  --version V    use a specific version hash (e.g. version-67c8b1dedbb74fed)");
                return Ok(());
            }
            _ => {}
        }
    }

    let dir = api_dir();
    fs::create_dir_all(&dir)?;
    let dump_path = dir.join("API-Dump.json");
    let meta_path = dir.join("meta.json");

    let version = if let Some(v) = version_override {
        v
    } else {
        read_text_url("https://setup.rbxcdn.com/versionQTStudio")
            .context("fetching current Studio version (versionQTStudio)")?
    };
    let endpoint = format!("https://setup.rbxcdn.com/{version}-API-Dump.json");

    let stored = read_meta(&meta_path)
        .and_then(|m| m.get("version").and_then(|v| v.as_str()).map(|s| s.to_string()));

    if !force {
        if stored.as_deref() == Some(version.as_str()) && dump_path.is_file() {
            println!("API dump up to date: {version}");
            println!("  {}", dump_path.display());
            return Ok(());
        }
    }

    if let Some(s) = stored.as_deref() {
        if s != version {
            println!("Studio version changed: {s} -> {version}");
        }
    }
    println!("Downloading {endpoint}");
    download_file(&endpoint, &dump_path)?;

    let meta = json!({
        "version": version,
        "channel": "production",
        "source": "https://setup.rbxcdn.com/versionQTStudio",
        "endpoint": endpoint,
        "fetchedAt": chrono_like_now(),
    });
    fs::write(&meta_path, serde_json::to_string_pretty(&meta)? + "\n")?;
    let size = fs::metadata(&dump_path).map(|m| m.len()).unwrap_or(0);
    println!("Saved {} ({} bytes)", dump_path.display(), size);
    println!("Saved {}", meta_path.display());
    Ok(())
}

fn chrono_like_now() -> String {
    // Avoid adding chrono dep; use a simple UTC-ish timestamp via powershell/cmd fallback
    // or a coarse format. Best-effort.
    if let Ok(out) = Command::new("powershell")
        .args(["-NoProfile", "-Command", "(Get-Date).ToString('o')"])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    "unknown".to_string()
}
