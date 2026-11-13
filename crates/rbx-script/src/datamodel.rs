//! Instance tree backing the Luau data model.
//!
//! Nodes are index-based (`NodeId = usize`) so the Lua side can hold stable
//! handles while the tree mutates. `InstanceStore` owns the nodes; the Lua
//! userdata (`instance::LuaInstance`) is a thin `(store, id)` handle.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type NodeId = usize;

/// Script-visible property values.
///
/// This mirrors the API dump value types the runtime converts to/from Luau tables
/// (see `convert.rs`). Unsupported types degrade to `Opaque` with a display string
/// rather than panicking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PropValue {
    Nil,
    Bool(bool),
    Number(f64),
    Int(i64),
    String(String),
    Vector2(f32, f32),
    Vector3(f32, f32, f32),
    Color3(f32, f32, f32),
    CFrame {
        position: [f32; 3],
        rotation: [[f32; 3]; 3],
    },
    UDim(f32, i32),
    UDim2 {
        xs: f32,
        xo: i32,
        ys: f32,
        yo: i32,
    },
    NumberRange(f32, f32),
    Rect {
        min: (f32, f32),
        max: (f32, f32),
    },
    /// Enum item, stored as `"EnumType.Item"`.
    Enum(String),
    /// Reference to another instance.
    Instance(NodeId),
    /// `NumberSequence` / `ColorSequence` / `Content` / anything unmodelled.
    Opaque(String),
}

impl PropValue {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Nil => "nil",
            Self::Bool(_) => "boolean",
            Self::Number(_) | Self::Int(_) => "number",
            Self::String(_) => "string",
            Self::Vector2(..) => "Vector2",
            Self::Vector3(..) => "Vector3",
            Self::Color3(..) => "Color3",
            Self::CFrame { .. } => "CFrame",
            Self::UDim(..) => "UDim",
            Self::UDim2 { .. } => "UDim2",
            Self::NumberRange(..) => "NumberRange",
            Self::Rect { .. } => "Rect",
            Self::Enum(_) => "EnumItem",
            Self::Instance(_) => "Instance",
            Self::Opaque(_) => "opaque",
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            Self::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) | Self::Enum(s) | Self::Opaque(s) => Some(s),
            _ => None,
        }
    }

    /// Human-readable form used by `print()`/the console.
    pub fn display(&self) -> String {
        match self {
            Self::Nil => "nil".to_string(),
            Self::Bool(b) => b.to_string(),
            Self::Number(n) => format!("{n}"),
            Self::Int(i) => format!("{i}"),
            Self::String(s) => s.clone(),
            Self::Vector2(x, y) => format!("{x}, {y}"),
            Self::Vector3(x, y, z) => format!("{x}, {y}, {z}"),
            Self::Color3(r, g, b) => format!("{r}, {g}, {b}"),
            Self::CFrame { position, .. } => {
                format!("{}, {}, {}", position[0], position[1], position[2])
            }
            Self::UDim(s, o) => format!("{{{s}, {o}}}"),
            Self::UDim2 { xs, xo, ys, yo } => format!("{{{xs}, {xo}}}, {{{ys}, {yo}}}"),
            Self::NumberRange(a, b) => format!("{a} {b}"),
            Self::Rect { min, max } => format!("{min:?} {max:?}"),
            Self::Enum(e) => e.clone(),
            Self::Instance(id) => format!("Instance({id})"),
            Self::Opaque(s) => s.clone(),
        }
    }
}

/// One data model instance.
#[derive(Debug, Clone)]
pub struct Node {
    pub class: String,
    pub name: String,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub props: BTreeMap<String, PropValue>,
    pub attributes: BTreeMap<String, PropValue>,
    pub tags: Vec<String>,
    /// Luau source for script classes.
    pub source: Option<String>,
    pub destroyed: bool,
}

impl Node {
    pub fn is_script(&self) -> bool {
        matches!(self.class.as_str(), "Script" | "LocalScript" | "ModuleScript")
    }
}

/// The data model (`game`).
pub struct InstanceStore {
    nodes: Vec<Node>,
    pub root: NodeId,
    /// Bumped on every structural or property change; lets the runtime detect
    /// whether waiting coroutines should re-check their predicate.
    pub revision: u64,
    next_name: BTreeMap<String, u64>,
}

impl Default for InstanceStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InstanceStore {
    pub fn new() -> Self {
        let root = Node {
            class: "DataModel".to_string(),
            name: "Game".to_string(),
            parent: None,
            children: Vec::new(),
            props: BTreeMap::new(),
            attributes: BTreeMap::new(),
            tags: Vec::new(),
            source: None,
            destroyed: false,
        };
        let mut store = Self {
            nodes: vec![root],
            root: 0,
            revision: 1,
            next_name: BTreeMap::new(),
        };
        store.set_prop(0, "PlaceId", PropValue::Int(0));
        store.set_prop(0, "JobId", PropValue::String(String::new()));
        store.set_prop(0, "GameId", PropValue::Int(0));
        store.set_prop(0, "PlaceVersion", PropValue::Int(1));
        store
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn try_node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.revision += 1;
        &mut self.nodes[id]
    }

    pub fn class_name(&self, id: NodeId) -> &str {
        &self.nodes[id].class
    }

    pub fn name(&self, id: NodeId) -> &str {
        &self.nodes[id].name
    }

    /// Creates a node and (optionally) parents it.
    pub fn create(&mut self, class: &str, name: &str, parent: Option<NodeId>) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node {
            class: class.to_string(),
            name: name.to_string(),
            parent: None,
            children: Vec::new(),
            props: BTreeMap::new(),
            attributes: BTreeMap::new(),
            tags: Vec::new(),
            source: if matches!(class, "Script" | "LocalScript" | "ModuleScript") {
                Some(String::new())
            } else {
                None
            },
            destroyed: false,
        });
        if let Some(parent) = parent {
            self.set_parent(id, Some(parent));
        }
        self.revision += 1;
        id
    }

    /// Creates a node with a name that is unique among its siblings
    /// (`Part`, `Part`, `Part 2`, ...), matching Studio behaviour.
    pub fn create_unique(&mut self, class: &str, parent: Option<NodeId>) -> NodeId {
        let base = class.to_string();
        let mut candidate = base.clone();
        let mut n = 0u64;
        if let Some(parent) = parent {
            while self.child(parent, &candidate).is_some() {
                n += 1;
                candidate = format!("{base}{n}");
            }
        }
        self.create(class, &candidate, parent)
    }

    pub fn child(&self, parent: NodeId, name: &str) -> Option<NodeId> {
        self.nodes[parent]
            .children
            .iter()
            .copied()
            .find(|id| !self.nodes[*id].destroyed && self.nodes[*id].name == name)
    }

    pub fn children(&self, parent: NodeId) -> Vec<NodeId> {
        self.nodes[parent]
            .children
            .iter()
            .copied()
            .filter(|id| !self.nodes[*id].destroyed)
            .collect()
    }

    pub fn is_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        let mut cur = self.nodes[node].parent;
        while let Some(p) = cur {
            if p == ancestor {
                return true;
            }
            cur = self.nodes[p].parent;
        }
        false
    }

    pub fn set_parent(&mut self, id: NodeId, parent: Option<NodeId>) {
        if self.nodes[id].parent == parent {
            return;
        }
        if let Some(old) = self.nodes[id].parent {
            self.nodes[old].children.retain(|c| *c != id);
        }
        self.nodes[id].parent = parent;
        if let Some(new) = parent {
            if !self.nodes[new].children.contains(&id) {
                self.nodes[new].children.push(id);
            }
        }
        self.revision += 1;
    }

    pub fn destroy(&mut self, id: NodeId) {
        if id == self.root {
            return;
        }
        let children = self.children(id);
        for child in children {
            self.destroy(child);
        }
        if let Some(parent) = self.nodes[id].parent {
            self.nodes[parent].children.retain(|c| *c != id);
        }
        self.nodes[id].destroyed = true;
        self.nodes[id].parent = None;
        self.revision += 1;
    }

    pub fn find_first_child_with_class(&self, parent: NodeId, class: &str) -> Option<NodeId> {
        self.children(parent)
            .into_iter()
            .find(|id| self.nodes[*id].class == class)
    }

    /// Full path below the root, e.g. `["Workspace", "Part"]`.
    pub fn path(&self, id: NodeId) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            if c == self.root {
                break;
            }
            out.push(self.nodes[c].name.clone());
            cur = self.nodes[c].parent;
        }
        out.reverse();
        out
    }

    pub fn full_name(&self, id: NodeId) -> String {
        let parts = self.path(id);
        if parts.is_empty() {
            "game".to_string()
        } else {
            parts.join(".")
        }
    }

    /// Resolves `.`-separated names from `start` (`nil` = root).
    pub fn find_by_path(&self, start: Option<NodeId>, path: &str) -> Option<NodeId> {
        let mut cur = start.unwrap_or(self.root);
        for segment in path.split('.') {
            cur = self.child(cur, segment)?;
        }
        Some(cur)
    }

    pub fn get_prop(&self, id: NodeId, name: &str) -> Option<PropValue> {
        if name == "Name" {
            return Some(PropValue::String(self.nodes[id].name.clone()));
        }
        if name == "ClassName" {
            return Some(PropValue::String(self.nodes[id].class.clone()));
        }
        if name == "Parent" {
            return match self.nodes[id].parent {
                Some(p) => Some(PropValue::Instance(p)),
                None => Some(PropValue::Nil),
            };
        }
        self.nodes[id].props.get(name).cloned()
    }

    pub fn set_prop(&mut self, id: NodeId, name: &str, value: PropValue) {
        match name {
            "Name" => {
                if let PropValue::String(s) = &value {
                    self.nodes[id].name = s.clone();
                }
            }
            "Parent" => {
                if let PropValue::Instance(p) = value {
                    self.set_parent(id, Some(p));
                } else {
                    self.set_parent(id, None);
                }
            }
            "Source" => {
                if let PropValue::String(s) = &value {
                    self.nodes[id].source = Some(s.clone());
                }
                self.nodes[id].props.insert(name.to_string(), value);
            }
            _ => {
                self.nodes[id].props.insert(name.to_string(), value);
            }
        }
        self.revision += 1;
    }

    pub fn source(&self, id: NodeId) -> Option<&str> {
        self.nodes[id].source.as_deref()
    }

    pub fn unique_name(&mut self, base: &str) -> String {
        let counter = self.next_name.entry(base.to_string()).or_insert(0);
        *counter += 1;
        if *counter == 1 {
            base.to_string()
        } else {
            format!("{base}{}", *counter - 1)
        }
    }

    /// Serializable snapshot for the asset/runtime format.
    pub fn to_snapshot(&self) -> Vec<SnapshotNode> {
        (0..self.nodes.len())
            .map(|id| {
                let n = &self.nodes[id];
                SnapshotNode {
                    class: n.class.clone(),
                    name: n.name.clone(),
                    parent: n.parent,
                    props: n.props.clone(),
                    source: n.source.clone(),
                    destroyed: n.destroyed,
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotNode {
    pub class: String,
    pub name: String,
    pub parent: Option<NodeId>,
    pub props: BTreeMap<String, PropValue>,
    pub source: Option<String>,
    pub destroyed: bool,
}
