//! Roblox-style signals (`RBXScriptSignal`) and connections.
//!
//! Handlers are run in their own coroutine, so a handler may yield (`task.wait`)
//! exactly like it can in Roblox. `Signal:Wait()` is implemented as a Luau helper
//! (see `runtime::install_luau_helpers`) so the yield never crosses a C boundary.

use crate::datamodel::NodeId;
use crate::runtime::Shared;
use mlua::{Function, Lua, MultiValue, Result as LuaResult, Thread, UserData, UserDataMethods, Value};
use std::collections::BTreeMap;
use std::rc::Rc;

pub struct Handler {
    pub func: Function,
    pub once: bool,
}

#[derive(Default)]
pub struct SignalRegistry {
    next_id: usize,
    pub signals: BTreeMap<usize, SignalData>,
}

pub struct SignalData {
    pub node: NodeId,
    pub name: String,
    /// Handlers in connection order (`None` marks a disconnected slot).
    pub handlers: Vec<Option<Handler>>,
    /// Threads blocked in `:Wait()`.
    pub waiting: Vec<Thread>,
}

impl SignalRegistry {
    pub fn create(&mut self, node: NodeId, name: &str) -> usize {
        self.next_id += 1;
        let id = self.next_id;
        self.signals.insert(
            id,
            SignalData {
                node,
                name: name.to_string(),
                handlers: Vec::new(),
                waiting: Vec::new(),
            },
        );
        id
    }

    pub fn get(&self, id: usize) -> Option<&SignalData> {
        self.signals.get(&id)
    }

    pub fn get_mut(&mut self, id: usize) -> Option<&mut SignalData> {
        self.signals.get_mut(&id)
    }
}

/// `RBXScriptSignal`
#[derive(Clone)]
pub struct LuaSignal {
    pub shared: Rc<Shared>,
    pub id: usize,
}

impl LuaSignal {
    pub fn new(shared: Rc<Shared>, id: usize) -> Self {
        Self { shared, id }
    }
}

impl UserData for LuaSignal {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(mlua::MetaMethod::Index, |lua, this, key: String| -> LuaResult<Value> {
            // `signal:Connect(fn)` passes the signal userdata as the first
            // argument (the functions come from `__index`), so accept any
            // argument list and pick the handler out of it.
            let handler = |args: MultiValue| -> Option<Function> {
                args.into_iter().rev().find_map(|v| match v {
                    Value::Function(f) => Some(f),
                    _ => None,
                })
            };
            match key.as_str() {
                "Connect" | "connect" => {
                    let shared = this.shared.clone();
                    let id = this.id;
                    Ok(Value::Function(lua.create_function(move |lua, args: MultiValue| {
                        let Some(func) = handler(args) else {
                            return Err(mlua::Error::runtime("Connect expects a function"));
                        };
                        connect(lua, &shared, id, func, false)
                    })?))
                }
                "Once" => {
                    let shared = this.shared.clone();
                    let id = this.id;
                    Ok(Value::Function(lua.create_function(move |lua, args: MultiValue| {
                        let Some(func) = handler(args) else {
                            return Err(mlua::Error::runtime("Once expects a function"));
                        };
                        connect(lua, &shared, id, func, true)
                    })?))
                }
                "ConnectParallel" => {
                    let shared = this.shared.clone();
                    let id = this.id;
                    Ok(Value::Function(lua.create_function(move |lua, args: MultiValue| {
                        let Some(func) = handler(args) else {
                            return Err(mlua::Error::runtime("ConnectParallel expects a function"));
                        };
                        connect(lua, &shared, id, func, false)
                    })?))
                }
                "Wait" => {
                    // Luau helper so `yield` happens on a pure-Lua stack.
                    let helpers: Option<mlua::Table> = this.shared.helpers.borrow().clone();
                    match helpers.and_then(|h| h.get::<Option<Function>>("signal_wait").ok().flatten()) {
                        Some(f) => Ok(Value::Function(f)),
                        None => Ok(Value::Nil),
                    }
                }
                "Fire" | "FireAll" => {
                    // Custom bindable-style firing (also handy for tests).
                    let shared = this.shared.clone();
                    let id = this.id;
                    Ok(Value::Function(lua.create_function(
                        move |lua, args: MultiValue| {
                            let mut values = args.into_vec();
                            // Drop the leading `self` userdata from `sig:Fire(...)`.
                            if values
                                .first()
                                .map(|v| matches!(v, Value::UserData(ud) if ud.is::<LuaSignal>()))
                                .unwrap_or(false)
                            {
                                values.remove(0);
                            }
                            shared.fire(lua, id, MultiValue::from_vec(values));
                            Ok(())
                        },
                    )?))
                }
                _ => Ok(Value::Nil),
            }
        });

        methods.add_meta_method(mlua::MetaMethod::ToString, |_, this, ()| {
            let name = this
                .shared
                .signals
                .borrow()
                .get(this.id)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "Signal".to_string());
            Ok(format!("RBXScriptSignal({name})"))
        });
    }
}

fn connect(
    lua: &Lua,
    shared: &Rc<Shared>,
    id: usize,
    func: Function,
    once: bool,
) -> LuaResult<Value> {
    let slot = {
        let mut reg = shared.signals.borrow_mut();
        match reg.get_mut(id) {
            Some(data) => {
                data.handlers.push(Some(Handler { func, once }));
                data.handlers.len() - 1
            }
            None => 0,
        }
    };
    let ud = lua.create_userdata(LuaConnection {
        shared: shared.clone(),
        signal: id,
        slot,
    })?;
    Ok(Value::UserData(ud))
}

/// `RBXScriptConnection`
#[derive(Clone)]
pub struct LuaConnection {
    pub shared: Rc<Shared>,
    pub signal: usize,
    pub slot: usize,
}

impl LuaConnection {
    pub fn connected(&self) -> bool {
        self.shared
            .signals
            .borrow()
            .get(self.signal)
            .and_then(|s| s.handlers.get(self.slot))
            .map(|h| h.is_some())
            .unwrap_or(false)
    }
}

impl UserData for LuaConnection {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("Disconnect", |_, this, ()| {
            if let Some(data) = this.shared.signals.borrow_mut().get_mut(this.signal) {
                if let Some(slot) = data.handlers.get_mut(this.slot) {
                    *slot = None;
                }
            }
            Ok(())
        });
        methods.add_method("disconnect", |_, this, ()| {
            if let Some(data) = this.shared.signals.borrow_mut().get_mut(this.signal) {
                if let Some(slot) = data.handlers.get_mut(this.slot) {
                    *slot = None;
                }
            }
            Ok(())
        });
        methods.add_meta_method(mlua::MetaMethod::Index, |_, this, key: String| {
            match key.as_str() {
                "Connected" => Ok(Value::Boolean(this.connected())),
                _ => Ok(Value::Nil),
            }
        });
    }
}

impl Shared {
    pub fn signal(&self, node: NodeId, name: &str) -> usize {
        let key = (node, name.to_string());
        if let Some(id) = self.signal_index.borrow().get(&key) {
            return *id;
        }
        let id = self.signals.borrow_mut().create(node, name);
        self.signal_index.borrow_mut().insert(key, id);
        id
    }

    /// Runs all handlers (each in its own thread) and resumes `:Wait()`ers.
    pub fn fire(&self, lua: &Lua, signal_id: usize, args: MultiValue) {
        let (handlers, waiting) = {
            let mut reg = self.signals.borrow_mut();
            let Some(data) = reg.get_mut(signal_id) else { return };
            let mut handlers = Vec::new();
            for handler in data.handlers.iter_mut() {
                if let Some(h) = handler {
                    let func = h.func.clone();
                    if h.once {
                        *handler = None;
                    }
                    handlers.push(func);
                }
            }
            let waiting = std::mem::take(&mut data.waiting);
            (handlers, waiting)
        };

        for func in handlers {
            match lua.create_thread(func) {
                Ok(thread) => {
                    if let Err(e) = thread.resume::<()>(args.clone()) {
                        self.report_error(lua, &e.to_string());
                    }
                }
                Err(e) => self.report_error(lua, &e.to_string()),
            }
        }
        for thread in waiting {
            let _ = thread.resume::<()>(args.clone());
        }
    }

    /// Registers a thread in `Signal:Wait()`; the next `fire` resumes it.
    pub fn wait_signal(&self, signal_id: usize, thread: Thread) {
        if let Some(data) = self.signals.borrow_mut().get_mut(signal_id) {
            data.waiting.push(thread);
        }
    }
}
