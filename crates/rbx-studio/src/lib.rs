//! `rbx-studio` — the Rustblox Studio editor crate.
//!
//! Current modules:
//! - [`icons`] — registry for the extracted Roblox Studio icon set.
//! - [`theme`] — Roblox Foundation design tokens (colors, typography, spacing).
//! - [`layout`] — parsed Qtitan dock layouts (exact Studio panel arrangement).
//! - [`strings`] — localized UI strings from built-in plugins (en-us/source).
//! - [`ribbon`] — reference-derived ribbon tab/group/control definition.
//! - [`api_dump`] — Roblox API reflection (classes/properties/enums) from API-Dump.json.

pub mod api_dump;
pub mod icons;
pub mod layout;
pub mod ribbon;
pub mod strings;
pub mod theme;

pub use api_dump::{ApiDump, ApiEnum, ReflectionDatabase, ResolvedProperty, ValueType};
pub use icons::{IconRegistry, IconScale, ResolvedIcon, Theme};
pub use layout::{DockLayout, LayoutMode, PanelKind, WindowRect};
pub use ribbon::RibbonDefinition;
pub use strings::{StringBundle, Strings};
pub use theme::{ColorMode, DesignTokens, ThemeName, TokenColor, Typography};

use std::path::{Path, PathBuf};

/// Resolves the root of bundled Studio assets (`assets/studio`).
/// Prefers the dev workspace layout, then the executable directory (installed), then cwd.
pub fn studio_assets_root() -> PathBuf {
    let dev = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("assets")
        .join("studio");
    if dev.join("design").is_dir() || dev.join("icons").is_dir() {
        return dev.canonicalize().unwrap_or(dev);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("assets").join("studio");
            if cand.exists() {
                return cand;
            }
        }
    }
    let cwd = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("assets")
        .join("studio");
    if cwd.exists() {
        cwd
    } else {
        dev
    }
}
