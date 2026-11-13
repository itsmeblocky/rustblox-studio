//! Loading and lookup of the extracted Roblox Studio icon set.
//!
//! Icons live in `assets/studio/icons/` and are indexed by a generated
//! `manifest.json` that maps each logical icon key to its `1x`/`2x`/`3x`
//! file variants.
//!
//! A logical key looks like:
//! `Shared/InsertableObjects/Dark/Standard/Part.png`
//!
//! Lookups accept the key with or without the trailing `.png`, and with
//! forward or back slashes.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Which scale variant of an icon to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconScale {
    X1,
    X2,
    X3,
}

impl IconScale {
    /// Variant key as used in the manifest.
    pub fn manifest_key(self) -> &'static str {
        match self {
            IconScale::X1 => "1x",
            IconScale::X2 => "2x",
            IconScale::X3 => "3x",
        }
    }

    /// Order in which to try variants when the requested one is missing.
    /// Prefer larger (upscaling looks better than downscaling), then smaller.
    pub fn fallback_order(self) -> [IconScale; 3] {
        match self {
            IconScale::X1 => [IconScale::X1, IconScale::X2, IconScale::X3],
            IconScale::X2 => [IconScale::X2, IconScale::X3, IconScale::X1],
            IconScale::X3 => [IconScale::X3, IconScale::X2, IconScale::X1],
        }
    }

    /// Choose a scale from a DPI scale factor (e.g. `1.0`, `1.5`, `2.0`).
    pub fn from_dpi(scale: f32) -> IconScale {
        if scale >= 2.5 {
            IconScale::X3
        } else if scale >= 1.5 {
            IconScale::X2
        } else {
            IconScale::X1
        }
    }
}

/// Roblox Studio theme variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Theme {
    Dark,
    Light,
}

impl Theme {
    pub fn dir_name(self) -> &'static str {
        match self {
            Theme::Dark => "Dark",
            Theme::Light => "Light",
        }
    }
}

/// The available file variants for one logical icon.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct IconVariants {
    #[serde(rename = "1x", default)]
    pub one_x: Option<String>,
    #[serde(rename = "2x", default)]
    pub two_x: Option<String>,
    #[serde(rename = "3x", default)]
    pub three_x: Option<String>,
}

impl IconVariants {
    pub fn get(&self, scale: IconScale) -> Option<&str> {
        match scale {
            IconScale::X1 => self.one_x.as_deref(),
            IconScale::X2 => self.two_x.as_deref(),
            IconScale::X3 => self.three_x.as_deref(),
        }
    }

    /// Resolve using `scale`'s fallback order, returning the first available variant.
    pub fn resolve(&self, scale: IconScale) -> Option<(&'static str, &str)> {
        for candidate in scale.fallback_order() {
            if let Some(path) = self.get(candidate) {
                return Some((candidate.manifest_key(), path));
            }
        }
        None
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ManifestEntry {
    key: String,
    variants: IconVariants,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct Manifest {
    #[serde(default)]
    generated_from: String,
    #[serde(default)]
    total_unique: usize,
    #[serde(default)]
    icons: Vec<ManifestEntry>,
}

/// A resolved icon on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIcon {
    /// Absolute (or root-joined) path to the image file.
    pub path: PathBuf,
    /// Which scale variant was actually selected after fallback.
    pub scale: IconScale,
}

/// Index of every extracted Studio icon.
#[derive(Debug, Clone)]
pub struct IconRegistry {
    root: PathBuf,
    generated_from: String,
    entries: BTreeMap<String, IconVariants>,
}

impl IconRegistry {
    /// Load a registry from a directory containing `manifest.json`
    /// (normally `assets/studio/icons`).
    pub fn load(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let manifest_path = root.join("manifest.json");
        let raw = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading icon manifest {}", manifest_path.display()))?;
        let manifest: Manifest = serde_json::from_str(&raw)
            .with_context(|| format!("parsing icon manifest {}", manifest_path.display()))?;

        let mut entries = BTreeMap::new();
        for entry in manifest.icons {
            entries.insert(normalize_key(&entry.key), entry.variants);
        }

        Ok(Self {
            root,
            generated_from: manifest.generated_from,
            entries,
        })
    }

    /// Load from the workspace's extracted assets (developer convenience).
    /// Resolves `CARGO_MANIFEST_DIR/../../assets/studio/icons`.
    pub fn load_bundled() -> Result<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("assets")
            .join("studio")
            .join("icons");
        Self::load(root)
    }

    /// Directory the icons were loaded from.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Roblox Studio build the icons were extracted from.
    pub fn generated_from(&self) -> &str {
        &self.generated_from
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all logical icon keys.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Look up an icon by its exact logical key (with or without `.png`).
    pub fn path(&self, key: &str, scale: IconScale) -> Option<ResolvedIcon> {
        let key = normalize_key(key);
        let variants = self.entries.get(&key)?;
        let (_, rel) = variants.resolve(scale)?;
        Some(ResolvedIcon {
            path: self.root.join(rel),
            scale: scale_of_variant(rel).unwrap_or(scale),
        })
    }

    /// Look up an icon by a loose name (e.g. `"Part"`), searching all themes/sizes.
    ///
    /// Prefers a `Dark` `Standard` match, then any theme, then any size.
    pub fn find(&self, name: &str, scale: IconScale) -> Option<ResolvedIcon> {
        let name = name.trim_end_matches(".png");
        if name.contains('/') || name.contains('\\') {
            return self.path(name, scale);
        }

        let file = format!("/{name}.png");
        let mut fallback: Option<&IconVariants> = None;
        for (key, variants) in &self.entries {
            if key.ends_with(&file) {
                if key.contains("/Dark/Standard/") {
                    return self.resolve_entry(variants, scale);
                }
                if fallback.is_none() {
                    fallback = Some(variants);
                }
            }
        }
        fallback.and_then(|v| self.resolve_entry(v, scale))
    }

    /// Class icon for an Explorer / Insert Object entry, e.g. `class_icon("Part", Theme::Dark, X1)`.
    pub fn class_icon(&self, class_name: &str, theme: Theme, scale: IconScale) -> Option<ResolvedIcon> {
        let class_name = class_name.trim_end_matches(".png");
        let key = format!(
            "Shared/InsertableObjects/{}/Standard/{}.png",
            theme.dir_name(),
            class_name
        );
        self.path(&key, scale)
    }

    /// Ribbon / toolbar icon, e.g. `ribbon_icon("AddCollaborator", Theme::Dark, X1)`.
    pub fn ribbon_icon(&self, name: &str, theme: Theme, scale: IconScale) -> Option<ResolvedIcon> {
        let name = name.trim_end_matches(".png");
        let key = format!("Shared/Ribbon/{}/Standard/{}.png", theme.dir_name(), name);
        self.path(&key, scale)
    }

    /// Panel / widget icon, e.g. `widget_icon("Explorer", Theme::Dark, X1)`.
    pub fn widget_icon(&self, name: &str, theme: Theme, scale: IconScale) -> Option<ResolvedIcon> {
        let name = name.trim_end_matches(".png");
        let key = format!("Shared/WidgetIcons/{}/Standard/{}.png", theme.dir_name(), name);
        self.path(&key, scale)
    }

    fn resolve_entry(&self, variants: &IconVariants, scale: IconScale) -> Option<ResolvedIcon> {
        let (_, rel) = variants.resolve(scale)?;
        Some(ResolvedIcon {
            path: self.root.join(rel),
            scale: scale_of_variant(rel).unwrap_or(scale),
        })
    }
}

fn scale_of_variant(rel: &str) -> Option<IconScale> {
    if rel.ends_with("@3x.png") {
        Some(IconScale::X3)
    } else if rel.ends_with("@2x.png") {
        Some(IconScale::X2)
    } else if rel.ends_with(".png") {
        Some(IconScale::X1)
    } else {
        None
    }
}

/// Normalize an icon key: forward slashes, no leading slash, no variant suffix, no `.png`.
pub fn normalize_key(key: &str) -> String {
    let mut key = key.trim().replace('\\', "/");
    while key.starts_with('/') {
        key.remove(0);
    }
    if let Some(stripped) = key.strip_suffix("@3x.png") {
        key = stripped.to_string();
    } else if let Some(stripped) = key.strip_suffix("@2x.png") {
        key = stripped.to_string();
    } else if let Some(stripped) = key.strip_suffix(".png") {
        key = stripped.to_string();
    }
    key
}

/// Resolve icon paths for a batch of keys, reporting which are missing.
pub fn check_missing(registry: &IconRegistry, keys: &[&str], scale: IconScale) -> Vec<String> {
    let mut missing = Vec::new();
    for key in keys {
        if registry.path(key, scale).is_none() {
            missing.push(normalize_key(key));
        }
    }
    missing
}

/// Count of icons per top-level category (useful for diagnostics).
pub fn category_counts(registry: &IconRegistry) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for key in registry.keys() {
        let category = key
            .split_once('/')
            .map(|(head, _)| head.to_string())
            .unwrap_or_else(|| "<root>".to_string());
        *counts.entry(category).or_insert(0) += 1;
    }
    counts
}
