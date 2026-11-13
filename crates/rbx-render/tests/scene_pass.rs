//! Exercises the shared `ScenePass` (the embedded/native viewport draw path)
//! headlessly: pipeline + texture-array bind group + draw + readback.

use rbx_core::{Part, Scene, Vector3};
use rbx_render::camera::Camera;
use rbx_render::lighting::LightingState;
use rbx_render::scene_pass::ScenePass;

const W: u32 = 320;
const H: u32 = 240;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))?;
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("scene-pass-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    ))
    .ok()?;
    Some(Gpu { device, queue })
}

fn render(pass: &mut ScenePass, gpu: &Gpu, camera: &Camera) -> Vec<u8> {
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());

    let bytes_per_row = W * 4; // 1280 is already 256-aligned
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (bytes_per_row * H) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let lighting = LightingState::default();
    pass.draw(&gpu.device, &gpu.queue, &view, camera, &lighting);

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &readback,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range().to_vec();
    readback.unmap();
    data
}

fn top_down(camera: &mut Camera) {
    camera.focus = glam::Vec3::ZERO;
    camera.pitch = 1.5;
    camera.distance = 24.0;
}

#[test]
fn scene_pass_draws_textured_and_decal_geometry() {
    let Some(gpu) = gpu() else { return };

    // Red decal through the same code path Toolbox image inserts use.
    let path = std::env::temp_dir().join("rustblox-scene-pass-decal.png");
    let mut img = image::RgbaImage::new(8, 8);
    for pixel in img.pixels_mut() {
        *pixel = image::Rgba([230, 20, 20, 255]);
    }
    img.save(&path).expect("write test png");

    let mut slab = Part::default();
    slab.name = "Slab".to_string();
    slab.size = Vector3::new(40.0, 1.0, 40.0);
    slab.color = [0.5, 0.5, 0.5];
    slab.material = "Plastic".to_string(); // studded pattern

    let mut decal_part = Part::default();
    decal_part.name = "Image".to_string();
    decal_part.position = Vector3::new(0.0, 1.0, 0.0);
    decal_part.cframe = rbx_core::CFrameValue::from_position(Vector3::new(0.0, 1.0, 0.0));
    decal_part.size = Vector3::new(10.0, 0.1, 10.0);
    decal_part.color = [1.0, 1.0, 1.0];
    decal_part.cast_shadow = false;
    decal_part.decal = Some(path.display().to_string());

    let mut scene = Scene::default();
    scene.parts = vec![slab, decal_part];

    let mut pass = ScenePass::new(&gpu.device, &gpu.queue, wgpu::TextureFormat::Rgba8UnormSrgb, W, H);
    pass.set_scene(&scene);

    let mut camera = Camera::default();
    top_down(&mut camera);
    let rgba = render(&mut pass, &gpu, &camera);

    // Centre of the frame: the decal quad, which must read red.
    let i = ((H as usize / 2) * W as usize + W as usize / 2) * 4;
    let px = [rgba[i] as i32, rgba[i + 1] as i32, rgba[i + 2] as i32];
    assert!(px[0] > px[1] + 30 && px[0] > px[2] + 30, "centre pixel {px:?}");

    // A window on the slab away from the decal: studs must modulate the colour.
    let mut lumas = Vec::new();
    for y in 90..150usize {
        for x in 30..110usize {
            let i = (y * W as usize + x) * 4;
            lumas.push(
                0.2126 * rgba[i] as f32 + 0.7152 * rgba[i + 1] as f32 + 0.0722 * rgba[i + 2] as f32,
            );
        }
    }
    let mean = lumas.iter().sum::<f32>() / lumas.len() as f32;
    let spread = (lumas.iter().map(|l| (l - mean).powi(2)).sum::<f32>() / lumas.len() as f32).sqrt();
    assert!(spread > 3.0, "slab luminance spread {spread:.3}");

    let _ = std::fs::remove_file(&path);
}
