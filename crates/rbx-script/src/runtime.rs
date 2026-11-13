//! The runtime: data model services, Luau globals, script execution and stepping.

use crate::context::ExecContext;
use crate::convert::{self, Converters};
use crate::datamodel::{InstanceStore, NodeId, PropValue};
use crate::instance::{as_instance_id, instance_value, LuaInstance};
use crate::log::{LogLevel, LogLine, LogSink};
use crate::scheduler::Scheduler;
use crate::signal::SignalRegistry;
use crate::steam::{NoopSteam, SteamBackend};
use anyhow::Result;
use mlua::{
    Function, Lua, MultiValue, Result as LuaResult, Table, Thread, Value, Variadic,
};
use rbx_core::{Part, ScriptInstance, ScriptOrigin, Scene};
use rbx_studio::api_dump::ReflectionDatabase;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

/// Services created on the DataModel at startup, in Studio order.
const SERVICES: &[&str] = &[
    "Workspace",
    "Players",
    "Lighting",
    "MaterialService",
    "ReplicatedFirst",
    "ReplicatedStorage",
    "ServerScriptService",
    "ServerStorage",
    "StarterGui",
    "StarterPack",
    "StarterPlayer",
    "SoundService",
    "Chat",
    "Teams",
    "Debris",
    "CollectionService",
    "PhysicsService",
    "ProcessInstancePhysicsService",
    "JointsService",
    "RunService",
    "ScriptContext",
    "ContentProvider",
    "HttpService",
    "UserInputService",
    "ContextActionService",
    "MarketplaceService",
    "TeleportService",
    "TextChatService",
    "TweenService",
    "SettingsService",
    "SteamworksService",
    "LogService",
    "TestService",
    "Selection",
];

/// Collision group state backing `PhysicsService`.
#[derive(Default)]
pub struct CollisionGroups {
    pub groups: BTreeMap<String, HashSet<NodeId>>,
    pub collidable: BTreeMap<(String, String), bool>,
}

/// Statistics surfaced by the Studio UI.
#[derive(Debug, Clone, Copy, Default)]
pub struct RuntimeStats {
    pub scripts_run: usize,
    pub errors: usize,
    pub steps: u64,
}

pub struct HostConfig {
    /// API dump used for property defaults and class ancestry.
    pub api_dump: Option<ReflectionDatabase>,
    /// Where `SettingsService` persists values.
    pub settings_path: Option<PathBuf>,
    /// Steam backend; defaults to the offline no-op.
    pub steam: Option<Rc<dyn SteamBackend>>,
    /// Context the command bar executes in.
    pub context: ExecContext,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            api_dump: None,
            settings_path: None,
            steam: None,
            context: ExecContext::Server,
        }
    }
}

impl HostConfig {
    pub fn with_bundled_api_dump(mut self) -> Self {
        if self.api_dump.is_none() {
            self.api_dump = ReflectionDatabase::load_bundled().ok();
        }
        self
    }
}

/// Everything the host needs, shared with the Lua userdata.
pub struct Shared {
    pub store: RefCell<InstanceStore>,
    pub db: Option<ReflectionDatabase>,
    pub converters: Converters,
    pub log: LogSink,
    pub context: Cell<ExecContext>,
    pub scheduler: RefCell<Scheduler>,
    pub signals: RefCell<SignalRegistry>,
    pub signal_index: RefCell<HashMap<(NodeId, String), usize>>,
    pub helpers: RefCell<Option<Table>>,
    pub settings: RefCell<BTreeMap<String, PropValue>>,
    pub settings_path: RefCell<Option<PathBuf>>,
    pub steam: Rc<dyn SteamBackend>,
    pub collision_groups: RefCell<CollisionGroups>,
    pub modules: RefCell<HashMap<NodeId, Value>>,
    pub loading_modules: RefCell<HashSet<NodeId>>,
    pub current_script: RefCell<Option<(NodeId, String)>>,
    pub destroys: RefCell<Vec<(f64, NodeId)>>,
    pub warnings: RefCell<HashSet<String>>,
    pub defaults: RefCell<HashMap<String, Rc<BTreeMap<String, PropValue>>>>,
    pub stats: RefCell<RuntimeStats>,
    pub start: Instant,
    pub core_scripts_root: RefCell<Option<NodeId>>,
}

impl Shared {
    fn new(lua: &Lua, config: HostConfig) -> Result<Self> {
        let settings_path = config.settings_path.clone();
        let steam: Rc<dyn SteamBackend> = config.steam.unwrap_or_else(|| Rc::new(NoopSteam::default()));
        let shared = Shared {
            store: RefCell::new(InstanceStore::new()),
            db: config.api_dump,
            converters: Converters::new(lua)?,
            log: LogSink::default(),
            context: Cell::new(config.context),
            scheduler: RefCell::new(Scheduler::default()),
            signals: RefCell::new(SignalRegistry::default()),
            signal_index: RefCell::new(HashMap::new()),
            helpers: RefCell::new(None),
            settings: RefCell::new(BTreeMap::new()),
            settings_path: RefCell::new(settings_path),
            steam,
            collision_groups: RefCell::new(CollisionGroups::default()),
            modules: RefCell::new(HashMap::new()),
            loading_modules: RefCell::new(HashSet::new()),
            current_script: RefCell::new(None),
            destroys: RefCell::new(Vec::new()),
            warnings: RefCell::new(HashSet::new()),
            defaults: RefCell::new(HashMap::new()),
            stats: RefCell::new(RuntimeStats::default()),
            start: Instant::now(),
            core_scripts_root: RefCell::new(None),
        };
        shared.create_services();
        shared.load_settings();
        Ok(shared)
    }

    fn create_services(&self) {
        let root = self.store.borrow().root;
        for service in SERVICES {
            let existing = {
                let store = self.store.borrow();
                store.child(root, service)
            };
            if existing.is_some() {
                continue;
            }
            let parent = if *service == "SteamworksService" && !self.steam.init() {
                // Still created so `game:GetService` can gate it; just offline.
                Some(root)
            } else {
                Some(root)
            };
            let id = self.store.borrow_mut().create(service, service, parent);
            self.apply_defaults(id);
        }
        // StarterPlayer containers
        if let Some(sp) = self.find_service("StarterPlayer") {
            for child in ["StarterPlayerScripts", "StarterCharacterScripts"] {
                if self.store.borrow().child(sp, child).is_none() {
                    let id = self.store.borrow_mut().create(child, child, Some(sp));
                    self.apply_defaults(id);
                }
            }
        }
        // Workspace: a Terrain child like Roblox.
        if let Some(ws) = self.find_service("Workspace") {
            if self.store.borrow().child(ws, "Terrain").is_none() {
                let id = self.store.borrow_mut().create("Terrain", "Terrain", Some(ws));
                self.apply_defaults(id);
            }
        }
        // Players: one joinable player for play-solo.
        if let Some(players) = self.find_service("Players") {
            if self.store.borrow().children(players).is_empty() {
                let id = self.store.borrow_mut().create("Player", "Player1", Some(players));
                self.apply_defaults(id);
            }
        }
    }

    /// Fills a node's properties with typed API-dump defaults.
    pub fn apply_defaults(&self, id: NodeId) {
        let class = self.store.borrow().class_name(id).to_string();
        let Some(defaults) = self.defaults_for(&class) else {
            let name = self.store.borrow().get_prop(id, "Name");
            if let Some(name) = name {
                self.store.borrow_mut().set_prop(id, "Name", name);
            }
            return;
        };
        let mut store = self.store.borrow_mut();
        let name = store.name(id).to_string();
        for (key, value) in defaults.iter() {
            // `Parent` defaults to nil (which would detach the node) and
            // `ClassName` is derived from the node; neither is stored.
            if matches!(key.as_str(), "Name" | "Parent" | "ClassName") {
                continue;
            }
            if !store.node(id).props.contains_key(key) {
                store.set_prop(id, key, value.clone());
            }
        }
        store.set_prop(id, "Name", PropValue::String(name));
    }

    pub fn defaults_for(&self, class: &str) -> Option<Rc<BTreeMap<String, PropValue>>> {
        if let Some(cached) = self.defaults.borrow().get(class) {
            return Some(cached.clone());
        }
        let db = self.db.as_ref()?;
        let mut map = BTreeMap::new();
        for prop in db.properties_for_class(class, true, true) {
            if prop.not_scriptable {
                continue;
            }
            map.insert(
                prop.name.clone(),
                convert::default_prop(db, class, &prop.name, &prop.value_type),
            );
        }
        let rc = Rc::new(map);
        self.defaults.borrow_mut().insert(class.to_string(), rc.clone());
        Some(rc)
    }

    pub fn is_a(&self, id: NodeId, class: &str) -> bool {
        let actual = self.store.borrow().class_name(id).to_string();
        if actual == class {
            return true;
        }
        if let Some(db) = &self.db {
            if db.class(&actual).is_some() {
                return db.is_a(&actual, class);
            }
        }
        fallback_is_a(&actual, class)
    }

    pub fn find_service(&self, name: &str) -> Option<NodeId> {
        let root = self.store.borrow().root;
        self.store
            .borrow()
            .children(root)
            .into_iter()
            .find(|id| self.store.borrow().class_name(*id) == name)
    }

    pub fn get_service(&self, name: &str) -> Option<NodeId> {
        if name == "SteamworksService" && !self.context.get().can_use_steam() {
            self.warn_once(
                "steam-client",
                "SteamworksService is server-only; GetService returned nil from a client context",
            );
            return None;
        }
        if let Some(id) = self.find_service(name) {
            return Some(id);
        }
        // Lazily create unknown services so scripts don't hard-fail.
        let root = self.store.borrow().root;
        let id = self.store.borrow_mut().create(name, name, Some(root));
        self.apply_defaults(id);
        Some(id)
    }

    pub fn local_player(&self) -> Option<NodeId> {
        let players = self.find_service("Players")?;
        self.store.borrow().children(players).into_iter().find(|id| {
            let store = self.store.borrow();
            store.class_name(*id) == "Player"
        })
    }

    pub fn now(&self) -> f64 {
        self.scheduler.borrow().now()
    }

    pub fn current_script_name(&self) -> Option<String> {
        self.current_script.borrow().as_ref().map(|(_, name)| name.clone())
    }

    pub fn print(&self, level: LogLevel, message: String) {
        let source = self.current_script_name();
        self.log.push(level, message, source, self.now());
    }

    pub fn warn_once(&self, key: &str, message: &str) {
        let mut warnings = self.warnings.borrow_mut();
        if warnings.insert(key.to_string()) {
            drop(warnings);
            self.print(LogLevel::Warn, message.to_string());
        }
    }

    pub fn report_error(&self, _lua: &Lua, message: &str) {
        self.stats.borrow_mut().errors += 1;
        self.print(LogLevel::Error, message.to_string());
    }

    /// Fires `event` on `node` only if something is connected (avoids signal spam).
    pub fn fire_node_event(&self, lua: &Lua, node: NodeId, event: &str, args: MultiValue) {
        let id = self
            .signal_index
            .borrow()
            .get(&(node, event.to_string()))
            .copied();
        if let Some(id) = id {
            self.fire(lua, id, args);
        }
    }

    pub fn fire_node_event_with(&self, lua: &Lua, node: NodeId, event: &str, args: Vec<Value>) {
        self.fire_node_event(lua, node, event, MultiValue::from_vec(args));
    }

    fn load_settings(&self) {
        let path = self.settings_path.borrow().clone();
        let Some(path) = path else {
            self.install_default_settings();
            return;
        };
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(values) = serde_json::from_str::<BTreeMap<String, PropValue>>(&text) {
                *self.settings.borrow_mut() = values;
                return;
            }
        }
        self.install_default_settings();
    }

    fn install_default_settings(&self) {
        let mut settings = self.settings.borrow_mut();
        for (name, value) in default_settings() {
            settings.entry(name).or_insert(value);
        }
    }

    pub fn save_settings(&self) {
        let path = self.settings_path.borrow().clone();
        let Some(path) = path else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(&*self.settings.borrow()) {
            if let Err(e) = std::fs::write(&path, text) {
                self.print(LogLevel::Error, format!("SettingsService: could not write {}: {e}", path.display()));
            }
        }
    }
}

fn default_settings() -> Vec<(String, PropValue)> {
    vec![
        ("WindowMode".into(), PropValue::String("Windowed".into())),
        ("Resolution".into(), PropValue::Vector2(1920.0, 1080.0)),
        ("VSync".into(), PropValue::Bool(true)),
        ("ShiftLock".into(), PropValue::Bool(false)),
        ("MouseSensitivity".into(), PropValue::Number(2.0)),
        ("CameraMode".into(), PropValue::String("Classic".into())),
        ("Anisotropy".into(), PropValue::Int(16)),
        ("FrameGeneration".into(), PropValue::String("Off".into())),
        ("FrameGenQuality".into(), PropValue::String("Quality".into())),
        ("AntiAliasing".into(), PropValue::String("TAA".into())),
        ("RenderScale".into(), PropValue::Number(1.0)),
    ]
}

fn fallback_is_a(actual: &str, class: &str) -> bool {
    if actual == class {
        return true;
    }
    let baseparts = [
        "Part", "WedgePart", "CornerWedgePart", "TrussPart", "MeshPart", "SpawnLocation", "Seat",
        "VehicleSeat", "SkateboardPlatform", "Platform", "UnionOperation", "PartOperation",
    ];
    match class {
        "BasePart" => baseparts.contains(&actual),
        "Part" => matches!(actual, "Part" | "SpawnLocation" | "Seat" | "VehicleSeat" | "Platform"),
        "Instance" => true,
        "PVInstance" => baseparts.contains(&actual) || actual == "Model",
        "LuaSourceContainer" => matches!(actual, "Script" | "LocalScript" | "ModuleScript"),
        "GuiObject" => actual.starts_with("Text") || actual.starts_with("Image") || matches!(
            actual,
            "Frame" | "ScrollingFrame" | "ViewportFrame" | "VideoFrame" | "CanvasGroup"
        ),
        "LayerCollector" => matches!(actual, "ScreenGui" | "BillboardGui" | "SurfaceGui"),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Signal/method name tables
// ---------------------------------------------------------------------------

pub fn is_signal(class: &str, name: &str) -> bool {
    match name {
        "Changed" | "ChildAdded" | "ChildRemoved" | "DescendantAdded" | "DescendantRemoving"
        | "Destroying" | "AncestryChanged" | "AttributeChanged" | "AttributesChanged" => true,
        "Touched" | "TouchEnded" | "TouchStarted" => fallback_is_a(class, "BasePart"),
        "Heartbeat" | "Stepped" | "RenderStepped" | "PreSimulation" | "PostSimulation"
        | "PreRender" | "PostRender" => class == "RunService",
        "PlayerAdded" | "PlayerRemoving" => class == "Players",
        "Died" | "HealthChanged" | "StateChanged" | "Running" | "Jumping" | "Climbing"
        | "Seated" | "GettingUp" | "FallingDown" | "MoveToFinished" => class == "Humanoid",
        "Activated" | "MouseButton1Click" | "MouseButton1Down" | "MouseButton1Up"
        | "MouseButton2Click" | "MouseEnter" | "MouseLeave" | "InputBegan" | "InputEnded"
        | "InputChanged" => fallback_is_a(class, "GuiObject"),
        "PromptButtonHoldBegan" | "Triggered" => class == "ProximityPrompt",
        _ => false,
    }
}

pub fn is_method(class: &str, name: &str) -> bool {
    if matches!(
        name,
        "FindFirstChild"
            | "FindFirstChildOfClass"
            | "FindFirstChildWhichIsA"
            | "FindFirstAncestor"
            | "FindFirstAncestorOfClass"
            | "FindFirstAncestorWhichIsA"
            | "GetChildren"
            | "GetDescendants"
            | "IsA"
            | "IsDescendantOf"
            | "Destroy"
            | "ClearAllChildren"
            | "Clone"
            | "GetFullName"
            | "GetAttribute"
            | "SetAttribute"
            | "GetAttributes"
            | "AddTag"
            | "RemoveTag"
            | "HasTag"
            | "GetTags"
            | "GetPropertyChangedSignal"
            | "GetPivot"
            | "SetPivot"
            | "PivotTo"
            | "GetMass"
            | "IsGrounded"
            | "GetState"
            | "ChangeState"
            | "Move"
            | "MoveTo"
            | "TakeDamage"
            | "Kick"
            | "GetService"
            | "FindService"
            | "IsLoaded"
            | "Raycast"
    ) {
        return true;
    }
    service_method_names(class, name)
}

fn service_method_names(class: &str, name: &str) -> bool {
    match class {
        "RunService" => matches!(
            name,
            "IsServer" | "IsClient" | "IsStudio" | "IsRunMode" | "IsRunning" | "BindToRenderStep"
                | "UnbindFromRenderStep" | "Set3dRenderingEnabled" | "Pause" | "Run" | "Stop"
                | "BindToClose" | "GetRobloxVersion"
        ),
        "Players" => matches!(name, "GetPlayers" | "GetPlayerFromCharacter" | "GetPlayerByUserId" | "GetUserIdFromNameAsync"),
        "PhysicsService" => matches!(
            name,
            "CreateCollisionGroup" | "RegisterCollisionGroup" | "UnregisterCollisionGroup"
                | "CollisionGroupContainsPart" | "SetPartCollisionGroup" | "GetRegisteredCollisionGroups"
                | "GetCreateCollisionGroups" | "CollisionGroupsAreCollidable" | "GetCollisionGroups"
                | "SetCollisionGroupCollidable" | "GetCollisionGroupContainsPart" | "GetMaxCollisionGroups"
        ),
        "SettingsService" => matches!(
            name,
            "GetSetting" | "SetSetting" | "GetWindowModes" | "GetFrameGenerationModes"
                | "GetAnisotropyLevels" | "ApplyPreset" | "CreateDefaultSettingsGui"
        ),
        "SteamworksService" => matches!(
            name,
            "Init" | "IsOverlayEnabled" | "RequestCurrentStats" | "UnlockAchievement"
                | "ClearAchievement" | "GetAchievement" | "SetStat" | "GetStat" | "StoreStats"
                | "FileWrite" | "FileRead"
        ),
        "Debris" => name == "AddItem",
        "CollectionService" => matches!(
            name,
            "AddTag" | "RemoveTag" | "HasTag" | "GetTags" | "GetTagged" | "GetInstanceAddedSignal"
        ),
        "TweenService" => matches!(name, "Create" | "GetValue"),
        "HttpService" => matches!(name, "JSONEncode" | "JSONDecode" | "GenerateGUID" | "GetAsync" | "UrlEncode"),
        "UserInputService" => matches!(name, "GetMouseLocation" | "IsKeyDown" | "GetKeysPressed" | "GetFocusedTextBox"),
        "ContentProvider" => matches!(name, "Preload" | "PreloadAsync"),
        "Workspace" => matches!(name, "GetRealPhysicsFPS" | "BreakJoints" | "GetNumAwakeParts" | "GetPhysicsThrottling"),
        "Selection" => matches!(name, "Set" | "Get" | "Add" | "Remove"),
        "Terrain" => false,
        _ => false,
    }
}

fn value_to_string(_lua: &Lua, value: &Value) -> String {
    match value {
        Value::Nil => "nil".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => {
            if n.fract() == 0.0 {
                format!("{n:.1}")
            } else {
                format!("{n}")
            }
        }
        Value::String(s) => s.to_str().map(|s| s.to_string()).unwrap_or_default(),
        Value::Table(t) => {
            if let Some(kind) = t.get::<Option<String>>("__datatype").ok().flatten() {
                match kind.as_str() {
                    "Instance" => "Instance".to_string(),
                    "EnumItem" => {
                        let name = t.get::<Option<String>>("Name").ok().flatten().unwrap_or_default();
                        format!("Enum.{name}")
                    }
                    _ => format!("{kind}: {}", convert::from_lua(value).display()),
                }
            } else {
                format!("table: {:p}", t.to_pointer())
            }
        }
        Value::UserData(ud) => {
            if let Ok(inst) = ud.borrow::<LuaInstance>() {
                let shared = inst.shared.clone();
                return shared.store.borrow().full_name(inst.id);
            }
            if ud.is::<crate::signal::LuaSignal>() {
                return "RBXScriptSignal".to_string();
            }
            "userdata".to_string()
        }
        Value::Function(f) => format!("function: {:p}", f.to_pointer()),
        Value::Thread(t) => format!("thread: {:p}", t.to_pointer()),
        _ => "userdata".to_string(),
    }
    .replace('\u{0}', "")
}

// ---------------------------------------------------------------------------
// ScriptHost
// ---------------------------------------------------------------------------

pub struct ScriptHost {
    lua: Lua,
    shared: Rc<Shared>,
    started: RefCell<HashSet<NodeId>>,
}

impl ScriptHost {
    pub fn new(config: HostConfig) -> Result<Self> {
        let lua = Lua::new();
        lua.load(crate::PRELUDE_SRC)
            .set_name("RobloxPrelude")
            .exec()
            .map_err(|e| anyhow::anyhow!("Luau prelude failed: {e}"))?;
        let shared = Rc::new(Shared::new(&lua, config)?);
        let host = Self {
            lua,
            shared,
            started: RefCell::new(HashSet::new()),
        };
        host.install_globals()?;
        Ok(host)
    }

    pub fn shared(&self) -> &Rc<Shared> {
        &self.shared
    }

    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    pub fn context(&self) -> ExecContext {
        self.shared.context.get()
    }

    pub fn set_context(&self, context: ExecContext) {
        self.shared.context.set(context);
    }

    pub fn logs(&self) -> Vec<LogLine> {
        self.shared.log.lines()
    }

    pub fn take_logs(&self) -> Vec<LogLine> {
        self.shared.log.drain()
    }

    pub fn stats(&self) -> RuntimeStats {
        *self.shared.stats.borrow()
    }

    fn install_globals(&self) -> Result<()> {
        let lua = &self.lua;
        let shared = self.shared.clone();
        let globals = lua.globals();
        let root = shared.store.borrow().root;

        globals.set("game", instance_value(lua, &shared, root)?)?;
        if let Some(ws) = shared.find_service("Workspace") {
            let ws_value = instance_value(lua, &shared, ws)?;
            globals.set("workspace", ws_value.clone())?;
            globals.set("Workspace", ws_value)?;
        }

        // Instance.new
        let instance_table = lua.create_table()?;
        {
            let shared = shared.clone();
            instance_table.set(
                "new",
                lua.create_function(move |lua, (class, parent): (String, Option<Value>)| {
                    let shared = &shared;
                    if let Some(db) = &shared.db {
                        if db.class(&class).is_none() {
                            shared.print(LogLevel::Warn, format!("Instance.new: {class} is not a valid class name"));
                        }
                    }
                    let parent_id = parent.and_then(|p| as_instance_id(&p));
                    let id = shared.store.borrow_mut().create(&class, &class, parent_id);
                    shared.apply_defaults(id);
                    if let Some(class_default_name) = shared
                        .defaults_for(&class)
                        .and_then(|d| d.get("Name").and_then(|v| v.as_str().map(|s| s.to_string())))
                    {
                        let _ = class_default_name;
                    }
                    instance_value(lua, shared, id)
                })?,
            )?;
        }
        globals.set("Instance", instance_table)?;

        // typeof
        {
            let lua_ref = lua;
            globals.set(
                "typeof",
                lua.create_function(move |lua, value: Value| {
                    let _ = lua_ref;
                    let result: String = match &value {
                        Value::UserData(ud) => {
                            if ud.is::<LuaInstance>() {
                                "Instance".to_string()
                            } else if ud.is::<crate::signal::LuaSignal>() {
                                "RBXScriptSignal".to_string()
                            } else if ud.is::<crate::signal::LuaConnection>() {
                                "RBXScriptConnection".to_string()
                            } else {
                                "userdata".to_string()
                            }
                        }
                        Value::Table(t) => t
                            .get::<Option<String>>("__datatype")
                            .ok()
                            .flatten()
                            .unwrap_or_else(|| "table".to_string()),
                        Value::Nil => "nil".to_string(),
                        Value::Boolean(_) => "boolean".to_string(),
                        Value::Integer(_) | Value::Number(_) => "number".to_string(),
                        Value::String(_) => "string".to_string(),
                        Value::Function(_) => "function".to_string(),
                        Value::Thread(_) => "thread".to_string(),
                        _ => "userdata".to_string(),
                    };
                    let _ = lua;
                    Ok(result)
                })?,
            )?;
        }

        // print / warn
        {
            let s = shared.clone();
            globals.set(
                "print",
                lua.create_function(move |lua, args: Variadic<Value>| {
                    let parts: Vec<String> = args.iter().map(|v| value_to_string(lua, v)).collect();
                    s.print(LogLevel::Print, parts.join(" "));
                    Ok(())
                })?,
            )?;
        }
        {
            let s = shared.clone();
            globals.set(
                "warn",
                lua.create_function(move |lua, args: Variadic<Value>| {
                    let parts: Vec<String> = args.iter().map(|v| value_to_string(lua, v)).collect();
                    s.print(LogLevel::Warn, parts.join(" "));
                    Ok(())
                })?,
            )?;
        }
        {
            let s = shared.clone();
            globals.set(
                "_rbx_now",
                lua.create_function(move |_, ()| Ok(s.now()))?,
            )?;
        }
        {
            let s = shared.clone();
            globals.set(
                "tick",
                lua.create_function(move |_, ()| Ok(1_700_000_000.0 + s.start.elapsed().as_secs_f64()))?,
            )?;
        }
        {
            let s = shared.clone();
            globals.set("time", lua.create_function(move |_, ()| Ok(s.now()))?)?;
        }
        {
            let s = shared.clone();
            globals.set("elapsedTime", lua.create_function(move |_, ()| Ok(s.now()))?)?;
        }

        self.install_task_api()?;
        self.install_helpers()?;
        self.install_require()?;

        Ok(())
    }

    fn install_task_api(&self) -> Result<()> {
        let lua = &self.lua;
        let shared = self.shared.clone();
        let globals = lua.globals();

        // _rbx_schedule_wait(delay): registers the running thread for resumption.
        {
            let s = shared.clone();
            globals.set(
                "_rbx_schedule_wait",
                lua.create_function(move |lua, delay: Option<f64>| {
                    let thread = lua.current_thread();
                    // Called from inside the running thread, so `is_resumable()`
                    // is false here; the scheduler resumes it after the delay.
                    s.scheduler
                        .borrow_mut()
                        .spawn(thread, delay.unwrap_or(0.0), s.current_script_name());
                    Ok(())
                })?,
            )?;
        }
        // _rbx_task_spawn(fn, ...)
        {
            let s = shared.clone();
            globals.set(
                "_rbx_task_spawn",
                lua.create_function(move |lua, (func, args): (Function, Variadic<Value>)| {
                    let thread = lua.create_thread(func)?;
                    if let Err(e) = thread.resume::<()>(MultiValue::from_vec(args.to_vec())) {
                        s.report_error(lua, &e.to_string());
                    }
                    Ok(Value::Thread(thread))
                })?,
            )?;
        }
        // _rbx_task_delay(delay, fn, ...) / _rbx_task_defer(fn, ...)
        {
            let s = shared.clone();
            globals.set(
                "_rbx_task_delay",
                lua.create_function(move |lua, (delay, func, args): (f64, Function, Variadic<Value>)| {
                    let thread = lua.create_thread(func)?;
                    s.scheduler.borrow_mut().spawn(
                        thread.clone(),
                        delay,
                        s.current_script_name(),
                    );
                    let _ = args;
                    Ok(Value::Thread(thread))
                })?,
            )?;
        }
        {
            let s = shared.clone();
            globals.set(
                "_rbx_task_defer",
                lua.create_function(move |lua, (func, args): (Function, Variadic<Value>)| {
                    let thread = lua.create_thread(func)?;
                    s.scheduler
                        .borrow_mut()
                        .spawn(thread.clone(), 0.0, s.current_script_name());
                    let _ = args;
                    Ok(Value::Thread(thread))
                })?,
            )?;
        }
        // _rbx_task_cancel(thread)
        {
            let s = shared.clone();
            globals.set(
                "_rbx_task_cancel",
                lua.create_function(move |_, thread: Thread| {
                    s.scheduler.borrow_mut().cancel(thread.to_pointer());
                    Ok(())
                })?,
            )?;
        }
        // _rbx_signal_wait(signal) — register then let the Luau helper yield.
        {
            let s = shared.clone();
            globals.set(
                "_rbx_signal_wait",
                lua.create_function(move |lua, value: Value| {
                    let id = match value {
                        Value::UserData(ud) => ud
                            .borrow::<crate::signal::LuaSignal>()
                            .map(|sig| sig.id)
                            .ok(),
                        _ => None,
                    };
                    if let Some(id) = id {
                        s.wait_signal(id, lua.current_thread());
                    }
                    Ok(())
                })?,
            )?;
        }
        // _rbx_find_child(instance, name)
        {
            let s = shared.clone();
            globals.set(
                "_rbx_find_child",
                lua.create_function(move |lua, (parent, name): (Value, String)| {
                    let Some(parent) = as_instance_id(&parent) else { return Ok(Value::Nil) };
                    match s.store.borrow().child(parent, &name) {
                        Some(child) => instance_value(lua, &s, child),
                        None => Ok(Value::Nil),
                    }
                })?,
            )?;
        }
        Ok(())
    }

    fn install_helpers(&self) -> Result<()> {
        let lua = &self.lua;
        let helpers_src = r#"
local helpers = {}

function helpers.WaitForChild(self, name, timeout)
    local deadline = timeout ~= nil and (os.clock() + timeout) or nil
    while true do
        local child = _rbx_find_child(self, name)
        if child ~= nil then
            return child
        end
        if deadline ~= nil and os.clock() >= deadline then
            error("WaitForChild(" .. tostring(name) .. ") timed out", 2)
        end
        _rbx_schedule_wait(1 / 60)
        coroutine.yield()
    end
end

function helpers.signal_wait(self)
    _rbx_signal_wait(self)
    return coroutine.yield()
end

local task = {}

function task.wait(t)
    _rbx_schedule_wait(t or 0)
    local elapsed = coroutine.yield()
    if elapsed == nil then
        return t or 0
    end
    return elapsed
end

function task.spawn(f, ...)
    return _rbx_task_spawn(f, ...)
end

function task.delay(t, f, ...)
    return _rbx_task_delay(t, f, ...)
end

function task.defer(f, ...)
    return _rbx_task_defer(f, ...)
end

function task.cancel(thread)
    _rbx_task_cancel(thread)
end

function task.synchronize() end
function task.desynchronize() end

_G.task = task
_G.wait = task.wait
_G.delay = task.delay
_G.spawn = task.spawn
_G.defer = task.defer

-- Simulated engine time drives os.clock so waits are frame-accurate.
os.clock = function()
    return _rbx_now()
end

return helpers
"#;
        let helpers: Table = lua.load(helpers_src).set_name("RustbloxRuntime").eval()?;
        *self.shared.helpers.borrow_mut() = Some(helpers);
        Ok(())
    }

    fn install_require(&self) -> Result<()> {
        let lua = &self.lua;
        let shared = self.shared.clone();
        let require = lua.create_function(move |lua, module: Value| -> LuaResult<Value> {
            let shared = &shared;
            let id = match &module {
                Value::UserData(ud) => match ud.borrow::<LuaInstance>() {
                    Ok(inst) => inst.id,
                    Err(_) => {
                        return Err(mlua::Error::RuntimeError(
                            "require expects a ModuleScript".into(),
                        ))
                    }
                },
                Value::String(s) => {
                    let name = s.to_str().map(|s| s.to_string()).unwrap_or_default();
                    find_module_by_name(shared, &name).ok_or_else(|| {
                        mlua::Error::RuntimeError(format!("require: module '{name}' not found"))
                    })?
                }
                _ => {
                    return Err(mlua::Error::RuntimeError(
                        "require expects a ModuleScript".into(),
                    ))
                }
            };

            if let Some(cached) = shared.modules.borrow().get(&id) {
                return Ok(cached.clone());
            }
            if !shared.loading_modules.borrow_mut().insert(id) {
                return Err(mlua::Error::RuntimeError(
                    "require: cyclic dependency detected".into(),
                ));
            }
            let (source, class_name, full_name) = {
                let store = shared.store.borrow();
                (
                    store.source(id).unwrap_or("").to_string(),
                    store.class_name(id).to_string(),
                    store.full_name(id),
                )
            };
            if class_name != "ModuleScript" {
                shared.loading_modules.borrow_mut().remove(&id);
                return Err(mlua::Error::RuntimeError(format!(
                    "require: {full_name} is a {class_name}, not a ModuleScript"
                )));
            }
            let chunk = format!("local script = ...;{source}");
            let func = lua
                .load(&chunk)
                .set_name(full_name.clone())
                .into_function()
                .map_err(|e| mlua::Error::RuntimeError(format!("{full_name}: {e}")))?;
            let script_value = instance_value(lua, shared, id)?;
            let result = func.call::<Value>(script_value);
            shared.loading_modules.borrow_mut().remove(&id);
            match result {
                Ok(value) => {
                    shared.modules.borrow_mut().insert(id, value.clone());
                    Ok(value)
                }
                Err(e) => {
                    shared.report_error(lua, &format!("{full_name}: {e}"));
                    Err(e)
                }
            }
        })?;
        lua.globals().set("require", require)?;
        Ok(())
    }

    // -- Places / scripts ----------------------------------------------------

    /// Adds every BasePart and script from an imported place to the data model.
    pub fn load_scene(&self, scene: &Scene) -> usize {
        let workspace = self.shared.find_service("Workspace").unwrap_or(self.shared.store.borrow().root);
        for part in &scene.parts {
            self.add_part(workspace, part);
        }
        for script in &scene.scripts {
            self.add_script(script);
        }
        scene.scripts.len()
    }

    fn add_part(&self, parent: NodeId, part: &Part) -> NodeId {
        let class = if part.class_name.is_empty() { "Part" } else { part.class_name.as_str() };
        let id = {
            let mut store = self.shared.store.borrow_mut();
            store.create(class, &part.name, Some(parent))
        };
        self.shared.apply_defaults(id);
        let rotation = part.cframe.rotation;
        let mut store = self.shared.store.borrow_mut();
        store.set_prop(id, "CFrame", PropValue::CFrame {
            position: [part.cframe.position.x, part.cframe.position.y, part.cframe.position.z],
            rotation,
        });
        store.set_prop(id, "Size", PropValue::Vector3(part.size.x, part.size.y, part.size.z));
        store.set_prop(id, "Color", PropValue::Color3(part.color[0], part.color[1], part.color[2]));
        store.set_prop(id, "Transparency", PropValue::Number(part.transparency as f64));
        store.set_prop(id, "Reflectance", PropValue::Number(part.reflectance as f64));
        store.set_prop(id, "Material", PropValue::Enum(format!("Material.{}", part.material)));
        store.set_prop(id, "CastShadow", PropValue::Bool(part.cast_shadow));
        drop(store);
        id
    }

    /// Adds a script (or module) to the data model, creating missing ancestors.
    pub fn add_script(&self, script: &ScriptInstance) -> NodeId {
        let parent = self.ensure_path(&script.path);
        let mut store = self.shared.store.borrow_mut();
        let id = store.create(&script.class_name, &script.name, Some(parent));
        store.set_prop(id, "Source", PropValue::String(script.source.clone()));
        store.set_prop(id, "Disabled", PropValue::Bool(!script.enabled));
        drop(store);
        self.shared.apply_defaults(id);
        id
    }

    /// Resolves `["Workspace", "Model", "Folder"]` to a node, creating Folders.
    fn ensure_path(&self, path: &[String]) -> NodeId {
        let mut current = self.shared.store.borrow().root;
        for (index, segment) in path.iter().enumerate() {
            let existing = self.shared.store.borrow().child(current, segment);
            current = match existing {
                Some(id) => id,
                None => {
                    let class = if index == 0 && SERVICES.contains(&segment.as_str()) {
                        segment.as_str()
                    } else {
                        "Folder"
                    };
                    let id = self.shared.store.borrow_mut().create(class, segment, Some(current));
                    self.shared.apply_defaults(id);
                    id
                }
            };
        }
        current
    }

    /// Loads a directory of `.lua`/`.luau` files as a folder of scripts.
    ///
    /// Used for Roblox's own `CoreScripts`/`PlayerScripts` (see `AGENTS.md`):
    /// suffix-tagged files run automatically, plain `.lua` files are modules.
    pub fn load_script_folder(&self, root: &Path, folder_name: &str, origin: ScriptOrigin) -> Result<NodeId> {
        let scripts = rbx_core::scripts::scripts_from_dir_with_origin(root, origin)?;
        let dm = self.shared.store.borrow().root;
        let existing = self.shared.store.borrow().child(dm, folder_name);
        let folder = match existing {
            Some(id) => id,
            None => {
                let id = self.shared.store.borrow_mut().create("Folder", folder_name, Some(dm));
                self.shared.apply_defaults(id);
                id
            }
        };
        for script in scripts {
            let parent = if script.path.is_empty() {
                folder
            } else {
                let mut current = folder;
                for segment in &script.path {
                    let existing = self.shared.store.borrow().child(current, segment);
                    current = match existing {
                        Some(id) => id,
                        None => {
                            let id = self.shared.store.borrow_mut().create("Folder", segment, Some(current));
                            self.shared.apply_defaults(id);
                            id
                        }
                    };
                }
                current
            };
            let id = {
                let mut store = self.shared.store.borrow_mut();
                let id = store.create(&script.class_name, &script.name, Some(parent));
                store.set_prop(id, "Source", PropValue::String(script.source));
                id
            };
            self.shared.apply_defaults(id);
        }
        *self.shared.core_scripts_root.borrow_mut() = Some(folder);
        Ok(folder)
    }

    /// Sugar for loading Roblox's core + player script drops from the local client.
    pub fn load_corescripts(&self, scripts_dir: &Path) -> Result<usize> {
        let core = scripts_dir.join("CoreScripts");
        let player = scripts_dir.join("PlayerScripts");
        let mut count = 0;
        if core.is_dir() {
            self.load_script_folder(&core, "CoreScripts", ScriptOrigin::CoreScripts)?;
            count += 1;
        }
        if player.is_dir() {
            self.load_script_folder(&player, "PlayerScripts", ScriptOrigin::PlayerScripts)?;
            count += 1;
        }
        Ok(count)
    }

    /// Runs every enabled `Script`/`LocalScript` that has not run yet.
    pub fn start_scripts(&self) -> usize {
        let scripts: Vec<NodeId> = {
            let store = self.shared.store.borrow();
            (0..store.len())
                .filter(|id| {
                    let node = store.node(*id);
                    node.is_script() && !node.destroyed
                })
                .collect()
        };
        let mut ran = 0;
        for id in scripts {
            if self.run_script(id) {
                ran += 1;
            }
        }
        ran
    }

    /// Runs one script on its own thread. Returns false for modules/disabled scripts.
    pub fn run_script(&self, id: NodeId) -> bool {
        let (class, source, full_name, disabled) = {
            let store = self.shared.store.borrow();
            let node = store.node(id);
            (
                node.class.clone(),
                node.source.clone().unwrap_or_default(),
                store.full_name(id),
                store.get_prop(id, "Disabled").and_then(|v| v.as_bool()).unwrap_or(false),
            )
        };
        if class == "ModuleScript" || disabled {
            return false;
        }
        if !self.started.borrow_mut().insert(id) {
            return false;
        }
        let context = if class == "LocalScript" {
            ExecContext::Client
        } else {
            ExecContext::Server
        };
        let result = self.run_source(&source, &full_name, context, Some(id));
        if result.is_err() {
            return true; // ran (and reported its error)
        }
        self.shared.stats.borrow_mut().scripts_run += 1;
        true
    }

    /// Compiles and runs Luau from the command bar.
    pub fn exec(&self, source: &str, name: &str, context: ExecContext) -> Result<Value, String> {
        self.run_source(source, name, context, None)
    }

    fn run_source(
        &self,
        source: &str,
        name: &str,
        context: ExecContext,
        script_id: Option<NodeId>,
    ) -> Result<Value, String> {
        let lua = &self.lua;
        let chunk = format!("local script = ...;{source}");
        let func = lua
            .load(&chunk)
            .set_name(name.to_string())
            .into_function()
            .map_err(|e| {
                let msg = format!("{name}: {e}");
                self.shared.report_error(lua, &msg);
                msg
            })?;
        let thread = lua.create_thread(func).map_err(|e| e.to_string())?;
        let script_value = match script_id {
            Some(id) => instance_value(lua, &self.shared, id).map_err(|e| e.to_string())?,
            None => Value::Nil,
        };

        let previous_context = self.shared.context.get();
        let previous_script = self.shared.current_script.borrow().clone();
        self.shared.context.set(context);
        if let Some(id) = script_id {
            *self.shared.current_script.borrow_mut() = Some((id, name.to_string()));
        }
        let result = thread.resume::<Value>(MultiValue::from_vec(vec![script_value]));
        self.shared.context.set(previous_context);
        *self.shared.current_script.borrow_mut() = previous_script;

        match result {
            Ok(Value::Nil) => Ok(Value::Nil),
            Ok(v) => Ok(v),
            Err(e) => {
                let message = format!("{e}");
                self.shared.report_error(lua, &format!("{name}: {message}"));
                Err(message)
            }
        }
    }

    /// Advances the runtime by one frame: engine signals, scheduled tasks, debris.
    pub fn step(&self, dt: f64) {
        let lua = &self.lua;
        let shared = &self.shared;

        shared.scheduler.borrow_mut().advance(dt);
        shared.stats.borrow_mut().steps += 1;
        let now = shared.now();

        if let Some(run_service) = shared.find_service("RunService") {
            let stepped = shared.signal(run_service, "Stepped");
            shared.fire(
                lua,
                stepped,
                MultiValue::from_vec(vec![Value::Number(now), Value::Number(dt)]),
            );
            if shared.context.get().is_client() {
                let render = shared.signal(run_service, "RenderStepped");
                shared.fire(lua, render, MultiValue::from_vec(vec![Value::Number(dt)]));
            }
            let heartbeat = shared.signal(run_service, "Heartbeat");
            shared.fire(
                lua,
                heartbeat,
                MultiValue::from_vec(vec![Value::Number(dt)]),
            );
        }

        let due = shared.scheduler.borrow_mut().take_due();
        for task in due {
            if !task.thread.is_resumable() {
                continue;
            }
            if let Err(e) = task.thread.resume::<()>(MultiValue::new()) {
                let name = task.script.unwrap_or_else(|| "<task>".to_string());
                shared.report_error(lua, &format!("{name}: {e}"));
            }
        }

        // Debris:AddItem
        let ready: Vec<NodeId> = {
            let mut destroys = shared.destroys.borrow_mut();
            let mut ready = Vec::new();
            let mut i = 0;
            while i < destroys.len() {
                if destroys[i].0 <= now {
                    ready.push(destroys.swap_remove(i).1);
                } else {
                    i += 1;
                }
            }
            ready
        };
        for id in ready {
            let _ = shared.store.borrow_mut().destroy(id);
        }
    }

    pub fn time(&self) -> f64 {
        self.shared.now()
    }
}

fn find_module_by_name(shared: &Shared, name: &str) -> Option<NodeId> {
    let store = shared.store.borrow();
    (0..store.len()).find(|id| {
        let node = store.node(*id);
        node.class == "ModuleScript" && !node.destroyed && (node.name == name || store.full_name(*id) == name)
    })
}

// ---------------------------------------------------------------------------
// Service methods
// ---------------------------------------------------------------------------

pub fn service_method(
    lua: &Lua,
    shared: &Rc<Shared>,
    id: NodeId,
    class: &str,
    name: &str,
    args: &[Value],
) -> LuaResult<Option<Value>> {
    let arg_str = |i: usize| -> Option<String> {
        args.get(i).and_then(|v| match v {
            Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
            _ => None,
        })
    };
    let arg_num = |i: usize| -> Option<f64> {
        args.get(i).and_then(|v| match v {
            Value::Number(n) => Some(*n),
            Value::Integer(n) => Some(*n as f64),
            _ => None,
        })
    };
    let arg_bool = |i: usize| -> Option<bool> {
        args.get(i).and_then(|v| match v {
            Value::Boolean(b) => Some(*b),
            _ => None,
        })
    };

    match class {
        "RunService" => match name {
            "IsServer" => Ok(Some(Value::Boolean(shared.context.get().is_server()))),
            "IsClient" => Ok(Some(Value::Boolean(shared.context.get().is_client()))),
            "IsStudio" => Ok(Some(Value::Boolean(true))),
            "IsRunMode" => Ok(Some(Value::Boolean(true))),
            "IsRunning" => Ok(Some(Value::Boolean(true))),
            "BindToRenderStep" | "UnbindFromRenderStep" | "Set3dRenderingEnabled" | "Pause"
            | "Run" | "Stop" | "BindToClose" => {
                shared.warn_once(
                    &format!("runservice-{name}"),
                    &format!("RunService:{name} is a no-op in Rustblox (frame stepping is host-driven)"),
                );
                Ok(Some(Value::Nil))
            }
            "GetRobloxVersion" => Ok(Some(Value::String(lua.create_string("0.0.0.0")?))),
            _ => Ok(None),
        },
        "Players" => match name {
            "GetPlayers" => {
                let players = shared
                    .store
                    .borrow()
                    .children(id)
                    .into_iter()
                    .filter(|p| shared.store.borrow().class_name(*p) == "Player")
                    .collect::<Vec<_>>();
                let table = lua.create_table_with_capacity(players.len(), 0)?;
                for (i, player) in players.into_iter().enumerate() {
                    table.set(i + 1, instance_value(lua, shared, player)?)?;
                }
                Ok(Some(Value::Table(table)))
            }
            "GetPlayerFromCharacter" => {
                let character = args.first().and_then(as_instance_id);
                if character.is_none() {
                    return Ok(Some(Value::Nil));
                }
                match shared.local_player() {
                    Some(player) => Ok(Some(instance_value(lua, shared, player)?)),
                    None => Ok(Some(Value::Nil)),
                }
            }
            "GetPlayerByUserId" | "GetUserIdFromNameAsync" => Ok(Some(Value::Nil)),
            _ => Ok(None),
        },
        "PhysicsService" => physics_service_method(lua, shared, name, args),
        "SettingsService" => settings_service_method(lua, shared, name, args),
        "SteamworksService" => steam_service_method(lua, shared, name, args),
        "Debris" => {
            if name == "AddItem" {
                if let Some(item) = args.first().and_then(as_instance_id) {
                    let lifetime = arg_num(1).unwrap_or(0.0);
                    shared.destroys.borrow_mut().push((shared.now() + lifetime, item));
                }
                Ok(Some(Value::Nil))
            } else {
                Ok(None)
            }
        }
        "CollectionService" => match name {
            "AddTag" | "RemoveTag" | "HasTag" | "GetTags" => {
                let Some(target) = args.first().and_then(as_instance_id) else { return Ok(Some(Value::Nil)) };
                let inst = LuaInstance { shared: shared.clone(), id: target };
                let mut call_args = args.to_vec();
                call_args.remove(0);
                let result = inst.call_method(lua, name, &MultiValue::from_vec(call_args))?;
                Ok(Some(result))
            }
            "GetTagged" => {
                let tag = arg_str(0).unwrap_or_default();
                let tagged: Vec<NodeId> = {
                    let store = shared.store.borrow();
                    (0..store.len())
                        .filter(|n| store.node(*n).tags.contains(&tag))
                        .collect()
                };
                let table = lua.create_table_with_capacity(tagged.len(), 0)?;
                for (i, n) in tagged.into_iter().enumerate() {
                    table.set(i + 1, instance_value(lua, shared, n)?)?;
                }
                Ok(Some(Value::Table(table)))
            }
            "GetInstanceAddedSignal" | "GetInstanceRemovedSignal" => {
                let sig = shared.signal(id, &format!("{name}:{}", arg_str(0).unwrap_or_default()));
                Ok(Some(Value::UserData(lua.create_userdata(crate::signal::LuaSignal::new(shared.clone(), sig))?)))
            }
            _ => Ok(None),
        },
        "TweenService" => {
            if name == "Create" {
                let Some(target) = args.first().and_then(as_instance_id) else { return Ok(Some(Value::Nil)) };
                let goal = match args.get(2) {
                    Some(Value::Table(t)) => t.clone(),
                    _ => lua.create_table()?,
                };
                let tween = lua.create_table()?;
                {
                    let shared = shared.clone();
                    let goal = goal.clone();
                    tween.set(
                        "Play",
                        lua.create_function(move |lua, ()| {
                            let inst = LuaInstance { shared: shared.clone(), id: target };
                            for pair in goal.clone().pairs::<String, Value>() {
                                let (key, value) = pair?;
                                inst.set(lua, &key, &value)?;
                            }
                            Ok(())
                        })?,
                    )?;
                }
                tween.set("Cancel", lua.create_function(|_, ()| Ok(()))?)?;
                tween.set("Pause", lua.create_function(|_, ()| Ok(()))?)?;
                let completed = shared.signal(target, "TweenCompleted");
                tween.set(
                    "Completed",
                    Value::UserData(lua.create_userdata(crate::signal::LuaSignal::new(shared.clone(), completed))?),
                )?;
                Ok(Some(Value::Table(tween)))
            } else {
                Ok(None)
            }
        }
        "HttpService" => match name {
            "JSONEncode" => {
                let Some(value) = args.first() else { return Ok(Some(Value::Nil)) };
                let json = lua_to_json(value);
                Ok(Some(Value::String(lua.create_string(&json)?)))
            }
            "JSONDecode" => {
                let text = arg_str(0).unwrap_or_default();
                match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(v) => Ok(Some(json_to_lua(lua, &v)?)),
                    Err(e) => Err(mlua::Error::RuntimeError(format!("JSONDecode: {e}"))),
                }
            }
            "GenerateGUID" => {
                let guid = format!(
                    "{:08X}-{:04X}-{:04X}-{:04X}-{:012X}",
                    std::process::id(),
                    (shared.now() * 1000.0) as u16,
                    0x4A3Bu16,
                    0x8F00u16,
                    (shared.start.elapsed().as_nanos() as u64) & 0xFFFF_FFFF_FFFF
                );
                Ok(Some(Value::String(lua.create_string(&guid)?)))
            }
            "UrlEncode" => {
                let text = arg_str(0).unwrap_or_default();
                Ok(Some(Value::String(lua.create_string(url_encode(&text))?)))
            }
            "GetAsync" => Err(mlua::Error::RuntimeError(
                "HttpService:GetAsync is disabled (HttpEnabled is false)".into(),
            )),
            _ => Ok(None),
        },
        "UserInputService" => match name {
            "GetMouseLocation" => Ok(Some(convert::to_lua(
                lua,
                &shared.converters,
                &PropValue::Vector2(0.0, 0.0),
            )?)),
            "IsKeyDown" => Ok(Some(Value::Boolean(false))),
            "GetKeysPressed" => Ok(Some(Value::Table(lua.create_table()?))),
            "GetFocusedTextBox" => Ok(Some(Value::Nil)),
            _ => Ok(None),
        },
        "ContentProvider" => {
            if name == "Preload" || name == "PreloadAsync" {
                Ok(Some(Value::Nil))
            } else {
                Ok(None)
            }
        }
        "Workspace" => match name {
            "GetRealPhysicsFPS" => Ok(Some(Value::Number(60.0))),
            "GetNumAwakeParts" => Ok(Some(Value::Number(0.0))),
            "GetPhysicsThrottling" => Ok(Some(Value::Number(0.0))),
            "BreakJoints" => {
                shared.warn_once(
                    "breakjoints",
                    "Workspace:BreakJoints is not implemented (native joints pending)",
                );
                Ok(Some(Value::Nil))
            }
            _ => Ok(None),
        },
        "Selection" => match name {
            "Set" => {
                let ids: Vec<NodeId> = match args.first() {
                    Some(Value::Table(t)) => t
                        .sequence_values::<Value>()
                        .filter_map(|v| v.ok())
                        .filter_map(|v| as_instance_id(&v))
                        .collect(),
                    _ => Vec::new(),
                };
                shared.store.borrow_mut().set_prop(id, "Selection", PropValue::Opaque(format!("{ids:?}")));
                Ok(Some(Value::Nil))
            }
            "Get" => Ok(Some(Value::Table(lua.create_table()?))),
            "Add" | "Remove" => Ok(Some(Value::Nil)),
            _ => Ok(None),
        },
        "MarketplaceService" | "TeleportService" => {
            shared.warn_once(
                &format!("{class}-{name}"),
                &format!("{class}:{name} is not implemented yet"),
            );
            Ok(Some(Value::Nil))
        }
        _ => {
            let _ = arg_bool;
            Ok(None)
        }
    }
}

fn physics_service_method(
    lua: &Lua,
    shared: &Rc<Shared>,
    name: &str,
    args: &[Value],
) -> LuaResult<Option<Value>> {
    let arg_str = |i: usize| -> Option<String> {
        args.get(i).and_then(|v| match v {
            Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
            _ => None,
        })
    };
    let arg_bool = |i: usize| -> Option<bool> {
        args.get(i).and_then(|v| match v {
            Value::Boolean(b) => Some(*b),
            _ => None,
        })
    };

    match name {
        "CreateCollisionGroup" | "RegisterCollisionGroup" => {
            let group = arg_str(0).unwrap_or_default();
            shared
                .collision_groups
                .borrow_mut()
                .groups
                .entry(group)
                .or_default();
            Ok(Some(Value::Nil))
        }
        "UnregisterCollisionGroup" => {
            let group = arg_str(0).unwrap_or_default();
            shared.collision_groups.borrow_mut().groups.remove(&group);
            Ok(Some(Value::Nil))
        }
        "SetPartCollisionGroup" => {
            let part = args.first().and_then(as_instance_id);
            let group = arg_str(1).unwrap_or_default();
            if let Some(part) = part {
                shared
                    .collision_groups
                    .borrow_mut()
                    .groups
                    .entry(group.clone())
                    .or_default()
                    .insert(part);
                shared
                    .store
                    .borrow_mut()
                    .set_prop(part, "CollisionGroup", PropValue::String(group));
            }
            Ok(Some(Value::Nil))
        }
        "CollisionGroupContainsPart" | "GetCollisionGroupContainsPart" => {
            let group = arg_str(0).unwrap_or_default();
            let part = args.get(1).and_then(as_instance_id);
            let contains = match part {
                Some(part) => shared
                    .collision_groups
                    .borrow()
                    .groups
                    .get(&group)
                    .map(|set| set.contains(&part))
                    .unwrap_or(false),
                None => false,
            };
            Ok(Some(Value::Boolean(contains)))
        }
        "GetRegisteredCollisionGroups" | "GetCollisionGroups" | "GetCreateCollisionGroups" => {
            let groups: Vec<String> = shared.collision_groups.borrow().groups.keys().cloned().collect();
            let table = lua.create_table_with_capacity(groups.len(), 0)?;
            for (i, group) in groups.into_iter().enumerate() {
                let entry = lua.create_table()?;
                entry.set("name", group.clone())?;
                entry.set("Name", group)?;
                table.set(i + 1, entry)?;
            }
            Ok(Some(Value::Table(table)))
        }
        "SetCollisionGroupCollidable" => {
            let a = arg_str(0).unwrap_or_default();
            let b = arg_str(1).unwrap_or_default();
            let collidable = arg_bool(2).unwrap_or(true);
            shared
                .collision_groups
                .borrow_mut()
                .collidable
                .insert((a, b), collidable);
            Ok(Some(Value::Nil))
        }
        "CollisionGroupsAreCollidable" => {
            let a = arg_str(0).unwrap_or_default();
            let b = arg_str(1).unwrap_or_default();
            let collidable = shared
                .collision_groups
                .borrow()
                .collidable
                .get(&(a, b))
                .copied()
                .unwrap_or(true);
            Ok(Some(Value::Boolean(collidable)))
        }
        "GetMaxCollisionGroups" => Ok(Some(Value::Number(32.0))),
        _ => Ok(None),
    }
}

fn settings_service_method(
    lua: &Lua,
    shared: &Rc<Shared>,
    name: &str,
    args: &[Value],
) -> LuaResult<Option<Value>> {
    let arg_str = |i: usize| -> Option<String> {
        args.get(i).and_then(|v| match v {
            Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
            _ => None,
        })
    };
    match name {
        "GetSetting" => {
            let Some(key) = arg_str(0) else { return Ok(Some(Value::Nil)) };
            let value = shared.settings.borrow().get(&key).cloned();
            match value {
                Some(v) => Ok(Some(convert::to_lua(lua, &shared.converters, &v)?)),
                None => Ok(Some(Value::Nil)),
            }
        }
        "SetSetting" => {
            if !shared.context.get().can_write_settings() {
                return Err(mlua::Error::RuntimeError(
                    "SettingsService:SetSetting is server-only".into(),
                ));
            }
            let Some(key) = arg_str(0) else { return Ok(Some(Value::Nil)) };
            let Some(value) = args.get(1) else { return Ok(Some(Value::Nil)) };
            let converted = convert::from_lua(value);
            shared.settings.borrow_mut().insert(key.clone(), converted);
            shared.save_settings();
            if let Some(service) = shared.find_service("SettingsService") {
                shared.fire_node_event_with(
                    lua,
                    service,
                    "SettingChanged",
                    vec![Value::String(lua.create_string(key)?), value.clone()],
                );
            }
            Ok(Some(Value::Nil))
        }
        "GetWindowModes" => {
            let table = lua.create_table()?;
            for (i, mode) in ["Windowed", "Fullscreen", "WindowedFullscreen"].iter().enumerate() {
                table.set(i + 1, *mode)?;
            }
            Ok(Some(Value::Table(table)))
        }
        "GetFrameGenerationModes" => {
            let table = lua.create_table()?;
            for (i, mode) in ["Off", "FSR", "DLSS"].iter().enumerate() {
                table.set(i + 1, *mode)?;
            }
            Ok(Some(Value::Table(table)))
        }
        "GetAnisotropyLevels" => {
            let table = lua.create_table()?;
            for (i, level) in [1, 2, 4, 8, 16].iter().enumerate() {
                table.set(i + 1, *level)?;
            }
            Ok(Some(Value::Table(table)))
        }
        "ApplyPreset" => {
            if !shared.context.get().can_write_settings() {
                return Err(mlua::Error::RuntimeError(
                    "SettingsService:ApplyPreset is server-only".into(),
                ));
            }
            let preset = arg_str(0).unwrap_or_else(|| "Performance".to_string());
            let (quality, frame_gen) = if preset == "Quality" {
                ("TAA", "Off")
            } else {
                ("FXAA", "FSR")
            };
            {
                let mut settings = shared.settings.borrow_mut();
                settings.insert("AntiAliasing".into(), PropValue::String(quality.into()));
                settings.insert("FrameGeneration".into(), PropValue::String(frame_gen.into()));
            }
            shared.save_settings();
            Ok(Some(Value::Nil))
        }
        "CreateDefaultSettingsGui" => Ok(Some(Value::Nil)),
        _ => Ok(None),
    }
}

fn steam_service_method(
    lua: &Lua,
    shared: &Rc<Shared>,
    name: &str,
    args: &[Value],
) -> LuaResult<Option<Value>> {
    if !shared.context.get().can_use_steam() {
        return Err(mlua::Error::RuntimeError(
            "SteamworksService is server-only".into(),
        ));
    }
    let arg_str = |i: usize| -> Option<String> {
        args.get(i).and_then(|v| match v {
            Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
            _ => None,
        })
    };
    let steam = &shared.steam;
    match name {
        "Init" => Ok(Some(Value::Boolean(steam.init()))),
        "IsOverlayEnabled" => Ok(Some(Value::Boolean(steam.is_overlay_enabled()))),
        "RequestCurrentStats" => Ok(Some(Value::Boolean(steam.request_current_stats()))),
        "UnlockAchievement" => Ok(Some(Value::Boolean(
            steam.unlock_achievement(&arg_str(0).unwrap_or_default()),
        ))),
        "ClearAchievement" => Ok(Some(Value::Boolean(
            steam.clear_achievement(&arg_str(0).unwrap_or_default()),
        ))),
        "GetAchievement" => Ok(Some(Value::Boolean(
            steam.get_achievement(&arg_str(0).unwrap_or_default()),
        ))),
        "SetStat" => {
            let key = arg_str(0).unwrap_or_default();
            let value = args.get(1).and_then(|v| match v {
                Value::Number(n) => Some(*n as i64),
                Value::Integer(i) => Some(*i),
                _ => None,
            }).unwrap_or(0);
            Ok(Some(Value::Boolean(steam.set_stat(&key, value))))
        }
        "GetStat" => Ok(Some(Value::Number(
            steam.get_stat(&arg_str(0).unwrap_or_default()) as f64,
        ))),
        "StoreStats" => Ok(Some(Value::Boolean(steam.store_stats()))),
        "FileWrite" => {
            let key = arg_str(0).unwrap_or_default();
            let data = arg_str(1).unwrap_or_default();
            Ok(Some(Value::Boolean(steam.file_write(&key, data.as_bytes()))))
        }
        "FileRead" => {
            let key = arg_str(0).unwrap_or_default();
            match steam.file_read(&key) {
                Some(bytes) => Ok(Some(Value::String(lua.create_string(&bytes)?))),
                None => Ok(Some(Value::Nil)),
            }
        }
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// JSON helpers (HttpService)
// ---------------------------------------------------------------------------

fn lua_to_json(value: &Value) -> String {
    match value {
        Value::Nil => "null".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => {
            if n.is_finite() {
                n.to_string()
            } else {
                "null".to_string()
            }
        }
        Value::String(s) => {
            let text = s.to_str().map(|s| s.to_string()).unwrap_or_default();
            serde_json::to_string(&text).unwrap_or_else(|_| "\"\"".to_string())
        }
        Value::Table(t) => {
            let len = t.raw_len();
            if len > 0 {
                let mut items = Vec::new();
                for i in 1..=len {
                    if let Ok(v) = t.get::<Value>(i) {
                        items.push(lua_to_json(&v));
                    }
                }
                format!("[{}]", items.join(","))
            } else {
                let mut entries = Vec::new();
                for pair in t.clone().pairs::<String, Value>() {
                    if let Ok((k, v)) = pair {
                        entries.push(format!(
                            "{}:{}",
                            serde_json::to_string(&k).unwrap_or_else(|_| "\"\"".to_string()),
                            lua_to_json(&v)
                        ));
                    }
                }
                format!("{{{}}}", entries.join(","))
            }
        }
        _ => "null".to_string(),
    }
}

fn json_to_lua(lua: &Lua, value: &serde_json::Value) -> LuaResult<Value> {
    Ok(match value {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => Value::String(lua.create_string(s)?),
        serde_json::Value::Array(items) => {
            let table = lua.create_table_with_capacity(items.len(), 0)?;
            for (i, item) in items.iter().enumerate() {
                table.set(i + 1, json_to_lua(lua, item)?)?;
            }
            Value::Table(table)
        }
        serde_json::Value::Object(map) => {
            let table = lua.create_table()?;
            for (k, v) in map {
                table.set(k.as_str(), json_to_lua(lua, v)?)?;
            }
            Value::Table(table)
        }
    })
}

fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
