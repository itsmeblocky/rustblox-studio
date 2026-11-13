pub mod mesh;
pub mod scripts;

pub use mesh::{parse_mesh, MeshGeometry};
pub use scripts::{collect_scripts, ScriptInstance, ScriptKind, ScriptOrigin};

use anyhow::{anyhow, Result};
use glam::{Mat3, Quat, Vec3 as GVec3};
use rbx_binary::from_reader;
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{ustr, Instance, WeakDom};
use serde::{Deserialize, Serialize};
use std::io::Cursor;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct Vector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vector3 {
    pub fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
    pub fn to_glam(self) -> GVec3 {
        GVec3::new(self.x, self.y, self.z)
    }
    pub fn from_glam(v: GVec3) -> Self {
        Self { x: v.x, y: v.y, z: v.z }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CFrameValue {
    pub position: Vector3,
    /// Row-major 3x3 rotation matrix (Roblox CFrame stores rotation matrix + position)
    pub rotation: [[f32; 3]; 3],
}

impl Default for CFrameValue {
    fn default() -> Self {
        Self {
            position: Vector3::default(),
            rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }
}

impl CFrameValue {
    pub fn from_position(pos: Vector3) -> Self {
        Self { position: pos, ..Default::default() }
    }
    pub fn to_mat4(&self) -> glam::Mat4 {
        let r = self.rotation;
        let m3 = Mat3::from_cols_array(&[
            r[0][0], r[1][0], r[2][0],
            r[0][1], r[1][1], r[2][1],
            r[0][2], r[1][2], r[2][2],
        ]);
        let q = Quat::from_mat3(&m3);
        glam::Mat4::from_rotation_translation(q, self.position.to_glam())
    }
    pub fn right(&self) -> GVec3 { GVec3::new(self.rotation[0][0], self.rotation[1][0], self.rotation[2][0]) }
    pub fn up(&self) -> GVec3 { GVec3::new(self.rotation[0][1], self.rotation[1][1], self.rotation[2][1]) }
    pub fn look(&self) -> GVec3 { GVec3::new(-self.rotation[0][2], -self.rotation[1][2], -self.rotation[2][2]) }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum PartShape {
    #[default]
    Block,
    Ball,
    Cylinder,
    Wedge,
    CornerWedge,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MeshRef {
    pub mesh_id: String,
    pub texture_id: String,
    pub mesh_type: Option<String>, // for SpecialMesh: Head/Sphere/Wedge/Brick/FileMesh
    pub scale: Option<Vector3>,
    pub offset: Option<Vector3>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Part {
    pub name: String,
    pub position: Vector3, // kept for rbx-player compatibility
    pub size: Vector3,
    pub color: [f32; 3],
    // extended BasePart data
    pub cframe: CFrameValue,
    pub transparency: f32,
    pub reflectance: f32,
    pub material: String,
    pub shape: PartShape,
    pub class_name: String,
    pub cast_shadow: bool,
    pub mesh: Option<MeshRef>, // MeshPart/SpecialMesh/CSG-baked representation hint
    /// Cached mesh file (`.mesh`) for `MeshPart`/`SpecialMesh` geometry.
    #[serde(default)]
    pub mesh_file: Option<String>,
    /// Cached image used as this part's surface texture (Toolbox Image/Decal insert).
    #[serde(default)]
    pub decal: Option<String>,
}

impl Default for Part {
    fn default() -> Self {
        Self {
            name: "Part".to_string(),
            position: Vector3::default(),
            size: Vector3::new(4.0, 1.0, 2.0),
            color: [0.63, 0.64, 0.65],
            cframe: CFrameValue::default(),
            transparency: 0.0,
            reflectance: 0.0,
            material: "Plastic".to_string(),
            shape: PartShape::Block,
            class_name: "Part".to_string(),
            cast_shadow: true,
            mesh: None,
            mesh_file: None,
            decal: None,
        }
    }
}

impl Part {
    /// Shifts the part in world space. Used when placing an inserted model.
    pub fn translate(&mut self, offset: Vector3) {
        self.position.x += offset.x;
        self.position.y += offset.y;
        self.position.z += offset.z;
        self.cframe.position.x += offset.x;
        self.cframe.position.y += offset.y;
        self.cframe.position.z += offset.z;
    }
}

/// World-space axis-aligned bounds of a set of parts (`(min, max)`).
///
/// Rotation is ignored, which is accurate enough to centre a freshly inserted
/// model in front of the camera.
pub fn parts_bounds(parts: &[Part]) -> Option<(Vector3, Vector3)> {
    let mut min = Vector3::new(f32::INFINITY, f32::INFINITY, f32::INFINITY);
    let mut max = Vector3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    let mut any = false;
    for part in parts {
        if part.size.x <= 0.0 && part.size.y <= 0.0 && part.size.z <= 0.0 {
            continue;
        }
        any = true;
        let half = part.size;
        let lo = Vector3::new(
            part.position.x - half.x * 0.5,
            part.position.y - half.y * 0.5,
            part.position.z - half.z * 0.5,
        );
        let hi = Vector3::new(
            part.position.x + half.x * 0.5,
            part.position.y + half.y * 0.5,
            part.position.z + half.z * 0.5,
        );
        min.x = min.x.min(lo.x);
        min.y = min.y.min(lo.y);
        min.z = min.z.min(lo.z);
        max.x = max.x.max(hi.x);
        max.y = max.y.max(hi.y);
        max.z = max.z.max(hi.z);
    }
    any.then_some((min, max))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LightingSettings {
    pub technology: String, // Legacy, Voxel, Compatibility, ShadowMap, Future
    pub ambient: [f32; 3],
    pub outdoor_ambient: [f32; 3],
    pub brightness: f32,
    pub clock_time: f32,
    pub geographic_latitude: f32,
    pub color_shift_top: [f32; 3],
    pub color_shift_bottom: [f32; 3],
    pub environment_diffuse_scale: f32,
    pub environment_specular_scale: f32,
    pub exposure_compensation: f32,
    pub global_shadows: bool,
    pub shadow_softness: f32,
    pub fog_color: [f32; 3],
    pub fog_start: f32,
    pub fog_end: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkySettings {
    pub skybox_bk: String,
    pub skybox_dn: String,
    pub skybox_ft: String,
    pub skybox_lf: String,
    pub skybox_rt: String,
    pub skybox_up: String,
    /// Resolved local image files for the sky faces (populated at load/insert time).
    #[serde(default)]
    pub skybox_bk_file: Option<String>,
    #[serde(default)]
    pub skybox_dn_file: Option<String>,
    #[serde(default)]
    pub skybox_ft_file: Option<String>,
    #[serde(default)]
    pub skybox_lf_file: Option<String>,
    #[serde(default)]
    pub skybox_rt_file: Option<String>,
    #[serde(default)]
    pub skybox_up_file: Option<String>,
    pub sun_angular_size: f32,
    pub moon_angular_size: f32,
    pub star_count: f32,
    pub celestial_bodies_shown: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AtmosphereSettings {
    pub density: f32,
    pub offset: f32,
    pub color: [f32; 3],
    pub decay: [f32; 3],
    pub glare: f32,
    pub haze: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PostEffectsSettings {
    pub bloom_enabled: bool,
    pub bloom_intensity: f32,
    pub bloom_size: f32,
    pub bloom_threshold: f32,
    pub sun_rays_enabled: bool,
    pub sun_rays_intensity: f32,
    pub sun_rays_spread: f32,
    pub color_correction_enabled: bool,
    pub color_correction_brightness: f32,
    pub color_correction_contrast: f32,
    pub color_correction_saturation: f32,
    pub color_correction_tint: [f32; 3],
    pub dof_enabled: bool,
    pub dof_far_intensity: f32,
    pub dof_focus_distance: f32,
    pub dof_in_focus_radius: f32,
    pub dof_near_intensity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scene {
    pub name: String,
    pub parts: Vec<Part>,
    pub lighting: LightingSettings,
    pub sky: Option<SkySettings>,
    pub atmosphere: Option<AtmosphereSettings>,
    pub post: PostEffectsSettings,
    /// Scripts extracted from the place (`Script`/`LocalScript`/`ModuleScript`).
    #[serde(default)]
    pub scripts: Vec<ScriptInstance>,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            name: "Baseplate".to_string(),
            parts: vec![default_baseplate()],
            lighting: default_lighting(),
            sky: None,
            atmosphere: None,
            post: PostEffectsSettings::default(),
            scripts: Vec::new(),
        }
    }
}

fn default_baseplate() -> Part {
    let pos = Vector3::new(0.0, -8.0, 0.0);
    Part {
        name: "Baseplate".to_string(),
        position: pos,
        size: Vector3::new(512.0, 16.0, 512.0),
        color: [0.388, 0.372, 0.384],
        cframe: CFrameValue::from_position(pos),
        transparency: 0.0,
        reflectance: 0.0,
        material: "Plastic".to_string(),
        shape: PartShape::Block,
        class_name: "Part".to_string(),
        cast_shadow: true,
        mesh: None,
        mesh_file: None,
        decal: None,
    }
}

fn default_lighting() -> LightingSettings {
    LightingSettings {
        technology: "Future".to_string(),
        ambient: [0.0, 0.0, 0.0],            // Studio-like defaults; Future often uses IBL
        outdoor_ambient: [0.5, 0.5, 0.5],
        brightness: 2.0,
        clock_time: 14.5,
        geographic_latitude: 0.0,
        color_shift_top: [0.0, 0.0, 0.0],
        color_shift_bottom: [0.0, 0.0, 0.0],
        environment_diffuse_scale: 1.0,
        environment_specular_scale: 1.0,
        exposure_compensation: 0.0,
        global_shadows: true,
        shadow_softness: 0.2,
        fog_color: [0.75, 0.75, 0.75],
        fog_start: 0.0,
        fog_end: 100000.0,
    }
}

/// Parse a `.rbxl`/`.rbxm` (binary) file into a `WeakDom`.
pub fn parse_binary(bytes: &[u8]) -> Result<WeakDom> {
    Ok(from_reader(Cursor::new(bytes))?)
}

/// Parse a `.rbxl` (binary) file into our Scene format.
pub fn import_rbxl(bytes: &[u8]) -> Result<Scene> {
    Ok(import_place(bytes, "ImportedPlace")?.scene)
}

/// A parsed place: renderable scene plus the instance tree for the Explorer.
#[derive(Debug, Clone)]
pub struct ImportedPlace {
    pub name: String,
    pub scene: Scene,
    /// Service-level children of the DataModel (Explorer roots).
    pub root: Vec<InstanceNode>,
}

/// Parses a place file (`.rbxl`/`.rbxlx` bytes) into a scene and Explorer tree.
pub fn import_place(bytes: &[u8], name: &str) -> Result<ImportedPlace> {
    let dom = parse_model(bytes)?;
    let mut parts = Vec::new();
    collect_baseparts(dom.root_ref(), &dom, &mut parts);
    let lighting = extract_lighting(dom.root_ref(), &dom).unwrap_or_else(default_lighting);
    let sky = extract_sky(dom.root_ref(), &dom);
    let atmosphere = extract_atmosphere(dom.root_ref(), &dom);
    let post = extract_post(dom.root_ref(), &dom);
    let scripts = collect_scripts(&dom);
    let scene = Scene {
        name: name.to_string(),
        parts,
        lighting,
        sky,
        atmosphere,
        post,
        scripts,
    };
    let root = dom
        .get_by_ref(dom.root_ref())
        .map(|inst| {
            inst.children()
                .iter()
                .map(|child| instance_node(&dom, *child))
                .collect()
        })
        .unwrap_or_default();
    Ok(ImportedPlace {
        name: name.to_string(),
        scene,
        root,
    })
}

/// One node of a parsed model's instance tree (what the Explorer shows).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstanceNode {
    pub name: String,
    pub class_name: String,
    #[serde(default)]
    pub children: Vec<InstanceNode>,
    /// Property values worth showing in the Properties panel (`SoundId`,
    /// `Texture`, `MeshId`, `Size`, …). Filled for parsed instances and for
    /// synthesized inserts; empty when unknown (the panel then shows defaults).
    #[serde(default)]
    pub props: std::collections::BTreeMap<String, String>,
}

impl InstanceNode {
    /// A leaf instance (used for synthesized Toolbox inserts).
    pub fn leaf(name: &str, class_name: &str) -> Self {
        Self {
            name: name.to_string(),
            class_name: class_name.to_string(),
            children: Vec::new(),
            props: Default::default(),
        }
    }

    /// Adds/overwrites a display property value; returns `self` for chaining.
    pub fn with_prop(mut self, key: &str, value: impl Into<String>) -> Self {
        self.props.insert(key.to_string(), value.into());
        self
    }

    /// Appends a child; returns `self` for chaining.
    pub fn with_child(mut self, child: InstanceNode) -> Self {
        self.children.push(child);
        self
    }
}

/// A parsed Toolbox asset: the instance tree plus the parts the viewport draws.
#[derive(Debug, Clone)]
pub struct ImportedModel {
    /// Name of the inserted root (the asset's top-level instance).
    pub name: String,
    pub class_name: String,
    /// Full instance tree of the inserted root.
    pub root: InstanceNode,
    /// Every `BasePart` inside the root, still in the model's saved coordinates.
    pub parts: Vec<Part>,
}

/// Parses a Roblox model file: binary `.rbxm` (`<roblox!`) or XML `.rbxmx`.
pub fn parse_model(bytes: &[u8]) -> Result<WeakDom> {
    if bytes.starts_with(b"<roblox!") {
        return parse_binary(bytes);
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| anyhow!("downloaded asset is neither binary .rbxm nor UTF-8 XML .rbxmx"))?;
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if text.starts_with("<roblox") {
        return Ok(rbx_xml::from_str(text, rbx_xml::DecodeOptions::new())?);
    }
    Err(anyhow!(
        "unrecognized model data (expected `<roblox!` binary or `<roblox` XML)"
    ))
}

fn instance_node(dom: &WeakDom, referent: Ref) -> InstanceNode {
    let Some(inst) = dom.get_by_ref(referent) else {
        return InstanceNode::default();
    };
    InstanceNode {
        name: inst.name.clone(),
        class_name: inst.class.as_str().to_string(),
        children: inst
            .children()
            .iter()
            .map(|child| instance_node(dom, *child))
            .collect(),
        props: instance_props(inst),
    }
}

/// Collects primitive property values so the Properties panel can show the real
/// values of a parsed/inserted instance instead of class defaults.
fn instance_props(inst: &Instance) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for (key, value) in inst.properties.iter() {
        let text = match value {
            Variant::String(s) => s.clone(),
            Variant::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
            Variant::Float32(v) => trim_float(*v),
            Variant::Float64(v) => trim_float(*v as f32),
            Variant::Int32(v) => v.to_string(),
            Variant::Int64(v) => v.to_string(),
            Variant::Content(c) => match c.value() {
                rbx_types::ContentType::Uri(uri) => uri.clone(),
                rbx_types::ContentType::Object(r) => format!("{r:?}"),
                _ => continue,
            },
            Variant::ContentId(c) => format!("{c:?}").trim_matches('"').to_string(),
            _ => continue,
        };
        if !text.is_empty() {
            out.insert(key.to_string(), text);
        }
    }
    out
}

fn trim_float(v: f32) -> String {
    if v.fract().abs() < 1e-6 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Parses a Toolbox model (`.rbxm`/`.rbxmx` bytes) into an instance tree plus
/// the `BasePart`s needed to render it.
pub fn import_model(bytes: &[u8]) -> Result<ImportedModel> {
    let dom = parse_model(bytes)?;
    let roots: Vec<Ref> = dom
        .get_by_ref(dom.root_ref())
        .map(|inst| inst.children().to_vec())
        .unwrap_or_default();
    if roots.is_empty() {
        return Err(anyhow!("the model contains no instances"));
    }
    let mut parts = Vec::new();
    for root in &roots {
        collect_baseparts(*root, &dom, &mut parts);
    }
    let root = if roots.len() == 1 {
        instance_node(&dom, roots[0])
    } else {
        // Several top-level instances: wrap them in a synthetic Model so the
        // Explorer still gets a single insertable node.
        InstanceNode {
            name: "Model".to_string(),
            class_name: "Model".to_string(),
            children: roots.iter().map(|r| instance_node(&dom, *r)).collect(),
            props: Default::default(),
        }
    };
    Ok(ImportedModel {
        name: root.name.clone(),
        class_name: root.class_name.clone(),
        root,
        parts,
    })
}

/// Scans a model payload for the first content property named in `props` whose
/// URI points at an `rbxassetid://<n>` asset, returning that id. Toolbox image
/// and mesh entries are often delivered as `.rbxm`/`.rbxmx` containers that wrap
/// the real `Decal`/`ImageLabel`/`MeshPart`, so the payload must be unwrapped
/// before it can be decoded.
pub fn find_asset_reference(bytes: &[u8], props: &[&str]) -> Option<i64> {
    let dom = parse_model(bytes).ok()?;
    fn walk(dom: &WeakDom, referent: Ref, props: &[&str]) -> Option<i64> {
        let inst = dom.get_by_ref(referent)?;
        for (key, value) in inst.properties.iter() {
            // rbx_xml stores `<Content name="Texture">` as `TextureContent`, and
            // binary `MeshId` shows up as `MeshContent` in XML.
            let raw = key.as_str();
            let base = raw.trim_end_matches("Content");
            let wanted = props.iter().any(|p| p.eq_ignore_ascii_case(base))
                || (base.eq_ignore_ascii_case("Mesh")
                    && props.iter().any(|p| p.eq_ignore_ascii_case("MeshId")))
                || (base.eq_ignore_ascii_case("Texture")
                    && props.iter().any(|p| p.eq_ignore_ascii_case("TextureId")));
            if !wanted {
                continue;
            }
            if let Variant::Content(content) = value {
                if let rbx_types::ContentType::Uri(uri) = content.value() {
                    if let Some(id) = parse_asset_id(uri) {
                        return Some(id);
                    }
                }
            }
        }
        for child in inst.children() {
            if let Some(id) = walk(dom, *child, props) {
                return Some(id);
            }
        }
        None
    }
    let root = dom.get_by_ref(dom.root_ref())?;
    root.children()
        .iter()
        .find_map(|child| walk(&dom, *child, props))
}

/// Extracts the numeric id from `rbxassetid://123`, `http://www.roblox.com/asset/?id=123`
/// or `https://host/123?x=1`.
fn parse_asset_id(uri: &str) -> Option<i64> {
    if let Some(pos) = uri.find("id=") {
        let digits: String = uri[pos + 3..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(id) = digits.parse::<i64>() {
            return Some(id);
        }
    }
    let tail = uri.rsplit('/').next()?.split(['?', '&']).next()?;
    tail.trim().parse::<i64>().ok()
}

fn is_basepart_class(class: &str) -> bool {
    matches!(
        class,
        "Part" | "SpawnLocation" | "Seat" | "VehicleSeat" | "Platform" | "SkateboardPlatform"
            | "WedgePart" | "CornerWedgePart" | "TrussPart" | "MeshPart" | "PartOperation"
            | "UnionOperation" | "NegateOperation" | "IntersectOperation"
    )
}

fn collect_baseparts(referent: Ref, dom: &WeakDom, out: &mut Vec<Part>) {
    let Some(inst) = dom.get_by_ref(referent) else { return; };
    let class = inst.class.as_str();
    if is_basepart_class(class) {
        let mut p = Part::default();
        p.name = inst.name.clone();
        p.class_name = class.to_string();
        p.size = get_vec3(inst, "Size").unwrap_or(p.size);
        p.color = get_color3(inst, "Color").unwrap_or(p.color);
        p.transparency = get_f32(inst, "Transparency").unwrap_or(0.0);
        p.reflectance = get_f32(inst, "Reflectance").unwrap_or(0.0);
        p.material = get_material(inst).unwrap_or_else(|| "Plastic".to_string());
        p.cast_shadow = get_bool(inst, "CastShadow").unwrap_or(true);
        if let Some(cf) = get_cframe(inst, "CFrame") {
            p.position = cf.position;
            p.cframe = cf;
        } else if let Some(pos) = get_vec3(inst, "Position") {
            p.position = pos;
            p.cframe = CFrameValue::from_position(pos);
        }
        p.shape = get_shape(inst, class);
        p.mesh = get_mesh_ref(inst, dom, referent, class);
        out.push(p);
    }
    for child in inst.children().to_vec() {
        collect_baseparts(child, dom, out);
    }
}

fn get_mesh_ref(inst: &Instance, dom: &WeakDom, referent: Ref, class: &str) -> Option<MeshRef> {
    if class == "MeshPart" {
        return Some(MeshRef {
            mesh_id: get_content(inst, "MeshId").unwrap_or_default(),
            texture_id: get_content(inst, "TextureID").unwrap_or_default(),
            mesh_type: Some("MeshPart".to_string()),
            scale: None,
            offset: None,
        });
    }
    if matches!(class, "UnionOperation" | "IntersectOperation") {
        // CSG has baked mesh data (AssetId/ChildData in some forms); treat as mesh-like
        let id = get_content(inst, "AssetId").unwrap_or_default();
        return Some(MeshRef {
            mesh_id: id,
            texture_id: String::new(),
            mesh_type: Some(class.to_string()),
            scale: None,
            offset: None,
        });
    }
    // SpecialMesh child
    if let Some(this) = dom.get_by_ref(referent) {
        for child_ref in this.children() {
            if let Some(ch) = dom.get_by_ref(*child_ref) {
                if ch.class.as_str() == "SpecialMesh" || ch.class.as_str() == "FileMesh" {
                    return Some(MeshRef {
                        mesh_id: get_content(ch, "MeshId").unwrap_or_default(),
                        texture_id: get_content(ch, "TextureId").unwrap_or_default(),
                        mesh_type: get_enum_name(ch, "MeshType"),
                        scale: get_vec3(ch, "Scale"),
                        offset: get_vec3(ch, "Offset"),
                    });
                }
            }
        }
    }
    let _ = inst;
    None
}

fn get_shape(inst: &Instance, class: &str) -> PartShape {
    match class {
        "WedgePart" => PartShape::Wedge,
        "CornerWedgePart" => PartShape::CornerWedge,
        _ => {
            // Part.Shape is an EnumItem
            if let Some(name) = get_enum_name(inst, "Shape") {
                return match name.as_str() {
                    "Ball" => PartShape::Ball,
                    "Cylinder" => PartShape::Cylinder,
                    "Wedge" => PartShape::Wedge,
                    "CornerWedge" => PartShape::CornerWedge,
                    _ => PartShape::Block,
                };
            }
            PartShape::Block
        }
    }
}

fn find_descendant_service<'a>(referent: Ref, dom: &'a WeakDom, class: &str) -> Option<&'a Instance> {
    let inst = dom.get_by_ref(referent)?;
    if inst.class.as_str() == class || inst.name == class { return Some(inst); }
    for child in inst.children() {
        if let Some(found) = find_descendant_service(*child, dom, class) { return Some(found); }
    }
    None
}

fn extract_lighting(root: Ref, dom: &WeakDom) -> Option<LightingSettings> {
    let inst = find_descendant_service(root, dom, "Lighting")?;
    let mut s = default_lighting();
    if let Some(v) = get_enum_name(inst, "Technology") { s.technology = v; }
    s.ambient = get_color3(inst, "Ambient").unwrap_or(s.ambient);
    s.outdoor_ambient = get_color3(inst, "OutdoorAmbient").unwrap_or(s.outdoor_ambient);
    s.brightness = get_f32(inst, "Brightness").unwrap_or(s.brightness);
    s.clock_time = get_f32(inst, "ClockTime").unwrap_or(s.clock_time);
    s.geographic_latitude = get_f32(inst, "GeographicLatitude").unwrap_or(s.geographic_latitude);
    s.color_shift_top = get_color3(inst, "ColorShift_Top").unwrap_or(s.color_shift_top);
    s.color_shift_bottom = get_color3(inst, "ColorShift_Bottom").unwrap_or(s.color_shift_bottom);
    s.environment_diffuse_scale = get_f32(inst, "EnvironmentDiffuseScale").unwrap_or(s.environment_diffuse_scale);
    s.environment_specular_scale = get_f32(inst, "EnvironmentSpecularScale").unwrap_or(s.environment_specular_scale);
    s.exposure_compensation = get_f32(inst, "ExposureCompensation").unwrap_or(s.exposure_compensation);
    s.global_shadows = get_bool(inst, "GlobalShadows").unwrap_or(s.global_shadows);
    s.shadow_softness = get_f32(inst, "ShadowSoftness").unwrap_or(s.shadow_softness);
    s.fog_color = get_color3(inst, "FogColor").unwrap_or(s.fog_color);
    s.fog_start = get_f32(inst, "FogStart").unwrap_or(s.fog_start);
    s.fog_end = get_f32(inst, "FogEnd").unwrap_or(s.fog_end);
    Some(s)
}

fn extract_sky(root: Ref, dom: &WeakDom) -> Option<SkySettings> {
    let lighting = find_descendant_service(root, dom, "Lighting")?;
    let mut sky_inst = None;
    for child in lighting.children() {
        if let Some(ch) = dom.get_by_ref(*child) {
            if ch.class.as_str() == "Sky" { sky_inst = Some(ch); break; }
        }
    }
    let inst = sky_inst?;
    Some(SkySettings {
        skybox_bk: get_content(inst, "SkyboxBk").unwrap_or_default(),
        skybox_dn: get_content(inst, "SkyboxDn").unwrap_or_default(),
        skybox_ft: get_content(inst, "SkyboxFt").unwrap_or_default(),
        skybox_lf: get_content(inst, "SkyboxLf").unwrap_or_default(),
        skybox_rt: get_content(inst, "SkyboxRt").unwrap_or_default(),
        skybox_up: get_content(inst, "SkyboxUp").unwrap_or_default(),
        skybox_bk_file: None,
        skybox_dn_file: None,
        skybox_ft_file: None,
        skybox_lf_file: None,
        skybox_rt_file: None,
        skybox_up_file: None,
        sun_angular_size: get_f32(inst, "SunAngularSize").unwrap_or(21.0),
        moon_angular_size: get_f32(inst, "MoonAngularSize").unwrap_or(11.0),
        star_count: get_f32(inst, "StarCount").unwrap_or(3000.0),
        celestial_bodies_shown: get_bool(inst, "CelestialBodiesShown").unwrap_or(true),
    })
}

fn extract_atmosphere(root: Ref, dom: &WeakDom) -> Option<AtmosphereSettings> {
    let lighting = find_descendant_service(root, dom, "Lighting")?;
    let mut atmo = None;
    for child in lighting.children() {
        if let Some(ch) = dom.get_by_ref(*child) {
            if ch.class.as_str() == "Atmosphere" { atmo = Some(ch); break; }
        }
    }
    let inst = atmo?;
    Some(AtmosphereSettings {
        density: get_f32(inst, "Density").unwrap_or(0.395),
        offset: get_f32(inst, "Offset").unwrap_or(0.0),
        color: get_color3(inst, "Color").unwrap_or([0.78, 0.78, 0.78]),
        decay: get_color3(inst, "Decay").unwrap_or([0.416, 0.439, 0.49]),
        glare: get_f32(inst, "Glare").unwrap_or(0.0),
        haze: get_f32(inst, "Haze").unwrap_or(0.0),
    })
}

fn extract_post(root: Ref, dom: &WeakDom) -> PostEffectsSettings {
    let mut p = PostEffectsSettings::default();
    fn walk(referent: Ref, dom: &WeakDom, p: &mut PostEffectsSettings) {
        let Some(inst) = dom.get_by_ref(referent) else { return; };
        match inst.class.as_str() {
            "BloomEffect" => {
                p.bloom_enabled = get_bool(inst, "Enabled").unwrap_or(true);
                p.bloom_intensity = get_f32(inst, "Intensity").unwrap_or(1.0);
                p.bloom_size = get_f32(inst, "Size").unwrap_or(24.0);
                p.bloom_threshold = get_f32(inst, "Threshold").unwrap_or(2.0);
            }
            "SunRaysEffect" => {
                p.sun_rays_enabled = get_bool(inst, "Enabled").unwrap_or(true);
                p.sun_rays_intensity = get_f32(inst, "Intensity").unwrap_or(0.25);
                p.sun_rays_spread = get_f32(inst, "Spread").unwrap_or(1.0);
            }
            "ColorCorrectionEffect" => {
                p.color_correction_enabled = get_bool(inst, "Enabled").unwrap_or(true);
                p.color_correction_brightness = get_f32(inst, "Brightness").unwrap_or(0.0);
                p.color_correction_contrast = get_f32(inst, "Contrast").unwrap_or(0.0);
                p.color_correction_saturation = get_f32(inst, "Saturation").unwrap_or(0.0);
                p.color_correction_tint = get_color3(inst, "TintColor").unwrap_or([1.0, 1.0, 1.0]);
            }
            "DepthOfFieldEffect" => {
                p.dof_enabled = get_bool(inst, "Enabled").unwrap_or(true);
                p.dof_far_intensity = get_f32(inst, "FarIntensity").unwrap_or(0.1);
                p.dof_focus_distance = get_f32(inst, "FocusDistance").unwrap_or(0.05);
                p.dof_in_focus_radius = get_f32(inst, "InFocusRadius").unwrap_or(30.0);
                p.dof_near_intensity = get_f32(inst, "NearIntensity").unwrap_or(0.75);
            }
            _ => {}
        }
        for c in inst.children() { walk(*c, dom, p); }
    }
    walk(root, dom, &mut p);
    p
}

// --- property helpers (tolerant of rbx_dom_weak/variant API differences) ---
fn prop<'a>(inst: &'a Instance, name: &str) -> Option<&'a Variant> {
    inst.properties.get(&ustr(name))
}

fn get_f32(inst: &Instance, name: &str) -> Option<f32> {
    match prop(inst, name)? {
        Variant::Float32(v) => Some(*v),
        Variant::Float64(v) => Some(*v as f32),
        Variant::Int32(v) => Some(*v as f32),
        Variant::Int64(v) => Some(*v as f32),
        _ => None,
    }
}

fn get_bool(inst: &Instance, name: &str) -> Option<bool> {
    match prop(inst, name)? { Variant::Bool(v) => Some(*v), _ => None }
}

fn get_vec3(inst: &Instance, name: &str) -> Option<Vector3> {
    match prop(inst, name)? {
        Variant::Vector3(v) => Some(Vector3::new(v.x, v.y, v.z)),
        _ => None,
    }
}

fn get_color3(inst: &Instance, name: &str) -> Option<[f32; 3]> {
    match prop(inst, name)? {
        Variant::Color3(c) => Some([c.r, c.g, c.b]),
        Variant::Color3uint8(c) => Some([c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0]),
        _ => None,
    }
}

fn get_enum_name(inst: &Instance, name: &str) -> Option<String> {
    match prop(inst, name)? {
        Variant::Enum(e) => Some(format!("{:?}", e)),
        Variant::EnumItem(e) => Some(format!("{}:{}", e.ty, e.value)),
        _ => None,
    }
}

fn get_material(inst: &Instance) -> Option<String> {
    match prop(inst, "Material")? {
        Variant::EnumItem(e) => Some(format!("{}:{}", e.ty, e.value)),
        Variant::Enum(e) => Some(format!("{:?}", e)),
        _ => None,
    }
}

fn get_content(inst: &Instance, name: &str) -> Option<String> {
    match prop(inst, name)? {
        Variant::Content(c) => Some(format!("{:?}", c)),
        Variant::ContentId(c) => Some(format!("{:?}", c)),
        Variant::String(s) => Some(s.to_string()),
        Variant::BinaryString(s) => Some(String::from_utf8_lossy(s.as_ref()).to_string()),
        _ => None,
    }
}

fn get_cframe(inst: &Instance, name: &str) -> Option<CFrameValue> {
    match prop(inst, name)? {
        Variant::CFrame(cf) => {
            // rbx_types CFrame: position + orientation (matrix)
            let pos = Vector3::new(cf.position.x, cf.position.y, cf.position.z);
            let o = cf.orientation;
            // orientation is typically [[f32;3];3] or accessors; handle common layouts
            let rot = [
                [o.x.x, o.y.x, o.z.x],
                [o.x.y, o.y.y, o.z.y],
                [o.x.z, o.y.z, o.z.z],
            ];
            Some(CFrameValue { position: pos, rotation: rot })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real binary model shipped with the extracted Studio branding assets.
    #[test]
    fn imports_binary_model() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/studio/branding/vLuau.rbxm");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skipping: {} not present", path.display());
            return;
        };
        assert!(bytes.starts_with(b"<roblox!"));
        let model = import_model(&bytes).expect("binary model import");
        assert!(!model.name.is_empty());
        assert!(!model.root.children.is_empty(), "tree should have children");
    }

    #[test]
    fn rejects_unknown_data() {
        assert!(import_model(b"not a model").is_err());
    }

    /// Toolbox image/mesh assets can arrive as a model wrapping a `Decal` or
    /// `MeshPart`; the inner asset id must be recoverable.
    #[test]
    fn finds_inner_asset_reference() {
        let xml = r#"<roblox version="4">
  <Item class="Decal" referent="RBX0">
    <Properties>
      <string name="Name">Decal</string>
      <Content name="Texture"><url>http://www.roblox.com/asset/?id=6319951708</url></Content>
    </Properties>
  </Item>
</roblox>"#;
        let dom = parse_model(xml.as_bytes()).expect("parse xml");
        assert_eq!(dom.get_by_ref(dom.root_ref()).unwrap().children().len(), 1);
        assert_eq!(find_asset_reference(xml.as_bytes(), &["Texture"]), Some(6319951708));

        let rbx = r#"<roblox version="4">
  <Item class="MeshPart" referent="RBX0">
    <Properties>
      <Content name="MeshId"><url>rbxassetid://123456</url></Content>
    </Properties>
  </Item>
</roblox>"#;
        let dom = parse_model(rbx.as_bytes()).expect("parse mesh xml");
        assert_eq!(dom.get_by_ref(dom.root_ref()).unwrap().children().len(), 1);
        assert_eq!(find_asset_reference(rbx.as_bytes(), &["MeshId"]), Some(123456));
        assert_eq!(find_asset_reference(b"not a model", &["Texture"]), None);
    }

    #[test]
    fn translate_moves_position_and_cframe() {
        let mut part = Part::default();
        part.position = Vector3::new(1.0, 2.0, 3.0);
        part.cframe = CFrameValue::from_position(Vector3::new(1.0, 2.0, 3.0));
        part.translate(Vector3::new(10.0, -2.0, 0.5));
        assert_eq!(part.position.y, 0.0);
        assert_eq!(part.cframe.position.x, 11.0);
        assert_eq!(part.cframe.position.z, 3.5);
    }

    #[test]
    fn bounds_cover_parts() {
        let mut a = Part::default();
        a.size = Vector3::new(4.0, 2.0, 4.0);
        a.position = Vector3::new(0.0, 1.0, 0.0);
        let (min, max) = parts_bounds(&[a]).unwrap();
        assert_eq!(min.y, 0.0);
        assert_eq!(max.y, 2.0);
        assert!(parts_bounds(&[]).is_none());
    }
}
