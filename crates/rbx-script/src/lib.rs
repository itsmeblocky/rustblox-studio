//! `rbx-script` — native Luau host for Rustblox Studio.
//!
//! This crate runs real `.rbxl` scripts (`Script`, `LocalScript`, `ModuleScript`)
//! and Roblox's own core/player scripts on Luau (via `mlua`) with a data model
//! backed by an instance tree, Roblox-style `task` scheduling, and the subset of
//! the scripting API a place actually touches while running.
//!
//! Design notes
//! - One VM (play-solo): server and client scripts share the data model, exactly
//!   like Studio's "Play" mode.
//! - Execution context is tracked per script and checked by gated services
//!   (`SteamworksService` is server-only, `SettingsService` writes are
//!   server-only — see `AGENTS.md`).
//! - Physics is **engine-side** in Roblox: no Luau script implements the solver.
//!   `physics` documents what is script-visible (`PhysicsService`, collision
//!   groups, `Humanoid` state) versus what must be implemented natively
//!   (`rapier3d` plan, see `docs/physics-scripts.md`).

pub mod context;
pub mod convert;
pub mod datamodel;
pub mod instance;
pub mod log;
pub mod runtime;
pub mod scheduler;
pub mod signal;
pub mod steam;

pub use context::ExecContext;
pub use datamodel::{InstanceStore, Node, NodeId, PropValue};
pub use log::{LogLevel, LogLine, LogSink};
pub use mlua;
pub use runtime::{HostConfig, ScriptHost};
pub use scheduler::Scheduler;
pub use steam::{NoopSteam, SteamBackend};

/// Roblox-like `tostring` for values returned by the host (command bar output).
pub fn value_display(lua: &mlua::Lua, value: &mlua::Value) -> String {
    match value {
        mlua::Value::Nil => "nil".to_string(),
        mlua::Value::Boolean(b) => b.to_string(),
        mlua::Value::Integer(i) => i.to_string(),
        mlua::Value::Number(n) => format!("{n}"),
        mlua::Value::String(s) => s.to_str().map(|s| s.to_string()).unwrap_or_default(),
        other => lua
            .coerce_string(other.clone())
            .ok()
            .flatten()
            .and_then(|s| s.to_str().ok().map(|s| s.to_string()))
            .unwrap_or_default(),
    }
}

/// The Roblox Luau prelude (datatypes + `Enum`) is embedded so the host never
/// depends on files being present at runtime.
pub const PRELUDE_SRC: &str = include_str!("prelude.luau");
