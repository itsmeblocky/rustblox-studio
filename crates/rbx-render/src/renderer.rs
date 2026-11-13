use crate::camera::Camera;
use crate::culling::{Aabb, Frustum};
use crate::lighting::LightingState;
use crate::mesh::{part_to_mesh, MeshData, Vertex};
use anyhow::{anyhow, Context, Result};
use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use image::{ImageBuffer, Rgba};
use rbx_core::{Part, Scene};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AaMode {
    Off,   // sample_count=1
    Msaa2x,
    Msaa4x,
}

impl AaMode {
    pub fn sample_count(self) -> u32 {
        // WebGPU spec guarantees [1,4] for common color formats; 2 is not portable on all devices/backends.
        // Clamp to safe values to avoid validation panics.
        match self { Self::Off => 1, Self::Msaa2x => 4, Self::Msaa4x => 4 }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "Off" | "1x" | "None" | "AA Off" => Self::Off,
            "2x" | "MSAA2x" | "MSAA 2x" => Self::Msaa2x, // mapped to safe 4x internally
            "4x" | "MSAA4x" | "MSAA 4x" => Self::Msaa4x,
            _ => Self::Off, // default to fastest/most compatible
        }
    }
    pub fn label(self) -> &'static str {
        match self { Self::Off => "AA Off", Self::Msaa2x => "MSAA 4x", Self::Msaa4x => "MSAA 4x" }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LightingUniform {
    sun_dir: [f32; 4],
    ambient: [f32; 4],
    outdoor_ambient: [f32; 4],
    fog_color: [f32; 4],
    fog_params: [f32; 4],
    tech_params: [f32; 4],
    color_shift: [f32; 4],
    color_shift_bottom: [f32; 4],
    atmo: [f32; 4],
    atmo_color: [f32; 4],
    atmo_decay: [f32; 4],
}

struct PreparedPart {
    mesh: MeshData,
    aabb: Aabb,
    center: glam::Vec3,
    radius: f32,
    transparent: bool,
}

pub struct ViewportRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    shader: wgpu::ShaderModule,
    camera_buffer: wgpu::Buffer,
    light_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    textures: crate::textures::TextureBank,
    // MSAA color target (sample_count may be 1)
    msaa_texture: wgpu::Texture,
    msaa_view: wgpu::TextureView,
    // Resolve/single-sample target (readback source); when sample_count==1, render directly here
    resolve_texture: wgpu::Texture,
    resolve_view: wgpu::TextureView,
    // Depth (matches sample_count)
    depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
    readback_buffer: wgpu::Buffer,
    readback_padded_bytes_per_row: u32,
    width: u32,
    height: u32,
    sample_count: u32,
    aa_mode: AaMode,
    max_draw_distance: f32,
    // scene data for per-frame frustum culling
    prepared_parts: Vec<PreparedPart>,
    // reusable frame buffers (grown as needed)
    frame_vertex_buffer: Option<wgpu::Buffer>,
    frame_vertex_capacity: u64,
    frame_index_buffer: Option<wgpu::Buffer>,
    frame_index_capacity: u64,
    frame_index_count: u32,
    // reusable CPU staging
    frame_vertices: Vec<Vertex>,
    frame_indices: Vec<u32>,
}

impl ViewportRenderer {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let width = width.max(1);
        let height = height.max(1);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok_or_else(|| anyhow!("no suitable wgpu adapter"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("rbx-render"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .context("request_device")?;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rbx-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let light_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light"),
            size: std::mem::size_of::<LightingUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let textures = crate::textures::TextureBank::new(&device, &queue);
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("main-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("main-bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: camera_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: light_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&textures.view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&textures.sampler) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&textures.metal_view) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&textures.rough_view) },
            ],
        });

        let aa_mode = AaMode::Off; // default: max compatibility + performance (avoids non-portable sample counts)
        let sample_count = aa_mode.sample_count();
        let pipeline = create_pipeline(&device, &shader, &bind_group_layout, sample_count);

        let (msaa_texture, msaa_view, resolve_texture, resolve_view, depth_texture, depth_view) = create_targets(&device, width, height, sample_count);
        let (readback_buffer, readback_padded_bytes_per_row) = create_readback(&device, width, height);

        Ok(Self {
            device, queue, pipeline, bind_group_layout, shader,
            camera_buffer, light_buffer, bind_group, textures,
            msaa_texture, msaa_view, resolve_texture, resolve_view,
            depth_texture, depth_view,
            readback_buffer, readback_padded_bytes_per_row,
            width, height, sample_count, aa_mode,
            max_draw_distance: 100000.0,
            prepared_parts: Vec::new(),
            frame_vertex_buffer: None, frame_vertex_capacity: 0,
            frame_index_buffer: None, frame_index_capacity: 0, frame_index_count: 0,
            frame_vertices: Vec::new(), frame_indices: Vec::new(),
        })
    }

    pub fn set_aa_mode(&mut self, mode: AaMode) {
        let sc = mode.sample_count();
        if sc == self.sample_count { self.aa_mode = mode; return; }
        self.sample_count = sc;
        self.aa_mode = mode;
        self.pipeline = create_pipeline(&self.device, &self.shader, &self.bind_group_layout, sc);
        let (mt, mv, rt, rv, dt, dv) = create_targets(&self.device, self.width, self.height, sc);
        self.msaa_texture = mt; self.msaa_view = mv;
        self.resolve_texture = rt; self.resolve_view = rv;
        self.depth_texture = dt; self.depth_view = dv;
    }

    pub fn aa_mode(&self) -> AaMode { self.aa_mode }

    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if width == self.width && height == self.height { return; }
        self.width = width; self.height = height;
        let (mt, mv, rt, rv, dt, dv) = create_targets(&self.device, width, height, self.sample_count);
        let (rb, rp) = create_readback(&self.device, width, height);
        self.msaa_texture = mt; self.msaa_view = mv;
        self.resolve_texture = rt; self.resolve_view = rv;
        self.depth_texture = dt; self.depth_view = dv;
        self.readback_buffer = rb; self.readback_padded_bytes_per_row = rp;
    }

    pub fn upload_parts(&mut self, parts: &[Part]) {
        self.prepared_parts.clear();
        self.prepared_parts.reserve(parts.len());
        for part in parts {
            let mut mesh = part_to_mesh(part);
            match part.decal.as_deref() {
                Some(path) => {
                    if let Some(layer) = self.textures.decal_layer(
                        &self.device,
                        &self.queue,
                        std::path::Path::new(path),
                    ) {
                        for v in &mut mesh.vertices {
                            v.layers[1] = layer as f32;
                        }
                    }
                }
                None => {}
            }
            let model = part.cframe.to_mat4() * Mat4::from_scale(part.size.to_glam());
            let aabb = Aabb::from_unit_cube(model);
            let center = (aabb.min + aabb.max) * 0.5;
            let radius = (aabb.max - aabb.min).length() * 0.5;
            let transparent = part.transparency > 0.001 || part.material.eq_ignore_ascii_case("Glass") || part.material.eq_ignore_ascii_case("ForceField");
            self.prepared_parts.push(PreparedPart { mesh, aabb, center, radius, transparent });
        }
        self.frame_index_count = 0;
    }

    pub fn upload_scene(&mut self, scene: &Scene) {
        self.upload_parts(&scene.parts);
    }

    fn ensure_frame_buffers(&mut self, vbytes: u64, ibytes: u64) {
        if self.frame_vertex_buffer.is_none() || self.frame_vertex_capacity < vbytes {
            let cap = (vbytes.max(1024) * 2).next_power_of_two();
            self.frame_vertex_buffer = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("frame-vertices"),
                size: cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.frame_vertex_capacity = cap;
        }
        if self.frame_index_buffer.is_none() || self.frame_index_capacity < ibytes {
            let cap = (ibytes.max(1024) * 2).next_power_of_two();
            self.frame_index_buffer = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("frame-indices"),
                size: cap,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.frame_index_capacity = cap;
        }
    }

    fn build_visible_geometry(&mut self, frustum: &Frustum, cam_pos: glam::Vec3, max_dist: f32) {
        self.frame_vertices.clear();
        self.frame_indices.clear();
        if self.prepared_parts.is_empty() { self.frame_index_count = 0; return; }

        // Collect visible (index, dist) with distance culling + frustum culling
        let mut visible: Vec<(usize, f32, bool)> = Vec::with_capacity(self.prepared_parts.len());
        for (i, pp) in self.prepared_parts.iter().enumerate() {
            let dist = (pp.center - cam_pos).length() - pp.radius;
            if dist > max_dist { continue; } // distance/fog-range culling
            // conservative: very far tiny objects could be screen-size culled (future); keep robust now
            if !frustum.intersects_aabb(&pp.aabb) { continue; } // frustum culling
            visible.push((i, (pp.center - cam_pos).length(), pp.transparent));
        }
        // Sort: opaque front-to-back (early-Z/overdraw reduction), transparent back-to-front
        let mut opaque: Vec<(usize,f32)> = visible.iter().filter(|v| !v.2).map(|v|(v.0,v.1)).collect();
        let mut trans: Vec<(usize,f32)> = visible.iter().filter(|v| v.2).map(|v|(v.0,v.1)).collect();
        opaque.sort_by(|a,b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        trans.sort_by(|a,b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut emit = |i: usize| {
            let pp = &self.prepared_parts[i];
            let base = self.frame_vertices.len() as u32;
            self.frame_vertices.extend_from_slice(&pp.mesh.vertices);
            self.frame_indices.extend(pp.mesh.indices.iter().map(|idx| idx + base));
        };
        for (i,_) in opaque { emit(i); }
        for (i,_) in trans { emit(i); }

        if self.frame_indices.is_empty() {
            self.frame_index_count = 0;
            return;
        }
        let vbytes = (self.frame_vertices.len() * std::mem::size_of::<Vertex>()) as u64;
        let ibytes = (self.frame_indices.len() * std::mem::size_of::<u32>()) as u64;
        self.ensure_frame_buffers(vbytes, ibytes);
        if let Some(vb) = &self.frame_vertex_buffer {
            self.queue.write_buffer(vb, 0, bytemuck::cast_slice(&self.frame_vertices));
        }
        if let Some(ib) = &self.frame_index_buffer {
            self.queue.write_buffer(ib, 0, bytemuck::cast_slice(&self.frame_indices));
        }
        self.frame_index_count = self.frame_indices.len() as u32;
    }

    pub fn render_to_rgba(&mut self, camera: &Camera, lighting: &LightingState) -> Result<Vec<u8>> {
        let aspect = self.width as f32 / self.height as f32;
        let vp = camera.view_proj(aspect);
        let frustum = Frustum::from_view_proj(vp);

        // distance culling bound (fog end if enabled, else far)
        let fog_end = lighting.fog_end;
        let max_dist = if fog_end > lighting.fog_start + 1.0 && fog_end < 90000.0 { (fog_end + 50.0).max(200.0) } else { 100000.0 };
        self.max_draw_distance = max_dist;
        self.build_visible_geometry(&frustum, camera.position(), max_dist);

        let cam_u = CameraUniform {
            view_proj: vp.to_cols_array_2d(),
            camera_pos: [camera.position().x, camera.position().y, camera.position().z, 1.0],
        };
        self.queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&cam_u));

        let has_atmo = if lighting.atmosphere.as_ref().map(|a| a.density > 0.0001).unwrap_or(false) { 1.0 } else { 0.0 };
        let (ad, ao, ag, ah) = lighting.atmosphere.as_ref().map(|a| (a.density, a.offset, a.glare, a.haze)).unwrap_or((0.0,0.0,0.0,0.0));
        let ac = lighting.atmosphere.as_ref().map(|a| a.color).unwrap_or([0.78, 0.78, 0.78]);
        let ady = lighting.atmosphere.as_ref().map(|a| a.decay).unwrap_or([0.416, 0.439, 0.49]);
        let light_u = LightingUniform {
            sun_dir: [lighting.sun_direction.x, lighting.sun_direction.y, lighting.sun_direction.z, lighting.brightness],
            ambient: [lighting.ambient.x, lighting.ambient.y, lighting.ambient.z, lighting.exposure_compensation],
            outdoor_ambient: [lighting.outdoor_ambient.x, lighting.outdoor_ambient.y, lighting.outdoor_ambient.z, lighting.environment_diffuse_scale],
            fog_color: [lighting.fog_color.x, lighting.fog_color.y, lighting.fog_color.z, lighting.fog_start],
            fog_params: [lighting.fog_end, lighting.environment_specular_scale, lighting.shadow_softness, if lighting.global_shadows {1.0} else {0.0}],
            tech_params: [lighting.technology.as_u32() as f32, lighting.clock_time, lighting.geographic_latitude, has_atmo],
            color_shift: [lighting.color_shift_top.x, lighting.color_shift_top.y, lighting.color_shift_top.z, 0.0],
            color_shift_bottom: [lighting.color_shift_bottom.x, lighting.color_shift_bottom.y, lighting.color_shift_bottom.z, 0.0],
            atmo: [ad, ao, ag, ah],
            atmo_color: [ac[0], ac[1], ac[2], 0.0],
            atmo_decay: [ady[0], ady[1], ady[2], 0.0],
        };
        self.queue.write_buffer(&self.light_buffer, 0, bytemuck::bytes_of(&light_u));

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("render") });
        {
            let (color_view, resolve_target) = if self.sample_count > 1 {
                (&self.msaa_view, Some(&self.resolve_view))
            } else {
                (&self.resolve_view, None)
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.11, g: 0.115, b: 0.125, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            if let (Some(vb), Some(ib)) = (&self.frame_vertex_buffer, &self.frame_index_buffer) {
                if self.frame_index_count > 0 {
                    pass.set_vertex_buffer(0, vb.slice(..));
                    pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..self.frame_index_count, 0, 0..1);
                }
            }
        }

        let bpp = 4u32;
        let unpadded = bpp * self.width;
        let padded = self.readback_padded_bytes_per_row;
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture { texture: &self.resolve_texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::ImageCopyBuffer { buffer: &self.readback_buffer, layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(self.height) } },
            wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(encoder.finish()));
        let slice = self.readback_buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv().context("map recv")?.context("map buffer")?;
        let data = slice.get_mapped_range();
        let mut rgba = vec![0u8; (unpadded * self.height) as usize];
        for row in 0..self.height as usize {
            let src = row * padded as usize;
            let dst = row * unpadded as usize;
            rgba[dst..dst + unpadded as usize].copy_from_slice(&data[src..src + unpadded as usize]);
        }
        drop(data);
        self.readback_buffer.unmap();
        Ok(rgba)
    }

    pub fn render_to_png(&mut self, camera: &Camera, lighting: &LightingState, path: &Path) -> Result<()> {
        let rgba = self.render_to_rgba(camera, lighting)?;
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        let img: ImageBuffer<Rgba<u8>, _> = ImageBuffer::from_raw(self.width, self.height, rgba)
            .ok_or_else(|| anyhow!("image buffer"))?;
        img.save(path).context("save png")?;
        Ok(())
    }
}

fn create_pipeline(device: &wgpu::Device, shader: &wgpu::ShaderModule, bind_group_layout: &wgpu::BindGroupLayout, sample_count: u32) -> wgpu::RenderPipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-layout"),
        bind_group_layouts: &[bind_group_layout],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("main-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: "vs_main",
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute { offset: 0, shader_location: 0, format: wgpu::VertexFormat::Float32x3 },
                    wgpu::VertexAttribute { offset: 12, shader_location: 1, format: wgpu::VertexFormat::Float32x3 },
                    wgpu::VertexAttribute { offset: 24, shader_location: 2, format: wgpu::VertexFormat::Float32x4 },
                    wgpu::VertexAttribute { offset: 40, shader_location: 3, format: wgpu::VertexFormat::Float32x4 },
                    wgpu::VertexAttribute { offset: 56, shader_location: 4, format: wgpu::VertexFormat::Float32x2 },
                    wgpu::VertexAttribute { offset: 64, shader_location: 5, format: wgpu::VertexFormat::Float32x2 },
                ],
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: sample_count.max(1),
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview: None,
        cache: None,
    })
}

fn create_readback(device: &wgpu::Device, width: u32, height: u32) -> (wgpu::Buffer, u32) {
    let bpp = 4u32;
    let unpadded = bpp * width;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = ((unpadded + align - 1) / align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    (readback, padded)
}

fn create_targets(device: &wgpu::Device, width: u32, height: u32, sample_count: u32) -> (wgpu::Texture, wgpu::TextureView, wgpu::Texture, wgpu::TextureView, wgpu::Texture, wgpu::TextureView) {
    let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
    let sc = sample_count.max(1);
    // Always create resolve (single-sample, readback)
    let resolve_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("resolve-color"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let resolve_view = resolve_texture.create_view(&Default::default());
    // MSAA color (when sc==1, create 1-sample; render path uses resolve_view directly)
    let msaa_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("msaa-color"),
        size,
        mip_level_count: 1,
        sample_count: sc,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let msaa_view = msaa_texture.create_view(&Default::default());
    let depth_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size,
        mip_level_count: 1,
        sample_count: sc,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth_texture.create_view(&Default::default());
    (msaa_texture, msaa_view, resolve_texture, resolve_view, depth_texture, depth_view)
}
