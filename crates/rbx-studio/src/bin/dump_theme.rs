//! Print a summary of the extracted design tokens for a theme + color mode.
//!
//! Usage:
//!   dump-theme [theme] [mode]      # e.g. dump-theme Default Dark

use anyhow::Result;
use rbx_studio::theme::{ColorMode, DesignTokens, ThemeName};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let theme = match args.first().map(String::as_str) {
        Some("Classic") => ThemeName::Classic,
        _ => ThemeName::Default,
    };
    let mode = match args.get(1).map(String::as_str) {
        Some("Light") => ColorMode::Light,
        _ => ColorMode::Dark,
    };

    let tokens = DesignTokens::bundled(theme, mode)?;

    println!("Theme: {:?} / {:?}", theme, mode);
    println!("Groups: {}", tokens.groups().join(", "));

    println!("\nSurfaces:");
    for name in ["Surface_0", "Surface_100", "Surface_200", "Surface_300"] {
        if let Some(c) = tokens.surface(name) {
            println!(
                "  {name:<12} #{:02X}{:02X}{:02X}  (a={:.2})",
                c.r,
                c.g,
                c.b,
                c.alpha()
            );
        }
    }

    println!("\nContent:");
    for name in ["Default", "Emphasis", "Muted", "Link"] {
        if let Some(c) = tokens.content(name) {
            println!("  {name:<12} #{:02X}{:02X}{:02X}", c.r, c.g, c.b);
        }
    }

    println!("\nTypography:");
    for name in ["HeadingLarge", "TitleMedium", "BodyMedium", "CaptionSmall"] {
        if let Some(t) = tokens.typography(name) {
            println!(
                "  {name:<14} {} {}px  lh={}  ls={}",
                t.weight.rsplit('.').next().unwrap_or(&t.weight),
                t.font_size,
                t.line_height,
                t.letter_spacing
            );
        }
    }

    println!("\nMetrics:");
    for (label, value) in [
        ("Size_400", tokens.size(400)),
        ("Radius.Medium", tokens.radius("Medium")),
        ("Padding.Medium", tokens.padding("Medium")),
        ("Gap.Small", tokens.gap("Small")),
        ("Stroke.Standard", tokens.stroke("Standard")),
    ] {
        match value {
            Some(v) => println!("  {label:<16} {v}"),
            None => println!("  {label:<16} <missing>"),
        }
    }

    Ok(())
}
