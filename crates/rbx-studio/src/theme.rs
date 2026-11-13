//! Roblox Foundation design tokens (colors, typography, spacing, sizes, motion).
//!
//! Tokens are extracted from the shipped `RbxDesignFoundations` theme files by
//! the `extract-tokens` binary and stored as JSON under
//! `assets/studio/design/tokens/<Theme>_<Mode>.json`.
//!
//! Lookups use dotted paths, e.g. `"Color.Surface.Surface_0"`,
//! `"Size.Size_400"`, `"Padding.Medium"`.

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

/// A token color: sRGB 0–255 plus Roblox-style transparency (0 = opaque).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub transparency: f32,
}

impl TokenColor {
    /// Alpha as used by renderers (1.0 = opaque).
    pub fn alpha(&self) -> f32 {
        1.0 - self.transparency
    }

    /// RGBA8 with alpha derived from transparency.
    pub fn to_rgba8(&self) -> [u8; 4] {
        [
            self.r,
            self.g,
            self.b,
            (self.alpha().clamp(0.0, 1.0) * 255.0).round() as u8,
        ]
    }

    /// Linear-ish float RGBA for wgpu/egui (sRGB channels normalized).
    pub fn to_f32(&self) -> [f32; 4] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.alpha().clamp(0.0, 1.0),
        ]
    }
}

/// A resolved typography token.
#[derive(Debug, Clone, PartialEq)]
pub struct Typography {
    pub family: String,
    pub weight: String,
    pub style: String,
    pub font_size: f32,
    pub letter_spacing: f32,
    pub line_height: f32,
}

/// Which shipped theme to load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    Default,
    Classic,
}

impl ThemeName {
    pub fn file_prefix(self) -> &'static str {
        match self {
            ThemeName::Default => "Default",
            ThemeName::Classic => "Classic",
        }
    }
}

/// Which color mode to load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Dark,
    Light,
}

impl ColorMode {
    pub fn file_suffix(self) -> &'static str {
        match self {
            ColorMode::Dark => "Dark",
            ColorMode::Light => "Light",
        }
    }
}

/// A loaded set of design tokens.
#[derive(Debug, Clone)]
pub struct DesignTokens {
    root: Value,
}

impl DesignTokens {
    /// Load tokens from a JSON file produced by `extract-tokens`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading design tokens {}", path.display()))?;
        let root: Value = serde_json::from_str(&raw)
            .with_context(|| format!("parsing design tokens {}", path.display()))?;
        Ok(Self { root })
    }

    /// Load the bundled tokens for a theme + color mode.
    pub fn bundled(theme: ThemeName, mode: ColorMode) -> Result<Self> {
        let path = crate::studio_assets_root()
            .join("design")
            .join("tokens")
            .join(format!("{}_{}.json", theme.file_prefix(), mode.file_suffix()));
        Self::load(path)
    }

    /// Raw JSON value at a dotted path.
    pub fn get(&self, path: &str) -> Option<&Value> {
        let mut current = &self.root;
        for segment in path.split('.') {
            current = current.get(segment)?;
        }
        Some(current)
    }

    pub fn number(&self, path: &str) -> Option<f32> {
        self.get(path)?.as_f64().map(|n| n as f32)
    }

    pub fn string(&self, path: &str) -> Option<&str> {
        self.get(path)?.as_str()
    }

    /// Resolve a color token. Accepts either `"Color.<group>.<name>"` or a
    /// path directly to a `{ Color3, Transparency }` object.
    pub fn color(&self, path: &str) -> Option<TokenColor> {
        let node = self.get(path)?;
        let rgb = node.get("Color3")?.get("$rgb")?.as_array()?;
        if rgb.len() < 3 {
            return None;
        }
        let r = rgb[0].as_f64()?.round().clamp(0.0, 255.0) as u8;
        let g = rgb[1].as_f64()?.round().clamp(0.0, 255.0) as u8;
        let b = rgb[2].as_f64()?.round().clamp(0.0, 255.0) as u8;
        let transparency = node
            .get("Transparency")
            .and_then(Value::as_f64)
            .unwrap_or(0.0) as f32;
        Some(TokenColor {
            r,
            g,
            b,
            transparency,
        })
    }

    /// Convenience: `Color.Surface.Surface_0` → panel background.
    pub fn surface(&self, name: &str) -> Option<TokenColor> {
        self.color(&format!("Color.Surface.{name}"))
    }

    /// Convenience: `Color.Content.<name>`.
    pub fn content(&self, name: &str) -> Option<TokenColor> {
        self.color(&format!("Color.Content.{name}"))
    }

    /// Resolve a typography token such as `"BodyMedium"`.
    pub fn typography(&self, name: &str) -> Option<Typography> {
        let node = self.get(&format!("Typography.{name}"))?;
        let font = node.get("Font")?.get("$font")?;
        Some(Typography {
            family: font.get("family")?.as_str()?.to_string(),
            weight: font.get("weight")?.as_str()?.to_string(),
            style: font.get("style")?.as_str()?.to_string(),
            font_size: node.get("FontSize")?.as_f64()? as f32,
            letter_spacing: node
                .get("LetterSpacing")
                .and_then(Value::as_f64)
                .unwrap_or(0.0) as f32,
            line_height: node
                .get("LineHeight")
                .and_then(Value::as_f64)
                .unwrap_or(1.0) as f32,
        })
    }

    /// Size token by number, e.g. `size(400)` → `Size.Size_400`.
    pub fn size(&self, step: u32) -> Option<f32> {
        self.number(&format!("Size.Size_{step}"))
    }

    pub fn padding(&self, name: &str) -> Option<f32> {
        self.number(&format!("Padding.{name}"))
    }

    pub fn gap(&self, name: &str) -> Option<f32> {
        self.number(&format!("Gap.{name}"))
    }

    pub fn radius(&self, name: &str) -> Option<f32> {
        self.number(&format!("Radius.{name}"))
    }

    pub fn stroke(&self, name: &str) -> Option<f32> {
        self.number(&format!("Stroke.{name}"))
    }

    /// Top-level group names present in the token set.
    pub fn groups(&self) -> Vec<&str> {
        self.root
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }
}
