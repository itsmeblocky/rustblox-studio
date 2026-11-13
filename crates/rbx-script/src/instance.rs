//! `Instance` userdata: property access, methods and signals.
//!
//! Property reads follow Roblox's resolution order: built-in derived values
//! (`Position`, `AssemblyMass`, ...), then explicitly stored properties, then the
//! API-dump default for the class, then children (so `workspace.Part` works), then
//! signals and methods.

use crate::convert;
use crate::datamodel::{NodeId, PropValue};
use crate::runtime::Shared;
use mlua::{Lua, MetaMethod, MultiValue, Result as LuaResult, UserData, UserDataMethods, Value};
use std::rc::Rc;

#[derive(Clone)]
pub struct LuaInstance {
    pub shared: Rc<Shared>,
    pub id: NodeId,
}

pub fn instance_value(lua: &Lua, shared: &Rc<Shared>, id: NodeId) -> LuaResult<Value> {
    Ok(Value::UserData(lua.create_userdata(LuaInstance {
        shared: shared.clone(),
        id,
    })?))
}

/// Extracts an instance id from a Luau value.
pub fn as_instance_id(value: &Value) -> Option<NodeId> {
    match value {
        Value::UserData(ud) => ud.borrow::<LuaInstance>().ok().map(|i| i.id),
        _ => None,
    }
}

fn arg_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
        _ => None,
    }
}

fn arg_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => Some(*n),
        Value::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

fn arg_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Boolean(b) => Some(*b),
        _ => None,
    }
}

/// Arguments after the implicit `self` when a method is called with `:`.
fn rest_args(args: &MultiValue, id: NodeId) -> Vec<Value> {
    let mut out = Vec::new();
    for (i, v) in args.iter().enumerate() {
        if i == 0 && as_instance_id(v) == Some(id) {
            continue;
        }
        out.push(v.clone());
    }
    out
}

impl LuaInstance {
    fn class(&self) -> String {
        self.shared.store.borrow().class_name(self.id).to_string()
    }

    pub fn get(&self, lua: &Lua, name: &str) -> LuaResult<Value> {
        let shared = &self.shared;

        // Derived / read-only properties.
        match name {
            "ClassName" => return Ok(Value::String(lua.create_string(self.class())?)),
            "Name" => {
                let n = shared.store.borrow().name(self.id).to_string();
                return Ok(Value::String(lua.create_string(n)?));
            }
            "Parent" => {
                let parent = shared.store.borrow().node(self.id).parent;
                return match parent {
                    Some(p) => instance_value(lua, shared, p),
                    None => Ok(Value::Nil),
                };
            }
            "Position" => {
                if let Some(PropValue::CFrame { position, .. }) = shared.store.borrow().get_prop(self.id, "CFrame") {
                    return convert::to_lua(lua, &shared.converters, &PropValue::Vector3(position[0], position[1], position[2]));
                }
            }
            "Orientation" | "Rotation" => {
                if let Some(PropValue::CFrame { rotation, .. }) = shared.store.borrow().get_prop(self.id, "CFrame") {
                    let (rx, ry, rz) = euler_from_rotation(&rotation);
                    return convert::to_lua(lua, &shared.converters, &PropValue::Vector3(rx.to_degrees(), ry.to_degrees(), rz.to_degrees()));
                }
            }
            "AbsolutePosition" => {
                if let Some(PropValue::CFrame { position, .. }) = shared.store.borrow().get_prop(self.id, "CFrame") {
                    return convert::to_lua(lua, &shared.converters, &PropValue::Vector3(position[0], position[1], position[2]));
                }
            }
            "AbsoluteSize" => {
                let size = self.get(lua, "Size")?;
                if !matches!(size, Value::Nil) {
                    return Ok(size);
                }
            }
            "AssemblyMass" | "Mass" => {
                let mass = self.compute_mass();
                return Ok(Value::Number(mass));
            }
            "LocalPlayer" if self.class() == "Players" => {
                if shared.context.get().is_client() {
                    if let Some(player) = shared.local_player() {
                        return instance_value(lua, shared, player);
                    }
                }
                return Ok(Value::Nil);
            }
            "CurrentCamera" => return Ok(Value::Nil),
            "Character" => return Ok(Value::Nil),
            "DataCost" => return Ok(Value::Number(0.0)),
            _ => {}
        }

        // Stored property.
        if let Some(v) = shared.store.borrow().get_prop(self.id, name) {
            if let PropValue::Instance(target) = v {
                return instance_value(lua, shared, target);
            }
            return convert::to_lua(lua, &shared.converters, &v);
        }

        // API-dump default for the class.
        if let Some(defaults) = shared.defaults_for(&self.class()) {
            if let Some(v) = defaults.get(name) {
                return convert::to_lua(lua, &shared.converters, v);
            }
        }

        // Children shadow signals/methods only after properties (Studio behaviour).
        if let Some(child) = shared.store.borrow().child(self.id, name) {
            return instance_value(lua, shared, child);
        }

        // Signals.
        if crate::runtime::is_signal(&self.class(), name) {
            let sig = shared.signal(self.id, name);
            return Ok(Value::UserData(lua.create_userdata(crate::signal::LuaSignal::new(shared.clone(), sig))?));
        }

        // Methods.
        if crate::runtime::is_method(&self.class(), name) {
            let shared = shared.clone();
            let id = self.id;
            let method = name.to_string();
            return Ok(Value::Function(lua.create_function(move |lua, args: MultiValue| {
                let inst = LuaInstance { shared: shared.clone(), id };
                inst.call_method(lua, &method, &args)
            })?));
        }

        // Luau-level helper methods (they need to yield, so they live in Lua).
        if let Some(helpers) = shared.helpers.borrow().as_ref() {
            if let Some(f) = helpers.get::<Option<mlua::Function>>(name).ok().flatten() {
                return Ok(Value::Function(f));
            }
        }

        Ok(Value::Nil)
    }

    fn compute_mass(&self) -> f64 {
        let store = self.shared.store.borrow();
        let size = match store.get_prop(self.id, "Size") {
            Some(PropValue::Vector3(x, y, z)) => (x as f64, y as f64, z as f64),
            _ => (4.0, 1.0, 2.0),
        };
        let material = store
            .get_prop(self.id, "Material")
            .and_then(|m| m.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        // Roblox material densities (roughly: plastic 0.7, metal 7.85, ...).
        let density = material_density(&material);
        (size.0 * size.1 * size.2) * density
    }

    pub fn set(&self, lua: &Lua, name: &str, value: &Value) -> LuaResult<()> {
        let shared = &self.shared;
        match name {
            "ClassName" => return Err(mlua::Error::RuntimeError("ClassName is read-only".into())),
            "Parent" => {
                let target = as_instance_id(value);
                if let Some(t) = target {
                    let store = shared.store.borrow();
                    if t == self.id || store.is_ancestor(self.id, t) {
                        drop(store);
                        return Err(mlua::Error::RuntimeError(
                            "Attempt to set parent of an instance to itself or a descendant".into(),
                        ));
                    }
                }
                let old_parent = shared.store.borrow().node(self.id).parent;
                shared.store.borrow_mut().set_parent(self.id, target);
                if let Some(old) = old_parent {
                    shared.fire_node_event(lua, old, "ChildRemoved", MultiValue::from_vec(vec![
                        instance_value(lua, shared, self.id)?,
                    ]));
                }
                if let Some(new) = target {
                    shared.fire_node_event(lua, new, "ChildAdded", MultiValue::from_vec(vec![
                        instance_value(lua, shared, self.id)?,
                    ]));
                }
                return Ok(());
            }
            "Position" => {
                let PropValue::Vector3(x, y, z) = convert::from_lua(value) else {
                    return Err(mlua::Error::RuntimeError("Position expects a Vector3".into()));
                };
                let mut store = shared.store.borrow_mut();
                let mut cf = match store.get_prop(self.id, "CFrame") {
                    Some(PropValue::CFrame { position, rotation }) => (position, rotation),
                    _ => ([0.0; 3], [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
                };
                cf.0 = [x, y, z];
                store.set_prop(self.id, "CFrame", PropValue::CFrame { position: cf.0, rotation: cf.1 });
                drop(store);
                shared.fire_node_event(lua, self.id, "Changed", MultiValue::from_vec(vec![
                    Value::String(lua.create_string("Position")?),
                ]));
                return Ok(());
            }
            "Orientation" => {
                let PropValue::Vector3(x, y, z) = convert::from_lua(value) else {
                    return Err(mlua::Error::RuntimeError("Orientation expects a Vector3".into()));
                };
                let rotation = rotation_from_euler(x.to_radians() as f64, y.to_radians() as f64, z.to_radians() as f64);
                let mut store = shared.store.borrow_mut();
                let position = match store.get_prop(self.id, "CFrame") {
                    Some(PropValue::CFrame { position, .. }) => position,
                    _ => [0.0; 3],
                };
                store.set_prop(self.id, "CFrame", PropValue::CFrame { position, rotation });
                return Ok(());
            }
            _ => {}
        }

        let converted = convert::from_lua(value);
        // Keep CFrame and Position consistent, like BasePart does.
        if name == "CFrame" {
            if let PropValue::CFrame { position, .. } = converted {
                shared.store.borrow_mut().set_prop(self.id, "Position", PropValue::Vector3(position[0], position[1], position[2]));
            }
        }
        shared.store.borrow_mut().set_prop(self.id, name, converted);

        let changed_value = value.clone();
        shared.fire_node_event(lua, self.id, "Changed", MultiValue::from_vec(vec![
            Value::String(lua.create_string(name)?),
        ]));
        shared.fire_node_event(lua, self.id, &format!("Changed:{name}"), MultiValue::from_vec(vec![changed_value]));
        Ok(())
    }

    pub fn call_method(&self, lua: &Lua, name: &str, args: &MultiValue) -> LuaResult<Value> {
        let shared = self.shared.clone();
        let id = self.id;
        let rest = rest_args(args, id);
        let class = self.class();

        // Service implementations can add/override methods per class.
        if let Some(v) = crate::runtime::service_method(lua, &shared, id, &class, name, &rest)? {
            return Ok(v);
        }

        match name {
            "FindFirstChild" => {
                let Some(child_name) = rest.first().and_then(arg_string) else { return Ok(Value::Nil) };
                let recursive = rest.get(1).and_then(arg_bool).unwrap_or(false);
                let found = if recursive {
                    find_recursive(&shared, id, &|n| n.name == child_name)
                } else {
                    shared.store.borrow().child(id, &child_name)
                };
                match found {
                    Some(c) => instance_value(lua, &shared, c),
                    None => Ok(Value::Nil),
                }
            }
            "FindFirstChildOfClass" => {
                let Some(class_name) = rest.first().and_then(arg_string) else { return Ok(Value::Nil) };
                match shared.store.borrow().find_first_child_with_class(id, &class_name) {
                    Some(c) => instance_value(lua, &shared, c),
                    None => Ok(Value::Nil),
                }
            }
            "FindFirstChildWhichIsA" => {
                let Some(class_name) = rest.first().and_then(arg_string) else { return Ok(Value::Nil) };
                let recursive = rest.get(1).and_then(arg_bool).unwrap_or(false);
                let found = find_where(&shared, id, recursive, &|shared, n| shared.is_a(n, &class_name));
                match found {
                    Some(c) => instance_value(lua, &shared, c),
                    None => Ok(Value::Nil),
                }
            }
            "FindFirstAncestor" => {
                let Some(target) = rest.first().and_then(arg_string) else { return Ok(Value::Nil) };
                let mut cur = shared.store.borrow().node(id).parent;
                while let Some(c) = cur {
                    if shared.store.borrow().name(c) == target {
                        return instance_value(lua, &shared, c);
                    }
                    cur = shared.store.borrow().node(c).parent;
                }
                Ok(Value::Nil)
            }
            "FindFirstAncestorOfClass" => {
                let Some(target) = rest.first().and_then(arg_string) else { return Ok(Value::Nil) };
                let mut cur = shared.store.borrow().node(id).parent;
                while let Some(c) = cur {
                    if shared.store.borrow().class_name(c) == target {
                        return instance_value(lua, &shared, c);
                    }
                    cur = shared.store.borrow().node(c).parent;
                }
                Ok(Value::Nil)
            }
            "FindFirstAncestorWhichIsA" => {
                let Some(target) = rest.first().and_then(arg_string) else { return Ok(Value::Nil) };
                let mut cur = shared.store.borrow().node(id).parent;
                while let Some(c) = cur {
                    if shared.is_a(c, &target) {
                        return instance_value(lua, &shared, c);
                    }
                    cur = shared.store.borrow().node(c).parent;
                }
                Ok(Value::Nil)
            }
            "GetChildren" => {
                let children = shared.store.borrow().children(id);
                let table = lua.create_table_with_capacity(children.len(), 0)?;
                for (i, child) in children.into_iter().enumerate() {
                    table.set(i + 1, instance_value(lua, &shared, child)?)?;
                }
                Ok(Value::Table(table))
            }
            "GetDescendants" => {
                let mut all = Vec::new();
                collect_descendants(&shared, id, &mut all);
                let table = lua.create_table_with_capacity(all.len(), 0)?;
                for (i, child) in all.into_iter().enumerate() {
                    table.set(i + 1, instance_value(lua, &shared, child)?)?;
                }
                Ok(Value::Table(table))
            }
            "IsA" => {
                let Some(target) = rest.first().and_then(arg_string) else { return Ok(Value::Boolean(false)) };
                Ok(Value::Boolean(shared.is_a(id, &target)))
            }
            "IsDescendantOf" => {
                let Some(target) = rest.first().and_then(as_instance_id) else { return Ok(Value::Boolean(false)) };
                Ok(Value::Boolean(shared.store.borrow().is_ancestor(target, id)))
            }
            "Destroy" => {
                destroy_with_children(lua, &shared, id)?;
                Ok(Value::Nil)
            }
            "ClearAllChildren" => {
                let children = shared.store.borrow().children(id);
                for child in children {
                    destroy_with_children(lua, &shared, child)?;
                }
                Ok(Value::Nil)
            }
            "Clone" => {
                let clone = clone_subtree(&shared, id);
                instance_value(lua, &shared, clone)
            }
            "GetFullName" => {
                let full = shared.store.borrow().full_name(id);
                Ok(Value::String(lua.create_string(full)?))
            }
            "GetAttribute" => {
                let Some(attr) = rest.first().and_then(arg_string) else { return Ok(Value::Nil) };
                let value = shared.store.borrow().node(id).attributes.get(&attr).cloned();
                match value {
                    Some(v) => convert::to_lua(lua, &shared.converters, &v),
                    None => Ok(Value::Nil),
                }
            }
            "SetAttribute" => {
                let attr = rest.first().and_then(arg_string).unwrap_or_default();
                let value = rest.get(1).map(convert::from_lua).unwrap_or(PropValue::Nil);
                shared.store.borrow_mut().node_mut(id).attributes.insert(attr.clone(), value);
                shared.fire_node_event(lua, id, "AttributeChanged", MultiValue::from_vec(vec![
                    Value::String(lua.create_string(attr)?),
                ]));
                Ok(Value::Nil)
            }
            "GetAttributes" => {
                let attrs = shared.store.borrow().node(id).attributes.clone();
                let table = lua.create_table()?;
                for (k, v) in attrs {
                    table.set(k, convert::to_lua(lua, &shared.converters, &v)?)?;
                }
                Ok(Value::Table(table))
            }
            "AddTag" => {
                let tag = rest.first().and_then(arg_string).unwrap_or_default();
                let mut store = shared.store.borrow_mut();
                if !store.node(id).tags.contains(&tag) {
                    store.node_mut(id).tags.push(tag);
                }
                Ok(Value::Nil)
            }
            "RemoveTag" => {
                let tag = rest.first().and_then(arg_string).unwrap_or_default();
                shared.store.borrow_mut().node_mut(id).tags.retain(|t| *t != tag);
                Ok(Value::Nil)
            }
            "HasTag" => {
                let tag = rest.first().and_then(arg_string).unwrap_or_default();
                Ok(Value::Boolean(shared.store.borrow().node(id).tags.contains(&tag)))
            }
            "GetTags" => {
                let tags = shared.store.borrow().node(id).tags.clone();
                let table = lua.create_table_with_capacity(tags.len(), 0)?;
                for (i, t) in tags.into_iter().enumerate() {
                    table.set(i + 1, t)?;
                }
                Ok(Value::Table(table))
            }
            "GetPropertyChangedSignal" => {
                let prop = rest.first().and_then(arg_string).unwrap_or_default();
                let sig = shared.signal(id, &format!("Changed:{prop}"));
                Ok(Value::UserData(lua.create_userdata(crate::signal::LuaSignal::new(shared.clone(), sig))?))
            }
            "GetPivot" => {
                let pos = shared.store.borrow().get_prop(id, "CFrame");
                match pos {
                    Some(v) => convert::to_lua(lua, &shared.converters, &v),
                    None => convert::to_lua(lua, &shared.converters, &PropValue::CFrame {
                        position: [0.0; 3],
                        rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                    }),
                }
            }
            "SetPivot" => {
                if let Some(v) = rest.first() {
                    let value = convert::from_lua(v);
                    shared.store.borrow_mut().set_prop(id, "CFrame", value);
                }
                Ok(Value::Nil)
            }
            "PivotTo" | "MoveTo" => {
                let Some(target) = rest.first() else { return Ok(Value::Nil) };
                let PropValue::CFrame { position, rotation } = convert::from_lua(target) else {
                    return Ok(Value::Nil)
                };
                let offset = {
                    let store = shared.store.borrow();
                    match store.get_prop(id, "CFrame") {
                        Some(PropValue::CFrame { position: cur, .. }) => [
                            position[0] - cur[0],
                            position[1] - cur[1],
                            position[2] - cur[2],
                        ],
                        _ => position,
                    }
                };
                let mut nodes = vec![id];
                collect_descendants(&shared, id, &mut nodes);
                let mut store = shared.store.borrow_mut();
                for node in nodes {
                    if let Some(PropValue::CFrame { position: p, rotation: r }) = store.get_prop(node, "CFrame") {
                        store.set_prop(node, "CFrame", PropValue::CFrame {
                            position: [p[0] + offset[0], p[1] + offset[1], p[2] + offset[2]],
                            rotation: if node == id { rotation } else { r },
                        });
                    }
                }
                drop(store);
                if matches!(class.as_str(), "Humanoid" | "Model") {
                    shared.fire_node_event(lua, id, "MoveToFinished", MultiValue::new());
                }
                Ok(Value::Nil)
            }
            "GetMass" => Ok(Value::Number(self.compute_mass())),
            "IsGrounded" => Ok(Value::Boolean(true)),
            "GetState" => {
                let state = shared
                    .store
                    .borrow()
                    .get_prop(id, "HumanoidState")
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| "HumanoidStateType.Running".to_string());
                convert::to_lua(lua, &shared.converters, &PropValue::Enum(state))
            }
            "ChangeState" => {
                if let Some(v) = rest.first() {
                    let value = convert::from_lua(v);
                    shared.store.borrow_mut().set_prop(id, "HumanoidState", value);
                }
                Ok(Value::Nil)
            }
            "Move" => {
                if let Some(v) = rest.first() {
                    let value = convert::from_lua(v);
                    shared.store.borrow_mut().set_prop(id, "MoveDirection", value);
                }
                Ok(Value::Nil)
            }
            "TakeDamage" => {
                let amount = rest.first().and_then(arg_number).unwrap_or(0.0);
                let mut store = shared.store.borrow_mut();
                let health = store.get_prop(id, "Health").and_then(|v| v.as_number()).unwrap_or(100.0);
                let new_health = (health - amount).max(0.0);
                store.set_prop(id, "Health", PropValue::Number(new_health));
                drop(store);
                shared.fire_node_event(lua, id, "HealthChanged", MultiValue::from_vec(vec![
                    Value::Number(new_health),
                ]));
                if new_health <= 0.0 {
                    shared.fire_node_event(lua, id, "Died", MultiValue::new());
                }
                Ok(Value::Nil)
            }
            "Kick" => {
                let msg = rest.first().and_then(arg_string).unwrap_or_default();
                shared.print(crate::log::LogLevel::Warn, format!("Player kicked: {msg}"));
                Ok(Value::Nil)
            }
            "GetService" => {
                let Some(service) = rest.first().and_then(arg_string) else { return Ok(Value::Nil) };
                match shared.get_service(&service) {
                    Some(s) => instance_value(lua, &shared, s),
                    None => Ok(Value::Nil),
                }
            }
            "FindService" => {
                let Some(service) = rest.first().and_then(arg_string) else { return Ok(Value::Nil) };
                match shared.find_service(&service) {
                    Some(s) => instance_value(lua, &shared, s),
                    None => Ok(Value::Nil),
                }
            }
            "IsLoaded" => Ok(Value::Boolean(true)),
            "Raycast" => {
                shared.warn_once("raycast", "Workspace:Raycast is not implemented yet (native physics/raycasts pending)");
                Ok(Value::Nil)
            }
            _ => Ok(Value::Nil),
        }
    }
}

impl UserData for LuaInstance {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Index, |lua, this, key: String| {
            this.get(lua, &key)
        });
        methods.add_meta_method(MetaMethod::NewIndex, |lua, this, (key, value): (String, Value)| {
            this.set(lua, &key, &value)
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("{}", this.shared.store.borrow().full_name(this.id)))
        });
        methods.add_meta_function(
            MetaMethod::Eq,
            |_, (a, b): (mlua::UserDataRef<LuaInstance>, mlua::UserDataRef<LuaInstance>)| {
                Ok(a.id == b.id && Rc::ptr_eq(&a.shared, &b.shared))
            },
        );
    }
}

fn collect_descendants(shared: &Shared, id: NodeId, out: &mut Vec<NodeId>) {
    let store = shared.store.borrow();
    for child in store.children(id) {
        out.push(child);
        collect_descendants(shared, child, out);
    }
}

fn find_recursive(shared: &Shared, id: NodeId, pred: &dyn Fn(&crate::datamodel::Node) -> bool) -> Option<NodeId> {
    let mut stack = shared.store.borrow().children(id);
    stack.reverse();
    while let Some(node) = stack.pop() {
        if pred(shared.store.borrow().node(node)) {
            return Some(node);
        }
        let mut children = shared.store.borrow().children(node);
        children.reverse();
        stack.extend(children);
    }
    None
}

fn find_where(shared: &Shared, id: NodeId, recursive: bool, pred: &dyn Fn(&Shared, NodeId) -> bool) -> Option<NodeId> {
    let children = shared.store.borrow().children(id);
    for child in children {
        if pred(shared, child) {
            return Some(child);
        }
    }
    if recursive {
        let children = shared.store.borrow().children(id);
        for child in children {
            if let Some(found) = find_where(shared, child, true, pred) {
                return Some(found);
            }
        }
    }
    None
}

fn destroy_with_children(lua: &Lua, shared: &Rc<Shared>, id: NodeId) -> LuaResult<()> {
    shared.fire_node_event(lua, id, "Destroying", MultiValue::new());
    let children = shared.store.borrow().children(id);
    for child in children {
        destroy_with_children(lua, shared, child)?;
    }
    let parent = shared.store.borrow().node(id).parent;
    shared.store.borrow_mut().destroy(id);
    if let Some(p) = parent {
        if let Ok(v) = instance_value(lua, shared, id) {
            shared.fire_node_event(lua, p, "ChildRemoved", MultiValue::from_vec(vec![v]));
        }
    }
    Ok(())
}

fn clone_subtree(shared: &Rc<Shared>, id: NodeId) -> NodeId {
    let mut store = shared.store.borrow_mut();
    let (class, name, props, source) = {
        let node = store.node(id);
        (node.class.clone(), node.name.clone(), node.props.clone(), node.source.clone())
    };
    let new_id = store.create(&class, &name, None);
    for (k, v) in props {
        store.set_prop(new_id, &k, v);
    }
    if let Some(src) = source {
        store.set_prop(new_id, "Source", PropValue::String(src));
    }
    let children = store.children(id);
    drop(store);
    for child in children {
        let child_clone = clone_subtree(shared, child);
        shared.store.borrow_mut().set_parent(child_clone, Some(new_id));
    }
    new_id
}

fn euler_from_rotation(m: &[[f32; 3]; 3]) -> (f32, f32, f32) {
    let sy = (-m[2][0]).clamp(-1.0, 1.0);
    let ry = sy.asin();
    let (rx, rz) = if sy.abs() > 0.9999 {
        (m[0][1].atan2(m[1][1]), 0.0)
    } else {
        (m[2][1].atan2(m[2][2]), m[1][0].atan2(m[0][0]))
    };
    (rx, ry, rz)
}

fn rotation_from_euler(rx: f64, ry: f64, rz: f64) -> [[f32; 3]; 3] {
    let (sx, cx) = (rx.sin(), rx.cos());
    let (sy, cy) = (ry.sin(), ry.cos());
    let (sz, cz) = (rz.sin(), rz.cos());
    // R = Rx * Ry * Rz (Roblox CFrame.fromEulerAnglesXYZ)
    [
        [
            (cy * cz) as f32,
            (-cy * sz) as f32,
            sy as f32,
        ],
        [
            (cx * sz + sx * sy * cz) as f32,
            (cx * cz - sx * sy * sz) as f32,
            (-sx * cy) as f32,
        ],
        [
            (sx * sz - cx * sy * cz) as f32,
            (sx * cz + cx * sy * sz) as f32,
            (cx * cy) as f32,
        ],
    ]
}

fn material_density(material: &str) -> f64 {
    let name = material.rsplit('.').next().unwrap_or(material);
    match name {
        "Metal" | "CorrodedMetal" | "DiamondPlate" | "Foil" => 7.85,
        "Concrete" | "Granite" | "Marble" | "Slate" | "Basalt" | "Limestone" => 2.4,
        "Brick" | "Cobblestone" | "Rock" | "Pavement" | "Sandstone" => 2.0,
        "Wood" | "WoodPlanks" => 0.6,
        "Ice" | "Glacier" => 0.92,
        "Water" => 1.0,
        "Sand" => 1.6,
        "Glass" | "Neon" => 2.5,
        _ => 0.7,
    }
}
