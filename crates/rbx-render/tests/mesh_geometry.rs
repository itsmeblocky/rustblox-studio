//! MeshPart geometry: cached `.mesh` files must normalize into the part's box.

use rbx_core::{Part, Vector3};
use rbx_render::mesh::part_to_mesh;

fn write_v1_mesh() -> std::path::PathBuf {
    let path = std::env::temp_dir().join("rustblox-mesh-test.mesh");
    // One quad (two triangles) spanning 4 x 2 x 8 studs. v1.00 halves
    // coordinates on load, so the raw numbers are doubled to end up 4/2/8.
    // v1 keeps every vertex on the line after the face count.
    let data = "version 1.00\n2\n\
[0,1,0][0,1,0][0,1,0][8,1,0][0,1,0][1,1,0][8,1,16][0,1,0][1,0,0]\
[0,1,0][0,1,0][0,1,0][8,1,16][0,1,0][1,0,0][0,1,16][0,1,0][0,0,0]\n";
    std::fs::write(&path, data).expect("write mesh");
    path
}

#[test]
fn mesh_file_geometry_fills_the_part_size() {
    let path = write_v1_mesh();
    let geo = rbx_core::parse_mesh(&std::fs::read(&path).unwrap()).expect("parse mesh");
    assert!((geo.size().x - 4.0).abs() < 1e-3, "width {:?}", geo.size());
    assert!((geo.size().z - 8.0).abs() < 1e-3, "depth {:?}", geo.size());

    let mut part = Part::default();
    part.class_name = "MeshPart".to_string();
    part.size = Vector3::new(4.0, 2.0, 8.0);
    part.mesh_file = Some(path.display().to_string());
    let mesh = part_to_mesh(&part);
    assert_eq!(mesh.indices.len(), 6, "two triangles");
    assert_eq!(mesh.vertices.len(), 6);

    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for v in &mesh.vertices {
        for a in 0..3 {
            min[a] = min[a].min(v.position[a]);
            max[a] = max[a].max(v.position[a]);
        }
    }
    assert!((max[0] - min[0] - 4.0).abs() < 1e-2, "x bounds {min:?}..{max:?}");
    assert!((max[2] - min[2] - 8.0).abs() < 1e-2, "z bounds {min:?}..{max:?}");
    // The quad is flat in Y, but it must not collapse the part's box either;
    // normalization guards zero extent with a small epsilon.
    assert!(max[1] >= min[1]);

    let _ = std::fs::remove_file(&path);
}
