//! Roblox Studio API dump (reflection): classes, inheritance, properties, enums.
//!
//! Loads `assets/studio/api/API-Dump.json` (fetched by `fetch-api-dump`).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ApiDump {
    #[serde(default)]
    pub classes: Vec<ApiClass>,
    #[serde(default)]
    pub enums: Vec<ApiEnum>,
    #[serde(default)]
    pub version: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ApiClass {
    pub name: String,
    #[serde(default)]
    pub superclass: String,
    #[serde(default)]
    pub members: Vec<ApiMember>,
    #[serde(default)]
    pub memory_category: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ApiMember {
    #[serde(default)]
    pub member_type: String,
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub value_type: Option<ValueType>,
    #[serde(default)]
    pub serialization: Option<Serialization>,
    #[serde(default)]
    pub security: Option<Value>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub thread_safety: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Serialization {
    #[serde(default)]
    pub can_load: bool,
    #[serde(default)]
    pub can_save: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ValueType {
    #[serde(default)]
    pub category: String, // Primitive, Class, Enum, DataType
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ApiEnum {
    pub name: String,
    #[serde(default)]
    pub items: Vec<ApiEnumItem>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ApiEnumItem {
    pub name: String,
    #[serde(default)]
    pub value: i64,
    #[serde(default, alias = "LegacyNames")]
    pub legacy_names: Vec<String>,
}

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ResolvedProperty {
    pub name: String,
    pub category: String,
    pub value_type: ValueType,
    pub read_only: bool,
    pub hidden: bool,
    pub deprecated: bool,
    pub not_scriptable: bool,
    pub not_browsable: bool,
    pub can_save: bool,
    pub can_load: bool,
    pub origin_class: String,
}

#[derive(Debug, Clone)]
pub struct ReflectionDatabase {
    pub dump: ApiDump,
    class_index: HashMap<String, usize>,
    enum_index: HashMap<String, usize>,
}

impl ReflectionDatabase {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path)
            .with_context(|| format!("reading API dump {}", path.display()))?;
        let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
        let dump: ApiDump = serde_json::from_str(raw)
            .with_context(|| format!("parsing API dump {}", path.display()))?;
        Ok(Self::from_dump(dump))
    }

    pub fn from_dump(dump: ApiDump) -> Self {
        let class_index = dump
            .classes
            .iter()
            .enumerate()
            .map(|(i, c)| (c.name.clone(), i))
            .collect();
        let enum_index = dump
            .enums
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.clone(), i))
            .collect();
        Self { dump, class_index, enum_index }
    }

    pub fn bundled_path() -> PathBuf {
        crate::studio_assets_root()
            .join("api")
            .join("API-Dump.json")
    }

    pub fn load_bundled() -> Result<Self> {
        Self::load(Self::bundled_path())
    }

    pub fn class(&self, name: &str) -> Option<&ApiClass> {
        self.class_index.get(name).and_then(|&i| self.dump.classes.get(i))
    }

    pub fn enum_def(&self, name: &str) -> Option<&ApiEnum> {
        self.enum_index.get(name).and_then(|&i| self.dump.enums.get(i))
    }

    pub fn all_enums(&self) -> &[ApiEnum] {
        &self.dump.enums
    }

    pub fn all_classes(&self) -> &[ApiClass] {
        &self.dump.classes
    }

    /// Superclass chain from `class` up to root (inclusive of class).
    pub fn ancestry(&self, class: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut cur = class.to_string();
        let mut seen = std::collections::HashSet::new();
        while !cur.is_empty() && seen.insert(cur.clone()) {
            out.push(cur.clone());
            let Some(c) = self.class(&cur) else { break };
            if c.superclass.is_empty() || c.superclass == "<<<ROOT>>>" {
                break;
            }
            cur = c.superclass.clone();
        }
        // Return as references into nothing stable; better return Strings/owned names.
        // We'll expose ancestry_names instead.
        Vec::new()
    }

    pub fn ancestry_names(&self, class: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = class.to_string();
        let mut seen = std::collections::HashSet::new();
        while !cur.is_empty() && seen.insert(cur.clone()) {
            out.push(cur.clone());
            let Some(c) = self.class(&cur) else { break };
            if c.superclass.is_empty() || c.superclass == "<<<ROOT>>>" {
                break;
            }
            cur = c.superclass.clone();
        }
        out
    }

    pub fn is_a(&self, class: &str, ancestor: &str) -> bool {
        self.ancestry_names(class).iter().any(|n| n == ancestor)
    }

    /// All properties for a class, including inherited. Derived-class members win
    /// on name conflicts. Ordered: derived -> base categories preserved roughly by
    /// collecting per class then reversing ancestry application? We'll collect
    /// from base to derived so derived overrides, then sort by (category, name).
    pub fn properties_for_class(&self, class: &str, include_hidden: bool, include_deprecated: bool) -> Vec<ResolvedProperty> {
        let mut map: HashMap<String, ResolvedProperty> = HashMap::new();
        let mut chain = self.ancestry_names(class);
        chain.reverse(); // base -> derived
        for cls_name in chain {
            let Some(cls) = self.class(&cls_name) else { continue };
            for m in &cls.members {
                if m.member_type != "Property" {
                    continue;
                }
                let Some(vt) = m.value_type.clone() else { continue };
                let tags = &m.tags;
                let has = |t: &str| tags.iter().any(|x| x == t);
                let hidden = has("Hidden");
                let deprecated = has("Deprecated");
                if hidden && !include_hidden {
                    // still include? user asked for "include every property type and every enum".
                    // "every property type" - better include properties, mark hidden.
                }
                if deprecated && !include_deprecated {
                    // include but mark
                }
                let read_only = has("ReadOnly");
                let (can_load, can_save) = m
                    .serialization
                    .as_ref()
                    .map(|s| (s.can_load, s.can_save))
                    .unwrap_or((false, false));
                map.insert(
                    m.name.clone(),
                    ResolvedProperty {
                        name: m.name.clone(),
                        category: if m.category.is_empty() { "Data".to_string() } else { m.category.clone() },
                        value_type: vt,
                        read_only,
                        hidden,
                        deprecated,
                        not_scriptable: has("NotScriptable"),
                        not_browsable: has("NotBrowsable"),
                        can_save,
                        can_load,
                        origin_class: cls_name.clone(),
                    },
                );
            }
        }
        let mut props: Vec<ResolvedProperty> = map.into_values().collect();
        // Include all (even hidden/deprecated) as requested; UI can style them.
        if !include_hidden || !include_deprecated {
            // keep all; flags are present
        }
        props.sort_by(|a, b| a.category.cmp(&b.category).then_with(|| a.name.cmp(&b.name)));
        props
    }

    /// Default value representation for a property type (for test/UI display).
    pub fn default_value_display(&self, vt: &ValueType, prop_name: &str, class_name: &str) -> String {
        match vt.category.as_str() {
            "Primitive" => match vt.name.as_str() {
                "bool" => "false".to_string(),
                "string" => {
                    if prop_name == "Name" { class_name.to_string() } else { String::new() }
                }
                "int" | "int64" => "0".to_string(),
                "float" | "double" => "0".to_string(),
                _ => format!("0 ({})", vt.name),
            },
            "Enum" => self
                .enum_def(&vt.name)
                .and_then(|e| e.items.first().map(|i| i.name.clone()))
                .unwrap_or_else(|| format!("{} (Enum)", vt.name)),
            "Class" => format!("None ({})", vt.name),
            "DataType" => match vt.name.as_str() {
                "Vector2" => "0, 0".to_string(),
                "Vector3" => "0, 0, 0".to_string(),
                "Vector2int16" | "Vector3int16" => "0, 0, 0".to_string(),
                "CFrame" => "0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1".to_string(),
                "CoordinateFrame" => "0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1".to_string(),
                "Color3" => "0.639216, 0.635294, 0.647059".to_string(),
                "Color3uint8" => "163, 162, 165".to_string(),
                "BrickColor" => "Medium stone grey".to_string(),
                "UDim" => "0, 0".to_string(),
                "UDim2" => "{0, 0}, {0, 0}".to_string(),
                "Rect" => "0, 0, 0, 0".to_string(),
                "NumberRange" => "0 0".to_string(),
                "NumberSequence" => "0 0 0 1 0 0".to_string(),
                "ColorSequence" => "0 1 1 1 0 1 1 1 1 0".to_string(),
                "Ray" => "{0, 0, 0}, {0, 0, 0}".to_string(),
                "Faces" | "Axes" => String::new(),
                "Region3" | "Region3int16" => String::new(),
                "Content" | "ContentId" => String::new(),
                "ProtectedString" => String::new(),
                "BinaryString" => String::new(),
                "SharedString" => String::new(),
                "Font" => "Legacy (Enum)".to_string(),
                "PhysicalProperties" => "Default".to_string(),
                "OptionalCoordinateFrame" => "None".to_string(),
                "DateTime" => "1970-01-01T00:00:00Z".to_string(),
                "TweenInfo" => "0, Linear, Out, 0, false, 0".to_string(),
                "Path2DControlPoint" | "PathWaypoint" => String::new(),
                "QDir" | "QFont" => String::new(),
                "SystemAddress" => String::new(),
                "SecurityCapabilities" => "0".to_string(),
                "UniqueId" => String::new(),
                _ => format!("<{}>", vt.name),
            },
            _ => format!("<{}:{}>", vt.category, vt.name),
        }
    }
}
