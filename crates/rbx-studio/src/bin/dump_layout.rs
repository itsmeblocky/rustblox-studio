//! Diagnose a parsed Qtitan dock layout.
//!
//! ```text
//! cargo run -p rbx-studio --bin dump-layout -- [edit|play|pserv] [--docks] [--tree]
//! ```

use anyhow::Result;
use rbx_studio::layout::{DockLayout, LayoutMode, Panel, PanelKind};

fn main() -> Result<()> {
    let mut mode = LayoutMode::Edit;
    let mut show_docks = true;
    let mut show_tree = true;

    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "edit" => mode = LayoutMode::Edit,
            "play" => mode = LayoutMode::Play,
            "pserv" | "play-server" => mode = LayoutMode::PlayServer,
            "--docks" => show_docks = true,
            "--no-docks" => show_docks = false,
            "--tree" => show_tree = true,
            "--no-tree" => show_tree = false,
            other => anyhow::bail!("unknown argument {other:?}"),
        }
    }

    let layout = DockLayout::bundled(mode)?;
    println!(
        "Layout: {:?}  panels={} floating_windows={}",
        mode,
        layout.len(),
        layout.windows().count()
    );
    println!(
        "TopLayout: {}   Client: {}",
        layout.top_layout.as_deref().unwrap_or("<none>"),
        layout.client.as_deref().unwrap_or("<none>")
    );

    if show_tree {
        println!("\nTree:");
        if let Some(root) = layout.top_layout.as_deref() {
            print_tree(&layout, root, 0);
        }
    }

    if show_docks {
        println!("\nDocked panels ({}):", layout.dock_widgets(true).len());
        for panel in layout.dock_widgets(true) {
            let group = layout
                .group_of(&panel.id)
                .map(|g| g.id.as_str())
                .unwrap_or("<none>");
            let tabs = layout
                .group_of(&panel.id)
                .map(|g| layout.tabs(&g.id).len())
                .unwrap_or(0);
            println!(
                "  {:<52} group={:<12} tabs={} {}",
                panel.id,
                group,
                tabs,
                if panel.active { "[active]" } else { "" }
            );
        }

        println!("\nTab groups ({}):", layout.tab_groups().len());
        for group in layout.tab_groups() {
            let tabs: Vec<&str> = layout
                .tabs(&group.id)
                .into_iter()
                .map(|p| p.id.as_str())
                .collect();
            println!(
                "  {:<12} size={:>4}x{:<4} tabs={:?}",
                group.id, group.dock_cx, group.dock_cy, tabs
            );
        }
    }

    if layout.windows().next().is_some() {
        println!("\nFloating windows:");
        for window in layout.windows() {
            let rect = window.window_rect.unwrap_or_default();
            println!(
                "  {:<10} at ({}, {}) {}x{}  top={}",
                window.id,
                rect.left,
                rect.top,
                rect.width(),
                rect.height(),
                window.top_layout.as_deref().unwrap_or("<none>")
            );
        }
    }

    Ok(())
}

fn print_tree(layout: &DockLayout, id: &str, depth: usize) {
    let Some(panel) = layout.panel(id) else {
        println!("{}{} <missing>", "  ".repeat(depth), id);
        return;
    };
    println!(
        "{}{}  {}{}",
        "  ".repeat(depth),
        panel.id,
        kind_name(panel),
        if panel.active { " [active]" } else { "" }
    );
    for child in &panel.children {
        print_tree(layout, child, depth + 1);
    }
}

fn kind_name(panel: &Panel) -> String {
    match panel.kind {
        Some(PanelKind::Split) => format!(
            "split({}) {}x{}",
            if panel.horizontal { "h" } else { "v" },
            panel.dock_cx,
            panel.dock_cy
        ),
        Some(PanelKind::TabGroup) => format!("tabs {}x{}", panel.dock_cx, panel.dock_cy),
        Some(PanelKind::DockWidget) => "dock".to_string(),
        Some(PanelKind::DocumentPanel) => "document".to_string(),
        Some(PanelKind::DockWindow) => "window".to_string(),
        Some(PanelKind::Other(code)) => format!("type={code}"),
        None => String::new(),
    }
}
