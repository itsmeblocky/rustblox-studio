//! Ribbon definition derived from Studio reference captures.
//!
//! Source data lives in `assets/studio/design/ribbon/definition.json`. The
//! structure is intentionally simple and data-driven so it can be refined
//! against `assets/studio/reference/ribbon_*.png`.
//!
//! Icon paths are manifest-relative keys (under `assets/studio/icons/`) and can
//! be resolved through [`crate::IconRegistry`].

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RibbonDefinition {
    #[serde(default)]
    pub meta: RibbonMeta,
    #[serde(default)]
    pub tab_order: Vec<String>,
    #[serde(default)]
    pub tabs: Vec<RibbonTab>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RibbonMeta {
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub captured_tabs: Vec<String>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RibbonTab {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub label_key: Option<String>,
    #[serde(default)]
    pub dynamic: bool,
    #[serde(default)]
    pub groups: Vec<RibbonGroup>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RibbonGroup {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub controls: Vec<RibbonControl>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RibbonControl {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub label_key: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub icon_candidates: Vec<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

impl RibbonDefinition {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading ribbon definition {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("parsing ribbon definition {}", path.display()))
    }

    pub fn bundled() -> Result<Self> {
        Self::load(bundled_path())
    }

    pub fn tab(&self, id: &str) -> Option<&RibbonTab> {
        self.tabs.iter().find(|tab| tab.id.eq_ignore_ascii_case(id))
    }
}

impl RibbonTab {
    pub fn display_label<'a>(&'a self) -> &'a str {
        self.label
            .as_deref()
            .or(self.label_key.as_deref())
            .unwrap_or(self.id.as_str())
    }
}

impl RibbonControl {
    pub fn display_label<'a>(&'a self) -> &'a str {
        self.label
            .as_deref()
            .or(self.label_key.as_deref())
            .unwrap_or(self.id.as_str())
    }

    pub fn preferred_icon(&self) -> Option<&str> {
        self.icon
            .as_deref()
            .or_else(|| self.icon_candidates.first().map(String::as_str))
    }
}

fn bundled_path() -> PathBuf {
    crate::studio_assets_root()
        .join("design")
        .join("ribbon")
        .join("definition.json")
}
