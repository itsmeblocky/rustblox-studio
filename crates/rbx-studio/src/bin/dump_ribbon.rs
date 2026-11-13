//! Diagnose the reference-derived ribbon definition.
//!
//! ```text
//! cargo run -p rbx-studio --bin dump-ribbon -- [Tab] [--icons]
//! ```

use anyhow::Result;
use rbx_studio::ribbon::RibbonDefinition;

fn main() -> Result<()> {
    let mut tab_filter: Option<String> = None;
    let mut show_icons = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--icons" => show_icons = true,
            other => tab_filter = Some(other.to_string()),
        }
    }

    let def = RibbonDefinition::bundled()?;
    println!("Source: {}", def.meta.source);
    println!("Tab order: {:?}", def.tab_order);
    println!("Tabs: {}", def.tabs.len());

    for tab in &def.tabs {
        if let Some(ref filter) = tab_filter {
            if !tab.id.eq_ignore_ascii_case(filter) {
                continue;
            }
        }
        println!(
            "\n== {} =={}",
            tab.id,
            if tab.dynamic { " (dynamic)" } else { "" }
        );
        if let Some(key) = &tab.label_key {
            println!("label_key: {key}");
        }
        for group in &tab.groups {
            let controls: Vec<String> = group
                .controls
                .iter()
                .map(|c| {
                    if show_icons {
                        format!(
                            "{}{}",
                            c.display_label(),
                            c.preferred_icon()
                                .map(|i| format!(" <{}>", i.rsplit('/').next().unwrap_or(i)))
                                .unwrap_or_default()
                        )
                    } else {
                        c.display_label().to_string()
                    }
                })
                .collect();
            println!("  [{}] {}", group.id, controls.join(", "));
        }
    }
    Ok(())
}
