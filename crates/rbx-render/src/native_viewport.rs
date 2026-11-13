//! Native wgpu swapchain viewport window (no CPU readback) - separate thread + winit.
//! Fixes the Qt/QML image-bridge lag by rendering directly to a surface.
//!
//! The drawing itself lives in [`crate::scene_pass::ScenePass`], which the
//! Qt-embedded viewport (`embedded`) shares.

use crate::camera::Camera;
use crate::lighting::LightingState;
use crate::scene_pass::ScenePass;
use anyhow::{anyhow, Context, Result};
use rbx_core::Scene;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId};

#[cfg(target_os = "windows")]
use winit::platform::windows::EventLoopBuilderExtWindows;

#[derive(Clone)]
pub struct ViewportSharedState {
    pub camera: Camera,
    pub lighting: LightingState,
    pub scene: Scene,
    pub scene_dirty: bool,
    pub close: bool,
    pub title_extra: String,
}

impl Default for ViewportSharedState {
    fn default() -> Self {
        Self {
            camera: Camera::default(),
            lighting: LightingState::default(),
            scene: Scene::default(),
            scene_dirty: true,
            close: false,
            title_extra: String::new(),
        }
    }
}

pub struct NativeViewportHandle {
    pub shared: Arc<Mutex<ViewportSharedState>>,
}

impl NativeViewportHandle {
    pub fn set_lighting(&self, lighting: LightingState) {
        if let Ok(mut s) = self.shared.lock() {
            s.lighting = lighting;
        }
    }
    pub fn set_camera(&self, camera: Camera) {
        if let Ok(mut s) = self.shared.lock() {
            s.camera = camera;
        }
    }
    pub fn set_scene(&self, scene: Scene) {
        if let Ok(mut s) = self.shared.lock() {
            s.scene = scene;
            s.scene_dirty = true;
        }
    }
    pub fn close(&self) {
        if let Ok(mut s) = self.shared.lock() {
            s.close = true;
        }
    }
}

/// Spawn a native viewport window on a dedicated thread. Returns a handle to control it.
pub fn spawn_native_viewport(initial: ViewportSharedState) -> Result<NativeViewportHandle> {
    let shared = Arc::new(Mutex::new(initial));
    let shared_thread = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("rbx-native-viewport".to_string())
        .spawn(move || {
            if let Err(e) = run_viewport(shared_thread) {
                eprintln!("[native viewport] error: {e:#}");
            }
        })
        .context("spawn viewport thread")?;
    Ok(NativeViewportHandle { shared })
}

struct GpuState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pass: ScenePass,
    scene_uploaded: bool,
}

struct App {
    shared: Arc<Mutex<ViewportSharedState>>,
    gpu: Option<GpuState>,
    mouse_pos: (f64, f64),
    lmb: bool,
    rmb: bool,
    mmb: bool,
    shift: bool,
    // WASD/QE flight: [forward, back, left, right, down, up]
    keys: [bool; 6],
    last_tick: Instant,
}

impl App {
    fn new(shared: Arc<Mutex<ViewportSharedState>>) -> Self {
        Self {
            shared,
            gpu: None,
            mouse_pos: (0.0, 0.0),
            lmb: false,
            rmb: false,
            mmb: false,
            shift: false,
            keys: [false; 6],
            last_tick: Instant::now(),
        }
    }

    /// Applies WASD/QE flight for the elapsed time. Movement is gated on the
    /// right mouse button (Studio behaviour) so typing elsewhere never drives
    /// the camera. Speed ramps up while a key is held and resets on release.
    fn tick_flight(&mut self) {
        let dt = self.last_tick.elapsed().as_secs_f32().min(0.1);
        self.last_tick = Instant::now();
        if !self.rmb {
            if let Ok(mut s) = self.shared.lock() {
                s.camera.reset_fly();
            }
            return;
        }
        let k = self.keys;
        let forward = k[0] as i32 as f32 - k[1] as i32 as f32;
        let strafe = k[3] as i32 as f32 - k[2] as i32 as f32;
        let rise = k[5] as i32 as f32 - k[4] as i32 as f32;
        let mult = if self.shift { 4.0 } else { 1.0 };
        if let Ok(mut s) = self.shared.lock() {
            s.camera.fly_input(forward, strafe, rise, mult, dt);
        }
    }

    fn ensure_gpu(&mut self, window: Arc<Window>) {
        if self.gpu.is_some() {
            return;
        }
        let size = window.inner_size();
        let size = PhysicalSize {
            width: size.width.max(1),
            height: size.height.max(1),
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            }))
            .expect("adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("rbx-native"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .expect("device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::AutoNoVsync) {
            wgpu::PresentMode::AutoNoVsync
        } else if caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
            wgpu::PresentMode::Fifo
        } else {
            caps.present_modes[0]
        };
        let alpha_mode = caps.alpha_modes[0];
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width,
            height: size.height,
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        let pass = ScenePass::new(&device, &queue, format, config.width, config.height);

        self.gpu = Some(GpuState {
            window,
            surface,
            device,
            queue,
            config,
            pass,
            scene_uploaded: false,
        });
    }

    fn upload_scene_if_needed(&mut self) {
        let Some(gpu) = &mut self.gpu else { return };
        let (scene, dirty) = {
            let mut s = self.shared.lock().unwrap();
            let d = s.scene_dirty;
            if d {
                s.scene_dirty = false;
            }
            (s.scene.clone(), d)
        };
        if !dirty && gpu.scene_uploaded {
            return;
        }
        gpu.pass.set_scene(&scene);
        gpu.scene_uploaded = true;
    }

    fn render(&mut self) {
        self.upload_scene_if_needed();
        let (camera, lighting, close) = {
            let s = self.shared.lock().unwrap();
            (s.camera.clone(), s.lighting.clone(), s.close)
        };
        let Some(gpu) = &mut self.gpu else { return };
        if close {
            gpu.window.set_visible(false);
            return;
        }
        let output = match gpu.surface.get_current_texture() {
            Ok(t) => t,
            Err(wgpu::SurfaceError::Lost) | Err(wgpu::SurfaceError::Outdated) => {
                let size = gpu.window.inner_size();
                gpu.config.width = size.width.max(1);
                gpu.config.height = size.height.max(1);
                gpu.surface.configure(&gpu.device, &gpu.config);
                gpu.pass
                    .resize(&gpu.device, gpu.config.width, gpu.config.height);
                return;
            }
            Err(wgpu::SurfaceError::OutOfMemory) => return,
            Err(wgpu::SurfaceError::Timeout) => return,
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        gpu.pass
            .resize(&gpu.device, gpu.config.width, gpu.config.height);
        gpu.pass
            .draw(&gpu.device, &gpu.queue, &view, &camera, &lighting);
        output.present();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_none() {
            let attrs = WindowAttributes::default()
                .with_title("Rustblox Studio - 3D Viewport (native/wgpu)")
                .with_inner_size(PhysicalSize::new(1280u32, 720u32));
            let window = Arc::new(event_loop.create_window(attrs).expect("window"));
            self.ensure_gpu(window);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    if size.width > 0 && size.height > 0 {
                        gpu.config.width = size.width;
                        gpu.config.height = size.height;
                        gpu.surface.configure(&gpu.device, &gpu.config);
                        gpu.pass.resize(&gpu.device, size.width, size.height);
                    }
                    gpu.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                self.render();
                if let Some(gpu) = &self.gpu {
                    gpu.window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let p = state == ElementState::Pressed;
                match button {
                    MouseButton::Left => self.lmb = p,
                    MouseButton::Right => self.rmb = p,
                    MouseButton::Middle => self.mmb = p,
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let dx = position.x - self.mouse_pos.0;
                let dy = position.y - self.mouse_pos.1;
                self.mouse_pos = (position.x, position.y);
                if self.lmb || self.rmb || self.mmb {
                    if let Ok(mut s) = self.shared.lock() {
                        if self.mmb || (self.lmb && self.shift) {
                            s.camera.pan(dx as f32, dy as f32);
                        } else {
                            s.camera.orbit(dx as f32, dy as f32);
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let d = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 60.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                if let Ok(mut s) = self.shared.lock() {
                    s.camera.zoom(d);
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.shift = m.state().shift_key();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let down = event.state == ElementState::Pressed;
                if let PhysicalKey::Code(code) = event.physical_key {
                    // WASD/QE flight (hold the right mouse button, like Studio).
                    match code {
                        KeyCode::KeyW => self.keys[0] = down,
                        KeyCode::KeyS => self.keys[1] = down,
                        KeyCode::KeyA => self.keys[2] = down,
                        KeyCode::KeyD => self.keys[3] = down,
                        KeyCode::KeyQ => self.keys[4] = down,
                        KeyCode::KeyE => self.keys[5] = down,
                        _ => {}
                    }
                    if down {
                        if code == KeyCode::Escape {
                            event_loop.exit();
                        }
                        // cycle lighting tech with T
                        if code == KeyCode::KeyT {
                            if let Ok(mut s) = self.shared.lock() {
                                let modes = [
                                    "Future",
                                    "ShadowMap",
                                    "Compatibility",
                                    "Voxel",
                                    "Legacy",
                                ];
                                let next = match s.lighting.technology {
                                    crate::LightingTechnology::Future => 0,
                                    crate::LightingTechnology::ShadowMap => 1,
                                    crate::LightingTechnology::Compatibility => 2,
                                    crate::LightingTechnology::Voxel => 3,
                                    crate::LightingTechnology::Legacy => 4,
                                };
                                let n = (next + 1) % 5;
                                s.lighting.technology = crate::LightingTechnology::from_str(modes[n]);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let close = self.shared.lock().map(|s| s.close).unwrap_or(false);
        if close {
            event_loop.exit();
            return;
        }
        self.tick_flight();
        if let Some(gpu) = &self.gpu {
            gpu.window.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(8),
        ));
    }
}

fn run_viewport(shared: Arc<Mutex<ViewportSharedState>>) -> Result<()> {
    let mut builder = EventLoop::builder();
    #[cfg(target_os = "windows")]
    {
        builder.with_any_thread(true);
    }
    let event_loop = builder.build().context("event loop")?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(shared);
    event_loop.run_app(&mut app).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}
