//! Roblox `.mesh` geometry (`MeshPart` / `SpecialMesh` FileMesh assets).
//!
//! Downloaded mesh assets are cached on disk (see the app's `asset_cache`) and
//! parsed here so both the editor and the player can turn them into triangles.
//! The parser accepts the versioned formats handled by `rbx_mesh`: v1 ASCII as
//! well as v2????????v5 binary. Legacy v1 files have no face list (three vertices per
//! triangle), newer revisions carry explicit indices.

use crate::Vector3;
use anyhow::{anyhow, Result};
use rbx_mesh::mesh::{Face2, Mesh, Vertices2};
use std::io::Cursor;

/// Renderer-agnostic triangle geometry, in the mesh's own coordinate space.
#[derive(Debug, Clone, Default)]
pub struct MeshGeometry {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub min: Vector3,
    pub max: Vector3,
}

impl MeshGeometry {
    /// Axis-aligned size of the mesh, in studs.
    pub fn size(&self) -> Vector3 {
        Vector3::new(
            (self.max.x - self.min.x).max(0.001),
            (self.max.y - self.min.y).max(0.001),
            (self.max.z - self.min.z).max(0.001),
        )
    }

    /// Centre of the bounding box (meshes are not always origin-centred).
    pub fn center(&self) -> Vector3 {
        Vector3::new(
            (self.min.x + self.max.x) * 0.5,
            (self.min.y + self.max.y) * 0.5,
            (self.min.z + self.max.z) * 0.5,
        )
    }
}

/// Parses a `.mesh` payload (any version `rbx_mesh` understands).
pub fn parse_mesh(bytes: &[u8]) -> Result<MeshGeometry> {
    if bytes.is_empty() {
        return Err(anyhow!("the mesh payload is empty"));
    }
    let mesh = rbx_mesh::read_mesh_versioned(Cursor::new(bytes))
        .map_err(|e| anyhow!("unreadable Roblox mesh: {e}"))?;
    let mut geo = MeshGeometry::default();
    let mut push = |pos: [f32; 3], norm: [f32; 3], uv: [f32; 2]| {
        geo.positions.push(pos);
        geo.normals.push(norm);
        geo.uvs.push(uv);
    };
    match mesh {
        Mesh::V1(m) => {
            for v in &m.vertices {
                push(v.pos, v.norm, [v.tex[0], v.tex[1]]);
            }
            geo.indices = (0..geo.positions.len() as u32).collect();
        }
        Mesh::V2(m) => {
            match &m.vertices {
                Vertices2::Full(verts) => {
                    for v in verts {
                        push(v.pos, v.norm, v.tex);
                    }
                }
                Vertices2::Truncated(verts) => {
                    for v in verts {
                        push(v.pos, v.norm, v.tex);
                    }
                }
            }
            geo.indices = faces_to_indices(&m.faces);
        }
        Mesh::V3(m) => {
            match &m.vertices {
                Vertices2::Full(verts) => {
                    for v in verts {
                        push(v.pos, v.norm, v.tex);
                    }
                }
                Vertices2::Truncated(verts) => {
                    for v in verts {
                        push(v.pos, v.norm, v.tex);
                    }
                }
            }
            geo.indices = faces_to_indices(&m.faces);
        }
        Mesh::V4(m) => {
            for v in &m.vertices {
                push(v.pos, v.norm, v.tex);
            }
            geo.indices = faces_to_indices(&m.faces);
        }
        Mesh::V5(m) => {
            for v in &m.vertices {
                push(v.pos, v.norm, v.tex);
            }
            geo.indices = faces_to_indices(&m.faces);
        }
    }
    if geo.positions.is_empty() || geo.indices.is_empty() {
        return Err(anyhow!("the mesh has no triangles"));
    }
    geo.indices.retain(|i| (*i as usize) < geo.positions.len());
    if geo.indices.is_empty() {
        return Err(anyhow!("the mesh indices point outside the vertex list"));
    }
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for p in &geo.positions {
        for a in 0..3 {
            min[a] = min[a].min(p[a]);
            max[a] = max[a].max(p[a]);
        }
    }
    geo.min = Vector3::new(min[0], min[1], min[2]);
    geo.max = Vector3::new(max[0], max[1], max[2]);
    Ok(geo)
}

fn faces_to_indices(faces: &[Face2]) -> Vec<u32> {
    let mut out = Vec::with_capacity(faces.len() * 3);
    for f in faces {
        out.push(f.0[0].0);
        out.push(f.0[1].0);
        out.push(f.0[2].0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synthetic_v1_mesh() {
        // "version 1.00" header, one triangle, 3 vertices (position, normal,
        // three-float UV) all on a single line. v1.00 coordinates are halved.
        let data = "version 1.00\n1\n\
[0,0,0][0,1,0][0,0,0]\
[2,0,0][0,1,0][1,0,0]\
[0,0,2][0,1,0][0,1,0]\n";
        let geo = parse_mesh(data.as_bytes()).expect("v1 mesh");
        assert_eq!(geo.positions.len(), 3);
        assert_eq!(geo.indices.len(), 3);
        assert!((geo.size().x - 1.0).abs() < 1e-4);
        assert!((geo.size().z - 1.0).abs() < 1e-4);
    }
}
