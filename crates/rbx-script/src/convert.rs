//! Luau <-> `PropValue` conversion, plus API-dump-derived property defaults.

use crate::datamodel::PropValue;
use mlua::{Function, Lua, Result as LuaResult, Table, Value};
use rbx_studio::api_dump::{ReflectionDatabase, ValueType};

/// Constructors extracted from the Luau prelude so Rust can build real datatype
/// values (`Vector3.new(...)`, `CFrame.new(...)`, `Enum.Material.Plastic`, ...).
pub struct Converters {
    pub vector2: Function,
    pub vector3: Function,
    pub color3: Function,
    pub cframe: Function,
    pub udim: Function,
    pub udim2: Function,
    pub number_range: Function,
    pub rect: Function,
    pub enum_root: Table,
}

impl Converters {
    pub fn new(lua: &Lua) -> LuaResult<Self> {
        let globals = lua.globals();
        let ctor = |name: &str, field: &str| -> LuaResult<Function> {
            let t: Table = globals.get(name)?;
            t.get(field)
        };
        Ok(Self {
            vector2: ctor("Vector2", "new")?,
            vector3: ctor("Vector3", "new")?,
            color3: ctor("Color3", "fromRGB")?,
            cframe: ctor("CFrame", "new")?,
            udim: ctor("UDim", "new")?,
            udim2: ctor("UDim2", "new")?,
            number_range: ctor("NumberRange", "new")?,
            rect: ctor("Rect", "new")?,
            enum_root: globals.get("Enum")?,
        })
    }
}

/// Converts a stored property into the Luau value scripts expect.
pub fn to_lua(lua: &Lua, cv: &Converters, value: &PropValue) -> LuaResult<Value> {
    Ok(match value {
        PropValue::Nil => Value::Nil,
        PropValue::Bool(b) => Value::Boolean(*b),
        PropValue::Number(n) => Value::Number(*n),
        PropValue::Int(i) => Value::Number(*i as f64),
        PropValue::String(s) => Value::String(lua.create_string(s)?),
        PropValue::Vector2(x, y) => cv.vector2.call((*x, *y))?,
        PropValue::Vector3(x, y, z) => cv.vector3.call((*x, *y, *z))?,
        PropValue::Color3(r, g, b) => cv
            .color3
            .call(((r * 255.0).round(), (g * 255.0).round(), (b * 255.0).round()))?,
        PropValue::CFrame { position, rotation } => cv.cframe.call((
            position[0],
            position[1],
            position[2],
            rotation[0][0],
            rotation[0][1],
            rotation[0][2],
            rotation[1][0],
            rotation[1][1],
            rotation[1][2],
            rotation[2][0],
            rotation[2][1],
            rotation[2][2],
        ))?,
        PropValue::UDim(s, o) => cv.udim.call((*s, *o))?,
        PropValue::UDim2 { xs, xo, ys, yo } => cv.udim2.call((*xs, *xo, *ys, *yo))?,
        PropValue::NumberRange(a, b) => cv.number_range.call((*a, *b))?,
        PropValue::Rect { min, max } => cv
            .rect
            .call((min.0, min.1, max.0 - min.0, max.1 - min.1))?,
        PropValue::Enum(path) => {
            let (enum_name, item) = path.split_once('.').unwrap_or((path.as_str(), ""));
            let enum_tbl: Option<Table> = cv.enum_root.get(enum_name).ok();
            match enum_tbl.and_then(|t| t.get::<Option<Value>>(item).ok().flatten()) {
                Some(v) => v,
                None => Value::Nil,
            }
        }
        PropValue::Instance(_) => Value::Nil,
        PropValue::Opaque(s) => Value::String(lua.create_string(s)?),
    })
}

/// Converts a Luau value assigned by a script into a stored property value.
pub fn from_lua(value: &Value) -> PropValue {
    match value {
        Value::Nil => PropValue::Nil,
        Value::Boolean(b) => PropValue::Bool(*b),
        Value::Integer(i) => PropValue::Int(*i),
        Value::Number(n) => PropValue::Number(*n),
        Value::String(s) => PropValue::String(s.to_str().map(|s| s.to_string()).unwrap_or_default()),
        Value::UserData(ud) => match ud.borrow::<crate::instance::LuaInstance>() {
            Ok(inst) => PropValue::Instance(inst.id),
            Err(_) => PropValue::Opaque("<userdata>".to_string()),
        },
        Value::Table(t) => from_lua_table(t),
        Value::LightUserData(_) => PropValue::Opaque("<lightuserdata>".to_string()),
        _ => PropValue::Opaque("<value>".to_string()),
    }
}

fn from_lua_table(t: &Table) -> PropValue {
    let kind: Option<String> = t.get("__datatype").ok().flatten();
    let num = |k: &str| -> f32 { t.get::<Option<f64>>(k).ok().flatten().unwrap_or(0.0) as f32 };
    match kind.as_deref() {
        Some("Vector2") => PropValue::Vector2(num("_x"), num("_y")),
        Some("Vector3") => PropValue::Vector3(num("_x"), num("_y"), num("_z")),
        Some("Color3") => PropValue::Color3(num("_r"), num("_g"), num("_b")),
        Some("UDim") => PropValue::UDim(num("_s"), num("_o") as i32),
        Some("UDim2") => PropValue::UDim2 {
            xs: num("_xs"),
            xo: num("_xo") as i32,
            ys: num("_ys"),
            yo: num("_yo") as i32,
        },
        Some("NumberRange") => PropValue::NumberRange(num("_min"), num("_max")),
        Some("Rect") => PropValue::Rect {
            min: (num("_minx"), num("_miny")),
            max: (num("_maxx"), num("_maxy")),
        },
        Some("CFrame") => PropValue::CFrame {
            position: [num("_x"), num("_y"), num("_z")],
            rotation: [
                [num("_m11"), num("_m12"), num("_m13")],
                [num("_m21"), num("_m22"), num("_m23")],
                [num("_m31"), num("_m32"), num("_m33")],
            ],
        },
        Some("EnumItem") => {
            let enum_name: Option<String> = t
                .get::<Option<Table>>("EnumType")
                .ok()
                .flatten()
                .and_then(|e| e.get::<Option<String>>("Name").ok().flatten());
            let item: Option<String> = t.get("Name").ok().flatten();
            match (enum_name, item) {
                (Some(e), Some(i)) => PropValue::Enum(format!("{e}.{i}")),
                _ => PropValue::Opaque("EnumItem".to_string()),
            }
        }
        _ => PropValue::Opaque("<table>".to_string()),
    }
}

/// Defaults for properties whose real value matters when a script reads them
/// before ever writing (Studio-like: constructing `Instance.new("Part")` yields a
/// usable part, not a zero-sized one).
fn builtin_default(class: &str, prop: &str) -> Option<PropValue> {
    let v = match (class, prop) {
        ("Workspace", "Gravity") => PropValue::Number(196.2),
        ("Workspace", "FallenPartsDestroyHeight") => PropValue::Number(-500.0),
        ("Lighting", "Brightness") => PropValue::Number(2.0),
        ("Lighting", "ClockTime") => PropValue::Number(14.0),
        ("Lighting", "GeographicLatitude") => PropValue::Number(41.733),
        ("Lighting", "Ambient") => PropValue::Color3(0.0, 0.0, 0.0),
        ("Lighting", "OutdoorAmbient") => PropValue::Color3(0.5, 0.5, 0.5),
        ("Lighting", "GlobalShadows") => PropValue::Bool(true),
        ("Lighting", "Technology") => PropValue::Enum("Technology.Future".to_string()),
        ("BasePart", "Size") | ("Part", "Size") => PropValue::Vector3(4.0, 1.0, 2.0),
        ("BasePart", "Color") | ("Part", "Color") => PropValue::Color3(0.639, 0.635, 0.647),
        ("BasePart", "Material") | ("Part", "Material") => {
            PropValue::Enum("Material.Plastic".to_string())
        }
        ("BasePart", "Anchored") | ("Part", "Anchored") => PropValue::Bool(false),
        ("BasePart", "CanCollide") | ("Part", "CanCollide") => PropValue::Bool(true),
        ("BasePart", "CanQuery") | ("Part", "CanQuery") => PropValue::Bool(true),
        ("BasePart", "CanTouch") | ("Part", "CanTouch") => PropValue::Bool(true),
        ("BasePart", "CastShadow") | ("Part", "CastShadow") => PropValue::Bool(true),
        ("BasePart", "Massless") | ("Part", "Massless") => PropValue::Bool(false),
        ("BasePart", "Transparency") | ("Part", "Transparency") => PropValue::Number(0.0),
        ("BasePart", "Reflectance") | ("Part", "Reflectance") => PropValue::Number(0.0),
        ("Humanoid", "WalkSpeed") => PropValue::Number(16.0),
        ("Humanoid", "JumpPower") => PropValue::Number(50.0),
        ("Humanoid", "HipHeight") => PropValue::Number(2.0),
        ("Humanoid", "MaxHealth") => PropValue::Number(100.0),
        ("Humanoid", "Health") => PropValue::Number(100.0),
        ("Humanoid", "MoveDirection") => PropValue::Vector3(0.0, 0.0, 0.0),
        _ => return None,
    };
    Some(v)
}

/// Typed default for an API-dump property.
pub fn default_prop(
    db: &ReflectionDatabase,
    class: &str,
    prop_name: &str,
    vt: &ValueType,
) -> PropValue {
    if let Some(v) = builtin_default(class, prop_name) {
        return v;
    }
    match vt.category.as_str() {
        "Primitive" => match vt.name.as_str() {
            "bool" => PropValue::Bool(false),
            "string" => PropValue::String(if prop_name == "Name" {
                class.to_string()
            } else {
                String::new()
            }),
            "int" | "int64" => PropValue::Int(0),
            "float" | "double" => PropValue::Number(0.0),
            _ => PropValue::Opaque(format!("0 ({})", vt.name)),
        },
        "Enum" => {
            let item = db
                .enum_def(&vt.name)
                .and_then(|e| e.items.first().map(|i| i.name.clone()))
                .unwrap_or_else(|| "Unknown".to_string());
            PropValue::Enum(format!("{}.{}", vt.name, item))
        }
        "Class" => PropValue::Nil,
        "DataType" => match vt.name.as_str() {
            "Vector2" => PropValue::Vector2(0.0, 0.0),
            "Vector3" => PropValue::Vector3(0.0, 0.0, 0.0),
            "CFrame" | "CoordinateFrame" => PropValue::CFrame {
                position: [0.0; 3],
                rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            },
            "Color3" => PropValue::Color3(0.639216, 0.635294, 0.647059),
            "Color3uint8" => PropValue::Color3(0.639216, 0.635294, 0.647059),
            "BrickColor" => PropValue::Number(194.0),
            "UDim" => PropValue::UDim(0.0, 0),
            "UDim2" => PropValue::UDim2 {
                xs: 0.0,
                xo: 0,
                ys: 0.0,
                yo: 0,
            },
            "Rect" => PropValue::Rect {
                min: (0.0, 0.0),
                max: (0.0, 0.0),
            },
            "NumberRange" => PropValue::NumberRange(0.0, 0.0),
            "Content" | "ContentId" => PropValue::String(String::new()),
            other => PropValue::Opaque(format!("{other}.new()")),
        },
        _ => PropValue::Nil,
    }
}

pub fn log_level(ctx: crate::context::ExecContext) -> &'static str {
    ctx.name()
}
