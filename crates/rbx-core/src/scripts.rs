//! Script extraction for the Luau host (`rbx-script`).
//!
//! Roblox stores script bodies in the `Source` property of `Script`, `LocalScript`
//! and `ModuleScript` instances. This module pulls those out of an imported place
//! (`WeakDom`) and also from directories of loose `.lua`/`.luau` files — which is
//! how Roblox ships its own core scripts and player scripts (see
//! `ExtraContent/scripts` in the local client install).
//!
//! The module is deliberately independent of the runtime: it only describes what
//! a script is, where it lives, and which execution context it belongs to.

use anyhow::{Context, Result};
use rbx_dom_weak::{types::{Ref, Variant}, Instance, WeakDom};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// `Script` classes that carry Luau source.
pub const SCRIPT_CLASSES: [&str; 3] = ["Script", "LocalScript", "ModuleScript"];

/// Execution context a script belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptKind {
    /// `Script` — runs on the server.
    Server,
    /// `LocalScript` — runs on the client.
    Client,
    /// `ModuleScript` — only runs when `require`d.
    Module,
}

impl ScriptKind {
    pub fn from_class(class_name: &str) -> Option<Self> {
        match class_name {
            "Script" => Some(Self::Server),
            "LocalScript" => Some(Self::Client),
            "ModuleScript" => Some(Self::Module),
            _ => None,
        }
    }

    pub fn class_name(self) -> &'static str {
        match self {
            Self::Server => "Script",
            Self::Client => "LocalScript",
            Self::Module => "ModuleScript",
        }
    }

    /// Only `Script`/`LocalScript` are auto-run; modules need `require`.
    pub fn runs_automatically(self) -> bool {
        !matches!(self, Self::Module)
    }
}

/// Where a script came from. Core scripts/player scripts are Roblox-owned and are
/// loaded from the local client install for reference-accurate behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ScriptOrigin {
    /// Stored inside the place file.
    #[default]
    Place,
    /// Roblox built-in core scripts (`ExtraContent/scripts/CoreScripts`).
    CoreScripts,
    /// Roblox default player scripts (`ExtraContent/scripts/PlayerScripts`).
    PlayerScripts,
    /// Editor plugin source (`.rbxm`/`.rbxmx`).
    Plugin,
    /// Loose file on disk (user script, test fixture).
    File,
}

/// A single script with its source and data model location.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptInstance {
    pub name: String,
    pub class_name: String,
    pub kind: ScriptKind,
    pub origin: ScriptOrigin,
    /// Ancestor names below the DataModel root, e.g. `["Workspace", "Rig"]`.
    pub path: Vec<String>,
    pub source: String,
    pub enabled: bool,
}

impl ScriptInstance {
    /// `Workspace.Rig.Script`
    pub fn full_name(&self) -> String {
        if self.path.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.path.join("."), self.name)
        }
    }

    /// Scripts that should run when the game starts (not modules, not disabled).
    pub fn runs_automatically(&self) -> bool {
        self.enabled && self.kind.runs_automatically()
    }

    pub fn new_file(name: impl Into<String>, kind: ScriptKind, path: Vec<String>, source: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            class_name: kind.class_name().to_string(),
            kind,
            origin: ScriptOrigin::File,
            path,
            source: source.into(),
            enabled: true,
        }
    }
}

/// Walks an imported place and collects every script body.
pub fn collect_scripts(dom: &WeakDom) -> Vec<ScriptInstance> {
    let mut out = Vec::new();
    walk(dom.root_ref(), dom, &mut Vec::new(), &mut out);
    out
}

fn walk(referent: Ref, dom: &WeakDom, path: &mut Vec<String>, out: &mut Vec<ScriptInstance>) {
    let Some(inst) = dom.get_by_ref(referent) else { return };
    for child in inst.children() {
        let Some(child_inst) = dom.get_by_ref(*child) else { continue };
        let name = child_inst.name.clone();
        let class = child_inst.class.as_str();
        if let Some(kind) = ScriptKind::from_class(class) {
            out.push(ScriptInstance {
                name: name.clone(),
                class_name: class.to_string(),
                kind,
                origin: ScriptOrigin::Place,
                path: path.clone(),
                source: script_source(child_inst).unwrap_or_default(),
                enabled: match child_inst.properties.get(&rbx_dom_weak::ustr("Disabled")) {
                    Some(Variant::Bool(disabled)) => !*disabled,
                    _ => true,
                },
            });
        }
        if !child_inst.children().is_empty() {
            path.push(name);
            walk(*child, dom, path, out);
            path.pop();
        }
    }
}

/// Reads the `Source` property, accepting the string encodings rbx-dom can produce.
pub fn script_source(inst: &Instance) -> Option<String> {
    match inst.properties.get(&rbx_dom_weak::ustr("Source"))? {
        Variant::String(s) => Some(s.to_string()),
        Variant::BinaryString(s) => Some(String::from_utf8_lossy(s.as_ref()).to_string()),
        _ => None,
    }
}

/// Reads every `.lua`/`.luau` file under `root` as a script.
///
/// Roblox's own script drops use `*.server.lua`, `*.client.lua` and
/// `*.module.lua` suffixes to declare a script's context; plain `.lua` files are
/// library modules (safe default: never auto-run).
pub fn scripts_from_dir(root: &Path) -> Result<Vec<ScriptInstance>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), Vec::<String>::new())];
    while let Some((dir, path)) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("reading script directory {}", dir.display()))?;
        for entry in entries {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                stack.push((p.clone(), {
                    let mut next = path.clone();
                    next.push(entry.file_name().to_string_lossy().to_string());
                    next
                }));
                continue;
            }
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default();
            if !matches!(ext, "lua" | "luau") {
                continue;
            }
            let file_stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("Script").to_string();
            let (name, kind) = infer_kind(&file_stem);
            let source = std::fs::read_to_string(&p)
                .with_context(|| format!("reading script {}", p.display()))?;
            out.push(ScriptInstance {
                name,
                class_name: kind.class_name().to_string(),
                kind,
                origin: ScriptOrigin::File,
                path: path.clone(),
                source,
                enabled: true,
            });
        }
    }
    out.sort_by(|a, b| a.full_name().cmp(&b.full_name()));
    Ok(out)
}

/// Same as [`scripts_from_dir`] but tags the origin (core scripts, player scripts, ...).
pub fn scripts_from_dir_with_origin(root: &Path, origin: ScriptOrigin) -> Result<Vec<ScriptInstance>> {
    let mut scripts = scripts_from_dir(root)?;
    for s in &mut scripts {
        s.origin = origin;
    }
    Ok(scripts)
}

/// `Foo.server` -> (`Foo`, Server), `Bar.client` -> (`Bar`, Client), otherwise Module.
pub fn infer_kind(file_stem: &str) -> (String, ScriptKind) {
    for (suffix, kind) in [
        (".server", ScriptKind::Server),
        (".client", ScriptKind::Client),
        (".module", ScriptKind::Module),
    ] {
        if let Some(name) = file_stem.strip_suffix(suffix) {
            return (name.to_string(), kind);
        }
    }
    (file_stem.to_string(), ScriptKind::Module)
}

/// Scripts whose location in the tree makes them run automatically.
///
/// Mirrors Roblox's rules that matter for a place:
/// * `Script` inside `ServerScriptService` (or anywhere on the server) runs.
/// * `LocalScript` runs from `StarterPlayerScripts`, `StarterCharacterScripts`,
///   `StarterGui` and `ReplicatedFirst` (and anywhere else on the client).
pub fn should_autorun(script: &ScriptInstance) -> bool {
    if !script.runs_automatically() {
        return false;
    }
    match script.kind {
        ScriptKind::Module => false,
        ScriptKind::Server => true,
        ScriptKind::Client => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_context_from_file_names() {
        assert_eq!(infer_kind("Foo.server"), ("Foo".into(), ScriptKind::Server));
        assert_eq!(infer_kind("Foo.client"), ("Foo".into(), ScriptKind::Client));
        assert_eq!(infer_kind("Foo.module"), ("Foo".into(), ScriptKind::Module));
        assert_eq!(infer_kind("PlayerModule"), ("PlayerModule".into(), ScriptKind::Module));
    }

    #[test]
    fn module_scripts_never_autorun() {
        let s = ScriptInstance::new_file("M", ScriptKind::Module, vec!["ReplicatedStorage".into()], "");
        assert!(!should_autorun(&s));
    }
}
