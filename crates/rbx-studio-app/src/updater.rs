//! GitHub Releases auto-updater.
//!
//! Checks `https://api.github.com/repos/<owner>/<repo>/releases/latest`, compares
//! the tag against the version this binary was built with, downloads the newest
//! Windows package into `%LOCALAPPDATA%\RustbloxStudio\updates` and applies it:
//!
//! * a portable `.exe` is swapped in by a detached `cmd` script that waits for
//!   this process to exit, copies the new binary over the old one and relaunches;
//! * an `.msi` is handed to `msiexec` after the same wait.
//!
//! No credentials are involved: the GitHub API is queried anonymously with an
//! honest `RustbloxStudio/<version>` user agent.

use anyhow::{anyhow, Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const OWNER: &str = "itsmeblocky";
pub const REPO: &str = "rustblox-studio";

const TIMEOUT: Duration = Duration::from_secs(20);
const USER_AGENT: &str = concat!("RustbloxStudio/", env!("CARGO_PKG_VERSION"));

/// Version this build reports; the workspace version from Cargo.toml.
pub fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// One downloadable file attached to a release.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// A published release as returned by the GitHub API.
#[derive(Clone, Debug, Default)]
pub struct Release {
    /// Tag without the leading `v` (for example `0.2.0`).
    pub version: String,
    pub notes: String,
    pub page_url: String,
    pub assets: Vec<Asset>,
}

/// The package an update should install.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageKind {
    /// Portable single `.exe`: self-replace + relaunch.
    Portable,
    /// Windows Installer package: hand to `msiexec`.
    Msi,
}

impl PackageKind {
    pub fn label(self) -> &'static str {
        match self {
            PackageKind::Portable => "portable executable",
            PackageKind::Msi => "Windows installer",
        }
    }
}

/// `%LOCALAPPDATA%\RustbloxStudio\updates`
pub fn update_root() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("RustbloxStudio").join("updates")
}

/// Parses `v1.2.3` / `1.2.3-beta.1` / `1.2` into a comparable triple.
pub fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    let trimmed = raw.trim();
    let trimmed = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    // Drop pre-release / build metadata: `1.2.3-rc.1` / `1.2.3+abc`.
    let core = trimmed
        .split(['-', '+'])
        .next()
        .unwrap_or(trimmed)
        .trim();
    if core.is_empty() {
        return None;
    }
    let mut parts = [0u64; 3];
    for (index, piece) in core.split('.').take(3).enumerate() {
        let digits: String = piece.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        parts[index] = digits.parse().ok()?;
    }
    Some((parts[0], parts[1], parts[2]))
}

/// `true` when `remote` is a strictly higher version than `local`.
pub fn is_newer(remote: &str, local: &str) -> bool {
    match (parse_version(remote), parse_version(local)) {
        (Some(remote), Some(local)) => remote > local,
        _ => false,
    }
}

/// `true` when the folder holding this executable can be written without
/// elevation. A per-machine MSI install lives under `Program Files`, where the
/// portable `.exe` swap cannot work, so the MSI itself is downloaded instead.
pub fn install_dir_writable() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return true;
    };
    let Some(dir) = exe.parent() else {
        return true;
    };
    let probe = dir.join(".rustblox-write-probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Chooses the package to install: a portable `.exe` when the install folder is
/// writable, otherwise the MSI for a proper elevated upgrade.
pub fn pick_asset(release: &Release) -> Option<(Asset, PackageKind)> {
    let find = |ext: &str| {
        release
            .assets
            .iter()
            .find(|asset| asset.name.to_ascii_lowercase().ends_with(ext))
            .cloned()
    };
    let exe = find(".exe");
    let msi = find(".msi");
    match (exe, msi) {
        (Some(exe), Some(msi)) => Some(if install_dir_writable() {
            (exe, PackageKind::Portable)
        } else {
            (msi, PackageKind::Msi)
        }),
        (Some(exe), None) => Some((exe, PackageKind::Portable)),
        (None, Some(msi)) => Some((msi, PackageKind::Msi)),
        (None, None) => None,
    }
}

fn http_error(url: &str) -> impl Fn(ureq::Error) -> anyhow::Error + '_ {
    move |error| match error {
        ureq::Error::Status(404, _) => anyhow!(
            "No release has been published for {OWNER}/{REPO} yet. \
             Push a `v<version>` tag with the release workflow to enable updates."
        ),
        ureq::Error::Status(403, response) => {
            let body = response.into_string().unwrap_or_default();
            if body.to_ascii_lowercase().contains("rate limit") {
                anyhow!("GitHub rate limit reached while checking for updates. Try again later.")
            } else {
                anyhow!("GitHub refused the update request (HTTP 403).")
            }
        }
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            let body = body.trim();
            if body.is_empty() {
                anyhow!("HTTP {code} from {url}")
            } else {
                anyhow!("HTTP {code} from {url}: {}", truncate(body, 240))
            }
        }
        other => anyhow!("request failed for {url}: {other}"),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

fn str_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Queries the latest published release.
pub fn check() -> Result<Release> {
    let url = format!("https://api.github.com/repos/{OWNER}/{REPO}/releases/latest");
    let response = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/vnd.github+json")
        .timeout(TIMEOUT)
        .call()
        .map_err(http_error(&url))?;
    let text = response
        .into_string()
        .with_context(|| format!("reading release metadata from {url}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parsing release metadata from {url}"))?;

    let tag = str_field(&value, "tag_name");
    let version = tag.trim_start_matches(['v', 'V']).to_string();
    if parse_version(&version).is_none() {
        return Err(anyhow!(
            "The latest release tag `{tag}` is not a version number, so it cannot be compared."
        ));
    }

    let assets = value
        .get("assets")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let name = str_field(item, "name");
                    let url = str_field(item, "browser_download_url");
                    if name.is_empty() || url.is_empty() {
                        return None;
                    }
                    Some(Asset {
                        name,
                        url,
                        size: item.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(Release {
        version,
        notes: str_field(&value, "body"),
        page_url: str_field(&value, "html_url"),
        assets,
    })
}

/// Downloads `asset` into the update folder, reporting a 0..1 progress value.
pub fn download(
    asset: &Asset,
    mut progress: impl FnMut(f64),
) -> Result<PathBuf> {
    let dir = update_root();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating update folder {}", dir.display()))?;
    let file_name = sanitize_file_name(&asset.name);
    let target = dir.join(&file_name);
    let temp = dir.join(format!("{file_name}.part"));

    let response = ureq::get(&asset.url)
        .set("User-Agent", USER_AGENT)
        .timeout(TIMEOUT)
        .call()
        .map_err(http_error(&asset.url))?;
    let total = response
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(asset.size);

    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(&temp)
        .with_context(|| format!("creating {}", temp.display()))?;
    let mut buffer = [0u8; 64 * 1024];
    let mut written: u64 = 0;
    let mut last_reported = f64::MIN;
    progress(0.0);
    loop {
        let read = reader
            .read(&mut buffer)
            .with_context(|| format!("downloading {}", asset.url))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .with_context(|| format!("writing {}", temp.display()))?;
        written += read as u64;
        if total > 0 {
            let fraction = (written as f64 / total as f64).clamp(0.0, 1.0);
            if fraction - last_reported >= 0.01 || fraction >= 1.0 {
                last_reported = fraction;
                progress(fraction);
            }
        }
    }
    file.flush().ok();
    drop(file);

    if written == 0 {
        let _ = std::fs::remove_file(&temp);
        return Err(anyhow!("The update package was empty (0 bytes)."));
    }
    if target.exists() {
        let _ = std::fs::remove_file(&target);
    }
    std::fs::rename(&temp, &target)
        .with_context(|| format!("moving download into {}", target.display()))?;
    progress(1.0);
    Ok(target)
}

fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.trim_matches('.').is_empty() {
        "RustbloxStudio-update.exe".to_string()
    } else {
        cleaned
    }
}

/// Applies a downloaded package and brings the new build up.
///
/// The caller must quit the process right after this returns `Ok(())`, because
/// the helper script waits for that exit before touching the running binary.
pub fn install(package: &Path, kind: PackageKind) -> Result<()> {
    if !package.is_file() {
        return Err(anyhow!(
            "The downloaded update is missing ({}). Download it again.",
            package.display()
        ));
    }
    let exe = std::env::current_exe().context("locating the running executable")?;
    let script = match kind {
        PackageKind::Portable => portable_script(package, &exe),
        PackageKind::Msi => msi_script(package),
    };
    let script_path = update_root().join("apply-update.cmd");
    std::fs::create_dir_all(update_root()).ok();
    {
        let mut file = std::fs::File::create(&script_path)
            .with_context(|| format!("writing update script {}", script_path.display()))?;
        file.write_all(script.as_bytes())?;
        file.flush()?;
    }
    spawn_detached(&script_path)
}

#[cfg(windows)]
fn spawn_detached(script: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("cmd.exe")
        .arg("/C")
        .arg(script)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("starting update helper {}", script.display()))?;
    Ok(())
}

#[cfg(not(windows))]
fn spawn_detached(_script: &Path) -> Result<()> {
    Err(anyhow!("Automatic updates are only supported on Windows."))
}

fn portable_script(package: &Path, target: &Path) -> String {
    let exe_name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "rbx-studio-app.exe".to_string());
    format!(
        "@echo off\r\n\
         rem Written by Rustblox Studio. Waits for the running build to exit, then\r\n\
         rem swaps the portable executable and relaunches it.\r\n\
         setlocal\r\n\
         ping -n 3 127.0.0.1 >nul\r\n\
         :wait\r\n\
         tasklist /FI \"IMAGENAME eq {exe_name}\" 2>nul | find /I \"{exe_name}\" >nul\r\n\
         if not errorlevel 1 (\r\n\
         \x20 ping -n 2 127.0.0.1 >nul\r\n\
         \x20 goto wait\r\n\
         )\r\n\
         copy /Y \"{package}\" \"{target}\" >nul\r\n\
         if errorlevel 1 (\r\n\
         \x20 echo Rustblox Studio could not replace the old executable. > \"%TEMP%\\rustblox-update-error.txt\"\r\n\
         \x20 pause\r\n\
         \x20 exit /b 1\r\n\
         )\r\n\
         start \"\" \"{target}\"\r\n\
         del \"{package}\" >nul 2>&1\r\n\
         (goto) 2>nul & del \"%~f0\"\r\n",
        exe_name = exe_name,
        package = package.display(),
        target = target.display(),
    )
}

fn msi_script(package: &Path) -> String {
    format!(
        "@echo off\r\n\
         rem Written by Rustblox Studio. Installs the newer MSI after this build exits.\r\n\
         setlocal\r\n\
         ping -n 3 127.0.0.1 >nul\r\n\
         msiexec /i \"{package}\" /passive /norestart\r\n\
         del \"{package}\" >nul 2>&1\r\n\
         (goto) 2>nul & del \"%~f0\"\r\n",
        package = package.display(),
    )
}

/// Opens a URL in the user's default browser.
pub fn open_url(url: &str) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd.exe")
            .args(["/C", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .with_context(|| format!("opening {url}"))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        Err(anyhow!("Opening links is only supported on Windows."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions_with_prefixes_and_suffixes() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("2.0.0-beta.4"), Some((2, 0, 0)));
        assert_eq!(parse_version(" 0.1.0 "), Some((0, 1, 0)));
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn prefers_portable_exe_over_msi() {
        let release = Release {
            assets: vec![
                Asset { name: "RustbloxStudio-0.2.0.msi".into(), url: "msi".into(), size: 1 },
                Asset { name: "RustbloxStudio-0.2.0-windows.exe".into(), url: "exe".into(), size: 2 },
            ],
            ..Default::default()
        };
        let (asset, kind) = pick_asset(&release).unwrap();
        assert_eq!(kind, PackageKind::Portable);
        assert_eq!(asset.url, "exe");
    }

    #[test]
    fn falls_back_to_msi_only() {
        let release = Release {
            assets: vec![Asset { name: "RustbloxStudio-0.2.0.msi".into(), url: "msi".into(), size: 1 }],
            ..Default::default()
        };
        let (_, kind) = pick_asset(&release).unwrap();
        assert_eq!(kind, PackageKind::Msi);
    }

    #[test]
    fn sanitizes_asset_names() {
        assert_eq!(sanitize_file_name("RustbloxStudio-0.2.0.exe"), "RustbloxStudio-0.2.0.exe");
        assert_eq!(sanitize_file_name("../evil path.exe"), ".._evil_path.exe");
    }

    #[test]
    #[ignore = "network: hits the public GitHub Releases API"]
    fn live_latest_release() {
        match check() {
            Ok(release) => {
                eprintln!(
                    "latest release: version={} page={} assets={:?}",
                    release.version, release.page_url, release.assets
                );
                assert!(parse_version(&release.version).is_some());
            }
            Err(e) => {
                // Before the first tagged release this is the expected path.
                eprintln!("no usable release yet: {e:#}");
            }
        }
    }
}
