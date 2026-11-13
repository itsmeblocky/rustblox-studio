//! Diagnostic binary: load the extracted Studio icon set and report on it.
//!
//! Usage:
//!   cargo run -p rbx-studio --bin dump-icons [icons_dir]

use anyhow::Result;
use rbx_studio::icons::{category_counts, IconRegistry, IconScale, Theme};
use std::path::PathBuf;

fn main() -> Result<()> {
    let root = std::env::args().nth(1).map(PathBuf::from);

    let registry = match root {
        Some(root) => IconRegistry::load(root)?,
        None => IconRegistry::load_bundled()?,
    };

    println!("Icon registry");
    println!("  source build : {}", registry.generated_from());
    println!("  root         : {}", registry.root().display());
    println!("  unique icons : {}", registry.len());

    let mut counts: Vec<(String, usize)> = category_counts(&registry).into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    println!("\nTop-level categories:");
    for (category, count) in counts.iter().take(12) {
        println!("  {count:>6}  {category}");
    }

    println!("\nSample lookups (Dark / Standard / 1x):");
    let samples = [
        "Part",
        "Folder",
        "Script",
        "LocalScript",
        "Model",
        "SpawnLocation",
        "AddCollaborator",
        "RibbonPlaySmall",
        "RibbonStopActiveSmall",
        "Explorer",
        "Properties",
        "Toolbox",
        "Output",
        "CommandBar",
    ];

    let mut missing = Vec::new();
    for name in samples {
        let resolved = registry
            .class_icon(name, Theme::Dark, IconScale::X1)
            .or_else(|| registry.widget_icon(name, Theme::Dark, IconScale::X1))
            .or_else(|| registry.ribbon_icon(name, Theme::Dark, IconScale::X1))
            .or_else(|| registry.find(name, IconScale::X1));

        match resolved {
            Some(icon) => {
                let exists = icon.path.exists();
                let mark = if exists { "ok " } else { "MISSING FILE" };
                println!("  {mark:<12} {name:<16} -> {}", icon.path.display());
                if !exists {
                    missing.push(name.to_string());
                }
            }
            None => {
                println!("  NOT FOUND     {name}");
                missing.push(name.to_string());
            }
        }
    }

    if missing.is_empty() {
        println!("\nAll sample icons resolved.");
    } else {
        println!("\nUnresolved samples: {}", missing.join(", "));
    }

    Ok(())
}
