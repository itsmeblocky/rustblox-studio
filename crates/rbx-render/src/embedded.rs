//! Viewport embedded in the Qt shell: a `WS_CHILD` window parented to the Qt
//! window, with its own wgpu swapchain and its own render thread.

use crate::camera::Camera;
use crate::lighting::LightingState;
use crate::scene_pass::ScenePass;
use crate::tools::{self, Axis, DragState, SnapSettings, ToolMode};
use anyhow::{anyhow, Context, Result};
use glam::{Mat3, Vec3};
use rbx_core::Scene;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Input/camera/tool state shared by the window procedure and the render thread.
struct Shared {
    state: Mutex<EmbeddedState>,
    cv: Condvar,
}

struct EmbeddedState {
    camera: Camera,
    lighting: LightingState,
    scene: Scene,
    scene_dirty: bool,
    visible: bool,
    /// Physical pixels of the child window's client area.
    width: u32,
    height: u32,
    /// Set whenever a frame needs to be produced (input, resize, scene edit).
    repaint: bool,
    close: bool,
    error: Option<String>,
    fps: f32,
    frames: u64,
    // pointer state
    buttons: u8,
    last_x: i32,
    last_y: i32,
    down_x: i32,
    down_y: i32,
    moved_since_down: f32,
    material_textures_dir: Option<std::path::PathBuf>,
    // tools / selection / manipulation
    tool: ToolMode,
    snap: SnapSettings,
    selected: Option<usize>,
    selected_name: String,
    selection_rev: u64,
    scene_edited_rev: u64,
    drag: Option<DragState>,
}

impl Default for EmbeddedState {
    fn default() -> Self {
        Self {
            camera: Camera::default(),
            lighting: LightingState::default(),
            scene: Scene::default(),
            scene_dirty: true,
            visible: false,
            width: 1,
            height: 1,
            repaint: true,
            close: false,
            error: None,
            fps: 0.0,
            frames: 0,
            buttons: 0,
            last_x: 0,
            last_y: 0,
            down_x: 0,
            down_y: 0,
            moved_since_down: 0.0,
            material_textures_dir: None,
            tool: ToolMode::Select,
            snap: SnapSettings::default(),
            selected: None,
            selected_name: String::new(),
            selection_rev: 0,
            scene_edited_rev: 0,
            drag: None,
        }
    }
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(EmbeddedState::default()),
            cv: Condvar::new(),
        })
    }

    /// Applies `f` under the state lock and wakes the render thread.
    fn update(&self, f: impl FnOnce(&mut EmbeddedState)) {
        if let Ok(mut st) = self.state.lock() {
            f(&mut st);
            st.repaint = true;
        }
        self.cv.notify_all();
    }
}

/// Handle to the viewport embedded in the Qt window. Dropping it destroys the
/// child window and stops the render thread.
pub struct EmbeddedViewport {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
    hwnd: isize,
    dpi_scale: f32,
    rect: (i32, i32, i32, i32),
    visible: bool,
    seen_selection_rev: u64,
    seen_scene_edited_rev: u64,
}

impl EmbeddedViewport {
    /// Finds the process' main window (the Qt one) and embeds a viewport in it.
    /// Must be called from the Qt GUI thread.
    pub fn attach() -> Result<Self> {
        #[cfg(windows)]
        {
            let parent = win::find_main_window()
                .ok_or_else(|| anyhow!("no Qt window found for this process"))?;
            Self::attach_to(parent)
        }
        #[cfg(not(windows))]
        {
            Err(anyhow!("the embedded viewport is Windows-only"))
        }
    }

    #[cfg(windows)]
    fn attach_to(parent: isize) -> Result<Self> {
        let hwnd = win::create_child(parent)?;
        let shared = Shared::new();
        win::set_active(Arc::clone(&shared));
        let shared_thread = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("rbx-embedded-viewport".to_string())
            .spawn(move || {
                if let Err(e) = render_loop(Arc::clone(&shared_thread), hwnd) {
                    if let Ok(mut st) = shared_thread.state.lock() {
                        st.error = Some(format!("{e:#}"));
                        st.visible = false;
                    }
                    shared_thread.cv.notify_all();
                }
            })
            .context("spawn embedded viewport thread")?;
        Ok(Self {
            shared,
            thread: Some(thread),
            hwnd,
            dpi_scale: win::dpi_scale(parent),
            rect: (0, 0, 0, 0),
            visible: false,
            seen_selection_rev: 0,
            seen_scene_edited_rev: 0,
        })
    }

    /// Places the child window. `x/y/w/h` are in Qt (logical) pixels relative to
    /// the Qt window's client area.
    pub fn set_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        #[cfg(windows)]
        {
            let s = self.dpi_scale.max(0.5);
            let rect = (
                (x * s).round() as i32,
                (y * s).round() as i32,
                ((w * s).round() as i32).max(1),
                ((h * s).round() as i32).max(1),
            );
            if rect == self.rect {
                return;
            }
            self.rect = rect;
            win::move_child(self.hwnd, rect);
            self.shared.update(|st| {
                st.width = rect.2 as u32;
                st.height = rect.3 as u32;
            });
        }
        #[cfg(not(windows))]
        {
            let _ = (x, y, w, h);
        }
    }

    /// Shows/hides the child window (hidden while Qt overlays cover the dock).
    pub fn set_visible(&mut self, visible: bool) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        #[cfg(windows)]
        {
            win::show_child(self.hwnd, visible);
            self.shared.update(|st| st.visible = visible);
        }
        #[cfg(not(windows))]
        {
            let _ = visible;
        }
    }

    pub fn set_camera(&self, camera: Camera) {
        self.shared.update(|st| st.camera = camera);
    }

    pub fn set_lighting(&self, lighting: LightingState) {
        self.shared.update(|st| st.lighting = lighting);
    }

    pub fn set_scene(&self, scene: Scene) {
        self.shared.update(|st| {
            st.scene = scene;
            st.scene_dirty = true;
        });
    }

    pub fn set_material_textures_dir(&self, dir: Option<std::path::PathBuf>) {
        self.shared.update(|st| {
            st.material_textures_dir = dir;
        });
    }

    pub fn set_tool(&self, tool: &str) {
        self.shared.update(|st| st.tool = ToolMode::from_str(tool));
    }

    pub fn set_snap(&self, enabled: bool, move_step: f32, rotate_deg: f32, scale_step: f32) {
        self.shared.update(|st| {
            st.snap = SnapSettings {
                enabled,
                move_step,
                rotate_deg,
                scale_step,
            };
        });
    }

    pub fn set_selected(&self, index: Option<usize>, name: String) {
        self.shared.update(|st| {
            st.selected = index;
            st.selected_name = name;
            st.selection_rev = st.selection_rev.wrapping_add(1);
        });
    }

    /// Returns (index, name) if selection changed since last poll.
    pub fn take_selection(&mut self) -> Option<(Option<usize>, String)> {
        let st = self.shared.state.lock().ok()?;
        if st.selection_rev == self.seen_selection_rev {
            return None;
        }
        self.seen_selection_rev = st.selection_rev;
        Some((st.selected, st.selected_name.clone()))
    }

    /// Returns a cloned scene if it was edited by viewport interaction since last poll.
    pub fn take_scene_edit(&mut self) -> Option<Scene> {
        let st = self.shared.state.lock().ok()?;
        if st.scene_edited_rev == self.seen_scene_edited_rev {
            return None;
        }
        self.seen_scene_edited_rev = st.scene_edited_rev;
        Some(st.scene.clone())
    }

    /// Reads back the camera (the child window owns viewport input now).
    pub fn camera(&self) -> Camera {
        self.shared
            .state
            .lock()
            .map(|st| st.camera.clone())
            .unwrap_or_default()
    }

    pub fn fps(&self) -> f32 {
        self.shared.state.lock().map(|st| st.fps).unwrap_or(0.0)
    }

    pub fn frames(&self) -> u64 {
        self.shared.state.lock().map(|st| st.frames).unwrap_or(0)
    }

    pub fn take_error(&self) -> Option<String> {
        self.shared
            .state
            .lock()
            .ok()
            .and_then(|mut st| st.error.take())
    }

    pub fn is_alive(&self) -> bool {
        self.shared
            .state
            .lock()
            .map(|st| st.error.is_none() && !st.close)
            .unwrap_or(false)
    }
}

impl Drop for EmbeddedViewport {
    fn drop(&mut self) {
        self.shared.update(|st| st.close = true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        #[cfg(windows)]
        {
            win::clear_active();
            win::destroy_child(self.hwnd);
        }
    }
}

fn aspect_of(w: u32, h: u32) -> f32 {
    (w.max(1) as f32) / (h.max(1) as f32)
}

fn pick_part(st: &EmbeddedState, x: f32, y: f32) -> Option<usize> {
    let aspect = aspect_of(st.width, st.height);
    let (origin, dir) = st.camera.screen_ray(
        x,
        y,
        st.width.max(1) as f32,
        st.height.max(1) as f32,
        aspect,
    );
    let mut best: Option<(f32, usize)> = None;
    for (i, part) in st.scene.parts.iter().enumerate() {
        let aabb = tools::part_world_aabb(part);
        if let Some(t) = tools::ray_aabb(origin, dir, aabb.min, aabb.max) {
            if best.map(|b| t < b.0).unwrap_or(true) {
                best = Some((t, i));
            }
        }
    }
    best.map(|b| b.1)
}

fn part_center(part: &rbx_core::Part) -> Vec3 {
    Vec3::new(part.position.x, part.position.y, part.position.z)
}

fn pick_gizmo_axis(st: &EmbeddedState, x: f32, y: f32) -> Option<Axis> {
    let sel = st.selected?;
    let part = st.scene.parts.get(sel)?;
    let origin_w = part_center(part);
    let aspect = aspect_of(st.width, st.height);
    let cam_pos = st.camera.position();
    let dist = (origin_w - cam_pos).length().max(1.0);
    let len = (dist * 0.18).clamp(1.5, 12.0);
    let (ray_o, ray_d) = st.camera.screen_ray(
        x,
        y,
        st.width.max(1) as f32,
        st.height.max(1) as f32,
        aspect,
    );
    let thickness = (len * 0.12).max(0.25);
    let mut best: Option<(f32, Axis)> = None;
    for axis in [Axis::X, Axis::Y, Axis::Z] {
        let dir = tools::axis_vec(axis);
        let end = origin_w + dir * len;
        let min = Vec3::new(
            origin_w.x.min(end.x) - thickness,
            origin_w.y.min(end.y) - thickness,
            origin_w.z.min(end.z) - thickness,
        );
        let max = Vec3::new(
            origin_w.x.max(end.x) + thickness,
            origin_w.y.max(end.y) + thickness,
            origin_w.z.max(end.z) + thickness,
        );
        if let Some(t) = tools::ray_aabb(ray_o, ray_d, min, max) {
            if best.map(|b| t < b.0).unwrap_or(true) {
                best = Some((t, axis));
            }
        }
    }
    best.map(|b| b.1)
}

fn begin_drag(st: &mut EmbeddedState, axis: Axis, x: f32, y: f32) -> bool {
    let Some(sel) = st.selected else { return false; };
    let Some(part) = st.scene.parts.get(sel) else { return false; };
    st.drag = Some(DragState {
        axis,
        start_mouse: (x, y),
        start_pos: part_center(part),
        start_size: Vec3::new(part.size.x, part.size.y, part.size.z),
        start_rot: part.cframe.rotation,
        origin: part_center(part),
    });
    true
}

fn apply_drag(st: &mut EmbeddedState, x: f32, y: f32) {
    let Some(drag) = st.drag.clone() else { return; };
    let Some(sel) = st.selected else { return; };
    let axis = drag.axis;
    let axis_v = tools::axis_vec(axis);
    let dx = x - drag.start_mouse.0;
    let dy = y - drag.start_mouse.1;
    let aspect = aspect_of(st.width, st.height);
    let vp = st.camera.view_proj(aspect);
    let p0 = vp.project_point3(drag.origin);
    let p1 = vp.project_point3(drag.origin + axis_v);
    let mut sdir = glam::Vec2::new(p1.x - p0.x, -(p1.y - p0.y));
    if sdir.length_squared() < 1e-6 {
        sdir = glam::Vec2::new(1.0, 0.0);
    }
    sdir = sdir.normalize();
    let mouse = glam::Vec2::new(dx, dy);
    let px_along = mouse.dot(sdir);
    let cam_dist = (drag.origin - st.camera.position()).length().max(1.0);
    let world_per_px = (2.0 * cam_dist * (st.camera.fov_y_degrees.to_radians() * 0.5).tan())
        / (st.height.max(1) as f32);
    let delta_world = px_along * world_per_px;

    if let Some(part) = st.scene.parts.get_mut(sel) {
        match st.tool {
            ToolMode::Move | ToolMode::Transform => {
                let step = st.snap.move_step.max(0.001);
                let snapped =
                    tools::snap_value(delta_world, step, st.snap.enabled);
                let v = drag.start_pos + axis_v * snapped;
                part.position = rbx_core::Vector3::new(v.x, v.y, v.z);
                part.cframe.position = part.position;
            }
            ToolMode::Scale => {
                let base = match axis {
                    Axis::X => drag.start_size.x,
                    Axis::Y => drag.start_size.y,
                    Axis::Z => drag.start_size.z,
                };
                let mut s = base + delta_world;
                s = s.max(0.05);
                s = tools::snap_value(s, st.snap.scale_step.max(0.001), st.snap.enabled).max(0.05);
                match axis {
                    Axis::X => part.size.x = s,
                    Axis::Y => part.size.y = s,
                    Axis::Z => part.size.z = s,
                }
            }
            ToolMode::Rotate => {
                let deg = delta_world * 18.0;
                let snapped =
                    tools::snap_angle_deg(deg, st.snap.rotate_deg.max(1.0), st.snap.enabled);
                let rad = snapped.to_radians();
                let (s, c) = rad.sin_cos();
                let r_axis = match axis {
                    Axis::X => Mat3::from_cols_array(&[1.0, 0.0, 0.0, 0.0, c, s, 0.0, -s, c]),
                    Axis::Y => Mat3::from_cols_array(&[c, 0.0, -s, 0.0, 1.0, 0.0, s, 0.0, c]),
                    Axis::Z => Mat3::from_cols_array(&[c, s, 0.0, -s, c, 0.0, 0.0, 0.0, 1.0]),
                };
                let r0 = drag.start_rot;
                let m0 = Mat3::from_cols_array(&[
                    r0[0][0], r0[1][0], r0[2][0],
                    r0[0][1], r0[1][1], r0[2][1],
                    r0[0][2], r0[1][2], r0[2][2],
                ]);
                let m = r_axis * m0;
                let cols = m.to_cols_array();
                part.cframe.rotation = [
                    [cols[0], cols[3], cols[6]],
                    [cols[1], cols[4], cols[7]],
                    [cols[2], cols[5], cols[8]],
                ];
            }
            ToolMode::Select => {}
        }
    }
    st.scene_dirty = true;
    st.scene_edited_rev = st.scene_edited_rev.wrapping_add(1);
}

// ---------------------------------------------------------------------------
// GPU side (render thread)
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn render_loop(shared: Arc<Shared>, hwnd: isize) -> Result<()> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    let target = win::HwndTarget { hwnd };
    let surface: wgpu::Surface<'static> =
        unsafe { instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::from_window(&target)?) }
            .context("create wgpu surface for the embedded viewport")?;

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&surface),
        force_fallback_adapter: false,
    }))
    .context("no adapter for the embedded viewport surface")?;

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("rbx-embedded"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                .using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .context("create device for the embedded viewport")?;

    let caps = surface.get_capabilities(&adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| f.is_srgb())
        .unwrap_or(caps.formats[0]);
    let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
        wgpu::PresentMode::Fifo
    } else {
        caps.present_modes[0]
    };
    let mut config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: 1,
        height: 1,
        present_mode,
        alpha_mode: caps.alpha_modes[0],
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };

    let mut pass = ScenePass::new(&device, &queue, format, 1, 1);
    if let Ok(st) = shared.state.lock() {
        pass.set_material_textures_dir(st.material_textures_dir.clone());
    }
    let mut sized = false;
    let mut fps_window = Instant::now();
    let mut fps_frames = 0u32;
    let mut last_tick = Instant::now();

    loop {
        let hover = win::cursor_over(hwnd);
        let (width, height, scene_dirty) = {
            let mut st = shared.state.lock().unwrap();
            while !((st.visible && st.repaint) || st.close) {
                let armed = st.visible && hover;
                let timeout = if armed { 33 } else { 250 };
                let (guard, _) = shared
                    .cv
                    .wait_timeout(st, Duration::from_millis(timeout))
                    .unwrap();
                st = guard;
                if st.close { break; }
                if st.visible && (st.repaint || armed) { break; }
            }
            if st.close { break; }
            st.repaint = false;
            (st.width.max(1), st.height.max(1), st.scene_dirty)
        };

        let (scene, mat_dir, sel, tool) = shared
            .state
            .lock()
            .map(|mut st| {
                let dirty = st.scene_dirty;
                if dirty { st.scene_dirty = false; }
                (st.scene.clone(), st.material_textures_dir.clone(), st.selected, st.tool)
            })
            .unwrap_or((Scene::default(), None, None, ToolMode::Select));
        if scene_dirty {
            pass.set_material_textures_dir(mat_dir);
            pass.set_scene(&scene);
        }
        pass.set_tool_overlay(sel, tool);

        if !sized || config.width != width || config.height != height {
            config.width = width;
            config.height = height;
            surface.configure(&device, &config);
            pass.resize(&device, width, height);
            sized = true;
        }

        let (camera, lighting) = {
            let st = shared.state.lock().unwrap();
            (st.camera.clone(), st.lighting.clone())
        };

        match surface.get_current_texture() {
            Ok(frame) => {
                let view = frame
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                pass.draw(&device, &queue, &view, &camera, &lighting);
                frame.present();
                fps_frames += 1;
                let elapsed = fps_window.elapsed();
                if elapsed >= Duration::from_millis(500) {
                    let fps = fps_frames as f32 / elapsed.as_secs_f32();
                    if let Ok(mut st) = shared.state.lock() {
                        st.fps = fps;
                        st.frames += fps_frames as u64;
                    }
                    fps_window = Instant::now();
                    fps_frames = 0;
                }
            }
            Err(wgpu::SurfaceError::Lost) | Err(wgpu::SurfaceError::Outdated) => {
                surface.configure(&device, &config);
                pass.resize(&device, width, height);
            }
            Err(wgpu::SurfaceError::Timeout) => {}
            Err(wgpu::SurfaceError::OutOfMemory) => return Err(anyhow!("out of GPU memory")),
        }

        let dt = last_tick.elapsed().as_secs_f32().min(0.1);
        last_tick = Instant::now();
        let (forward, strafe, rise, speed_mult) = crate::camera::keyboard_fly_axes();
        let hover = win::cursor_over(hwnd);
        if let Ok(mut st) = shared.state.lock() {
            let armed = st.visible && (st.buttons & win::BTN_RMB != 0 || hover);
            if armed {
                st.camera.fly_input(forward, strafe, rise, speed_mult, dt);
                if forward != 0.0 || strafe != 0.0 || rise != 0.0 || st.buttons & win::BTN_RMB != 0 {
                    st.repaint = true;
                }
            } else {
                st.camera.reset_fly();
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Win32
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::{apply_drag, begin_drag, pick_gizmo_axis, pick_part};
    use crate::tools::ToolMode;
    use anyhow::{anyhow, Result};
    use raw_window_handle::{
        DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
        RawWindowHandle, Win32WindowHandle, WindowHandle, WindowsDisplayHandle,
    };
    use std::ffi::c_void;
    use std::num::NonZeroIsize;
    use std::sync::{Arc, Mutex, OnceLock};
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::ValidateRect;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, ReleaseCapture, SetCapture, VK_SHIFT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, EnumWindows, GetClientRect, GetCursorPos,
        GetParent, GetWindow, GetWindowThreadProcessId, IsChild, IsWindowVisible, LoadCursorW,
        MoveWindow, RegisterClassW, ShowWindow, WindowFromPoint, CS_DBLCLKS, CS_OWNDC, GW_OWNER,
        IDC_ARROW, MA_NOACTIVATE, SW_HIDE, SW_SHOW, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP,
        WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT,
        WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SIZE, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN,
        WS_CLIPSIBLINGS, WS_EX_NOACTIVATE, WS_EX_NOPARENTNOTIFY, WS_VISIBLE,
    };

    pub struct HwndTarget { pub hwnd: isize }
    unsafe impl Send for HwndTarget {}
    unsafe impl Sync for HwndTarget {}

    impl HasWindowHandle for HwndTarget {
        fn window_handle(&self) -> std::result::Result<WindowHandle<'_>, HandleError> {
            let hwnd = NonZeroIsize::new(self.hwnd).ok_or(HandleError::Unavailable)?;
            let handle = Win32WindowHandle::new(hwnd);
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
        }
    }
    impl HasDisplayHandle for HwndTarget {
        fn display_handle(&self) -> std::result::Result<DisplayHandle<'_>, HandleError> {
            Ok(unsafe { DisplayHandle::borrow_raw(RawDisplayHandle::Windows(WindowsDisplayHandle::new())) })
        }
    }

    static ACTIVE: Mutex<Option<Arc<super::Shared>>> = Mutex::new(None);
    static CLASS_NAME: OnceLock<Vec<u16>> = OnceLock::new();

    pub const BTN_LMB: u8 = 1;
    pub const BTN_RMB: u8 = 2;
    pub const BTN_MMB: u8 = 4;

    pub fn set_active(shared: Arc<super::Shared>) {
        if let Ok(mut slot) = ACTIVE.lock() { *slot = Some(shared); }
    }
    pub fn clear_active() {
        if let Ok(mut slot) = ACTIVE.lock() { *slot = None; }
    }
    fn active() -> Option<Arc<super::Shared>> {
        ACTIVE.lock().ok().and_then(|slot| slot.clone())
    }
    fn class_name() -> &'static [u16] {
        CLASS_NAME.get_or_init(|| "RustbloxViewportChild\0".encode_utf16().collect::<Vec<u16>>())
    }

    pub fn find_main_window() -> Option<isize> {
        unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
            unsafe {
                let out = &mut *(lparam as *mut (isize, i64));
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, &mut pid);
                if pid != GetCurrentProcessId() || IsWindowVisible(hwnd) == 0 { return 1; }
                if !GetWindow(hwnd, GW_OWNER).is_null() || !GetParent(hwnd).is_null() { return 1; }
                let mut rect: RECT = std::mem::zeroed();
                if GetClientRect(hwnd, &mut rect) == 0 { return 1; }
                let area = (rect.right - rect.left) as i64 * (rect.bottom - rect.top) as i64;
                if area > out.1 { out.0 = hwnd as isize; out.1 = area; }
                1
            }
        }
        let mut best: (isize, i64) = (0, 0);
        unsafe { EnumWindows(Some(callback), &mut best as *mut _ as LPARAM); }
        if best.0 == 0 { None } else { Some(best.0) }
    }

    pub fn dpi_scale(hwnd: isize) -> f32 {
        let dpi = unsafe { GetDpiForWindow(hwnd as HWND) };
        if dpi == 0 { 1.0 } else { (dpi as f32 / 96.0).max(0.5) }
    }

    pub fn cursor_over(hwnd: isize) -> bool {
        unsafe {
            let mut pt = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut pt) == 0 { return false; }
            let under = WindowFromPoint(pt);
            if under.is_null() { return false; }
            under as isize == hwnd || IsChild(hwnd as HWND, under) != 0
        }
    }

    fn register_class() -> Result<()> {
        static REGISTERED: OnceLock<u16> = OnceLock::new();
        let atom = REGISTERED.get_or_init(|| unsafe {
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.style = CS_OWNDC | CS_DBLCLKS;
            wc.lpfnWndProc = Some(child_wndproc);
            wc.hInstance = GetModuleHandleW(std::ptr::null());
            wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
            wc.lpszClassName = class_name().as_ptr();
            RegisterClassW(&wc)
        });
        if *atom == 0 { return Err(anyhow!("RegisterClassW failed for the viewport child window")); }
        Ok(())
    }

    pub fn create_child(parent: isize) -> Result<isize> {
        register_class()?;
        let style = WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WS_CLIPCHILDREN;
        let ex_style = WS_EX_NOPARENTNOTIFY | WS_EX_NOACTIVATE;
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style, class_name().as_ptr(), std::ptr::null(), style,
                0, 0, 1, 1, parent as HWND, std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()), std::ptr::null() as *const c_void,
            )
        };
        if hwnd.is_null() { return Err(anyhow!("CreateWindowExW failed for the viewport child window")); }
        unsafe { ShowWindow(hwnd, SW_HIDE); }
        Ok(hwnd as isize)
    }

    pub fn move_child(hwnd: isize, rect: (i32, i32, i32, i32)) { unsafe { MoveWindow(hwnd as HWND, rect.0, rect.1, rect.2, rect.3, 1); } }
    pub fn show_child(hwnd: isize, visible: bool) { unsafe { ShowWindow(hwnd as HWND, if visible { SW_SHOW } else { SW_HIDE }); } }
    pub fn destroy_child(hwnd: isize) { unsafe { DestroyWindow(hwnd as HWND); } }

    fn shift_down() -> bool { unsafe { GetKeyState(VK_SHIFT as i32) < 0 } }
    fn as_i16_pair(lparam: LPARAM) -> (i32, i32) {
        let x = (lparam & 0xFFFF) as i16 as i32;
        let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
        (x, y)
    }

    pub unsafe extern "system" fn child_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_ERASEBKGND => 1,
            WM_PAINT => { ValidateRect(hwnd, std::ptr::null()); 0 }
            WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
            WM_SIZE => {
                if let Some(shared) = active() {
                    let w = (lparam & 0xFFFF) as u32;
                    let h = ((lparam >> 16) & 0xFFFF) as u32;
                    shared.update(|st| { st.width = w.max(1); st.height = h.max(1); });
                }
                0
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                SetCapture(hwnd);
                let bit = match msg { WM_LBUTTONDOWN => BTN_LMB, WM_RBUTTONDOWN => BTN_RMB, _ => BTN_MMB };
                let (x, y) = as_i16_pair(lparam);
                if let Some(shared) = active() {
                    shared.update(|st| {
                        st.buttons |= bit;
                        st.last_x = x; st.last_y = y;
                        st.down_x = x; st.down_y = y;
                        st.moved_since_down = 0.0;
                        if bit == BTN_LMB && st.tool != ToolMode::Select {
                            if let Some(axis) = pick_gizmo_axis(st, x as f32, y as f32) {
                                let _ = begin_drag(st, axis, x as f32, y as f32);
                            }
                        }
                    });
                }
                0
            }
            WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
                let bit = match msg { WM_LBUTTONUP => BTN_LMB, WM_RBUTTONUP => BTN_RMB, _ => BTN_MMB };
                if let Some(shared) = active() {
                    let mut any_left = false;
                    shared.update(|st| {
                        if bit == BTN_LMB {
                            let was_dragging = st.drag.is_some();
                            if was_dragging {
                                st.drag = None;
                                st.scene_dirty = true;
                                st.scene_edited_rev = st.scene_edited_rev.wrapping_add(1);
                            } else if st.moved_since_down < 6.0 {
                                if let Some(idx) = pick_part(st, st.down_x as f32, st.down_y as f32) {
                                    st.selected = Some(idx);
                                    st.selected_name = st.scene.parts.get(idx).map(|p| p.name.clone()).unwrap_or_default();
                                    st.selection_rev = st.selection_rev.wrapping_add(1);
                                } else if st.tool == ToolMode::Select {
                                    st.selected = None;
                                    st.selected_name.clear();
                                    st.selection_rev = st.selection_rev.wrapping_add(1);
                                }
                            }
                        }
                        st.buttons &= !bit;
                        any_left = st.buttons != 0;
                    });
                    if !any_left { ReleaseCapture(); }
                }
                0
            }
            WM_MOUSEMOVE => {
                let (x, y) = as_i16_pair(lparam);
                if let Some(shared) = active() {
                    shared.update(|st| {
                        let dx = (x - st.last_x) as f32;
                        let dy = (y - st.last_y) as f32;
                        st.last_x = x; st.last_y = y;
                        if st.buttons == 0 { return; }
                        st.moved_since_down += (dx*dx + dy*dy).sqrt();
                        if st.drag.is_some() && (st.buttons & BTN_LMB) != 0 {
                            apply_drag(st, x as f32, y as f32);
                            return;
                        }
                        let lmb = (st.buttons & BTN_LMB) != 0;
                        let rmb = (st.buttons & BTN_RMB) != 0;
                        let mmb = (st.buttons & BTN_MMB) != 0;
                        let pan = mmb || (lmb && shift_down());
                        if pan {
                            st.camera.pan(dx, dy);
                        } else if rmb || (lmb && st.tool == ToolMode::Select) {
                            st.camera.orbit(dx, dy);
                        }
                    });
                }
                0
            }
            WM_MOUSEWHEEL => {
                let delta = (((wparam >> 16) & 0xFFFF) as i16) as f32;
                if let Some(shared) = active() {
                    shared.update(|st| st.camera.zoom(delta));
                }
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
