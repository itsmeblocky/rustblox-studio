use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use rbx_core::{Part, PartShape};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 4], // base color + alpha (1-transparency)
    pub material: [f32; 4], // metallic-ish, roughness, reflectance, material_kind
    /// Planar UV for decals (per face, 0..1 across the part).
    pub uv: [f32; 2],
    /// x = material texture layer, y = decal texture layer (-1 = none).
    pub layers: [f32; 2],
}

#[derive(Clone)]
pub struct MeshData {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

pub fn part_to_mesh(part: &Part) -> MeshData {
    let file_mesh = part.mesh_file.as_deref().and_then(mesh_from_file);
    let base = if let Some(mesh) = file_mesh.as_ref() {
        MeshData { vertices: mesh.vertices.clone(), indices: mesh.indices.clone() }
    } else {
        match (&part.mesh, part.shape, part.class_name.as_str()) {
            (Some(m), _, _) if m.mesh_type.as_deref() == Some("Ball") || m.mesh_type.as_deref() == Some("Sphere") => sphere_mesh(16, 12),
            (Some(m), _, _) if m.mesh_type.as_deref() == Some("Wedge") => wedge_mesh(),
            (Some(m), _, _) if m.mesh_type.as_deref() == Some("Head") => sphere_mesh(16, 12),
            (Some(_), _, _) => box_mesh(), // MeshPart/CSG/FileMesh fallback until the asset is cached
            (_, PartShape::Ball, _) => sphere_mesh(20, 14),
            (_, PartShape::Cylinder, _) => cylinder_mesh(20),
            (_, PartShape::Wedge, _) | (_, _, "WedgePart") => wedge_mesh(),
            (_, PartShape::CornerWedge, _) | (_, _, "CornerWedgePart") => corner_wedge_mesh(),
            _ => box_mesh(),
        }
    };
    transform_mesh(&base, part, file_mesh.is_none())
}

/// Loads (and caches) a cached `.mesh` file as unit-sized render geometry.
///
/// The asset cache stores Roblox's own mesh payload; here it is parsed once,
/// re-centred to the origin and normalized to a unit cube so `Part.size` can
/// scale it — the same model the built-in primitive shapes use.
fn mesh_from_file(path: &str) -> Option<MeshData> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, MeshData>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(path).cloned()) {
        return Some(hit);
    }
    let bytes = std::fs::read(path).ok()?;
    let geo = rbx_core::parse_mesh(&bytes).ok()?;
    let size = geo.size();
    let center = geo.center();
    let inv = Vec3::new(1.0 / size.x, 1.0 / size.y, 1.0 / size.z);
    let mut vertices = Vec::with_capacity(geo.positions.len());
    for i in 0..geo.positions.len() {
        let p = Vec3::from_array(geo.positions[i]);
        let n = Vec3::from_array(geo.normals[i]);
        let local = (p - Vec3::new(center.x, center.y, center.z)) * inv;
        vertices.push(Vertex {
            position: local.to_array(),
            normal: n.normalize_or_zero().to_array(),
            color: [1.0; 4],
            material: [0.0, 0.6, 0.0, 0.0],
            uv: geo.uvs.get(i).copied().unwrap_or([0.0, 0.0]),
            layers: [0.0, -1.0],
        });
    }
    // Roblox mesh faces are clockwise; the pipeline culls back faces expecting CCW.
    let mut indices = Vec::with_capacity(geo.indices.len());
    for tri in geo.indices.chunks_exact(3) {
        indices.extend_from_slice(&[tri[0], tri[2], tri[1]]);
    }
    let data = MeshData { vertices, indices };
    if let Ok(mut c) = cache.lock() {
        c.insert(path.to_string(), data.clone());
    }
    Some(data)
}

fn transform_mesh(base: &MeshData, part: &Part, planar_uvs: bool) -> MeshData {
    let model = part.cframe.to_mat4() * Mat4::from_scale(part.size.to_glam());
    let normal_mat = glam::Mat3::from_mat4(model).inverse().transpose();
    let alpha = (1.0 - part.transparency).clamp(0.0, 1.0);
    let mat_params = material_params(&part.material, part.reflectance);
    let layer = crate::textures::material_layer(&part.material) as f32;
    let mut vertices = Vec::with_capacity(base.vertices.len());
    for v in &base.vertices {
        let local = Vec3::from_array(v.position);
        let local_n = Vec3::from_array(v.normal);
        let p = model.transform_point3(local);
        let n = (normal_mat * local_n).normalize_or_zero();
        vertices.push(Vertex {
            position: p.to_array(),
            normal: n.to_array(),
            color: [part.color[0], part.color[1], part.color[2], alpha],
            material: mat_params,
            // Unit-space planar UV for decals; mesh files keep their own UVs.
            uv: if planar_uvs { planar_uv(local, local_n) } else { v.uv },
            layers: [layer, -1.0],
        });
    }
    MeshData { vertices, indices: base.indices.clone() }
}

/// Projects a unit-cube-space point onto the plane facing its normal.
fn planar_uv(p: Vec3, n: Vec3) -> [f32; 2] {
    let a = n.abs();
    if a.x >= a.y && a.x >= a.z {
        [p.z + 0.5, 0.5 - p.y]
    } else if a.y >= a.z {
        [p.x + 0.5, 0.5 - p.z]
    } else {
        [p.x + 0.5, 0.5 - p.y]
    }
}

fn material_params(material: &str, reflectance: f32) -> [f32; 4] {
    // [metallic, roughness, reflectance, kind] - tuned to approximate Roblox material feel; refined per technology
    let (metallic, roughness) = match material {
        "SmoothPlastic" | "Plastic" => (0.0, 0.55),
        "Metal" | "DiamondPlate" | "CorrodedMetal" => (0.9, 0.25),
        "Foil" => (1.0, 0.15),
        "Glass" | "Ice" => (0.0, 0.05),
        "Neon" => (0.0, 1.0), // emissive-ish handled in shader by kind
        "Granite" | "Concrete" | "Brick" | "Cobblestone" | "Slate" | "Rock" | "Pebble" | "Asphalt" => (0.0, 0.9),
        "Wood" | "WoodPlanks" => (0.0, 0.75),
        "Grass" | "Ground" | "LeafyGrass" | "Sand" | "Snow" | "Mud" | "Salt" | "Glacier" | "Basalt" | "CrackedLava" => (0.0, 0.95),
        "Fabric" | "Carpet" => (0.0, 0.95),
        "Marble" => (0.0, 0.2),
        "ForceField" => (0.0, 0.2),
        _ => (0.0, 0.6),
    };
    let kind = match material {
        "Neon" => 1.0,
        "Glass" | "Ice" => 2.0,
        "ForceField" => 3.0,
        _ => 0.0,
    };
    [metallic, roughness, reflectance.clamp(0.0, 1.0), kind]
}

pub fn box_mesh() -> MeshData {
    // unit cube centered at origin [-0.5,0.5]
    let p = [
        ([-0.5,-0.5,-0.5],[ -1.0, 0.0, 0.0]), ([ -0.5,-0.5, 0.5],[-1.0,0.0,0.0]), ([-0.5, 0.5, 0.5],[-1.0,0.0,0.0]), ([-0.5, 0.5,-0.5],[-1.0,0.0,0.0]),
        ([ 0.5,-0.5, 0.5],[  1.0, 0.0, 0.0]), ([  0.5,-0.5,-0.5],[ 1.0,0.0,0.0]), ([ 0.5, 0.5,-0.5],[ 1.0,0.0,0.0]), ([ 0.5, 0.5, 0.5],[ 1.0,0.0,0.0]),
        ([-0.5,-0.5,-0.5],[  0.0,-1.0, 0.0]), ([  0.5,-0.5,-0.5],[ 0.0,-1.0,0.0]), ([ 0.5,-0.5, 0.5],[ 0.0,-1.0,0.0]), ([-0.5,-0.5, 0.5],[ 0.0,-1.0,0.0]),
        ([-0.5, 0.5, 0.5],[  0.0, 1.0, 0.0]), ([  0.5, 0.5, 0.5],[ 0.0, 1.0,0.0]), ([ 0.5, 0.5,-0.5],[ 0.0, 1.0,0.0]), ([-0.5, 0.5,-0.5],[ 0.0, 1.0,0.0]),
        ([-0.5,-0.5, 0.5],[  0.0, 0.0, 1.0]), ([  0.5,-0.5, 0.5],[ 0.0, 0.0,1.0]), ([ 0.5, 0.5, 0.5],[ 0.0, 0.0,1.0]), ([-0.5, 0.5, 0.5],[ 0.0, 0.0,1.0]),
        ([ 0.5,-0.5,-0.5],[  0.0, 0.0,-1.0]), ([-0.5,-0.5,-0.5],[ 0.0, 0.0,-1.0]), ([-0.5, 0.5,-0.5],[ 0.0, 0.0,-1.0]), ([ 0.5, 0.5,-0.5],[ 0.0, 0.0,-1.0]),
    ];
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (i, face) in p.chunks(4).enumerate() {
        let base = (i * 4) as u32;
        for (pos, n) in face { vertices.push(Vertex { position: *pos, normal: *n, color: [1.0;4], material: [0.0,0.6,0.0,0.0], uv: [0.0,0.0], layers: [0.0,-1.0] }); }
        indices.extend_from_slice(&[base, base+1, base+2, base, base+2, base+3]);
    }
    MeshData { vertices, indices }
}

pub fn wedge_mesh() -> MeshData {
    // Roblox wedge: ramp up along +Z? orientation varies; use a standard wedge (block with one sloped face)
    // points: base rect y=-0.5, top edge along x at y=0.5,z=-0.5
    let verts = vec![
        // bottom
        ([-0.5,-0.5,-0.5],[0.0,-1.0,0.0]),([0.5,-0.5,-0.5],[0.0,-1.0,0.0]),([0.5,-0.5,0.5],[0.0,-1.0,0.0]),([-0.5,-0.5,0.5],[0.0,-1.0,0.0]),
        // back (-Z)
        ([-0.5,-0.5,-0.5],[0.0,0.0,-1.0]),([-0.5,0.5,-0.5],[0.0,0.0,-1.0]),([0.5,0.5,-0.5],[0.0,0.0,-1.0]),([0.5,-0.5,-0.5],[0.0,0.0,-1.0]),
        // right
        ([0.5,-0.5,-0.5],[1.0,0.0,0.0]),([0.5,0.5,-0.5],[1.0,0.0,0.0]),([0.5,-0.5,0.5],[1.0,0.0,0.0]),
        // left
        ([-0.5,-0.5,0.5],[-1.0,0.0,0.0]),([-0.5,0.5,-0.5],[-1.0,0.0,0.0]),([-0.5,-0.5,-0.5],[-1.0,0.0,0.0]),
        // slope
        ([-0.5,-0.5,0.5],[0.0,0.7071,0.7071]),([0.5,-0.5,0.5],[0.0,0.7071,0.7071]),([0.5,0.5,-0.5],[0.0,0.7071,0.7071]),([-0.5,0.5,-0.5],[0.0,0.7071,0.7071]),
    ];
    let mut vertices = Vec::new();
    for (pos,n) in &verts { vertices.push(Vertex{ position:*pos, normal:*n, color:[1.0;4], material:[0.0,0.6,0.0,0.0], uv:[0.0,0.0], layers:[0.0,-1.0]}); }
    let indices = vec![
        0,1,2, 0,2,3, // bottom
        4,5,6, 4,6,7, // back
        8,9,10,       // right
        11,12,13,     // left
        14,15,16, 14,16,17, // slope
    ];
    MeshData { vertices, indices }
}

pub fn corner_wedge_mesh() -> MeshData {
    let verts = vec![
        ([-0.5,-0.5,-0.5],[0.0,-1.0,0.0]),([0.5,-0.5,-0.5],[0.0,-1.0,0.0]),([0.5,-0.5,0.5],[0.0,-1.0,0.0]),([-0.5,-0.5,0.5],[0.0,-1.0,0.0]),
        ([-0.5,-0.5,-0.5],[0.0,0.0,-1.0]),([-0.5,0.5,-0.5],[0.0,0.0,-1.0]),([0.5,-0.5,-0.5],[0.0,0.0,-1.0]),
        ([-0.5,-0.5,-0.5],[-1.0,0.0,0.0]),([-0.5,-0.5,0.5],[-1.0,0.0,0.0]),([-0.5,0.5,-0.5],[-1.0,0.0,0.0]),
        ([-0.5,-0.5,0.5],[0.577,0.577,0.577]),([0.5,-0.5,-0.5],[0.577,0.577,0.577]),([-0.5,0.5,-0.5],[0.577,0.577,0.577]),
    ];
    let mut vertices = Vec::new();
    for (pos,n) in &verts { vertices.push(Vertex{ position:*pos, normal:*n, color:[1.0;4], material:[0.0,0.6,0.0,0.0], uv:[0.0,0.0], layers:[0.0,-1.0]}); }
    let indices = vec![0,1,2,0,2,3, 4,5,6, 7,8,9, 10,11,12];
    MeshData { vertices, indices }
}

pub fn sphere_mesh(segs: u32, rings: u32) -> MeshData {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for y in 0..=rings {
        let v = y as f32 / rings as f32;
        let phi = (v - 0.5) * std::f32::consts::PI;
        let (sp, cp) = phi.sin_cos();
        for x in 0..=segs {
            let u = x as f32 / segs as f32;
            let theta = u * std::f32::consts::TAU;
            let (st, ct) = theta.sin_cos();
            let n = Vec3::new(ct*cp, sp, st*cp);
            vertices.push(Vertex{ position:(n*0.5).to_array(), normal:n.to_array(), color:[1.0;4], material:[0.0,0.6,0.0,0.0], uv:[0.0,0.0], layers:[0.0,-1.0]});
        }
    }
    let stride = segs + 1;
    for y in 0..rings { for x in 0..segs {
        let a = y*stride+x; let b=a+1; let c=a+stride; let d=c+1;
        indices.extend_from_slice(&[a,b,d,a,d,c]);
    }}
    MeshData { vertices, indices }
}

pub fn cylinder_mesh(segs: u32) -> MeshData {
    // unit cylinder along Y, radius 0.5, height 1
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    // side
    for y in 0..2 { for i in 0..=segs {
        let u = i as f32 / segs as f32;
        let a = u * std::f32::consts::TAU;
        let (s,c)=a.sin_cos();
        let py = if y==0 {-0.5} else {0.5};
        vertices.push(Vertex{ position:[c*0.5,py,s*0.5], normal:[c,0.0,s], color:[1.0;4], material:[0.0,0.6,0.0,0.0], uv:[0.0,0.0], layers:[0.0,-1.0]});
    }}
    let stride=segs+1;
    for i in 0..segs { let a=i; let b=a+1; let c=a+stride; let d=c+1; indices.extend_from_slice(&[a,c,b,b,c,d]); }
    // caps
    let top_center = vertices.len() as u32;
    vertices.push(Vertex{ position:[0.0,0.5,0.0], normal:[0.0,1.0,0.0], color:[1.0;4], material:[0.0,0.6,0.0,0.0], uv:[0.0,0.0], layers:[0.0,-1.0]});
    let top_start = vertices.len() as u32;
    for i in 0..=segs { let u=i as f32/segs as f32; let a=u*std::f32::consts::TAU; let (s,c)=a.sin_cos();
        vertices.push(Vertex{ position:[c*0.5,0.5,s*0.5], normal:[0.0,1.0,0.0], color:[1.0;4], material:[0.0,0.6,0.0,0.0], uv:[0.0,0.0], layers:[0.0,-1.0]}); }
    for i in 0..segs { indices.extend_from_slice(&[top_center, top_start+i, top_start+i+1]); }
    let bot_center = vertices.len() as u32;
    vertices.push(Vertex{ position:[0.0,-0.5,0.0], normal:[0.0,-1.0,0.0], color:[1.0;4], material:[0.0,0.6,0.0,0.0], uv:[0.0,0.0], layers:[0.0,-1.0]});
    let bot_start = vertices.len() as u32;
    for i in 0..=segs { let u=i as f32/segs as f32; let a=u*std::f32::consts::TAU; let (s,c)=a.sin_cos();
        vertices.push(Vertex{ position:[c*0.5,-0.5,s*0.5], normal:[0.0,-1.0,0.0], color:[1.0;4], material:[0.0,0.6,0.0,0.0], uv:[0.0,0.0], layers:[0.0,-1.0]}); }
    for i in 0..segs { indices.extend_from_slice(&[bot_center, bot_start+i+1, bot_start+i]); }
    MeshData { vertices, indices }
}
