//! GPU smoke tests for the new texture/decal paths.
//!
//! These run on whatever adapter `wgpu` finds; on a machine without one they
//! return early instead of failing, so `cargo test` stays useful offline.

use rbx_core::{Part, Scene, Vector3};
use rbx_render::{Camera, LightingState, ViewportRenderer};

fn renderer() -> Option<ViewportRenderer> {
    ViewportRenderer::new(320, 240).ok()
}

/// Uniform-colour surface → textured surface should add per-pixel variance.
fn luma_spread(rgba: &[u8]) -> f32 {
    let n = (rgba.len() / 4).max(1) as f32;
    let lumas: Vec<f32> = rgba
        .chunks_exact(4)
        .map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32)
        .collect();
    let mean = lumas.iter().sum::<f32>() / n;
    (lumas.iter().map(|l| (l - mean).powi(2)).sum::<f32>() / n).sqrt()
}

fn centre_pixel(rgba: &[u8], w: usize, h: usize) -> [u8; 4] {
    let x = w / 2;
    let y = h / 2;
    let i = (y * w + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

#[test]
fn baseplate_has_visible_stud_detail() {
    let Some(mut renderer) = renderer() else { return };
    // A flat plastic slab: with textures off this renders a single flat colour.
    let mut scene = Scene::default();
    scene.parts = vec![Part {
        name: "Slab".to_string(),
        position: Vector3::new(0.0, 0.0, 0.0),
        size: Vector3::new(40.0, 1.0, 40.0),
        color: [0.5, 0.5, 0.5],
        ..Part::default()
    }];
    renderer.upload_scene(&scene);

    let mut camera = Camera::default();
    camera.focus = glam::Vec3::new(0.0, 0.0, 0.0);
    camera.pitch = 1.35; // looking almost straight down at the slab
    camera.distance = 26.0;
    let lighting = LightingState::default();
    let rgba = renderer
        .render_to_rgba(&camera, &lighting)
        .expect("render");

    let spread = luma_spread(&rgba);
    assert!(
        spread > 1.0,
        "expected stud texture detail on the slab, got luminance spread {spread:.3}"
    );
}

#[test]
fn decal_image_reaches_the_surface() {
    let Some(mut renderer) = renderer() else { return };
    // A solid red 8x8 PNG stands in for a Toolbox Image asset.
    let path = std::env::temp_dir().join("rustblox-decal-test.png");
    let mut img = image::RgbaImage::new(8, 8);
    for pixel in img.pixels_mut() {
        *pixel = image::Rgba([220, 20, 20, 255]);
    }
    img.save(&path).expect("write test png");

    let mut part = Part::default();
    part.name = "Image".to_string();
    part.size = Vector3::new(20.0, 0.1, 20.0);
    part.color = [1.0, 1.0, 1.0];
    part.material = "SmoothPlastic".to_string();
    part.decal = Some(path.display().to_string());
    let mut scene = Scene::default();
    scene.parts = vec![part];
    renderer.upload_scene(&scene);

    let mut camera = Camera::default();
    camera.focus = glam::Vec3::new(0.0, 0.0, 0.0);
    camera.pitch = 1.5;
    camera.distance = 24.0;
    let lighting = LightingState::default();
    let rgba = renderer
        .render_to_rgba(&camera, &lighting)
        .expect("render");

    let px = centre_pixel(&rgba, 320, 240);
    assert!(
        px[0] > px[1] + 40 && px[0] > px[2] + 40,
        "expected a red decal at the centre, got {px:?}"
    );
    let _ = std::fs::remove_file(&path);
}
