//! Localized UI strings extracted from Studio's built-in plugins.
//!
//! Strings live in `assets/studio/strings/<bundle>.json`, produced by merging
//! the `LocalizationTable` instances found inside each built-in plugin's
//! `.rbxm` (see `rbx-inspect --dump-strings`). Only `en-us` values plus the
//! source strings are kept; the non-English values in the original dump are
//! encoding-damaged and can be re-extracted later.
//!
//! Keys use Studio's own namespacing, e.g.:
//! - `Studio.Ribbon.Plugin.Tab_Home`
//! - `Studio.Ribbon.Plugin.Tooltip_Part`

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One localization bundle (one plugin's strings).
#[derive(Debug, Clone, Default)]
pub struct Strings {
    plugin: String,
    keys: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct RawBundle {
    #[serde(default)]
    plugin: String,
    #[serde(default)]
    keys: BTreeMap<String, String>,
}

impl Strings {
    /// Load a bundle from a JSON file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading strings {}", path.display()))?;
        let bundle: RawBundle = serde_json::from_str(&raw)
            .with_context(|| format!("parsing strings {}", path.display()))?;
        Ok(Self {
            plugin: bundle.plugin,
            keys: bundle.keys,
        })
    }

    /// Load one of the bundled bundles by name, e.g. `"ribbon"`.
    pub fn bundled(name: &str) -> Result<Self> {
        Self::load(bundled_path(name))
    }

    /// Name of the plugin the strings belong to, e.g. `"ribbon"`.
    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.keys.keys().map(String::as_str)
    }

    /// Exact lookup.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.keys.get(key).map(String::as_str)
    }

    /// Lookup with a fallback, then the final path segment, then `key` itself.
    pub fn text<'a>(&'a self, key: &'a str) -> &'a str {
        if let Some(value) = self.get(key) {
            return value;
        }
        if let Some(short) = key.rsplit('.').next() {
            if let Some(value) = self.get(short) {
                return value;
            }
        }
        key
    }

    /// Lookup, returning `fallback` when the key is missing.
    pub fn text_or<'a>(&'a self, key: &str, fallback: &'a str) -> &'a str {
        self.get(key).unwrap_or(fallback)
    }
}

/// All bundles the Studio shell needs.
#[derive(Debug, Clone, Default)]
pub struct StringBundle {
    pub ribbon: Strings,
    pub explorer: Strings,
    pub properties: Strings,
    pub toolbox: Strings,
}

impl StringBundle {
    /// Load every bundled string table.
    pub fn load_bundled() -> Result<Self> {
        Ok(Self {
            ribbon: Strings::bundled("ribbon")?,
            explorer: Strings::bundled("explorer")?,
            properties: Strings::bundled("properties")?,
            toolbox: Strings::bundled("toolbox")?,
        })
    }
}

fn bundled_path(name: &str) -> PathBuf {
    crate::studio_assets_root()
        .join("strings")
        .join(format!("{name}.json"))
}
