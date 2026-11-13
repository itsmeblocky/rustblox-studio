//! Shared scene pass: swapchain-independent pipeline, geometry upload and draw.
//!
//! Both live viewport front-ends use this:
//! - `native_viewport`: a detached winit window with its own surface,
//! - `embedded`: a `WS_CHILD` window inside the Qt shell's window.
//!
//! `renderer::ViewportRenderer` keeps its own (AA-capable) headless path, because
//! it renders to an offscreen texture and reads the pixels back.

use crate::camera::Camera;
use crate::culling::{Aabb, Frustum};
use crate::lighting::LightingState;
use crate::mesh::{part_to_mesh, MeshData, Vertex};
use crate::tools::{self, ToolMode};
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use rbx_core::Scene;

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
    transparent: bool,
    /// Cached image this part was inserted with (Toolbox Image/Decal).
    decal: Option<std::path::PathBuf>,
    /// Roblox material name (for real texture resolution).
    material: String,
}

/// Owns everything needed to draw a [`Scene`] into a colour target.
pub struct ScenePass {
    pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    light_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Material pattern layers + dynamic decal slots.
    textures: crate::textures::TextureBank,
    depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
    depth_size: (u32, u32),
    prepared: Vec<PreparedPart>,
    vbuf: Option<wgpu::Buffer>,
    vcap: u64,
    ibuf: Option<wgpu::Buffer>,
    icap: u64,
    icount: u32,
    verts: Vec<Vertex>,
    idxs: Vec<u32>,
    sky_key: Option<String>,
    sky_clear: wgpu::Color,
    /// Optional path to cached material color textures (`assets/cache/textures`).
    material_textures_dir: Option<std::path::PathBuf>,
    selected: Option<usize>,
    tool: ToolMode,
}

impl ScenePass {
    /// Builds the pipeline. `format` must be the colour target's format.
    /// Single-sampled targets only (no MSAA) — both viewport surfaces are 1x.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
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
        let textures = crate::textures::TextureBank::new(device, queue);
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
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
            label: Some("bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: light_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&textures.view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&textures.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&textures.metal_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&textures.rough_view),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            offset: 12,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            offset: 24,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                        wgpu::VertexAttribute {
                            offset: 40,
                            shader_location: 3,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                        wgpu::VertexAttribute {
                            offset: 56,
                            shader_location: 4,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 64,
                            shader_location: 5,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
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
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
            cache: None,
        });

        let (depth_texture, depth_view) = make_depth(device, width, height);
        Self {
            pipeline,
            camera_buffer,
            light_buffer,
            bind_group,
            textures,
            depth_texture,
            depth_view,
            depth_size: (width.max(1), height.max(1)),
            prepared: Vec::new(),
            vbuf: None,
            vcap: 0,
            ibuf: None,
            icap: 0,
            icount: 0,
            verts: Vec::new(),
            idxs: Vec::new(),
            sky_key: None,
            sky_clear: wgpu::Color {
                r: 0.11,
                g: 0.115,
                b: 0.125,
                a: 1.0,
            },
            material_textures_dir: None,
            selected: None,
            tool: ToolMode::Select,
        }
    }

    /// Sets the directory containing cached Roblox material textures (keyed by asset id).
    pub fn set_material_textures_dir(&mut self, dir: Option<std::path::PathBuf>) {
        self.material_textures_dir = dir;
    }

    /// Recreates the depth attachment when the target size changes.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let size = (width.max(1), height.max(1));
        if self.depth_size == size {
            return;
        }
        let (texture, view) = make_depth(device, size.0, size.1);
        self.depth_texture = texture;
        self.depth_view = view;
        self.depth_size = size;
    }

    /// Rebuilds the CPU-side meshes. Call once per scene edit, not per frame.
    pub fn set_scene(&mut self, scene: &Scene) {
        self.prepared.clear();
        for part in &scene.parts {
            let mesh = part_to_mesh(part);
            let model = part.cframe.to_mat4() * Mat4::from_scale(part.size.to_glam());
            let aabb = Aabb::from_unit_cube(model);
            let center = (aabb.min + aabb.max) * 0.5;
            let transparent = part.transparency > 0.001;
            self.prepared.push(PreparedPart {
                mesh,
                aabb,
                center,
                transparent,
                decal: part.decal.as_ref().map(std::path::PathBuf::from),
                material: part.material.clone(),
            });
        }
    }

    pub fn is_empty(&self) -> bool {
        self.prepared.is_empty()
    }

    pub fn set_tool_overlay(&mut self, selected: Option<usize>, tool: ToolMode) {
        self.selected = selected;
        self.tool = tool;
    }

    fn append_gizmo(&mut self, camera: &Camera) {
        let Some(sel) = self.selected else { return; };
        let Some(pp) = self.prepared.get(sel) else { return; };
        let center = pp.center;
        let aabb_min = pp.aabb.min;
        let aabb_max = pp.aabb.max;
        let cam_pos = camera.position();
        let dist = (center - cam_pos).length().max(1.0);
        let len = (dist * 0.18).clamp(1.5, 12.0);
        let thick = (len * 0.03).max(0.06);
        let axes = [
            (Vec3::X, [0.95, 0.2, 0.2, 1.0]),
            (Vec3::Y, [0.2, 0.9, 0.2, 1.0]),
            (Vec3::Z, [0.25, 0.45, 1.0, 1.0]),
        ];
        for (dir, col) in axes {
            match self.tool {
                ToolMode::Move | ToolMode::Transform => {
                    self.append_box(center + dir * (len * 0.5), Vec3::new(
                        if dir.x.abs() > 0.5 { len } else { thick },
                        if dir.y.abs() > 0.5 { len } else { thick },
                        if dir.z.abs() > 0.5 { len } else { thick },
                    ), col);
                }
                ToolMode::Scale => {
                    self.append_box(center + dir * (len * 0.5), Vec3::new(
                        if dir.x.abs() > 0.5 { len } else { thick },
                        if dir.y.abs() > 0.5 { len } else { thick },
                        if dir.z.abs() > 0.5 { len } else { thick },
                    ), col);
                    self.append_box(center + dir * len, Vec3::splat(thick * 4.0), col);
                }
                ToolMode::Rotate => {
                    self.append_ring(center, dir, len * 0.8, thick * 1.5, col);
                }
                ToolMode::Select => {
                    // selection highlight box below
                }
            }
        }
        if self.tool == ToolMode::Select {
            let size = aabb_max - aabb_min;
            self.append_wire_box(center, size * 1.02, [0.2, 0.6, 1.0, 0.9]);
        }
    }

    fn append_box(&mut self, center: Vec3, size: Vec3, color: [f32; 4]) {
        let base = crate::mesh::box_mesh();
        let model = Mat4::from_translation(center) * Mat4::from_scale(size);
        let normal_mat = glam::Mat3::from_mat4(model).inverse().transpose();
        let base_idx = self.verts.len() as u32;
        for v in &base.vertices {
            let lp = Vec3::from_array(v.position);
            let ln = Vec3::from_array(v.normal);
            let p = model.transform_point3(lp);
            let n = (normal_mat * ln).normalize_or_zero();
            self.verts.push(Vertex {
                position: p.to_array(),
                normal: n.to_array(),
                color,
                // neon-ish so it stays visible
                material: [0.0, 0.3, 0.0, 1.0],
                uv: [0.0, 0.0],
                layers: [0.0, -1.0],
            });
        }
        self.idxs.extend(base.indices.iter().map(|i| i + base_idx));
    }

    fn append_ring(&mut self, center: Vec3, axis: Vec3, radius: f32, thick: f32, color: [f32; 4]) {
        let segs = 32;
        let (u, w) = if axis.y.abs() > 0.9 {
            (Vec3::X, Vec3::Z)
        } else {
            let u = axis.cross(Vec3::Y).normalize_or_zero();
            let wv = axis.cross(u).normalize_or_zero();
            (u, wv)
        };
        for i in 0..segs {
            let a0 = (i as f32 / segs as f32) * std::f32::consts::TAU;
            let a1 = ((i + 1) as f32 / segs as f32) * std::f32::consts::TAU;
            let p0 = center + (u * a0.cos() + w * a0.sin()) * radius;
            let p1 = center + (u * a1.cos() + w * a1.sin()) * radius;
            let mid = (p0 + p1) * 0.5;
            let seg_len = (p1 - p0).length().max(0.001);
            let _along = (p1 - p0).normalize_or_zero();
            // approximate ring segment with a box
            let size = if axis.x.abs() > 0.5 {
                Vec3::new(thick, thick.max(seg_len * 0.6), thick.max(seg_len * 0.6))
            } else if axis.y.abs() > 0.5 {
                Vec3::new(thick.max(seg_len * 0.6), thick, thick.max(seg_len * 0.6))
            } else {
                Vec3::new(thick.max(seg_len * 0.6), thick.max(seg_len * 0.6), thick)
            };
            self.append_box(mid, size, color);
        }
    }

    fn append_wire_box(&mut self, center: Vec3, size: Vec3, color: [f32; 4]) {
        let t = (size.x.min(size.y).min(size.z) * 0.03).clamp(0.03, 0.12);
        let hx = size.x * 0.5; let hy = size.y * 0.5; let hz = size.z * 0.5;
        let edges = [
            (Vec3::new(-hx, -hy, -hz), Vec3::new(hx, -hy, -hz)),
            (Vec3::new(-hx, hy, -hz), Vec3::new(hx, hy, -hz)),
            (Vec3::new(-hx, -hy, hz), Vec3::new(hx, -hy, hz)),
            (Vec3::new(-hx, hy, hz), Vec3::new(hx, hy, hz)),
            (Vec3::new(-hx, -hy, -hz), Vec3::new(-hx, hy, -hz)),
            (Vec3::new(hx, -hy, -hz), Vec3::new(hx, hy, -hz)),
            (Vec3::new(-hx, -hy, hz), Vec3::new(-hx, hy, hz)),
            (Vec3::new(hx, -hy, hz), Vec3::new(hx, hy, hz)),
            (Vec3::new(-hx, -hy, -hz), Vec3::new(-hx, -hy, hz)),
            (Vec3::new(hx, -hy, -hz), Vec3::new(hx, -hy, hz)),
            (Vec3::new(-hx, hy, -hz), Vec3::new(-hx, hy, hz)),
            (Vec3::new(hx, hy, -hz), Vec3::new(hx, hy, hz)),
        ];
        for (a,b) in edges {
            let mid = center + (a + b) * 0.5;
            let d = b - a;
            let sx = if d.x.abs() > 0.001 { d.x.abs() } else { t };
            let sy = if d.y.abs() > 0.001 { d.y.abs() } else { t };
            let sz = if d.z.abs() > 0.001 { d.z.abs() } else { t };
            self.append_box(mid, Vec3::new(sx, sy, sz), color);
        }
    }

    /// Records and submits one frame into `target`.
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        camera: &Camera,
        lighting: &LightingState,
    ) {
        let aspect = self.depth_size.0 as f32 / self.depth_size.1.max(1) as f32;
        let vp = camera.view_proj(aspect);
        let frustum = Frustum::from_view_proj(vp);

        // Collect visible parts: distance culled, then frustum culled.
        self.verts.clear();
        self.idxs.clear();
        let cam_pos = camera.position();
        let fog_end = lighting.fog_end;
        let max_dist = if fog_end > lighting.fog_start + 1.0 && fog_end < 90000.0 {
            fog_end + 50.0
        } else {
            100000.0
        };
        let mut visible: Vec<(usize, f32, bool)> = Vec::new();
        for (i, pp) in self.prepared.iter().enumerate() {
            let dist = (pp.center - cam_pos).length();
            if dist - (pp.aabb.max - pp.aabb.min).length() * 0.5 > max_dist {
                continue;
            }
            if !frustum.intersects_aabb(&pp.aabb) {
                continue;
            }
            visible.push((i, dist, pp.transparent));
        }
        let mut opaque: Vec<_> = visible.iter().filter(|v| !v.2).copied().collect();
        let mut trans: Vec<_> = visible.iter().filter(|v| v.2).copied().collect();
        opaque.sort_by(|a, b| a.1.total_cmp(&b.1));
        trans.sort_by(|a, b| b.1.total_cmp(&a.1));

        // Try to load cached real material textures for materials in use.
        if let Some(dir) = self.material_textures_dir.clone() {
            let mut seen = std::collections::HashSet::new();
            for pp in &self.prepared {
                if seen.insert(pp.material.clone()) {
                    self.textures
                        .ensure_material_texture(queue, &pp.material, Some(dir.as_path()));
                }
            }
        }

        // Resolve decal images to texture-array layers (uploads on first use).
        let mut decal_layers = Vec::with_capacity(self.prepared.len());
        for i in 0..self.prepared.len() {
            let path = self.prepared[i].decal.clone();
            let layer = match path {
                Some(path) => self.textures.decal_layer(device, queue, &path),
                None => None,
            };
            decal_layers.push(layer);
        }

        for (i, _, _) in opaque.into_iter().chain(trans.into_iter()) {
            let pp = &self.prepared[i];
            let base = self.verts.len() as u32;
            self.verts.extend_from_slice(&pp.mesh.vertices);
            if let Some(layer) = decal_layers[i] {
                for v in &mut self.verts[base as usize..] {
                    v.layers[1] = layer as f32;
                }
            }
            self.idxs.extend(pp.mesh.indices.iter().map(|x| x + base));
        }

        // Draw gizmo/selection overlay for the active tool.
        self.append_gizmo(camera);

        if !self.idxs.is_empty() {
            let vb = (self.verts.len() * std::mem::size_of::<Vertex>()) as u64;
            let ib = (self.idxs.len() * std::mem::size_of::<u32>()) as u64;
            if self.vbuf.is_none() || self.vcap < vb {
                let cap = (vb * 2).next_power_of_two();
                self.vbuf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("verts"),
                    size: cap,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.vcap = cap;
            }
            if self.ibuf.is_none() || self.icap < ib {
                let cap = (ib * 2).next_power_of_two();
                self.ibuf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("indices"),
                    size: cap,
                    usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.icap = cap;
            }
            if let (Some(vbuf), Some(ibuf)) = (&self.vbuf, &self.ibuf) {
                queue.write_buffer(vbuf, 0, bytemuck::cast_slice(&self.verts));
                queue.write_buffer(ibuf, 0, bytemuck::cast_slice(&self.idxs));
            }
            self.icount = self.idxs.len() as u32;
        } else {
            self.icount = 0;
        }

        let cam_u = CameraUniform {
            view_proj: vp.to_cols_array_2d(),
            camera_pos: [cam_pos.x, cam_pos.y, cam_pos.z, 1.0],
        };
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&cam_u));

        let has_atmo = if lighting
            .atmosphere
            .as_ref()
            .map(|a| a.density > 0.0001)
            .unwrap_or(false)
        {
            1.0
        } else {
            0.0
        };
        let (ad, ao, ag, ah) = lighting
            .atmosphere
            .as_ref()
            .map(|a| (a.density, a.offset, a.glare, a.haze))
            .unwrap_or((0.0, 0.0, 0.0, 0.0));
        let ac = lighting
            .atmosphere
            .as_ref()
            .map(|a| a.color)
            .unwrap_or([0.78, 0.78, 0.78]);
        let ady = lighting
            .atmosphere
            .as_ref()
            .map(|a| a.decay)
            .unwrap_or([0.416, 0.439, 0.49]);
        let light_u = LightingUniform {
            sun_dir: [
                lighting.sun_direction.x,
                lighting.sun_direction.y,
                lighting.sun_direction.z,
                lighting.brightness,
            ],
            ambient: [
                lighting.ambient.x,
                lighting.ambient.y,
                lighting.ambient.z,
                lighting.exposure_compensation,
            ],
            outdoor_ambient: [
                lighting.outdoor_ambient.x,
                lighting.outdoor_ambient.y,
                lighting.outdoor_ambient.z,
                lighting.environment_diffuse_scale,
            ],
            fog_color: [
                lighting.fog_color.x,
                lighting.fog_color.y,
                lighting.fog_color.z,
                lighting.fog_start,
            ],
            fog_params: [
                lighting.fog_end,
                lighting.environment_specular_scale,
                lighting.shadow_softness,
                if lighting.global_shadows { 1.0 } else { 0.0 },
            ],
            tech_params: [
                lighting.technology.as_u32() as f32,
                lighting.clock_time,
                lighting.geographic_latitude,
                has_atmo,
            ],
            color_shift: [
                lighting.color_shift_top.x,
                lighting.color_shift_top.y,
                lighting.color_shift_top.z,
                0.0,
            ],
            color_shift_bottom: [
                lighting.color_shift_bottom.x,
                lighting.color_shift_bottom.y,
                lighting.color_shift_bottom.z,
                0.0,
            ],
            atmo: [ad, ao, ag, ah],
            atmo_color: [ac[0], ac[1], ac[2], 0.0],
            atmo_decay: [ady[0], ady[1], ady[2], 0.0],
        };
        queue.write_buffer(&self.light_buffer, 0, bytemuck::bytes_of(&light_u));

        self.update_sky_clear(lighting);

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("scene-enc"),
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.sky_clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            if let (Some(vb), Some(ib)) = (&self.vbuf, &self.ibuf) {
                if self.icount > 0 {
                    pass.set_vertex_buffer(0, vb.slice(..));
                    pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..self.icount, 0, 0..1);
                }
            }
        }
        queue.submit(Some(enc.finish()));
    }
}

impl ScenePass {
    fn update_sky_clear(&mut self, lighting: &LightingState) {
        let Some(sky) = lighting.sky.as_ref() else {
            self.sky_key = None;
            self.sky_clear = wgpu::Color { r: 0.11, g: 0.115, b: 0.125, a: 1.0 };
            return;
        };
        let key = format!(
            "{}|{}|{}|{}|{}|{}",
            sky.skybox_up_file.as_deref().unwrap_or(""),
            sky.skybox_ft_file.as_deref().unwrap_or(""),
            sky.skybox_bk_file.as_deref().unwrap_or(""),
            sky.skybox_lf_file.as_deref().unwrap_or(""),
            sky.skybox_rt_file.as_deref().unwrap_or(""),
            sky.skybox_dn_file.as_deref().unwrap_or(""),
        );
        if self.sky_key.as_deref() == Some(&key) {
            return;
        }
        self.sky_key = Some(key);
        // Prefer resolved face images; fall back to a sky-like gradient tint.
        let faces = [
            sky.skybox_up_file.as_deref(),
            sky.skybox_ft_file.as_deref(),
            sky.skybox_bk_file.as_deref(),
            sky.skybox_lf_file.as_deref(),
            sky.skybox_rt_file.as_deref(),
            sky.skybox_dn_file.as_deref(),
        ];
        let mut sum = [0.0f32; 3];
        let mut count = 0.0f32;
        for path in faces.into_iter().flatten() {
            if let Some(avg) = average_image_color(path) {
                sum[0] += avg[0];
                sum[1] += avg[1];
                sum[2] += avg[2];
                count += 1.0;
            }
        }
        if count > 0.0 {
            let inv = 1.0 / count;
            // Brighten slightly so sky reads as light source/background.
            self.sky_clear = wgpu::Color {
                r: (sum[0] * inv * 1.15).clamp(0.0, 1.0) as f64,
                g: (sum[1] * inv * 1.15).clamp(0.0, 1.0) as f64,
                b: (sum[2] * inv * 1.15).clamp(0.0, 1.0) as f64,
                a: 1.0,
            };
        } else {
            // No resolved textures yet: use a pleasant procedural sky tint.
            self.sky_clear = wgpu::Color { r: 0.36, g: 0.58, b: 0.86, a: 1.0 };
        }
    }
}

fn average_image_color(path: &str) -> Option<[f32; 3]> {
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    // Sample a sparse grid for speed.
    let step = (w.max(h) / 32).max(1);
    let mut sum = [0u64; 3];
    let mut n = 0u64;
    let mut y = 0u32;
    while y < h {
        let mut x = 0u32;
        while x < w {
            let p = img.get_pixel(x, y).0;
            if p[3] > 8 {
                sum[0] += p[0] as u64;
                sum[1] += p[1] as u64;
                sum[2] += p[2] as u64;
                n += 1;
            }
            x += step;
        }
        y += step;
    }
    if n == 0 {
        return None;
    }
    let inv = 1.0 / (n as f32 * 255.0);
    Some([sum[0] as f32 * inv, sum[1] as f32 * inv, sum[2] as f32 * inv])
}

fn make_depth(device: &wgpu::Device, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let t = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let v = t.create_view(&Default::default());
    (t, v)
}
