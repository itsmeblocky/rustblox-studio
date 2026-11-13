//! Inspect a `.rbxm` / `.rbxl` file: print the instance tree, dump scripts,
//! and search script sources.
//!
//! Usage:
//!   rbx-inspect <file> [--tree] [--scripts] [--dump-scripts <dir>] [--grep <pattern>]

use anyhow::{Context, Result};
use rbx_core::parse_binary;
use rbx_dom_weak::types::Variant;
use rbx_dom_weak::{Instance, WeakDom};
use std::fs;
use std::path::{Path, PathBuf};

struct Node {
    depth: usize,
    path: String,
    class: String,
    name: String,
    source: Option<String>,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let input = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .context("usage: rbx-inspect <file> [--tree] [--scripts] [--dump-scripts <dir>] [--grep <pattern>]")?;

    let show_tree = args.iter().any(|a| a == "--tree");
    let show_scripts = args.iter().any(|a| a == "--scripts");
    let dump_dir = flag_value(&args, "--dump-scripts").map(PathBuf::from);
    let grep_pat = flag_value(&args, "--grep");
    let props_class = flag_value(&args, "--props");
    let dump_strings_dir = flag_value(&args, "--dump-strings").map(PathBuf::from);

    let bytes = fs::read(input).with_context(|| format!("reading {input}"))?;
    let dom = parse_binary(&bytes)?;

    let mut nodes = Vec::new();
    let root_name = Path::new(input)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());
    walk(&dom, dom.root_ref(), 0, &root_name, &mut nodes);

    println!("File: {input}");
    println!("Instances: {}", nodes.len());
    let scripts: Vec<&Node> = nodes.iter().filter(|n| n.source.is_some()).collect();
    println!("Scripts: {}", scripts.len());

    let mut class_counts: std::collections::BTreeMap<&str, usize> = Default::default();
    for n in &nodes {
        *class_counts.entry(&n.class).or_default() += 1;
    }
    println!("\nClass counts:");
    let mut counts: Vec<(&str, usize)> = class_counts.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    for (class, count) in counts.iter().take(20) {
        println!("  {count:>6}  {class}");
    }

    if show_tree {
        println!("\nTree:");
        for n in &nodes {
            let indent = "  ".repeat(n.depth);
            println!("{indent}{} [{}]", n.name, n.class);
        }
    }

    if show_scripts {
        println!("\nScripts:");
        for s in &scripts {
            println!("  {} [{}] ({} bytes)", s.path, s.class, s.source.as_ref().map(|x| x.len()).unwrap_or(0));
        }
    }

    if let Some(dir) = dump_dir {
        fs::create_dir_all(&dir)?;
        for s in &scripts {
            let safe: String = s
                .path
                .chars()
                .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
                .collect();
            let mut path = dir.join(&safe);
            path.set_extension("lua");
            if let Some(src) = &s.source {
                fs::write(&path, src)?;
            }
            println!("wrote {}", path.display());
        }
    }

    if let Some(pat) = grep_pat {
        println!("\nGrep for {pat:?}:");
        for s in &scripts {
            if let Some(src) = &s.source {
                for (i, line) in src.lines().enumerate() {
                    if line.contains(&pat) {
                        println!("  {}:{}: {}", s.path, i + 1, line.trim());
                    }
                }
            }
        }
    }

    if let Some(class) = props_class {
        println!("\nProperties for first instances of class {class}:");
        let mut shown = 0;
        print_props(&dom, dom.root_ref(), &class, &mut shown, 3);
    }

    if let Some(dir) = dump_strings_dir {
        fs::create_dir_all(&dir)?;
        let mut index = 0usize;
        dump_strings(&dom, dom.root_ref(), &dir, &mut index)?;
        println!("\nDumped {index} string blobs to {}", dir.display());
    }

    Ok(())
}

fn dump_strings(
    dom: &WeakDom,
    referent: rbx_dom_weak::types::Ref,
    dir: &Path,
    index: &mut usize,
) -> Result<()> {
    let Some(inst) = dom.get_by_ref(referent) else {
        return Ok(());
    };

    let content = match inst.class.as_str() {
        "StringValue" => match inst.properties.get(&rbx_dom_weak::ustr("Value")) {
            Some(Variant::String(s)) => Some(s.clone()),
            _ => None,
        },
        "LocalizationTable" => match inst.properties.get(&rbx_dom_weak::ustr("Contents")) {
            Some(Variant::String(s)) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    };

    if let Some(content) = content {
        *index += 1;
        let safe: String = inst
            .name
            .chars()
            .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
            .collect();
        let path = dir.join(format!("{index:03}_{}_{}.txt", inst.class.as_str(), safe));
        fs::write(&path, content)?;
    }

    for child in inst.children() {
        dump_strings(dom, *child, dir, index)?;
    }
    Ok(())
}

fn print_props(dom: &WeakDom, referent: rbx_dom_weak::types::Ref, class: &str, shown: &mut usize, max: usize) {
    if *shown >= max {
        return;
    }
    let Some(inst) = dom.get_by_ref(referent) else {
        return;
    };
    if inst.class.as_str() == class {
        *shown += 1;
        println!("  {} ({})", inst.name, inst.class);
        let mut keys: Vec<_> = inst.properties.iter().collect();
        keys.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        for (k, v) in keys {
            let preview = match v {
                Variant::String(s) => {
                    format!("String({:?})", s.chars().take(80).collect::<String>())
                }
                Variant::SharedString(s) => format!("SharedString(len {})", s.data().len()),
                Variant::BinaryString(s) => {
                    let bytes: &[u8] = s.as_ref();
                    format!("BinaryString(len {})", bytes.len())
                }
                other => format!("{other:?}").chars().take(60).collect(),
            };
            println!("    {k}: {preview}");
        }
        println!();
    }
    for child in inst.children() {
        print_props(dom, *child, class, shown, max);
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn walk(dom: &WeakDom, referent: rbx_dom_weak::types::Ref, depth: usize, parent_path: &str, out: &mut Vec<Node>) {
    let Some(inst) = dom.get_by_ref(referent) else {
        return;
    };

    let path = if depth == 0 {
        parent_path.to_string()
    } else {
        format!("{parent_path}/{}", inst.name)
    };

    let class = inst.class.as_str().to_string();
    let source = script_source(inst);
    out.push(Node {
        depth,
        path: path.clone(),
        class: class.clone(),
        name: inst.name.clone(),
        source,
    });

    for child in inst.children() {
        walk(dom, *child, depth + 1, &path, out);
    }
}

fn script_source(inst: &Instance) -> Option<String> {
    if !matches!(inst.class.as_str(), "Script" | "LocalScript" | "ModuleScript") {
        return None;
    }
    match inst.properties.get(&rbx_dom_weak::ustr("Source"))? {
        Variant::String(s) => Some(s.clone()),
        Variant::SharedString(s) => Some(String::from_utf8_lossy(s.data()).to_string()),
        _ => None,
    }
}
