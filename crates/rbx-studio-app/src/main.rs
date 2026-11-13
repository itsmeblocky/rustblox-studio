//! Rustblox Studio — Qt UI shell (bootstrap).
//!
//! Requires Qt development libraries to be installed and discoverable.
//! Qt 6.8.3 (msvc2022_64) is expected at `.tools/Qt/6.8.3/msvc2022_64` when
//! installed via the workspace aqt task.
//!
//! Runtime env (MSVC/Windows):
//!   QTDIR/QT_DIR = <repo>\.tools\Qt\6.8.3\msvc2022_64
//!   PATH += %QTDIR%\bin
//!   QT_PLUGIN_PATH = %QTDIR%\plugins
//!   QML2_IMPORT_PATH = %QTDIR%\qml

mod asset_cache;
mod insert;
mod roblox_api;

use anyhow::{anyhow, Result};
use qmetaobject::prelude::*;
use rbx_core::Scene;
use rbx_render::{
    spawn_native_viewport, AaMode, Camera, EmbeddedViewport, LightingState, NativeViewportHandle,
    ViewportRenderer, ViewportSharedState,
};
use rbx_script::{ExecContext, HostConfig, ScriptHost};
use rbx_studio::{
    ColorMode, DesignTokens, IconRegistry, IconScale, ReflectionDatabase, RibbonDefinition, Strings,
    Theme as StudioTheme, ThemeName,
};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn write_bmp_rgba(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let row_bytes = (w * 4) as usize;
    let pad = (4 - (row_bytes % 4)) % 4;
    let pixel_data_size = ((row_bytes + pad) * h as usize) as u32;
    let file_size = 54u32 + pixel_data_size;
    let mut f = std::fs::File::create(path)?;
    // BITMAPFILEHEADER
    f.write_all(b"BM")?;
    f.write_all(&file_size.to_le_bytes())?;
    f.write_all(&0u16.to_le_bytes())?;
    f.write_all(&0u16.to_le_bytes())?;
    f.write_all(&54u32.to_le_bytes())?;
    // BITMAPINFOHEADER
    f.write_all(&40u32.to_le_bytes())?;
    f.write_all(&(w as i32).to_le_bytes())?;
    f.write_all(&(h as i32).to_le_bytes())?; // bottom-up
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&32u16.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?; // BI_RGB
    f.write_all(&pixel_data_size.to_le_bytes())?;
    f.write_all(&2835i32.to_le_bytes())?;
    f.write_all(&2835i32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    let pad_bytes = [0u8; 3];
    for y in (0..h as usize).rev() { // bottom-up rows
        let src = y * row_bytes;
        let row = &rgba[src..src + row_bytes];
        for px in row.chunks_exact(4) {
            // RGBA -> BGRA
            f.write_all(&[px[2], px[1], px[0], px[3]])?;
        }
        if pad > 0 { f.write_all(&pad_bytes[..pad])?; }
    }
    Ok(())
}

/// State shared with background workers (toolbox search, OAuth login, experiences).
///
/// The GUI thread polls `StudioApp::poll_async`; workers only ever touch this
/// struct, never Qt objects.
#[derive(Default)]
struct AsyncState {
    toolbox_gen: u64,
    toolbox_json: String,
    toolbox_status: String,
    toolbox_busy: bool,
    login_gen: u64,
    login_status: String,
    login_user: String,
    login_busy: bool,
    insert_gen: u64,
    insert_status: String,
    insert_busy: bool,
    insert_result: Option<PendingInsert>,
    exp_gen: u64,
    experiences_json: String,
    experiences_status: String,
    experiences_busy: bool,
    place_gen: u64,
    place_status: String,
    place_busy: bool,
    place_result: Option<PendingPlace>,
}

/// A Toolbox asset that finished downloading on a worker thread and is waiting
/// for the GUI thread to put it into the scene + Explorer.
struct PendingInsert {
    node: rbx_core::InstanceNode,
    parts: Vec<rbx_core::Part>,
    label: String,
    bytes: usize,
    note: String,
}

/// A place file downloaded on a worker thread and waiting for the GUI thread
/// to swap the open place (scene + Explorer tree + title).
struct PendingPlace {
    name: String,
    imported: rbx_core::ImportedPlace,
    bytes: usize,
    note: String,
}

#[derive(QObject, Default)]
struct StudioApp {
    base: qt_base_class!(trait QObject),
    active_tab: qt_property!(QString; NOTIFY active_tab_changed WRITE set_active_tab),
    active_tab_changed: qt_signal!(),
    viewport_frame: qt_property!(QString; NOTIFY viewport_frame_changed),
    viewport_frame_changed: qt_signal!(),
    viewport_status: qt_property!(QString; NOTIFY viewport_status_changed),
    viewport_status_changed: qt_signal!(),
    // True while the docked viewport is a native child window (direct swapchain,
    // no readback); QML then hides the Image bridge and its render timer.
    viewport_embedded: qt_property!(bool; NOTIFY viewport_embedded_changed),
    viewport_embedded_changed: qt_signal!(),
    // Viewport picking/selection (driven by embedded/native viewport tools).
    viewport_selected_name: qt_property!(QString; NOTIFY viewport_selection_changed),
    viewport_selected_props: qt_property!(QString; NOTIFY viewport_selection_changed),
    viewport_selection_rev: qt_property!(i32; NOTIFY viewport_selection_changed),
    viewport_selection_changed: qt_signal!(),
    lighting_technology: qt_property!(QString; NOTIFY lighting_changed WRITE set_lighting_technology),
    lighting_changed: qt_signal!(),
    aa_mode: qt_property!(QString; NOTIFY aa_changed WRITE set_aa_mode),
    aa_changed: qt_signal!(),
    // internal state (not Qt properties)
    renderer: RefCell<Option<ViewportRenderer>>,
    camera: RefCell<Camera>,
    scene: RefCell<Scene>,
    lighting: RefCell<LightingState>,
    frame_seq: RefCell<u64>,
    dirty: RefCell<bool>,
    last_w: RefCell<u32>,
    last_h: RefCell<u32>,
    interacting_until: RefCell<Option<Instant>>,
    last_render: RefCell<Option<Instant>>,
    native_viewport: RefCell<Option<NativeViewportHandle>>,
    embedded_viewport: RefCell<Option<EmbeddedViewport>>,
    script_host: RefCell<Option<ScriptHost>>,
    playing: qt_property!(bool; NOTIFY playing_changed WRITE set_playing),
    playing_changed: qt_signal!(),
    console_output: qt_signal!(level: QString, text: QString),

    // Toolbox / account state mirrored from the async workers.
    toolbox_json: qt_property!(QString; NOTIFY toolbox_changed),
    toolbox_status: qt_property!(QString; NOTIFY toolbox_changed),
    toolbox_busy: qt_property!(bool; NOTIFY toolbox_changed),
    toolbox_changed: qt_signal!(),
    login_user: qt_property!(QString; NOTIFY login_changed),
    login_status: qt_property!(QString; NOTIFY login_changed),
    login_busy: qt_property!(bool; NOTIFY login_changed),
    login_changed: qt_signal!(),
    // Open Cloud API key (stored in the OS credential store, never in the place)
    opencloud_linked: qt_property!(bool; NOTIFY opencloud_changed),
    opencloud_mask: qt_property!(QString; NOTIFY opencloud_changed),
    opencloud_status: qt_property!(QString; NOTIFY opencloud_changed),
    opencloud_changed: qt_signal!(),
    // Toolbox insert progress/result (download → parse → scene + Explorer)
    insert_status: qt_property!(QString; NOTIFY insert_changed),
    insert_busy: qt_property!(bool; NOTIFY insert_changed),
    insert_changed: qt_signal!(),
    /// Emitted once an asset has been parsed: `(nodeJson, explorerPath)`.
    /// QML owns the Explorer tree, so it splices the node in itself.
    explorer_insert: qt_signal!(node: QString, path: QString),
    // Editable experiences (OAuth, token/resources + games/thumbnails).
    experiences_json: qt_property!(QString; NOTIFY experiences_changed),
    experiences_status: qt_property!(QString; NOTIFY experiences_changed),
    experiences_busy: qt_property!(bool; NOTIFY experiences_changed),
    experiences_changed: qt_signal!(),
    place_status: qt_property!(QString; NOTIFY place_changed),
    place_busy: qt_property!(bool; NOTIFY place_changed),
    place_name: qt_property!(QString; NOTIFY place_changed),
    place_changed: qt_signal!(),
    /// Replaces the whole Explorer tree (opening a place).
    explorer_load: qt_signal!(tree: QString),
    async_state: Arc<Mutex<AsyncState>>,
    seen_toolbox: RefCell<u64>,
    seen_login: RefCell<u64>,
    seen_insert: RefCell<u64>,
    seen_exp: RefCell<u64>,
    seen_place: RefCell<u64>,

    set_active_tab: qt_method!(fn set_active_tab(&mut self, tab: QString) {
        if self.active_tab != tab {
            self.active_tab = tab;
            self.active_tab_changed();
        }
    }),
    set_lighting_technology: qt_method!(fn set_lighting_technology(&mut self, tech: QString) {
        let t = tech.to_string();
        if self.lighting_technology.to_string() != t {
            self.lighting_technology = QString::from(t.clone());
            let mut l = self.lighting.borrow_mut();
            l.technology = rbx_render::LightingTechnology::from_str(&t);
            let l_clone = l.clone();
            drop(l);
            if let Some(nv) = self.native_viewport.borrow().as_ref() {
                nv.set_lighting(l_clone.clone());
            }
            if let Some(ev) = self.embedded_viewport.borrow().as_ref() {
                ev.set_lighting(l_clone);
            }
            *self.dirty.borrow_mut() = true;
            self.lighting_changed();
        }
    }),
    set_aa_mode: qt_method!(fn set_aa_mode(&mut self, mode: QString) {
        let m = mode.to_string();
        if self.aa_mode.to_string() != m {
            self.aa_mode = QString::from(m.clone());
            if let Some(r) = self.renderer.borrow_mut().as_mut() {
                r.set_aa_mode(AaMode::from_str(&m));
            }
            *self.dirty.borrow_mut() = true;
            self.aa_changed();
        }
    }),
    render_viewport: qt_method!(fn render_viewport(&mut self, width: i32, height: i32) {
        // The embedded viewport draws straight to its own swapchain; this
        // readback path only runs as the fallback.
        if self.embedded_viewport.borrow().is_some() {
            return;
        }
        let mut w = (width.max(1)) as u32;
        let mut h = (height.max(1)) as u32;
        if w < 8 || h < 8 { return; }
        let now = Instant::now();
        let interacting = self.interacting_until.borrow().map(|t| now < t).unwrap_or(false);
        // Dynamic resolution: ultra-low while interacting (readback+BMP+QML reload is the main bottleneck)
        let max_dim = if interacting { 320u32 } else { 960u32 };
        // Strict throttle during interaction to keep UI responsive
        if interacting {
            if let Some(lr) = *self.last_render.borrow() {
                let ms = now.duration_since(lr).as_millis();
                if ms < 110 { return; }
            }
        }

        if w > max_dim || h > max_dim {
            let scale = (max_dim as f32 / w.max(h) as f32).min(1.0);
            w = ((w as f32 * scale).round() as u32).max(8);
            h = ((h as f32 * scale).round() as u32).max(8);
        }
        let size_changed = *self.last_w.borrow() != w || *self.last_h.borrow() != h;
        let is_dirty = *self.dirty.borrow() || self.renderer.borrow().is_none();
        if !is_dirty && !size_changed && !interacting { return; }
        // during interaction, render if enough time passed or dirty/size changed


        let result = (|| -> Result<String> {
            if self.renderer.borrow().is_none() {
                *self.scene.borrow_mut() = Scene::default();
                let mut l = LightingState::default();
                let cur = self.lighting_technology.to_string();
                if !cur.is_empty() { l.technology = rbx_render::LightingTechnology::from_str(&cur); }
                *self.lighting.borrow_mut() = l;
                let mut r = ViewportRenderer::new(w, h)?;
                let aa = self.aa_mode.to_string();
                if !aa.is_empty() { r.set_aa_mode(AaMode::from_str(&aa)); }
                r.upload_scene(&self.scene.borrow());
                *self.renderer.borrow_mut() = Some(r);
                if self.lighting_technology.to_string().is_empty() {
                    self.lighting_technology = QString::from("Future");
                    self.lighting_changed();
                }
                if self.aa_mode.to_string().is_empty() {
                    self.aa_mode = QString::from("AA Off");
                    self.aa_changed();
                }
            }
            {
                let mut rb = self.renderer.borrow_mut();
                if let Some(r) = rb.as_mut() {
                    r.resize(w, h);
                    *self.last_w.borrow_mut() = w;
                    *self.last_h.borrow_mut() = h;
                    let cam = self.camera.borrow().clone();
                    let light = self.lighting.borrow().clone();
                    let mut seq = self.frame_seq.borrow_mut();
                    *seq += 1;
                    let frame_path = std::env::temp_dir().join("opencode").join("rbx-viewport").join("frame.bmp");
                    let rgba = r.render_to_rgba(&cam, &light)?;
                    write_bmp_rgba(&frame_path, w, h, &rgba)?;
                    let url = file_url(&frame_path);
                    let url = format!("{}?v={}", url, *seq);
                    *self.dirty.borrow_mut() = false;
                    *self.last_render.borrow_mut() = Some(Instant::now());
                    return Ok(url);
                }
            }
            Err(anyhow!("renderer unavailable"))
        })();
        match result {
            Ok(url) => {
                let tech = self.lighting_technology.to_string();
                let aa = self.aa_mode.to_string();
                self.viewport_status = QString::from(format!("wgpu | {} | {} | Parts:{} | {}x{}{}", if tech.is_empty() { "Future" } else { &tech }, if aa.is_empty() { "AA Off" } else { &aa }, self.scene.borrow().parts.len(), *self.last_w.borrow(), *self.last_h.borrow(), if interacting { " | fast" } else { "" }));
                self.viewport_status_changed();
                self.viewport_frame = QString::from(url);
                self.viewport_frame_changed();
            }
            Err(e) => {
                self.viewport_status = QString::from(format!("viewport error: {e:#}"));
                self.viewport_status_changed();
            }
        }
    }),
    orbit_camera: qt_method!(fn orbit_camera(&mut self, dx: f32, dy: f32) {
        self.camera.borrow_mut().orbit(dx, dy);
        *self.dirty.borrow_mut() = true;
        *self.interacting_until.borrow_mut() = Some(Instant::now() + std::time::Duration::from_millis(180));
    }),
    pan_camera: qt_method!(fn pan_camera(&mut self, dx: f32, dy: f32) {
        self.camera.borrow_mut().pan(dx, dy);
        *self.dirty.borrow_mut() = true;
        *self.interacting_until.borrow_mut() = Some(Instant::now() + std::time::Duration::from_millis(180));
    }),
    zoom_camera: qt_method!(fn zoom_camera(&mut self, delta: f32) {
        self.camera.borrow_mut().zoom(delta);
        *self.dirty.borrow_mut() = true;
        *self.interacting_until.borrow_mut() = Some(Instant::now() + std::time::Duration::from_millis(220));
    }),
    /// Flies the camera rig for `dt` seconds using the physically-held keys
    /// (WASD + Q/E, Shift = fast). Speed ramps up while a key is held and resets
    /// when released. Used by the QML readback path, where the viewport item
    /// owns input; the embedded/native viewports tick themselves.
    fly_camera: qt_method!(fn fly_camera(&mut self, dt: f32) {
        let (forward, strafe, rise, mult) = rbx_render::camera::keyboard_fly_axes();
        {
            let mut camera = self.camera.borrow_mut();
            camera.fly_input(forward, strafe, rise, mult, dt);
        }
        if forward == 0.0 && strafe == 0.0 && rise == 0.0 {
            return;
        }
        *self.dirty.borrow_mut() = true;
        *self.interacting_until.borrow_mut() =
            Some(Instant::now() + std::time::Duration::from_millis(400));
    }),
    begin_interaction: qt_method!(fn begin_interaction(&mut self) {
        *self.interacting_until.borrow_mut() = Some(Instant::now() + std::time::Duration::from_millis(220));
    }),
    end_interaction: qt_method!(fn end_interaction(&mut self) {
        *self.interacting_until.borrow_mut() = None;
        *self.dirty.borrow_mut() = true; // trigger a full-quality render
    }),
    open_native_viewport: qt_method!(fn open_native_viewport(&mut self) {
        // Sync latest state to an existing native viewport, or spawn a new one (separate window, direct wgpu swapchain).
        let state = ViewportSharedState {
            camera: self.camera.borrow().clone(),
            lighting: self.lighting.borrow().clone(),
            scene: self.scene.borrow().clone(),
            scene_dirty: true,
            close: false,
            title_extra: self.lighting_technology.to_string(),
        };
        if let Some(nv) = self.native_viewport.borrow().as_ref() {
            nv.set_camera(state.camera);
            nv.set_lighting(state.lighting);
            nv.set_scene(state.scene);
            self.viewport_status = QString::from("Native viewport synced (separate wgpu window). Use T to cycle lighting.");
            self.viewport_status_changed();
            return;
        }
        match spawn_native_viewport(state) {
            Ok(handle) => {
                *self.native_viewport.borrow_mut() = Some(handle);
                self.viewport_status = QString::from("Native 3D viewport opened (direct wgpu swapchain, no readback). LMB/RMB orbit, MMB/Shift+LMB pan, wheel zoom, T cycles lighting, Esc closes.");
                self.viewport_status_changed();
            }
            Err(e) => {
                self.viewport_status = QString::from(format!("Native viewport failed: {e:#}"));
                self.viewport_status_changed();
            }
        }
    }),
    /// Attaches the docked viewport as a native child window of the Qt shell.
    /// Safe to call repeatedly: the QML side retries until the Qt window exists.
    viewport_attach: qt_method!(fn viewport_attach(&mut self) {
        if self.embedded_viewport.borrow().is_some() {
            return;
        }
        match EmbeddedViewport::attach() {
            Ok(vp) => {
                vp.set_camera(self.camera.borrow().clone());
                vp.set_lighting(self.lighting.borrow().clone());
                vp.set_scene(self.scene.borrow().clone());
                *self.embedded_viewport.borrow_mut() = Some(vp);
                self.viewport_embedded = true;
                self.viewport_embedded_changed();
                eprintln!("[embedded viewport] attached native child window");
                let tech = self.lighting_technology.to_string();
                self.viewport_status = QString::from(format!(
                    "wgpu | {} | direct swapchain (embedded, no readback) | Parts:{}",
                    if tech.is_empty() { "Future" } else { &tech },
                    self.scene.borrow().parts.len(),
                ));
                self.viewport_status_changed();
            }
            Err(e) => {
                // Expected before the Qt window is mapped; the caller retries.
                self.viewport_status = QString::from(format!("embedded viewport pending: {e:#}"));
                self.viewport_status_changed();
            }
        }
    }),
    /// Positions the embedded viewport. `x/y/w/h` are Qt logical pixels
    /// relative to the shell window's client area.
    viewport_set_rect: qt_method!(fn viewport_set_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if let Some(vp) = self.embedded_viewport.borrow_mut().as_mut() {
            vp.set_rect(x, y, w, h);
            vp.set_visible(w > 8.0 && h > 8.0);
        }
    }),
    /// Hides the child window while a QML overlay (start page, settings) covers it.
    viewport_show: qt_method!(fn viewport_show(&mut self, visible: bool) {
        if let Some(vp) = self.embedded_viewport.borrow_mut().as_mut() {
            vp.set_visible(visible);
        }
    }),
    set_active_tool: qt_method!(fn set_active_tool(&mut self, tool: QString) {
        let t = tool.to_string();
        if let Some(ev) = self.embedded_viewport.borrow().as_ref() {
            ev.set_tool(&t);
        }
        if let Some(nv) = self.native_viewport.borrow().as_ref() {
            // native viewport tool sync (best-effort)
            let _ = nv;
        }
    }),
    set_tool_snap: qt_method!(fn set_tool_snap(&mut self, enabled: bool, move_step: f32, rotate_deg: f32, scale_step: f32) {
        if let Some(ev) = self.embedded_viewport.borrow().as_ref() {
            ev.set_snap(enabled, move_step, rotate_deg, scale_step);
        }
    }),
    run_luau: qt_method!(fn run_luau(&mut self, code: QString) {
        let source = code.to_string();
        if source.trim().is_empty() {
            return;
        }
        self.console_output(QString::from("command"), QString::from(format!("> {source}")));
        self.ensure_script_host();
        let outcome = {
            let host = self.script_host.borrow();
            match host.as_ref() {
                Some(host) => host.exec(&source, "CommandBar", ExecContext::Server),
                None => Err("Luau host unavailable".to_string()),
            }
        };
        match outcome {
            Ok(value) => {
                let text = {
                    let host = self.script_host.borrow();
                    host.as_ref()
                        .map(|h| rbx_script::value_display(h.lua(), &value))
                        .unwrap_or_default()
                };
                if !matches!(value, rbx_script::mlua::Value::Nil) {
                    self.console_output(QString::from("info"), QString::from(text));
                }
            }
            Err(e) => self.console_output(QString::from("error"), QString::from(e)),
        }
        self.drain_script_logs();
    }),
    step_scripts: qt_method!(fn step_scripts(&mut self, dt: f64) {
        if !self.playing {
            return;
        }
        {
            let host = self.script_host.borrow();
            if let Some(host) = host.as_ref() {
                host.step(dt.clamp(0.0, 0.25));
            }
        }
        self.drain_script_logs();
    }),
    set_playing: qt_method!(fn set_playing(&mut self, playing: bool) {
        if playing == self.playing {
            return;
        }
        if playing {
            // Play sessions always start from a clean runtime.
            *self.script_host.borrow_mut() = None;
            self.ensure_script_host();
            let ran = {
                let host = self.script_host.borrow();
                host.as_ref().map(|h| h.start_scripts()).unwrap_or(0)
            };
            self.playing = true;
            self.playing_changed();
            self.console_output(
                QString::from("info"),
                QString::from(format!("Play (solo): {ran} script(s) started")),
            );
            self.drain_script_logs();
        } else {
            *self.script_host.borrow_mut() = None;
            self.playing = false;
            self.playing_changed();
            self.console_output(QString::from("info"), QString::from("Play stopped."));
        }
    }),
    toolbox_search: qt_method!(fn toolbox_search(&mut self, category: i32, keyword: QString) {
        let keyword = keyword.to_string();
        let category = if category <= 0 { 10u32 } else { category as u32 };
        let shared = self.async_state.clone();
        {
            let Ok(mut state) = shared.lock() else { return };
            state.toolbox_busy = true;
            state.toolbox_status = if keyword.trim().is_empty() {
                "Loading featured items…".to_string()
            } else {
                format!("Searching “{}”…", keyword.trim())
            };
            state.toolbox_gen += 1;
        }
        self.sync_async();
        std::thread::spawn(move || {
            let outcome = roblox_api::search(category, &keyword, 30);
            let Ok(mut state) = shared.lock() else { return };
            match outcome {
                Ok(items) => {
                    state.toolbox_json =
                        serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string());
                    state.toolbox_status = if items.is_empty() {
                        "No results.".to_string()
                    } else {
                        format!(
                            "{} result{}",
                            items.len(),
                            if items.len() == 1 { "" } else { "s" }
                        )
                    };
                }
                Err(e) => {
                    state.toolbox_json = "[]".to_string();
                    state.toolbox_status = format!("Search failed: {e}");
                }
            }
            state.toolbox_busy = false;
            state.toolbox_gen += 1;
        });
    }),
    poll_async: qt_method!(fn poll_async(&mut self) {
        self.sync_async();
    }),
    insert_asset: qt_method!(fn insert_asset(&mut self, asset_id: i64, name: QString, asset_type: i32) {
        let label = name.to_string();
        {
            let Ok(mut state) = self.async_state.lock() else { return };
            if state.insert_busy {
                return;
            }
            state.insert_busy = true;
            state.insert_status = format!("Downloading “{label}”…");
            state.insert_gen += 1;
        }
        self.sync_async();
        let shared = self.async_state.clone();
        std::thread::spawn(move || {
            let outcome = insert::build(asset_id, &label, asset_type);
            let Ok(mut state) = shared.lock() else { return };
            match outcome {
                Ok(built) => {
                    state.insert_status = format!(
                        "Parsed “{}” ({} bytes) — inserting…",
                        built.node.name,
                        built.bytes
                    );
                    state.insert_result = Some(PendingInsert {
                        node: built.node,
                        parts: built.parts,
                        label: built.label,
                        bytes: built.bytes,
                        note: built.note,
                    });
                }
                Err(e) => {
                    state.insert_status = format!("Insert failed: {e}");
                    state.insert_result = None;
                }
            }
            state.insert_busy = false;
            state.insert_gen += 1;
        });
    }),
    login_roblox: qt_method!(fn login_roblox(&mut self) {
        if self
            .async_state
            .lock()
            .map(|s| s.login_busy)
            .unwrap_or(false)
        {
            return;
        }
        let path = workspace_root().join("secrets").join("roblox_oauth.json");
        let cfg = match roblox_api::OAuthConfig::load(&path) {
            Ok(cfg) => cfg,
            Err(e) => {
                self.login_status = QString::from(format!("{e}"));
                self.login_changed();
                return;
            }
        };
        let shared = self.async_state.clone();
        {
            let Ok(mut state) = shared.lock() else { return };
            state.login_busy = true;
            state.login_status = "Starting Roblox sign-in…".to_string();
            state.login_gen += 1;
        }
        self.sync_async();
        std::thread::spawn(move || {
            let mut progress = |text: &str| {
                if let Ok(mut state) = shared.lock() {
                    state.login_status = text.to_string();
                    state.login_gen += 1;
                }
            };
            let outcome = roblox_api::login(&cfg, &mut progress);
            let Ok(mut state) = shared.lock() else { return };
            match outcome {
                Ok(session) => {
                    if let Err(e) = roblox_api::save_session(&session) {
                        state.login_status =
                            format!("Signed in, but the session could not be saved: {e}");
                    } else {
                        state.login_status = format!("Signed in as @{}", session.name);
                    }
                    state.login_user = if session.display_name.is_empty() {
                        session.name
                    } else {
                        session.display_name
                    };
                }
                Err(e) => {
                    state.login_status = format!("Sign-in failed: {e}");
                }
            }
            state.login_busy = false;
            state.login_gen += 1;
        });
    }),
    restore_session: qt_method!(fn restore_session(&mut self) {
        let path = workspace_root().join("secrets").join("roblox_oauth.json");
        let shared = self.async_state.clone();
        std::thread::spawn(move || {
            let Ok(Some(mut session)) = roblox_api::load_session() else { return };
            let display = if session.display_name.is_empty() {
                session.name.clone()
            } else {
                session.display_name.clone()
            };
            if let Ok(mut state) = shared.lock() {
                state.login_user = display;
                state.login_status = format!("Restoring session for @{}…", session.name);
                state.login_gen += 1;
            }
            let Ok(cfg) = roblox_api::OAuthConfig::load(&path) else { return };
            let refreshed = roblox_api::refresh(&cfg, &mut session);
            let Ok(mut state) = shared.lock() else { return };
            match refreshed {
                Ok(_) => {
                    let _ = roblox_api::save_session(&session);
                    state.login_status = format!("Signed in as @{}", session.name);
                }
                Err(e) => {
                    let _ = roblox_api::clear_session();
                    state.login_user = String::new();
                    state.login_status = format!("Previous session expired: {e}");
                }
            }
            state.login_gen += 1;
        });
    }),
    sign_out: qt_method!(fn sign_out(&mut self) {
        let _ = roblox_api::clear_session();
        if let Ok(mut state) = self.async_state.lock() {
            state.login_user.clear();
            state.login_status = "Signed out.".to_string();
            state.login_busy = false;
            state.login_gen += 1;
            state.experiences_json = "[]".to_string();
            state.experiences_status = "Sign in to see your experiences.".to_string();
            state.experiences_busy = false;
            state.exp_gen += 1;
        }
        self.sync_async();
    }),
    refresh_experiences: qt_method!(fn refresh_experiences(&mut self) {
        if self
            .async_state
            .lock()
            .map(|s| s.experiences_busy)
            .unwrap_or(false)
        {
            return;
        }
        let path = workspace_root().join("secrets").join("roblox_oauth.json");
        let cfg = match roblox_api::OAuthConfig::load(&path) {
            Ok(cfg) => cfg,
            Err(e) => {
                if let Ok(mut state) = self.async_state.lock() {
                    state.experiences_status = format!("{e}");
                    state.exp_gen += 1;
                }
                self.sync_async();
                return;
            }
        };
        let shared = self.async_state.clone();
        {
            let Ok(mut state) = shared.lock() else { return };
            state.experiences_busy = true;
            state.experiences_status = "Loading your experiences…".to_string();
            state.exp_gen += 1;
        }
        self.sync_async();
        std::thread::spawn(move || {
            let mut progress = |text: &str| {
                if let Ok(mut state) = shared.lock() {
                    state.experiences_status = text.to_string();
                    state.exp_gen += 1;
                }
            };
            let outcome = roblox_api::list_experiences(&cfg, &mut progress);
            let Ok(mut state) = shared.lock() else { return };
            match outcome {
                Ok(items) => {
                    if items.is_empty() {
                        state.experiences_status = "No experiences available. Grant access to this app on the Roblox consent screen (universe:read), then sign in again.".to_string();
                    } else {
                        state.experiences_status = format!(
                            "{} experience{}",
                            items.len(),
                            if items.len() == 1 { "" } else { "s" }
                        );
                    }
                    state.experiences_json =
                        serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string());
                }
                Err(e) => {
                    state.experiences_json = "[]".to_string();
                    state.experiences_status = format!("Could not load experiences: {e}");
                }
            }
            state.experiences_busy = false;
            state.exp_gen += 1;
        });
    }),
    open_experience: qt_method!(fn open_experience(&mut self, universe_id: i64, place_id: i64, name: QString) {
        let name = name.to_string();
        let place_id = if place_id > 0 { place_id as u64 } else { 0 };
        if place_id == 0 {
            if let Ok(mut state) = self.async_state.lock() {
                state.place_status = "This experience has no root place to open.".to_string();
                state.place_gen += 1;
            }
            self.sync_async();
            return;
        }
        if self
            .async_state
            .lock()
            .map(|s| s.place_busy)
            .unwrap_or(false)
        {
            return;
        }
        let shared = self.async_state.clone();
        {
            let Ok(mut state) = shared.lock() else { return };
            state.place_busy = true;
            state.place_status = format!("Downloading “{name}”…");
            state.place_gen += 1;
        }
        self.sync_async();
        std::thread::spawn(move || {
            let outcome = (|| -> anyhow::Result<PendingPlace> {
                let dl = roblox_api::download_place(place_id)?;
                let mut imported = rbx_core::import_place(&dl.bytes, &name)?;
                let (m_resolved, m_failed) = insert::resolve_meshes(&mut imported.scene.parts);
                let (s_resolved, s_failed) = if let Some(sky) = imported.scene.sky.as_mut() {
                    insert::resolve_sky_textures(sky)
                } else {
                    (0, 0)
                };
                let mut extra = Vec::new();
                if m_resolved > 0 {
                    extra.push(format!("{m_resolved} mesh{} resolved", if m_resolved == 1 { "" } else { "es" }));
                }
                if m_failed > 0 {
                    extra.push(format!("{m_failed} mesh{} unavailable", if m_failed == 1 { "" } else { "es" }));
                }
                if s_resolved > 0 {
                    extra.push(format!("{s_resolved} sky face{}", if s_resolved == 1 { "" } else { "s" }));
                }
                if s_failed > 0 {
                    extra.push(format!("{s_failed} sky face{} unavailable", if s_failed == 1 { "" } else { "s" }));
                }
                let note = if extra.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", extra.join(", "))
                };
                Ok(PendingPlace {
                    name,
                    imported,
                    bytes: dl.bytes.len(),
                    note,
                })
            })();
            let Ok(mut state) = shared.lock() else { return };
            match outcome {
                Ok(pending) => {
                    let kb = (pending.bytes / 1024).max(1);
                    state.place_status = format!(
                        "Opening “{}” ({kb} KB, {} parts{})…",
                        pending.name,
                        pending.imported.scene.parts.len(),
                        pending.note
                    );
                    state.place_result = Some(pending);
                }
                Err(e) => {
                    state.place_status = format!("Could not open place: {e}");
                    state.place_result = None;
                }
            }
            state.place_busy = false;
            state.place_gen += 1;
        });
        let _ = universe_id; // reserved for future metadata refreshes
    }),
    // -- Open Cloud key (Settings > Link OpenCloud) -------------------------
    opencloud_refresh: qt_method!(fn opencloud_refresh(&mut self) {
        match roblox_api::load_api_key() {
            Ok(Some(key)) => {
                let mask = roblox_api::mask_key(&key);
                self.apply_opencloud(true, mask, "Linked on this device.".to_string());
            }
            Ok(None) => self.apply_opencloud(false, String::new(), String::new()),
            Err(e) => self.apply_opencloud(
                false,
                String::new(),
                format!("Could not read the stored key: {e}"),
            ),
        }
    }),
    opencloud_save: qt_method!(fn opencloud_save(&mut self, key: QString) {
        let key = key.to_string();
        match roblox_api::save_api_key(&key) {
            Ok(()) => {
                let mask = roblox_api::mask_key(&key);
                self.apply_opencloud(
                    true,
                    mask.clone(),
                    "Linked on this device (stored in Windows Credential Manager).".to_string(),
                );
                self.console_output(
                    QString::from("info"),
                    QString::from(format!(
                        "OpenCloud: API key {mask} linked. Stored in the OS credential store; project files and logs never contain it."
                    )),
                );
            }
            Err(e) => {
                let status = format!("Could not link the key: {e}");
                let mask = self.opencloud_mask.to_string();
                let linked = self.opencloud_linked;
                self.apply_opencloud(linked, mask, status.clone());
                self.console_output(QString::from("error"), QString::from(status));
            }
        }
    }),
    opencloud_unlink: qt_method!(fn opencloud_unlink(&mut self) {
        match roblox_api::clear_api_key() {
            Ok(()) => {
                self.apply_opencloud(
                    false,
                    String::new(),
                    "Open Cloud API key removed from this device.".to_string(),
                );
                self.console_output(
                    QString::from("info"),
                    QString::from("OpenCloud: API key unlinked."),
                );
            }
            Err(e) => {
                let status = format!("Could not remove the key: {e}");
                let mask = self.opencloud_mask.to_string();
                let linked = self.opencloud_linked;
                self.apply_opencloud(linked, mask, status.clone());
                self.console_output(QString::from("error"), QString::from(status));
            }
        }
    }),
    // Rebuilds rendered scene parts from the Workspace explorer subtree (JSON).
    // Best-effort: preserves mesh/decal assets by matching existing parts.
    sync_workspace: qt_method!(fn sync_workspace(&mut self, workspace_json: QString) {
        let text = workspace_json.to_string();
        let node: ExplorerWireNode = match serde_json::from_str(&text) {
            Ok(n) => n,
            Err(e) => {
                self.console_output(QString::from("warn"), QString::from(format!("Workspace sync skipped: {e}")));
                return;
            }
        };
        let mut new_parts = Vec::new();
        collect_parts_from_wire(&node, &mut new_parts);
        // Carry over render-only assets from existing parts when they appear unchanged.
        let old = self.scene.borrow().parts.clone();
        for np in new_parts.iter_mut() {
            if let Some(op) = old.iter().find(|op| {
                op.name == np.name
                    && op.class_name == np.class_name
                    && (op.position.x - np.position.x).abs() < 1e-3
                    && (op.position.y - np.position.y).abs() < 1e-3
                    && (op.position.z - np.position.z).abs() < 1e-3
                    && (op.size.x - np.size.x).abs() < 1e-3
                    && (op.size.y - np.size.y).abs() < 1e-3
                    && (op.size.z - np.size.z).abs() < 1e-3
            }) {
                if np.mesh_file.is_none() { np.mesh_file = op.mesh_file.clone(); }
                if np.decal.is_none() { np.decal = op.decal.clone(); }
                if np.mesh.is_none() { np.mesh = op.mesh.clone(); }
            }
        }
        self.scene.borrow_mut().parts = new_parts;
        self.push_scene_to_viewports();
        *self.dirty.borrow_mut() = true;
    }),
    // Best-effort property edit for a selected renderable part (Workspace path).
    // `path` is the explorer path like "Workspace/Model/Part"; this updates the
    // first matching part by traversed name under Workspace and reuploads.
    set_part_property: qt_method!(fn set_part_property(&mut self, path: QString, name: QString, value: QString) {
        let path = path.to_string();
        let name = name.to_string();
        let value = value.to_string();
        if !path.starts_with("Workspace/") && path != "Workspace" {
            return;
        }
        let target = path.rsplit('/').next().unwrap_or("").to_string();
        let mut scene = self.scene.borrow_mut();
        // Prefer exact name match; if multiple, mutate the first (best-effort).
        let part = scene.parts.iter_mut().find(|p| p.name == target);
        if let Some(p) = part {
            match name.as_str() {
                "Name" => p.name = value.clone(),
                "Position" => {
                    if let Some(v) = parse_vec3(&value) {
                        p.position = v;
                        p.cframe = rbx_core::CFrameValue::from_position(v);
                    }
                }
                "Size" => { if let Some(v) = parse_vec3(&value) { p.size = v; } }
                "Color" | "Color3" => { if let Some(c) = parse_color3(&value) { p.color = c; } }
                "Transparency" => { if let Ok(v) = value.parse::<f32>() { p.transparency = v.clamp(0.0, 1.0); } }
                "Reflectance" => { if let Ok(v) = value.parse::<f32>() { p.reflectance = v.clamp(0.0, 1.0); } }
                "Material" => {
                    p.material = value.clone();
                    // Apply Roblox material defaults on material change.
                    p.color = rbx_render::materials::default_color(&p.material);
                    // Physical defaults (density/friction/elasticity/weights) are available in
                    // rbx_render::materials::default_physical for future physics integration.
                }
                "CastShadow" => { p.cast_shadow = value == "true"; }
                _ => {}
            }
        }
        drop(scene);
        self.push_scene_to_viewports();
        *self.dirty.borrow_mut() = true;
    }),

    // Sets/replaces the active Sky from a JSON object of Sky properties (e.g. SkyboxBk..Up).
    // The sky is stored in the scene and applied to renderer lighting so it becomes the skybox.
    set_sky: qt_method!(fn set_sky(&mut self, sky_json: QString) {
        let text = sky_json.to_string();
        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return,
        };
        let get_s = |k: &str| -> String {
            v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
        };
        let get_f = |k: &str, d: f32| -> f32 {
            v.get(k).and_then(|x| x.as_str()).and_then(|s| s.parse::<f32>().ok())
                .or_else(|| v.get(k).and_then(|x| x.as_f64()).map(|n| n as f32))
                .unwrap_or(d)
        };
        let mut sky = rbx_core::SkySettings {
            skybox_bk: get_s("SkyboxBk"),
            skybox_dn: get_s("SkyboxDn"),
            skybox_ft: get_s("SkyboxFt"),
            skybox_lf: get_s("SkyboxLf"),
            skybox_rt: get_s("SkyboxRt"),
            skybox_up: get_s("SkyboxUp"),
            skybox_bk_file: None,
            skybox_dn_file: None,
            skybox_ft_file: None,
            skybox_lf_file: None,
            skybox_rt_file: None,
            skybox_up_file: None,
            sun_angular_size: get_f("SunAngularSize", 21.0),
            moon_angular_size: get_f("MoonAngularSize", 11.0),
            celestial_bodies_shown: v.get("CelestialBodiesShown").and_then(|x| x.as_str()).map(|s| s == "true").unwrap_or(true),
            star_count: v.get("StarCount").and_then(|x| x.as_str()).and_then(|s| s.parse::<f32>().ok()).unwrap_or(3000.0),
        };
        // Best-effort: resolve sky texture ids to cached files if available.
        let _ = crate::insert::resolve_sky_textures(&mut sky);
        self.scene.borrow_mut().sky = Some(sky.clone());

        let scene_snapshot = self.scene.borrow();
        let lighting = rbx_render::LightingState::from_settings(
            &scene_snapshot.lighting,
            scene_snapshot.sky.as_ref(),
            scene_snapshot.atmosphere.as_ref(),
            &scene_snapshot.post,
        );
        drop(scene_snapshot);
        *self.lighting.borrow_mut() = lighting.clone();
        if let Some(ev) = self.embedded_viewport.borrow().as_ref() {
            ev.set_lighting(lighting.clone());
            ev.set_scene(self.scene.borrow().clone());
        }
        if let Some(nv) = self.native_viewport.borrow().as_ref() {
            nv.set_lighting(lighting.clone());
            nv.set_scene(self.scene.borrow().clone());
        }
        *self.dirty.borrow_mut() = true;
    }),

    // Best-effort background download of Roblox material texture maps into the
    // asset cache so the renderer can use Color/Metalness/Roughness (and normal) data.
    prefetch_material_textures: qt_method!(fn prefetch_material_textures(&mut self) {
        std::thread::spawn(|| {
            for m in rbx_render::materials::MATERIALS {
                let ids = [
                    m.textures.color,
                    m.textures.normal,
                    m.textures.metalness,
                    m.textures.roughness,
                ];
                for id_opt in ids {
                    let Some(id) = id_opt else { continue; };
                    if asset_cache::find(asset_cache::Kind::Texture, id as i64).is_some() {
                        continue;
                    }
                    match roblox_api::download_asset(id as i64) {
                        Ok(dl) => {
                            let ext = asset_cache::ext_for(
                                asset_cache::Kind::Texture,
                                &dl.bytes,
                                &dl.content_type,
                            );
                            let _ = asset_cache::store(
                                asset_cache::Kind::Texture,
                                id as i64,
                                ext,
                                &dl.bytes,
                                dl.asset_type_id,
                                &dl.content_type,
                            );
                        }
                        Err(_) => {
                            // Non-fatal: procedural fallback remains available.
                        }
                    }
                }
            }
        });
    }),
}

impl StudioApp {
    /// Pushes Open Cloud key state into the Qt properties (GUI thread only).
    fn apply_opencloud(&mut self, linked: bool, mask: String, status: String) {
        self.opencloud_linked = linked;
        self.opencloud_mask = QString::from(mask);
        self.opencloud_status = QString::from(status);
        self.opencloud_changed();
    }

    /// Copies worker state into the Qt properties (GUI thread only).
    fn sync_async(&mut self) {
        self.pump_embedded_viewport();
        let seen_insert = *self.seen_insert.borrow();
        let seen_place = *self.seen_place.borrow();
        let snapshot = match self.async_state.lock() {
            Ok(mut state) => {
                let pending_insert = if state.insert_gen != seen_insert {
                    state.insert_result.take()
                } else {
                    None
                };
                let pending_place = if state.place_gen != seen_place {
                    state.place_result.take()
                } else {
                    None
                };
                (
                    (
                        state.toolbox_gen,
                        state.toolbox_json.clone(),
                        state.toolbox_status.clone(),
                        state.toolbox_busy,
                        state.login_gen,
                        state.login_user.clone(),
                        state.login_status.clone(),
                        state.login_busy,
                        state.insert_gen,
                        state.insert_status.clone(),
                        state.insert_busy,
                        state.exp_gen,
                        state.experiences_json.clone(),
                        state.experiences_status.clone(),
                        state.experiences_busy,
                        state.place_gen,
                        state.place_status.clone(),
                        state.place_busy,
                    ),
                    pending_insert,
                    pending_place,
                )
            }
            Err(_) => return,
        };
        let (
            (
                tgen,
                tjson,
                tstatus,
                tbusy,
                lgen,
                luser,
                lstatus,
                lbusy,
                igen,
                istatus,
                ibusy,
                egen,
                ejson,
                estatus,
                ebusy,
                pgen,
                pstatus,
                pbusy,
            ),
            pending_insert,
            pending_place,
        ) = snapshot;
        if tgen != *self.seen_toolbox.borrow() {
            *self.seen_toolbox.borrow_mut() = tgen;
            self.toolbox_json = QString::from(tjson);
            self.toolbox_status = QString::from(tstatus);
            self.toolbox_busy = tbusy;
            self.toolbox_changed();
        }
        if lgen != *self.seen_login.borrow() {
            *self.seen_login.borrow_mut() = lgen;
            self.login_user = QString::from(luser);
            self.login_status = QString::from(lstatus);
            self.login_busy = lbusy;
            self.login_changed();
        }
        if igen != seen_insert {
            *self.seen_insert.borrow_mut() = igen;
            self.insert_status = QString::from(istatus);
            self.insert_busy = ibusy;
            self.insert_changed();
        }
        if egen != *self.seen_exp.borrow() {
            *self.seen_exp.borrow_mut() = egen;
            self.experiences_json = QString::from(ejson);
            self.experiences_status = QString::from(estatus);
            self.experiences_busy = ebusy;
            self.experiences_changed();
        }
        if pgen != seen_place {
            *self.seen_place.borrow_mut() = pgen;
            self.place_status = QString::from(pstatus);
            self.place_busy = pbusy;
            self.place_changed();
        }
        if let Some(pending) = pending_insert {
            self.apply_insert(pending);
        }
        if let Some(pending) = pending_place {
            self.apply_place(pending);
        }
    }

    /// Swaps the open place with one downloaded from Roblox.
    fn apply_place(&mut self, pending: PendingPlace) {
        let PendingPlace {
            name,
            imported,
            bytes,
            note,
        } = pending;
        let part_count = imported.scene.parts.len();
        // Apply place lighting/sky/atmosphere/post to the renderer state.
        let lighting = LightingState::from_settings(
            &imported.scene.lighting,
            imported.scene.sky.as_ref(),
            imported.scene.atmosphere.as_ref(),
            &imported.scene.post,
        );
        let tech_label = match lighting.technology {
            rbx_render::LightingTechnology::Legacy => "Legacy",
            rbx_render::LightingTechnology::Voxel => "Voxel",
            rbx_render::LightingTechnology::Compatibility => "Compatibility",
            rbx_render::LightingTechnology::ShadowMap => "ShadowMap",
            rbx_render::LightingTechnology::Future => "Future",
        }
        .to_string();
        self.lighting_technology = QString::from(tech_label);
        self.lighting_changed();
        *self.lighting.borrow_mut() = lighting.clone();
        *self.scene.borrow_mut() = imported.scene.clone();
        // Reset camera to a sensible default for the new scene.
        let mut cam = Camera::default();
        if let Some((min, max)) = rbx_core::parts_bounds(&imported.scene.parts) {
            let cx = (min.x + max.x) * 0.5;
            let cy = (min.y + max.y) * 0.5;
            let cz = (min.z + max.z) * 0.5;
            let span = ((max.x - min.x).powi(2) + (max.y - min.y).powi(2) + (max.z - min.z).powi(2)).sqrt().max(60.0);
            cam.focus.x = cx;
            cam.focus.y = cy;
            cam.focus.z = cz;
            cam.distance = span * 1.2;
        }
        *self.camera.borrow_mut() = cam.clone();
        if let Some(ev) = self.embedded_viewport.borrow().as_ref() {
            ev.set_camera(cam.clone());
            ev.set_lighting(lighting.clone());
            ev.set_scene(imported.scene.clone());
        }
        if let Some(nv) = self.native_viewport.borrow().as_ref() {
            nv.set_camera(cam);
            nv.set_lighting(lighting.clone());
            nv.set_scene(imported.scene.clone());
        }
        if let Some(r) = self.renderer.borrow_mut().as_mut() {
            r.upload_scene(&imported.scene);
        }
        // Stop any running play session when switching places.
        *self.script_host.borrow_mut() = None;
        if self.playing {
            self.playing = false;
            self.playing_changed();
        }
        *self.dirty.borrow_mut() = true;

        // Rebuild Explorer tree from the place's instance hierarchy.
        let icons_root = workspace_root().join("assets").join("studio").join("icons");
        let registry = IconRegistry::load(&icons_root).ok();
        let nodes: Vec<ExplorerNode> = imported
            .root
            .iter()
            .map(instance_node_to_explorer)
            .collect();
        let tree_json = explorer_to_json(&nodes, registry.as_ref(), &icons_root).to_string();
        self.explorer_load(QString::from(tree_json));

        self.place_name = QString::from(name.clone());
        let kb = (bytes / 1024).max(1);
        self.place_status = QString::from(format!(
            "Opened “{name}” — {part_count} part{}, {kb} KB{note}",
            if part_count == 1 { "" } else { "s" }
        ));
        self.place_changed();
        self.console_output(
            QString::from("info"),
            QString::from(format!(
                "Opened place “{name}” ({part_count} parts, {kb} KB{note})."
            )),
        );
    }
    /// Places a downloaded Toolbox asset into the scene and hands its Explorer
    /// node to QML (which owns the tree model).
    fn apply_insert(&mut self, pending: PendingInsert) {
        let PendingInsert {
            node,
            mut parts,
            label,
            bytes,
            note,
        } = pending;
        if let Some((min, max)) = rbx_core::parts_bounds(&parts) {
            // Centre on the camera's focus point and rest on the ground plane.
            let focus = self.camera.borrow().focus;
            let offset = rbx_core::Vector3::new(
                focus.x - (min.x + max.x) * 0.5,
                -min.y + 0.01,
                focus.z - (min.z + max.z) * 0.5,
            );
            for part in parts.iter_mut() {
                part.translate(offset);
            }
        }
        let part_count = parts.len();
        if part_count > 0 {
            self.scene.borrow_mut().parts.extend(parts);
            let scene = self.scene.borrow().clone();
            if let Some(ev) = self.embedded_viewport.borrow().as_ref() {
                ev.set_scene(scene.clone());
            }
            if let Some(nv) = self.native_viewport.borrow().as_ref() {
                nv.set_scene(scene);
            }
            *self.dirty.borrow_mut() = true;
        }

        // Explorer: QML splices this node under Workspace and selects it.
        let icons_root = workspace_root().join("assets").join("studio").join("icons");
        let registry = IconRegistry::load(&icons_root).ok();
        let explorer_node = instance_node_to_explorer(&node);
        let json = explorer_to_json(
            std::slice::from_ref(&explorer_node),
            registry.as_ref(),
            &icons_root,
        );
        let json = json.get(0).cloned().unwrap_or(Value::Null);
        self.explorer_insert(
            QString::from(json.to_string()),
            QString::from(format!("Workspace/{}", node.name)),
        );

        let kb = (bytes / 1024).max(1);
        let status = if part_count > 0 {
            format!(
                "Inserted “{}” as {label} — {part_count} part{}, {kb} KB ({note})",
                node.name,
                if part_count == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "Inserted “{}” as {label} — {kb} KB ({note})",
                node.name
            )
        };
        self.insert_status = QString::from(status.clone());
        self.insert_changed();
        eprintln!(
            "[toolbox insert] {} -> scene parts: {} (cache: {})",
            status,
            self.scene.borrow().parts.len(),
            asset_cache::describe()
        );
        self.console_output(
            QString::from("info"),
            QString::from(format!("Toolbox: {status}")),
        );
    }

    /// Mirrors the embedded viewport's camera/fps/selection/scene edits back into the shell.
    fn pump_embedded_viewport(&mut self) {
        let (camera, fps, frames, error, selection, scene_edit) = {
            let mut ev = self.embedded_viewport.borrow_mut();
            match ev.as_mut() {
                Some(ev) => (
                    ev.camera(),
                    ev.fps(),
                    ev.frames(),
                    ev.take_error(),
                    ev.take_selection(),
                    ev.take_scene_edit(),
                ),
                None => return,
            }
        };
        if let Some(err) = error {
            *self.embedded_viewport.borrow_mut() = None;
            self.viewport_embedded = false;
            self.viewport_embedded_changed();
            *self.dirty.borrow_mut() = true;
            self.viewport_status = QString::from(format!("embedded viewport stopped: {err}"));
            self.viewport_status_changed();
            return;
        }
        // The child window owns viewport input now, so its camera is the truth.
        *self.camera.borrow_mut() = camera;

        if let Some(scene) = scene_edit.as_ref() {
            // Transform edited in viewport: apply to app scene and refresh properties.
            let sel_name = self.viewport_selected_name.to_string();
            let props = if sel_name.is_empty() {
                None
            } else {
                scene.parts.iter().find(|p| p.name == sel_name).map(|p| {
                    serde_json::json!({
                        "Position": format!("{}, {}, {}", p.position.x, p.position.y, p.position.z),
                        "Size": format!("{}, {}, {}", p.size.x, p.size.y, p.size.z),
                    }).to_string()
                })
            };
            *self.scene.borrow_mut() = scene.clone();
            if let Some(nv) = self.native_viewport.borrow().as_ref() {
                nv.set_scene(scene.clone());
            }
            if let Some(r) = self.renderer.borrow_mut().as_mut() {
                r.upload_scene(scene);
            }
            *self.dirty.borrow_mut() = true;
            if let Some(p) = props {
                self.viewport_selected_props = QString::from(p);
                let rev = self.viewport_selection_rev + 1;
                self.viewport_selection_rev = rev;
                self.viewport_selection_changed();
            }
        }

        if let Some((idx, name)) = selection {
            self.viewport_selected_name = QString::from(name.clone());
            let props = idx
                .and_then(|i| self.scene.borrow().parts.get(i).cloned())
                .map(|p| {
                    serde_json::json!({
                        "Position": format!("{}, {}, {}", p.position.x, p.position.y, p.position.z),
                        "Size": format!("{}, {}, {}", p.size.x, p.size.y, p.size.z),
                    }).to_string()
                })
                .unwrap_or_default();
            self.viewport_selected_props = QString::from(props);
            let rev = self.viewport_selection_rev + 1;
            self.viewport_selection_rev = rev;
            self.viewport_selection_changed();
        }

        let tech = self.lighting_technology.to_string();
        let pace = if fps < 1.0 {
            "draws on demand".to_string()
        } else {
            format!("{fps:.0} fps")
        };
        let text = format!(
            "wgpu | {} | embedded child window (no readback) | {} | Parts:{} | frames:{}",
            if tech.is_empty() { "Future" } else { &tech },
            pace,
            self.scene.borrow().parts.len(),
            frames,
        );
        if self.viewport_status.to_string() != text {
            self.viewport_status = QString::from(text);
            self.viewport_status_changed();
        }
    }
    /// Creates the Luau host on first use and loads the current scene into it.
    fn ensure_script_host(&mut self) {
        if self.script_host.borrow().is_some() {
            return;
        }
        let config = HostConfig::default().with_bundled_api_dump();
        match ScriptHost::new(config) {
            Ok(host) => {
                host.load_scene(&self.scene.borrow());
                *self.script_host.borrow_mut() = Some(host);
            }
            Err(e) => {
                self.console_output(
                    QString::from("error"),
                    QString::from(format!("Luau host failed to start: {e:#}")),
                );
            }
        }
    }

    /// Moves runtime log lines into the Output panel.
    fn drain_script_logs(&mut self) {
        let lines = match self.script_host.borrow().as_ref() {
            Some(host) => host.take_logs(),
            None => return,
        };
        for line in lines {
            self.console_output(
                QString::from(line.level.as_str()),
                QString::from(line.message),
            );
        }
    }

    fn push_scene_to_viewports(&self) {
        let scene = self.scene.borrow().clone();
        if let Some(ev) = self.embedded_viewport.borrow().as_ref() {
            ev.set_scene(scene.clone());
        }
        if let Some(nv) = self.native_viewport.borrow().as_ref() {
            nv.set_scene(scene.clone());
        }
        if let Some(r) = self.renderer.borrow_mut().as_mut() {
            r.upload_scene(&scene);
        }
    }
}

#[derive(Clone, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct ExplorerWireNode {
    name: String,
    class_name: String,
    children: Vec<ExplorerWireNode>,
    props: std::collections::BTreeMap<String, String>,
}

fn is_basepart_class_name(class: &str) -> bool {
    matches!(
        class,
        "Part"
            | "SpawnLocation"
            | "Seat"
            | "VehicleSeat"
            | "Platform"
            | "SkateboardPlatform"
            | "WedgePart"
            | "CornerWedgePart"
            | "TrussPart"
            | "MeshPart"
            | "PartOperation"
            | "UnionOperation"
            | "NegateOperation"
            | "IntersectOperation"
    )
}

fn parse_vec3(s: &str) -> Option<rbx_core::Vector3> {
    let nums: Vec<f32> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse::<f32>().ok())
        .collect();
    if nums.len() >= 3 {
        Some(rbx_core::Vector3::new(nums[0], nums[1], nums[2]))
    } else {
        None
    }
}

fn parse_color3(s: &str) -> Option<[f32; 3]> {
    let nums: Vec<f32> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse::<f32>().ok())
        .collect();
    if nums.len() < 3 {
        return None;
    }
    let mut c = [nums[0], nums[1], nums[2]];
    // Accept 0-255 RGB as well as 0-1.
    if c.iter().any(|v| *v > 1.0) {
        for v in &mut c { *v /= 255.0; }
    }
    Some([c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0), c[2].clamp(0.0, 1.0)])
}

fn part_from_wire(node: &ExplorerWireNode) -> Option<rbx_core::Part> {
    if !is_basepart_class_name(&node.class_name) {
        return None;
    }
    let mut p = rbx_core::Part::default();
    p.name = node.name.clone();
    p.class_name = node.class_name.clone();
    if let Some(v) = node.props.get("Size").and_then(|s| parse_vec3(s)) { p.size = v; }
    if let Some(v) = node.props.get("Color").and_then(|s| parse_color3(s)) { p.color = v; }
    if let Some(v) = node.props.get("Transparency").and_then(|s| s.parse::<f32>().ok()) { p.transparency = v; }
    if let Some(v) = node.props.get("Reflectance").and_then(|s| s.parse::<f32>().ok()) { p.reflectance = v; }
    if let Some(v) = node.props.get("Material") { p.material = v.clone(); }
    if let Some(v) = node.props.get("CastShadow").and_then(|s| match s.as_str() { "true" => Some(true), "false" => Some(false), _ => None }) { p.cast_shadow = v; }
    // CFrame/Position: accept "x, y, z" as position; full 12-component CFrame parsed if present.
    if let Some(cf) = node.props.get("CFrame").and_then(|s| parse_vec3(s)) {
        p.position = cf;
        p.cframe = rbx_core::CFrameValue::from_position(cf);
    }
    if let Some(pos) = node.props.get("Position").and_then(|s| parse_vec3(s)) {
        p.position = pos;
        p.cframe = rbx_core::CFrameValue::from_position(pos);
    }
    // Shape from class or Shape prop
    p.shape = match node.class_name.as_str() {
        "WedgePart" => rbx_core::PartShape::Wedge,
        "CornerWedgePart" => rbx_core::PartShape::CornerWedge,
        _ => match node.props.get("Shape").map(|s| s.as_str()) {
            Some("Ball") => rbx_core::PartShape::Ball,
            Some("Cylinder") => rbx_core::PartShape::Cylinder,
            Some("Wedge") => rbx_core::PartShape::Wedge,
            Some("CornerWedge") => rbx_core::PartShape::CornerWedge,
            _ => rbx_core::PartShape::Block,
        },
    };
    // Preserve mesh/decal references if present in props for known cases.
    if node.class_name == "MeshPart" {
        let mesh_id = node.props.get("MeshId").cloned().unwrap_or_default();
        if !mesh_id.is_empty() {
            p.mesh = Some(rbx_core::MeshRef {
                mesh_id,
                texture_id: node.props.get("TextureID").or_else(|| node.props.get("TextureId")).cloned().unwrap_or_default(),
                mesh_type: Some("MeshPart".to_string()),
                scale: None,
                offset: None,
            });
        }
    }
    // Look for SpecialMesh child providing mesh_file-less ref (best-effort).
    for child in &node.children {
        if child.class_name == "SpecialMesh" || child.class_name == "FileMesh" {
            let mesh_id = child.props.get("MeshId").cloned().unwrap_or_default();
            p.mesh = Some(rbx_core::MeshRef {
                mesh_id,
                texture_id: child.props.get("TextureId").cloned().unwrap_or_default(),
                mesh_type: child.props.get("MeshType").cloned(),
                scale: child.props.get("Scale").and_then(|s| parse_vec3(s)),
                offset: child.props.get("Offset").and_then(|s| parse_vec3(s)),
            });
        }
        if child.class_name == "Decal" {
            // image/texture path not represented in props here; keep existing decal none (best-effort)
            let _ = child;
        }
    }
    Some(p)
}

fn collect_parts_from_wire(node: &ExplorerWireNode, out: &mut Vec<rbx_core::Part>) {
    if let Some(p) = part_from_wire(node) {
        out.push(p);
    }
    for child in &node.children {
        collect_parts_from_wire(child, out);
    }
}

fn workspace_root() -> PathBuf {
    // Dev workspace (source tree)
    let dev = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let dev_canon = dev.canonicalize().unwrap_or_else(|_| dev.clone());
    if dev_canon.join("assets").join("studio").exists() {
        return dev_canon;
    }
    // Installed/runtime: prefer directory next to the executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.join("assets").join("studio").exists() {
                return dir.to_path_buf();
            }
        }
    }
    dev_canon
}

fn file_url(path: &Path) -> String {
    let mut p = path.to_string_lossy().replace('\\', "/");
    if let Some(stripped) = p.strip_prefix("//?/") {
        p = stripped.to_string();
    } else if let Some(stripped) = p.strip_prefix("//./") {
        p = stripped.to_string();
    }
    p = p.replace(' ', "%20");
    if p.starts_with('/') {
        format!("file://{p}")
    } else {
        format!("file:///{p}")
    }
}

fn resolve_icon(icons_root: &Path, rel: &str) -> Option<String> {
    let rel = rel.trim_start_matches('/').replace('\\', "/");
    let path = icons_root.join(&rel);
    if path.is_file() {
        return Some(file_url(&path));
    }
    // Try theme/size fallbacks by filename within same relative area.
    let name = Path::new(&rel).file_name()?.to_string_lossy().to_string();
    let parent = Path::new(&rel).parent()?;
    // Light->Dark fallback if needed
    let dark_rel = rel.replace("/Light/", "/Dark/");
    let dark = icons_root.join(&dark_rel);
    if dark.is_file() {
        return Some(file_url(&dark));
    }
    let _ = (name, parent);
    None
}

fn resolve_label(strings: Option<&Strings>, key_or_label: &str, fallback: &str) -> String {
    if let Some(s) = strings {
        if let Some(v) = s.get(key_or_label) {
            return v.to_string();
        }
    }
    // If it looks like a localization key, prefer the fallback/id.
    if key_or_label.contains('.') {
        return fallback.to_string();
    }
    key_or_label.to_string()
}

fn ribbon_to_json(ribbon: &RibbonDefinition, icons_root: &Path, strings: Option<&Strings>) -> Value {
    let tabs: Vec<Value> = ribbon
        .tabs
        .iter()
        .map(|tab| {
            let tab_label = tab
                .label
                .as_deref()
                .map(|l| resolve_label(strings, l, &tab.id))
                .or_else(|| {
                    tab.label_key
                        .as_deref()
                        .map(|k| resolve_label(strings, k, &tab.id))
                })
                .unwrap_or_else(|| tab.id.clone());
            let groups: Vec<Value> = tab
                .groups
                .iter()
                .map(|group| {
                    let controls: Vec<Value> = group
                        .controls
                        .iter()
                        .map(|c| {
                            let mut icon = String::new();
                            let mut candidates: Vec<&str> = Vec::new();
                            if let Some(i) = c.icon.as_deref() { candidates.push(i); }
                            for i in &c.icon_candidates { candidates.push(i.as_str()); }
                            for rel in candidates {
                                if let Some(u) = resolve_icon(icons_root, rel) {
                                    icon = u;
                                    break;
                                }
                            }
                            json!({
                                "id": c.id,
                                "label": c.display_label(),
                                "detail": c.detail.clone().unwrap_or_default(),
                                "icon": icon,
                                "kind": c.kind.clone().unwrap_or_else(|| "button".to_string()),
                            })
                        })
                        .collect();
                    json!({ "id": group.id, "label": group.label.clone().unwrap_or_default(), "controls": controls })
                })
                .collect();
            json!({ "id": tab.id, "label": tab_label, "dynamic": tab.dynamic, "groups": groups })
        })
        .collect();
    json!({ "tabOrder": ribbon.tab_order, "tabs": tabs })
}

fn tok_color(tokens: &DesignTokens, path: &str, fallback: &str) -> String {
    tokens
        .get(path)
        .and_then(|v| {
            let rgb = v.get("Color3")?.get("$rgb")?.as_array()?;
            Some(format!(
                "#{:02X}{:02X}{:02X}",
                rgb[0].as_f64()? as u8,
                rgb[1].as_f64()? as u8,
                rgb[2].as_f64()? as u8
            ))
        })
        .unwrap_or_else(|| fallback.to_string())
}

#[derive(Clone)]
struct ExplorerNode {
    name: String,
    class_name: String,
    children: Vec<ExplorerNode>,
    expanded: bool,
    /// Real property values for the Properties panel (empty = class defaults).
    props: BTreeMap<String, String>,
}

fn node(name: &str, class_name: &str, expanded: bool, children: Vec<ExplorerNode>) -> ExplorerNode {
    ExplorerNode {
        name: name.to_string(),
        class_name: class_name.to_string(),
        children,
        expanded,
        props: BTreeMap::new(),
    }
}

/// Converts a parsed model's instance tree into the Explorer's tree shape.
fn instance_node_to_explorer(node: &rbx_core::InstanceNode) -> ExplorerNode {
    ExplorerNode {
        name: node.name.clone(),
        class_name: node.class_name.clone(),
        expanded: false,
        children: node.children.iter().map(instance_node_to_explorer).collect(),
        props: node.props.clone(),
    }
}

fn all_insertable_classes(icons: Option<&IconRegistry>) -> Vec<String> {
    let mut classes = Vec::new();
    if let Some(reg) = icons {
        for key in reg.keys() {
            // Shared/InsertableObjects/Dark/Standard/<Class>.png (normalized key has no .png)
            if let Some(rest) = key.strip_prefix("Shared/InsertableObjects/Dark/Standard/") {
                if !rest.contains('/') {
                    classes.push(rest.to_string());
                }
            }
        }
    }
    if classes.is_empty() {
        // Fallback: scan directory
        let dir = workspace_root().join("assets/studio/icons/Shared/InsertableObjects/Dark/Standard");
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) == Some("png") {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        if !stem.ends_with("@2x") && !stem.ends_with("@3x") {
                            classes.push(stem.to_string());
                        }
                    }
                }
            }
        }
    }
    classes.sort();
    classes.dedup();
    classes
}

fn default_explorer_tree(icons: Option<&IconRegistry>) -> Vec<ExplorerNode> {
    let all = all_insertable_classes(icons);
    let all_nodes: Vec<ExplorerNode> = all
        .into_iter()
        .map(|c| node(&c, &c, false, vec![]))
        .collect();

    vec![
        node(
            "Workspace",
            "Workspace",
            true,
            vec![
                node("Camera", "Camera", false, vec![]),
                node("Terrain", "Terrain", false, vec![]),
                node("SpawnLocation", "SpawnLocation", false, vec![]),
                node("Baseplate", "Part", false, vec![]),
            ],
        ),
        node("Players", "Players", false, vec![]),
        node("Lighting", "Lighting", false, vec![]),
        node("MaterialService", "MaterialService", false, vec![]),
        node("ReplicatedFirst", "ReplicatedFirst", false, vec![]),
        node("ReplicatedStorage", "ReplicatedStorage", true, vec![
            node("All Roblox Instances (Test)", "Folder", false, all_nodes),
        ]),
        node("ServerScriptService", "ServerScriptService", false, vec![]),
        node("ServerStorage", "ServerStorage", false, vec![]),
        node(
            "StarterGui",
            "StarterGui",
            false,
            vec![node("ScreenGui", "ScreenGui", false, vec![])],
        ),
        node("StarterPack", "StarterPack", false, vec![]),
        node(
            "StarterPlayer",
            "StarterPlayer",
            false,
            vec![
                node("StarterCharacterScripts", "StarterCharacterScripts", false, vec![]),
                node("StarterPlayerScripts", "StarterPlayerScripts", false, vec![]),
            ],
        ),
        node("Teams", "Teams", false, vec![]),
        node("SoundService", "SoundService", false, vec![]),
        node("Chat", "Chat", false, vec![]),
        node("TextChatService", "TextChatService", false, vec![]),
        node("LocalizationService", "LocalizationService", false, vec![]),
        node("TestService", "TestService", false, vec![]),
    ]
}

fn collect_class_names(nodes: &[ExplorerNode], out: &mut BTreeSet<String>) {
    for n in nodes {
        out.insert(n.class_name.clone());
        collect_class_names(&n.children, out);
    }
}

fn all_enums_json(db: &ReflectionDatabase) -> Value {
    let mut m = serde_json::Map::new();
    for e in db.all_enums() {
        let mut items = e.items.clone();
        items.sort_by(|a, b| a.value.cmp(&b.value).then_with(|| a.name.cmp(&b.name)));
        let items_json: Vec<Value> = items
            .iter()
            .map(|i| json!({"name": i.name, "value": i.value}))
            .collect();
        m.insert(e.name.clone(), json!({"name": e.name, "items": items_json}));
    }
    Value::Object(m)
}

fn class_properties_json(db: &ReflectionDatabase, classes: &BTreeSet<String>) -> Value {
    let mut m = serde_json::Map::new();
    for class in classes {
        let props = db.properties_for_class(class, true, true);
        let props_json: Vec<Value> = props
            .iter()
            .map(|p| {
                let default_value = db.default_value_display(&p.value_type, &p.name, class);
                json!({
                    "name": p.name,
                    "category": p.category,
                    "typeCategory": p.value_type.category, // Primitive|Enum|DataType|Class
                    "typeName": p.value_type.name,
                    "readOnly": p.read_only,
                    "hidden": p.hidden,
                    "deprecated": p.deprecated,
                    "notBrowsable": p.not_browsable,
                    "notScriptable": p.not_scriptable,
                    "canSave": p.can_save,
                    "canLoad": p.can_load,
                    "originClass": p.origin_class,
                    "defaultValue": default_value,
                })
            })
            .collect();
        m.insert(class.clone(), Value::Array(props_json));
    }
    Value::Object(m)
}

fn explorer_to_json(nodes: &[ExplorerNode], icons: Option<&IconRegistry>, icons_root: &Path) -> Value {
    fn to_val(node: &ExplorerNode, icons: Option<&IconRegistry>, icons_root: &Path) -> Value {
        let icon = icons
            .and_then(|r| r.class_icon(&node.class_name, StudioTheme::Dark, IconScale::X1))
            .or_else(|| icons.and_then(|r| r.class_icon("Folder", StudioTheme::Dark, IconScale::X1)))
            .map(|ri| file_url(&ri.path))
            .or_else(|| {
                let p = icons_root.join(format!(
                    "Shared/InsertableObjects/Dark/Standard/{}.png",
                    node.class_name
                ));
                if p.is_file() { Some(file_url(&p)) } else { None }
            })
            .unwrap_or_default();
        json!({
            "name": node.name,
            "className": node.class_name,
            "icon": icon,
            "expanded": node.expanded,
            "props": node.props,
            "children": node.children.iter().map(|c| to_val(c, icons, icons_root)).collect::<Vec<_>>(),
        })
    }
    json!(nodes.iter().map(|n| to_val(n, icons, icons_root)).collect::<Vec<_>>())
}

fn build_qml(
    tokens: &DesignTokens,
    ribbon: &RibbonDefinition,
    root: &Path,
    strings: Option<&Strings>,
    place_name: &str,
    _window_icon: &str,
) -> String {
    let icons_root = root.join("assets").join("studio").join("icons");
    let ribbon_json = serde_json::to_string(&ribbon_to_json(ribbon, &icons_root, strings)).unwrap_or_else(|_| "{}".to_string());
    let icon_registry = IconRegistry::load(&icons_root).ok();
    let explorer_tree = default_explorer_tree(icon_registry.as_ref());
    let explorer_json = serde_json::to_string(&explorer_to_json(
        &explorer_tree,
        icon_registry.as_ref(),
        &icons_root,
    ))
    .unwrap_or_else(|_| "[]".to_string());

    // Classes offered by the Explorer "New" (+) menu, in insertion order.
    let insertable_classes: &[&str] = &[
        "Part",
        "SpawnLocation",
        "Model",
        "Folder",
        "Script",
        "LocalScript",
        "ModuleScript",
        "Attachment",
        "WeldConstraint",
        "BoolValue",
        "IntValue",
        "NumberValue",
        "StringValue",
        "ObjectValue",
        "ScreenGui",
        "Frame",
        "TextLabel",
        "TextButton",
        "ImageLabel",
        "BillboardGui",
        "SurfaceGui",
        "PointLight",
        "SpotLight",
        "ParticleEmitter",
        "Fire",
        "Smoke",
        "Sparkles",
        "Sound",
        "ClickDetector",
        "Camera",
        "Sky",
    ];

    // Roblox API reflection: all enums + class properties (from API dump)
    let reflection = ReflectionDatabase::load_bundled().ok();
    let (all_enums_json_str, class_properties_json_str) = if let Some(db) = reflection.as_ref() {
        let mut classes = BTreeSet::new();
        collect_class_names(&explorer_tree, &mut classes);
        // ensure common bases exist
        for b in ["Instance", "ServiceProvider", "DataModel", "GuiBase2d", "GuiObject"] {
            classes.insert(b.to_string());
        }
        // Every class the "New" menu can create needs reflected properties too.
        for c in insertable_classes {
            classes.insert((*c).to_string());
        }
        let enums = serde_json::to_string(&all_enums_json(db)).unwrap_or_else(|_| "{}".to_string());
        let props = serde_json::to_string(&class_properties_json(db, &classes)).unwrap_or_else(|_| "{}".to_string());
        (enums, props)
    } else {
        ("{}".to_string(), "{}".to_string())
    };
    let class_icons_json = {
        let mut m = serde_json::Map::new();
        for c in insertable_classes {
            let url = resolve_icon(
                &icons_root,
                &format!("Shared/InsertableObjects/Dark/Standard/{c}.png"),
            )
            .unwrap_or_default();
            m.insert((*c).to_string(), Value::String(url));
        }
        serde_json::to_string(&Value::Object(m)).unwrap_or_else(|_| "{}".to_string())
    };
    let insertable_classes_json =
        serde_json::to_string(insertable_classes).unwrap_or_else(|_| "[]".to_string());
    let dropdown_icon = resolve_icon(
        &icons_root,
        "Shared/Ribbon/Dark/Standard/RibbonDropDownArrowDownSmall.png",
    )
    .unwrap_or_default();
    let chevron_right_icon = resolve_icon(
        &icons_root,
        "Shared/Ribbon/Dark/Standard/RibbonChevronRightSmall.png",
    )
    .or_else(|| resolve_icon(&icons_root, "Shared/Navigation/Dark/Standard/ArrowRight.png"))
    .unwrap_or_default();
    let chevron_down_icon = resolve_icon(
        &icons_root,
        "Shared/Ribbon/Dark/Standard/RibbonArrowDownSmall.png",
    )
    .or_else(|| resolve_icon(&icons_root, "Shared/Navigation/Dark/Standard/ArrowDown.png"))
    .unwrap_or_default();

    // Start page icons (Lua/StartPage)
    let sp = |name: &str| {
        resolve_icon(&icons_root, &format!("Lua/StartPage/Dark/Large/{name}.png"))
            .or_else(|| resolve_icon(&icons_root, &format!("Lua/StartPage/Dark/Standard/{name}.png")))
            .unwrap_or_default()
    };
    let sp_home = sp("Home"); let sp_home_f = sp("HomeFilled");
    let sp_exp = sp("Experience"); let sp_exp_f = sp("ExperienceFilled");
    let sp_tpl = sp("Template"); let sp_tpl_f = sp("TemplateFill");
    let sp_recent = sp("Recent"); let sp_recent_f = sp("RecentFilled");
    let sp_search = sp("Search"); let sp_archive = sp("Archive");
    let sp_plus = resolve_icon(&icons_root, "Lua/StartPage/Dark/Standard/Plus.png").unwrap_or_default();
    let studio_logo = file_url(&root.join("assets/studio/branding/RustbloxStudio.png"));
    let icon_script = resolve_icon(&icons_root, "Shared/InsertableObjects/Dark/Standard/Script.png").unwrap_or_default();
    let icon_local_script = resolve_icon(&icons_root, "Shared/InsertableObjects/Dark/Standard/LocalScript.png").unwrap_or_default();
    let icon_viewport = resolve_icon(&icons_root, "Shared/InsertableObjects/Dark/Standard/Workspace.png").unwrap_or_default();

    let font_regular = file_url(&root.join("assets/studio/fonts/BuilderSans-Regular.otf"));
    let font_medium = file_url(&root.join("assets/studio/fonts/BuilderSans-Medium.otf"));
    let font_bold = file_url(&root.join("assets/studio/fonts/BuilderSans-Bold.otf"));
    let font_mono = file_url(&root.join("assets/studio/fonts/FiraMono-Regular.ttf"));

    let bg = tok_color(tokens, "Color.Surface.Surface_0", "#121215");
    let s100 = tok_color(tokens, "Color.Surface.Surface_100", "#191A1F");
    let s200 = tok_color(tokens, "Color.Surface.Surface_200", "#202227");
    let s300 = tok_color(tokens, "Color.Surface.Surface_300", "#272930");
    let stroke = tok_color(tokens, "Color.Stroke.Default", "#2B2D33");
    let text = tok_color(tokens, "Color.Content.Default", "#D5D7DD");
    let text_muted = tok_color(tokens, "Color.Content.Muted", "#BCBEC8");
    let accent = tok_color(tokens, "Color.Content.Link", "#528BFF");
    let hover = tok_color(tokens, "Color.State.Hover", "#272930");

    let _radius = tokens.radius("Medium").unwrap_or(8.0);
    let _pad = tokens.padding("Medium").unwrap_or(12.0);
    let _gap = tokens.gap("Small").unwrap_or(8.0);
    let stroke_w = tokens.stroke("Standard").unwrap_or(1.0);
    let _fs = tokens
        .typography("BodyMedium")
        .map(|t| t.font_size)
        .unwrap_or(18.0);
    let menu_fs = 13.0_f32;
    let tab_fs = 13.0_f32;
    let ribbon_fs = 11.0_f32;

    let active_tab = ribbon.tab_order.first().cloned().unwrap_or_else(|| "Home".to_string());
    let toolbox_categories_json =
        serde_json::to_string(&roblox_api::categories()).unwrap_or_else(|_| "[]".to_string());

    format!(
        r##"import QtQuick 2.15
import QtQuick.Window 2.15
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.15

ApplicationWindow {{
    id: root
    visible: true
    width: 1600
    height: 920
    title: root.placeName + " - Rustblox Studio"
    color: "{bg}"

    property string placeName: "{place_name}"
    property var ribbonDef: {ribbon_json}
    property var explorerTree: {explorer_json}
    property var explorerExpanded: ({{}})
    property string explorerSelected: ""
    property var selectedNode: null
    property string activeTool: "Select"
    property bool snapToGrid: true
    property real moveIncrement: 1.0
    property real rotateIncrement: 15.0
    property real scaleIncrement: 1.0
    property int seenViewportSelRev: 0
    property var allEnums: {all_enums_json}
    property var classProperties: {class_properties_json}
    property var classIcons: {class_icons_json}
    property var insertableClasses: {insertable_classes_json}
    property string explorerFilter: ""
    property int propsRevision: 0
    property var toolboxCategories: {toolbox_categories_json}
    property var toolboxItems: []
    property int toolboxCategory: 10
    property var toolboxCurrent: null
    property var experiences: []
    property string experiencesStatus: ""
    property bool showSettings: false
    property bool showOpenCloudDialog: false
    property string openCloudKey: ""
    property bool openCloudReveal: false
    property string openCloudError: ""
    property var consoleLines: [
        {{ time: "00:00:00", level: "info", text: "Rustblox Studio initialized." }},
        {{ time: "00:00:00", level: "print", text: "API dump properties and enums loaded." }},
        {{ time: "00:00:00", level: "print", text: "Explorer: default DataModel loaded (ReplicatedStorage contains all Roblox instances for testing)." }},
        {{ time: "00:00:00", level: "warn", text: "Renderer/scripting backends are not yet wired (wgpu + mlua planned)." }}
    ]
    property string activeTab: "{active_tab}"
    property bool showStartPage: true
    property string startNav: "Home"
    property var docTabs: [
        {{ id: "viewport", title: root.placeName, kind: "viewport", icon: "{icon_viewport}" }},
        {{ id: "script_1", title: "Script", kind: "script", icon: "{icon_script}", source: "-- Server script\nprint(\"Hello from Rustblox Studio\")\n\nlocal Players = game:GetService(\"Players\")\n" }},
        {{ id: "localscript_1", title: "LocalScript", kind: "script", icon: "{icon_local_script}", source: "-- Local script\nlocal Players = game:GetService(\"Players\")\nlocal player = Players.LocalPlayer\n\nprint(\"Client ready\")\n" }}
    ]
    property string activeDocTab: "viewport"
    property string uiFont: builderSansRegular.name !== "" ? builderSansRegular.name : "Segoe UI"
    property string uiFontMedium: builderSansMedium.name !== "" ? builderSansMedium.name : uiFont
    property string uiFontBold: builderSansBold.name !== "" ? builderSansBold.name : uiFont
    property string monoFont: firaMonoRegular.name !== "" ? firaMonoRegular.name : "Consolas"

    Component.onCompleted: {{
        studio.activeTab = activeTab;
        // Seed default expanded state from tree
        function seed(nodes, prefix) {{
            if (!nodes) return;
            for (var i = 0; i < nodes.length; i++) {{
                var n = nodes[i];
                var path = prefix ? (prefix + "/" + n.name) : n.name;
                if (n.expanded) explorerExpanded[path] = true;
                seed(n.children, path);
            }}
        }}
        seed(explorerTree, "");
        explorerExpandedChanged();
        studio.restore_session();
        studio.opencloud_refresh();
        studio.toolbox_search(10, "");
        studio.prefetch_material_textures();
    }}

    FontLoader {{ id: builderSansRegular; source: "{font_regular}" }}
    FontLoader {{ id: builderSansMedium; source: "{font_medium}" }}
    FontLoader {{ id: builderSansBold; source: "{font_bold}" }}
    FontLoader {{ id: firaMonoRegular; source: "{font_mono}" }}

    Connections {{
        target: studio
        function onConsole_output(level, text) {{
            var d = new Date();
            var ts = Qt.formatTime(d, "hh:mm:ss");
            var lines = root.consoleLines.slice();
            lines.push({{ time: ts, level: level, text: text }});
            root.consoleLines = lines;
        }}
        function onToolbox_changed() {{
            try {{
                root.toolboxItems = JSON.parse(studio.toolbox_json || "[]");
            }} catch (e) {{
                root.toolboxItems = [];
            }}
        }}
        function onOpencloud_changed() {{
            if (studio.opencloud_linked) root.openCloudError = "";
        }}
        function onLogin_changed() {{
            if (studio.login_user !== "" && root.startNav === "Experiences") {{
                studio.refresh_experiences();
            }}
        }}
        function onExperiences_changed() {{
            try {{
                root.experiences = JSON.parse(studio.experiences_json || "[]");
            }} catch (e) {{
                root.experiences = [];
            }}
            root.experiencesStatus = studio.experiences_status || "";
        }}
        function onPlace_changed() {{
            if (studio.place_name && studio.place_name !== "") {{
                root.placeName = studio.place_name;
                root.showStartPage = false;
            }}
        }}
        function onExplorer_load(treeJson) {{
            try {{
                root.explorerTree = JSON.parse(treeJson || "[]");
            }} catch (e) {{
                return;
            }}
            root.explorerExpanded = ({{}});
            root.explorerSelected = "";
            root.selectedNode = null;
            root.explorerFilter = "";
            if (typeof explorerSearch !== "undefined" && explorerSearch) explorerSearch.text = "";
            function seed(nodes, prefix) {{
                if (!nodes) return;
                for (var i = 0; i < nodes.length; i++) {{
                    var n = nodes[i];
                    var path = prefix ? (prefix + "/" + n.name) : n.name;
                    if (n.className === "Workspace") root.explorerExpanded[path] = true;
                    seed(n.children, path);
                }}
            }}
            seed(root.explorerTree, "");
            root.explorerExpanded = Object.assign({{}}, root.explorerExpanded);
        }}
        // A Toolbox asset finished downloading/parsing: splice its node under
        // Workspace and select it. QML owns the Explorer tree.
        function onExplorer_insert(nodeJson, focusPath) {{
            var newNode = null;
            try {{ newNode = JSON.parse(nodeJson); }} catch (e) {{ return; }}
            if (!newNode) return;
            var tree = (root.explorerTree || []).slice();
            var wsIndex = -1;
            for (var i = 0; i < tree.length; i++) {{
                if (tree[i] && tree[i].name === "Workspace") {{ wsIndex = i; break; }}
            }}
            if (wsIndex < 0) return;
            var ws = tree[wsIndex];
            var kids = (ws.children || []).slice();
            // Roblox-style unique sibling names: "Tree", "Tree (2)", …
            var base = newNode.name || "Model";
            var name = base;
            var n = 1;
            while (kids.some(function (k) {{ return k && k.name === name; }})) {{
                n += 1;
                name = base + " (" + n + ")";
            }}
            newNode.name = name;
            kids.push(newNode);
            tree[wsIndex] = {{ name: ws.name, className: ws.className, icon: ws.icon, expanded: true, children: kids }};
            root.explorerTree = tree;
            root.explorerExpanded["Workspace"] = true;
            root.explorerExpanded["Workspace/" + name] = false;
            root.explorerExpanded = Object.assign({{}}, root.explorerExpanded);
            root.explorerSelected = "Workspace/" + name;
            root.selectedNode = newNode;
        }}
    }}

    Timer {{
        interval: 33
        running: studio.playing
        repeat: true
        onTriggered: studio.step_scripts(0.033)
    }}

    // Drains results from the toolbox/API worker threads.
    Timer {{
        interval: 120
        running: true
        repeat: true
        onTriggered: {{
            studio.poll_async();
            if (studio.viewport_selection_rev !== root.seenViewportSelRev) {{
                root.seenViewportSelRev = studio.viewport_selection_rev;
                root.selectByViewport(studio.viewport_selected_name);
            }}
        }}
    }}

    function categoryLabel() {{
        for (var i = 0; i < toolboxCategories.length; i++) {{
            if (toolboxCategories[i].id === toolboxCategory) return toolboxCategories[i].label;
        }}
        return "assets";
    }}

    // -- Settings / Open Cloud key -----------------------------------------
    function openSettings() {{
        root.showSettings = true;
        root.showOpenCloudDialog = false;
        root.openCloudError = "";
        studio.opencloud_refresh();
    }}
    function closeSettings() {{
        root.showSettings = false;
        root.showOpenCloudDialog = false;
        root.openCloudError = "";
    }}
    function openOpenCloudDialog() {{
        openCloudInput.text = "";
        root.openCloudKey = "";
        root.openCloudError = "";
        root.openCloudReveal = false;
        root.showOpenCloudDialog = true;
    }}
    function closeOpenCloudDialog() {{
        root.showOpenCloudDialog = false;
        root.openCloudError = "";
        root.openCloudKey = "";
        openCloudInput.text = "";
    }}
    function submitOpenCloudKey() {{
        var k = (openCloudInput.text || "").trim();
        if (k.length === 0) {{
            root.openCloudError = "Paste an API key first.";
            return;
        }}
        root.openCloudError = "";
        studio.opencloud_save(k);
        root.openCloudKey = "";
        openCloudInput.text = "";
        root.showOpenCloudDialog = false;
    }}
    function ribbonControl(id) {{
        if (id === "Settings") {{
            root.openSettings();
            return;
        }}
        if (id === "Select" || id === "Move" || id === "Scale" || id === "Rotate" || id === "Transform") {{
            root.activeTool = id;
            studio.set_active_tool(id);
            return;
        }}
        if (id === "SnapToGrid") {{
            root.snapToGrid = !root.snapToGrid;
            studio.set_tool_snap(root.snapToGrid, root.moveIncrement, root.rotateIncrement, root.scaleIncrement);
            return;
        }}
    }}

    function tabById(id) {{
        var tabs = ribbonDef.tabs || [];
        for (var i = 0; i < tabs.length; i++) {{ if (tabs[i].id === id) return tabs[i]; }}
        return tabs.length ? tabs[0] : {{ id: id, label: id, dynamic: false, groups: [] }};
    }}

    function isExpanded(path) {{ return explorerExpanded ? !!explorerExpanded[path] : false; }}
    function toggleExpanded(path) {{
        var m = explorerExpanded || ({{}});
        m[path] = !m[path];
        explorerExpanded = m; // reassign to notify bindings
    }}

    function propertiesForSelected() {{
        var _rev = root.propsRevision;
        if (!root.selectedNode) return [];
        var cn = root.selectedNode.className || "Instance";
        var base = root.classProperties ? root.classProperties[cn] : null;
        if (!base || !base.length) {{
            // fallback
            return [
                {{category:"Data",name:"Name",typeCategory:"Primitive",typeName:"string",defaultValue:root.selectedNode.name||cn,readOnly:false,hidden:false,deprecated:false,notBrowsable:false}},
                {{category:"Data",name:"ClassName",typeCategory:"Primitive",typeName:"string",defaultValue:cn,readOnly:true,hidden:false,deprecated:false,notBrowsable:false}}
            ];
        }}
        var out = [];
        var overrides = root.selectedNode.props || null;
        for (var i = 0; i < base.length; i++) {{
            var p = base[i];
            // Hide properties that cannot be edited / should not be surfaced.
            if (p.readOnly || p.notBrowsable || p.hidden) continue;
            var v = p.defaultValue;
            if (p.name === "Name") v = root.selectedNode.name || v;
            if (overrides && overrides[p.name] !== undefined && overrides[p.name] !== "") v = overrides[p.name];
            out.push({{
                category: p.category || "Data",
                name: p.name,
                typeCategory: p.typeCategory || "Primitive",
                typeName: p.typeName || "string",
                value: v,
                readOnly: !!p.readOnly,
                hidden: !!p.hidden,
                deprecated: !!p.deprecated,
                notBrowsable: !!p.notBrowsable,
                notScriptable: !!p.notScriptable,
                originClass: p.originClass || ""
            }});
        }}
        return out;
    }}

    function propCategories(props) {{
        var out = [];
        var seen = {{}};
        if (!props) return out;
        for (var i = 0; i < props.length; i++) {{
            var c = props[i].category || "Data";
            if (!seen[c]) {{ seen[c] = []; out.push({{ name: c, props: seen[c] }}); }}
            seen[c].push(props[i]);
        }}
        return out;
    }}

    function enumItems(enumName) {{
        if (!root.allEnums || !enumName) return [];
        var e = root.allEnums[enumName];
        return e && e.items ? e.items : [];
    }}

    // ---- Explorer search filter ----
    function nodeMatchesFilter(n, q) {{
        if (!n) return false;
        if (String(n.name || "").toLowerCase().indexOf(q) >= 0) return true;
        var kids = n.children || [];
        for (var i = 0; i < kids.length; i++) {{
            if (nodeMatchesFilter(kids[i], q)) return true;
        }}
        return false;
    }}
    function filterNodes(nodes) {{
        var q = String(root.explorerFilter || "").trim().toLowerCase();
        if (q === "") return nodes || [];
        return (nodes || []).filter(function (n) {{ return nodeMatchesFilter(n, q); }});
    }}
    function visibleExplorerRoots() {{ return filterNodes(root.explorerTree); }}
    function visibleExplorerChildren(n) {{ return filterNodes(n ? n.children : []); }}

    // ---- Explorer "New" (+) insertion ----
    function selectedContainerPath() {{
        var target = root.explorerSelected;
        if (!target) return "Workspace";
        var n = nodeByPath(target);
        if (!n) return "Workspace";
        var c = n.className || "";
        var containers = {{
            Workspace: 1, Model: 1, Folder: 1, StarterGui: 1, StarterPack: 1,
            StarterPlayer: 1, StarterPlayerScripts: 1, StarterCharacterScripts: 1,
            ServerScriptService: 1, ServerStorage: 1, ReplicatedStorage: 1,
            ReplicatedFirst: 1, Lighting: 1, SoundService: 1, Teams: 1
        }};
        if (containers[c]) return target;
        return parentPathOf(target) || "Workspace";
    }}
    function filteredInsertables(query) {{
        var list = root.insertableClasses || [];
        var q = String(query || "").trim().toLowerCase();
        if (q === "") return list;
        return list.filter(function (c) {{ return String(c).toLowerCase().indexOf(q) >= 0; }});
    }}
    function insertObject(className) {{
        var parentPath = selectedContainerPath();
        var props = {{}};
        if (className === "Sky") {{
            // Skies belong in Lighting and become the active skybox.
            parentPath = "Lighting";
        }}
        if (className === "Part") {{
            props = {{ Size: "4, 1, 2", Position: "0, 0.5, 0", Color: "0.639216, 0.635294, 0.647059", Material: "Plastic" }};
        }} else if (className === "SpawnLocation") {{
            props = {{ Size: "6, 1, 6", Position: "0, 0.5, 0", Color: "0.639216, 0.635294, 0.647059" }};
        }} else if (className === "Frame") {{
            props = {{ Size: "0, 200, 0, 100", Position: "0, 0, 0, 0", BackgroundColor3: "0.639216, 0.635294, 0.647059" }};
        }} else if (className === "TextLabel" || className === "TextButton") {{
            props = {{ Size: "0, 200, 0, 50", Position: "0, 0, 0, 0", Text: className }};
        }} else if (className === "ImageLabel") {{
            props = {{ Size: "0, 100, 0, 100", Position: "0, 0, 0, 0" }};
        }} else if (className === "Sound") {{
            props = {{ Volume: "0.5" }};
        }} else if (className === "PointLight" || className === "SpotLight") {{
            props = {{ Brightness: "1", Range: "16" }};
        }} else if (className === "Sky") {{
            props = {{
                SkyboxBk: "", SkyboxDn: "", SkyboxFt: "", SkyboxLf: "", SkyboxRt: "", SkyboxUp: "",
                CelestialBodiesShown: "true", StarCount: "3000", SunAngularSize: "21", MoonAngularSize: "11"
            }};
        }}
        var node = {{
            name: className,
            className: className,
            icon: (root.classIcons && root.classIcons[className]) ? root.classIcons[className] : "",
            props: props,
            children: []
        }};
        // A Lighting sky is the active skybox; replace any existing Sky child.
        if (className === "Sky" && parentPath === "Lighting") {{
            var lightingNode = nodeByPath("Lighting");
            if (lightingNode && lightingNode.children) {{
                lightingNode.children = lightingNode.children.filter(function (c) {{ return !c || c.className !== "Sky"; }});
            }}
        }}
        insertNodeAt(parentPath, node, true);
        if (parentPath) root.explorerExpanded[parentPath] = true;
        root.explorerExpanded = Object.assign({{}}, root.explorerExpanded);
        if (className === "Sky") {{
            studio.set_sky(JSON.stringify(node.props || {{}}));
        }}
    }}

    // ---- Color3 helpers / picker ----
    function clampByte(v) {{
        var n = Math.round(Number(v));
        if (isNaN(n)) return 0;
        return Math.max(0, Math.min(255, n));
    }}
    function colorParts(value) {{
        var s = String(value === undefined || value === null ? "" : value);
        var nums = s.split(/[^0-9.+-]+/).filter(function (t) {{ return t.length > 0; }})
                    .map(function (t) {{ return parseFloat(t); }});
        if (nums.length < 3 || isNaN(nums[0]) || isNaN(nums[1]) || isNaN(nums[2])) return [163, 162, 165];
        var mx = Math.max(nums[0], nums[1], nums[2]);
        var scale = mx <= 1.0 ? 255.0 : 1.0;
        return [clampByte(nums[0] * scale), clampByte(nums[1] * scale), clampByte(nums[2] * scale)];
    }}
    function rgbHex(r, g, b) {{
        function h(n) {{ var s = clampByte(n).toString(16); return s.length < 2 ? ("0" + s) : s; }}
        return "#" + h(r) + h(g) + h(b);
    }}
    function hexFromValue(value) {{
        var c = colorParts(value);
        return rgbHex(c[0], c[1], c[2]);
    }}
    function applyColorParts(propName, r, g, b) {{
        if (!propName) return;
        var v = (clampByte(r) / 255).toFixed(6) + ", " + (clampByte(g) / 255).toFixed(6) + ", " + (clampByte(b) / 255).toFixed(6);
        root.setSelectedProperty(propName, v);
    }}
    function openColorPicker(propName, value, anchorItem) {{
        var c = root.colorParts(value);
        colorPopup.propName = propName;
        colorPopup.r = c[0];
        colorPopup.g = c[1];
        colorPopup.b = c[2];
        var w = 268;
        var ih = (typeof colorPopupCol !== "undefined" && colorPopupCol) ? colorPopupCol.implicitHeight : 0;
        var h = ih > 0 ? Math.min(420, Math.max(300, ih + 20)) : 300;
        colorPopup.width = w;
        colorPopup.height = h;
        var p = anchorItem ? anchorItem.mapToItem(null, 0, 0) : {{ x: 0, y: 0 }};
        var px = Math.min(Math.max(4, p.x), root.width - w - 6);
        var py = p.y + (anchorItem ? anchorItem.height : 20) + 2;
        if (py + h > root.height - 6) {{
            var above = p.y - h - 2;
            py = above > 6 ? above : Math.max(6, root.height - h - 6);
        }}
        colorPopup.x = px;
        colorPopup.y = py;
        root.syncColorInputs();
        colorPopup.open();
    }}
    function commitColorFields() {{
        colorPopup.r = clampByte(colorPopup.r);
        colorPopup.g = clampByte(colorPopup.g);
        colorPopup.b = clampByte(colorPopup.b);
        root.applyColorParts(colorPopup.propName, colorPopup.r, colorPopup.g, colorPopup.b);
        root.syncColorInputs();
    }}
    function syncColorInputs() {{
        if (typeof colorHexInput === "undefined" || !colorHexInput) return;
        var c = [colorPopup.r, colorPopup.g, colorPopup.b];
        colorHexInput.text = root.rgbHex(c[0], c[1], c[2]);
        if (typeof rgbRepeater === "undefined" || !rgbRepeater) return;
        for (var i = 0; i < rgbRepeater.count; i++) {{
            var row = rgbRepeater.itemAt(i);
            if (!row) continue;
            var rect = row.children[1];
            if (rect && rect.children.length > 0) rect.children[0].text = String(c[i]);
        }}
    }}
    function applyHexField(text) {{
        var s = String(text || "").trim().replace(/^#/, "");
        if (s.length === 3) s = s[0] + s[0] + s[1] + s[1] + s[2] + s[2];
        if (!/^[0-9a-fA-F]{{6}}$/.test(s)) return;
        colorPopup.r = parseInt(s.substr(0, 2), 16);
        colorPopup.g = parseInt(s.substr(2, 2), 16);
        colorPopup.b = parseInt(s.substr(4, 2), 16);
        root.commitColorFields();
    }}

    function typeDisplay(prop) {{
        var tc = prop.typeCategory || "Primitive";
        var tn = prop.typeName || "";
        if (tc === "Enum") return "Enum." + tn;
        if (tc === "Class") return tn;
        if (tc === "DataType") return tn;
        return tn;
    }}

    property var explorerClipboard: null

    function nodeByPath(path) {{
        if (!path) return null;
        var parts = String(path).split("/");
        var nodes = root.explorerTree || [];
        var cur = null;
        for (var i = 0; i < parts.length; i++) {{
            var found = null;
            for (var j = 0; j < nodes.length; j++) {{
                if (nodes[j] && nodes[j].name === parts[i]) {{ found = nodes[j]; break; }}
            }}
            if (!found) return null;
            cur = found;
            nodes = found.children || [];
        }}
        return cur;
    }}
    function parentPathOf(path) {{
        var p = String(path || "");
        var i = p.lastIndexOf("/");
        return i >= 0 ? p.slice(0, i) : "";
    }}
    function childrenOfPath(path) {{
        if (!path) return root.explorerTree || [];
        var n = nodeByPath(path);
        return n ? (n.children || []) : [];
    }}
    function syncWorkspaceToStudio() {{
        var ws = nodeByPath("Workspace");
        if (ws) studio.sync_workspace(JSON.stringify(ws));
    }}
    function findPathByName(rootNode, prefix, name) {{
        if (!rootNode) return "";
        var path = prefix ? (prefix + "/" + rootNode.name) : rootNode.name;
        if (path.indexOf("Workspace") === 0 && rootNode.name === name) return path;
        var kids = rootNode.children || [];
        for (var i = 0; i < kids.length; i++) {{
            var r = findPathByName(kids[i], path, name);
            if (r) return r;
        }}
        return "";
    }}
    function selectByViewport(name) {{
        var nameStr = (!name || String(name).length === 0) ? "" : String(name);
        var propsJson = studio.viewport_selected_props || "";
        if (nameStr.length === 0) {{
            root.explorerSelected = "";
            root.selectedNode = null;
            root.propsRevision = root.propsRevision + 1;
            return;
        }}
        var node = root.selectedNode;
        var path = root.explorerSelected;
        if (!(node && path && (node.name === nameStr || String(path).indexOf(nameStr) >= 0))) {{
            path = "";
            var roots = root.explorerTree || [];
            for (var i = 0; i < roots.length; i++) {{
                var r = findPathByName(roots[i], "", nameStr);
                if (r) {{ path = r; break; }}
            }}
            if (!path) {{
                var ws = nodeByPath("Workspace");
                if (ws && ws.children) {{
                    for (var j = 0; j < ws.children.length; j++) {{
                        if (ws.children[j] && ws.children[j].name === nameStr) {{
                            path = "Workspace/" + ws.children[j].name;
                            break;
                        }}
                    }}
                }}
            }}
            if (path) {{
                root.explorerSelected = path;
                node = nodeByPath(path);
                root.selectedNode = node;
                root.explorerExpanded["Workspace"] = true;
                root.explorerExpanded = Object.assign({{}}, root.explorerExpanded);
            }}
        }} else {{
            node = nodeByPath(path);
            root.selectedNode = node;
        }}
        if (node && propsJson && String(propsJson).length > 0) {{
            try {{
                var p = JSON.parse(String(propsJson));
                if (!node.props) node.props = {{}};
                for (var k in p) {{ if (p.hasOwnProperty(k)) node.props[k] = String(p[k]); }}
                root.selectedNode = node;
            }} catch (e) {{}}
        }}
        root.propsRevision = root.propsRevision + 1;
    }}
    function uniqueName(parent, base) {{
        var kids = (parent && parent.children) ? parent.children : (root.explorerTree || []);
        var name = base || "Instance";
        var n = 1;
        while (kids.some(function (k) {{ return k && k.name === name; }})) {{
            n += 1;
            name = (base || "Instance") + " (" + n + ")";
        }}
        return name;
    }}
    function deepCloneNode(node) {{
        return JSON.parse(JSON.stringify(node || {{}}));
    }}
    function removeNodeAt(path) {{
        var parentPath = parentPathOf(path);
        var name = String(path).split("/").pop();
        var arr = childrenOfPath(parentPath);
        var idx = -1;
        for (var i = 0; i < arr.length; i++) {{ if (arr[i] && arr[i].name === name) {{ idx = i; break; }} }}
        if (idx < 0) return null;
        var removed = arr.splice(idx, 1)[0];
        if (parentPath === "") root.explorerTree = arr.slice();
        else {{
            var parent = nodeByPath(parentPath);
            if (parent) parent.children = arr;
            root.explorerTree = (root.explorerTree || []).slice();
        }}
        root.explorerSelected = "";
        root.selectedNode = null;
        if (String(path).indexOf("Workspace") === 0) syncWorkspaceToStudio();
        return removed;
    }}
    function insertNodeAt(parentPath, node, selectIt) {{
        var parent = parentPath ? nodeByPath(parentPath) : null;
        var kids = parent ? (parent.children || []) : (root.explorerTree || []);
        var copy = deepCloneNode(node);
        copy.name = uniqueName(parent, copy.name || "Instance");
        kids.push(copy);
        if (parent) parent.children = kids; else root.explorerTree = kids.slice();
        root.explorerTree = (root.explorerTree || []).slice();
        var newPath = (parentPath ? (parentPath + "/") : "") + copy.name;
        if (selectIt) {{
            root.explorerSelected = newPath;
            root.selectedNode = copy;
        }}
        if (String(newPath).indexOf("Workspace") === 0) syncWorkspaceToStudio();
        return newPath;
    }}
    function copySelected() {{
        var n = nodeByPath(root.explorerSelected);
        if (n) root.explorerClipboard = deepCloneNode(n);
    }}
    function cutSelected() {{
        if (!root.explorerSelected) return;
        copySelected();
        removeNodeAt(root.explorerSelected);
    }}
    function pasteClipboard() {{
        if (!root.explorerClipboard) return;
        var target = root.explorerSelected || "Workspace";
        var tnode = nodeByPath(target);
        // If selected is not a container-ish node, paste next to it (into its parent).
        var parentPath = target;
        if (tnode && tnode.className && tnode.className.indexOf("Service") < 0 && tnode.className !== "Folder" && tnode.className !== "Model" && tnode.className !== "Workspace" && tnode.className !== "StarterGui" && tnode.className !== "StarterPlayer" && tnode.className !== "StarterPack" && (!tnode.children || tnode.children.length === 0)) {{
            parentPath = parentPathOf(target) || "Workspace";
        }}
        if (!parentPath) parentPath = "Workspace";
        insertNodeAt(parentPath, root.explorerClipboard, true);
    }}
    function duplicateSelected() {{
        if (!root.explorerSelected) return;
        var n = nodeByPath(root.explorerSelected);
        if (!n) return;
        var parentPath = parentPathOf(root.explorerSelected) || "Workspace";
        insertNodeAt(parentPath, n, true);
    }}
    function deleteSelected() {{
        if (!root.explorerSelected) return;
        removeNodeAt(root.explorerSelected);
    }}
    function setSelectedProperty(propName, value) {{
        if (!root.selectedNode || !root.explorerSelected) return;
        var node = nodeByPath(root.explorerSelected);
        if (!node) return;
        if (propName === "Name") {{
            // Rename: update path-dependent selection by re-keying via remove/insert semantics is complex;
            // instead mutate name in place and fix selection path.
            var oldPath = root.explorerSelected;
            var parentPath = parentPathOf(oldPath);
            var desired = String(value);
            node.name = uniqueName(parentPath ? nodeByPath(parentPath) : null, desired);
            // If uniqueName bumped, reflect desired; keep it simple: set then ensure uniqueness by manual check not necessary except collisions.
            node.name = desired;
            // Recompute unique among siblings excluding self - simple pass:
            var siblings = childrenOfPath(parentPath);
            var base = node.name;
            var finalName = base; var k = 1;
            while (siblings.filter(function(s){{return s===node;}}).length >= 0 && siblings.some(function(s){{return s!==node && s.name===finalName;}})) {{ k++; finalName = base + " (" + k + ")"; }}
            node.name = finalName;
            var newPath = (parentPath ? (parentPath + "/") : "") + node.name;
            root.explorerSelected = newPath;
            root.selectedNode = node;
            root.explorerTree = (root.explorerTree || []).slice();
            root.propsRevision = root.propsRevision + 1;
            studio.set_part_property(newPath, "Name", node.name);
            if (String(newPath).indexOf("Workspace") === 0) syncWorkspaceToStudio();
            return;
        }}
        if (!node.props) node.props = {{}};
        node.props[propName] = String(value);
        root.selectedNode = node;
        root.explorerTree = (root.explorerTree || []).slice();
        root.propsRevision = root.propsRevision + 1;
        studio.set_part_property(root.explorerSelected, propName, String(value));
        if (String(root.explorerSelected).indexOf("Workspace") === 0) {{
            // For transform/structural-affecting props, full sync keeps render accurate.
            if (propName === "Position" || propName === "Size" || propName === "CFrame" || propName === "Shape" || propName === "Color") syncWorkspaceToStudio();
        }}
    }}

    function escHtml(s) {{
        return String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;");
    }}

    function highlightLuau(code) {{
        if (code === null || code === undefined) return "";
        var s = String(code);
        // Roblox Studio Dark theme syntax colors (hex)
        var cBackground = "#202227";
        var cText = "#BCBEC8";
        var cKeyword = "#EB7973";      // keyword/local/function/self/luau keyword
        var cBool = "#F2BA2A";         // bool
        var cNil = "#F2BA2A";          // nil
        var cNumber = "#F2BA2A";       // number
        var cString = "#8EE9B6";       // string
        var cComment = "#6A6F81";      // comment/TODO
        var cBuiltin = "#8FB4FF";      // built-in function
        var cFunction = "#FAE4AA";     // function name
        var cMethod = "#FAE4AA";       // method
        var cProperty = "#70A0FF";     // property
        var cType = "#528BFF";         // type
        var cOperator = "#BCBEC8";     // operator
        var cBracket = "#BCBEC8";      // bracket
        var cSelectionBg = "#13235D";  // selection background

        var keywords = {{
            "and":1,"break":1,"continue":1,"do":1,"else":1,"elseif":1,"end":1,
            "export":1,"for":1,"function":1,"if":1,"in":1,"local":1,
            "not":1,"or":1,"repeat":1,"return":1,"self":1,"then":1,
            "type":1,"typeof":1,"until":1,"while":1
        }};
        var constants = {{ "true":cBool, "false":cBool, "nil":cNil }};
        var builtins = {{
            "print":1,"warn":1,"error":1,"require":1,"pcall":1,"xpcall":1,"typeof":1,"type":1,
            "tostring":1,"tonumber":1,"pairs":1,"ipairs":1,"next":1,"select":1,"unpack":1,
            "rawget":1,"rawset":1,"rawequal":1,"rawlen":1,"getmetatable":1,"setmetatable":1,
            "newproxy":1,"gcinfo":1,"collectgarbage":1,"assert":1,"loadstring":1,
            "delay":1,"elapsedTime":1,"spawn":1,"tick":1,"time":1,"wait":1,
            "settings":1,"UserSettings":1,"Version":1
        }};
        var servicesGlobals = {{
            "game":cProperty,"workspace":cProperty,"script":cProperty,"plugin":cProperty,
            "shared":cProperty,"_G":cProperty,"_VERSION":cBuiltin
        }};
        var librariesTypes = {{
            "task":cBuiltin,"math":cBuiltin,"string":cBuiltin,"table":cBuiltin,"os":cBuiltin,
            "debug":cBuiltin,"coroutine":cBuiltin,"utf8":cBuiltin,"buffer":cBuiltin,"bit32":cBuiltin,
            "Instance":cType,"Vector2":cType,"Vector3":cType,"CFrame":cType,"Color3":cType,
            "UDim":cType,"UDim2":cType,"Rect":cType,"BrickColor":cType,"Enum":cType,
            "Ray":cType,"TweenInfo":cType,"NumberSequence":cType,"ColorSequence":cType,
            "NumberRange":cType,"PhysicalProperties":cType,"DateTime":cType,"Random":cType,
            "Path":cType,"Region3":cType,"Region3int16":cType,"Vector2int16":cType,"Vector3int16":cType,
            "OverlapParams":cType,"RaycastParams":cType,"Axes":cType,"Faces":cType,"DockWidgetPluginGuiInfo":cType,
            "NumberSequenceKeypoint":cType,"ColorSequenceKeypoint":cType,"PathWaypoint":cType,
            "Path2DControlPoint":cType,"Font":cType,"FloatCurveKey":cType,"RotationCurveKey":cType
        }};

        var i = 0, out = "", n = s.length;
        function span(col, t) {{ return "<span style='color:" + col + ";'>" + escHtml(t) + "</span>"; }}
        while (i < n) {{
            var ch = s[i], nx = i+1 < n ? s[i+1] : "";
            // block comment --[[ ... ]] (simple)
            if (ch === "-" && nx === "-" && i+3 < n && s[i+2] === "[" && s[i+3] === "[") {{
                var j = s.indexOf("]]", i+4);
                if (j < 0) j = n; else j += 2;
                out += span(cComment, s.slice(i,j)); i = j; continue;
            }}
            // line comment
            if (ch === "-" && nx === "-") {{
                var j = s.indexOf("\n", i); if (j < 0) j = n;
                out += span(cComment, s.slice(i,j)); i = j; continue;
            }}
            // long string [[...]]
            if (ch === "[" && nx === "[") {{
                var j = s.indexOf("]]", i+2);
                if (j < 0) j = n; else j += 2;
                out += span(cString, s.slice(i,j)); i = j; continue;
            }}
            // string
            if (ch === "\"" || ch === "'") {{
                var q = ch, j = i+1;
                while (j < n) {{
                    if (s[j] === "\\") {{ j += 2; continue; }}
                    if (s[j] === q) {{ j++; break; }}
                    if (s[j] === "\n") break;
                    j++;
                }}
                out += span(cString, s.slice(i,j)); i = j; continue;
            }}
            // number
            if ((ch >= "0" && ch <= "9") || (ch === "." && nx >= "0" && nx <= "9")) {{
                var j = i;
                if (ch === "0" && (nx === "x" || nx === "X")) {{
                    j += 2; while (j < n && /[0-9a-fA-F_]/.test(s[j])) j++;
                }} else {{
                    while (j < n && /[0-9_]/.test(s[j])) j++;
                    if (j < n && s[j] === ".") {{ j++; while (j < n && /[0-9_]/.test(s[j])) j++; }}
                    if (j < n && (s[j] === "e" || s[j] === "E")) {{
                        j++; if (j < n && (s[j]==="+"||s[j]==="-")) j++;
                        while (j < n && /[0-9_]/.test(s[j])) j++;
                    }}
                }}
                out += span(cNumber, s.slice(i,j)); i = j; continue;
            }}
            // identifier / keyword
            if ((ch >= "A" && ch <= "Z") || (ch >= "a" && ch <= "z") || ch === "_") {{
                var j = i+1;
                while (j < n && /[A-Za-z0-9_]/.test(s[j])) j++;
                var w = s.slice(i,j);
                if (constants[w]) out += span(constants[w], w);
                else if (keywords[w]) out += span(cKeyword, w);
                else if (builtins[w]) out += span(cBuiltin, w);
                else if (servicesGlobals[w]) out += span(servicesGlobals[w], w);
                else if (librariesTypes[w]) out += span(librariesTypes[w], w);
                else out += span(cText, w);
                i = j; continue;
            }}
            // brackets / operators / punct
            if ("(){{}}[]".indexOf(ch) >= 0) {{
                out += span(cBracket, ch); i++; continue;
            }}
            if ("+-*/%^#=<>&|~;:,.".indexOf(ch) >= 0) {{
                out += span(cOperator, ch); i++; continue;
            }}
            // whitespace & other
            if (ch === "\n") {{ out += "\n"; i++; continue; }}
            if (ch === "\t" || ch === " ") {{ out += escHtml(ch); i++; continue; }}
            out += span(cText, ch); i++;
        }}
        // default foreground matches Text Color
        return "<pre style='margin:0;color:" + cText + ";background:transparent;'>" + out + "</pre>";
    }}

    Component {{
        id: explorerNodeDelegate
        Column {{
            id: nodeCol
            width: parent ? parent.width : 0
            property var node: (typeof nodeData !== "undefined" && nodeData) ? nodeData : ((typeof modelData !== "undefined") ? modelData : ({{}}))
            property string nodePath: (typeof parentPath !== "undefined" ? parentPath : "")
            property int depth: (typeof nodeDepth !== "undefined" ? nodeDepth : 0)
            property string fullPath: nodePath ? (nodePath + "/" + node.name) : node.name
            property var visibleKids: root.visibleExplorerChildren(node)
            property bool hasKids: visibleKids && visibleKids.length > 0
            property bool expanded: {{ if (String(root.explorerFilter || "").trim() !== "") return true; var _e = root.explorerExpanded; return _e ? !!_e[fullPath] : false; }}

            Rectangle {{
                width: nodeCol.width
                height: 22
                color: root.explorerSelected === nodeCol.fullPath ? "{accent}" : (rowMa.containsMouse ? "{hover}" : "transparent")
                opacity: root.explorerSelected === nodeCol.fullPath ? 0.22 : 1.0
                Rectangle {{
                    anchors.fill: parent
                    color: "transparent"
                    border.width: 0
                }}
                Row {{
                    anchors.left: parent.left
                    anchors.leftMargin: 4 + nodeCol.depth * 14
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 2
                    Item {{
                        width: 12; height: 12
                        anchors.verticalCenter: parent.verticalCenter
                        Image {{
                            anchors.centerIn: parent
                            width: 10; height: 10
                            visible: nodeCol.hasKids
                            source: nodeCol.expanded ? "{chevron_down_icon}" : "{chevron_right_icon}"
                            fillMode: Image.PreserveAspectFit
                            smooth: true
                            opacity: 0.9
                        }}
                        MouseArea {{
                            anchors.fill: parent
                            enabled: nodeCol.hasKids
                            onClicked: root.toggleExpanded(nodeCol.fullPath)
                        }}
                    }}
                    Image {{
                        width: 16; height: 16
                        anchors.verticalCenter: parent.verticalCenter
                        source: node.icon || ""
                        fillMode: Image.PreserveAspectFit
                        smooth: true
                        mipmap: true
                        visible: (node.icon || "") !== ""
                    }}
                    Rectangle {{
                        width: 16; height: 16
                        visible: !(node.icon || "")
                        color: "{s300}"; radius: 2
                        anchors.verticalCenter: parent.verticalCenter
                    }}
                    Text {{
                        anchors.verticalCenter: parent.verticalCenter
                        text: node.name
                        color: "{text}"
                        font.family: root.uiFont
                        font.pixelSize: 12
                        elide: Text.ElideRight
                    }}
                }}
                MouseArea {{
                    id: rowMa
                    anchors.fill: parent
                    hoverEnabled: true
                    acceptedButtons: Qt.LeftButton | Qt.RightButton
                    onClicked: function(mouse) {{
                        root.explorerSelected = nodeCol.fullPath;
                        root.selectedNode = node;
                        if (mouse.button === Qt.RightButton) {{
                            explorerContextMenu.popup();
                        }} else {{
                            if (nodeCol.hasKids) root.toggleExpanded(nodeCol.fullPath);
                        }}
                    }}
                }}
            }}

            Column {{
                width: nodeCol.width
                visible: nodeCol.hasKids && nodeCol.expanded
                Repeater {{
                    model: nodeCol.visibleKids
                    delegate: Loader {{
                        width: nodeCol.width
                        property var nodeData: modelData
                        property string parentPath: nodeCol.fullPath
                        property int nodeDepth: nodeCol.depth + 1
                        sourceComponent: explorerNodeDelegate
                    }}
                }}
            }}
        }}
    }}

    ColumnLayout {{
        anchors.fill: parent
        spacing: 0

        // Menu bar
        Rectangle {{
            Layout.fillWidth: true
            Layout.preferredHeight: 30
            color: "{bg}"
            RowLayout {{
                anchors.fill: parent
                anchors.leftMargin: 8
                anchors.rightMargin: 8
                spacing: 2

                component MenuTab: Rectangle {{
                    id: mtab
                    property string label
                    property var menuObj
                    Layout.preferredHeight: 24
                    Layout.preferredWidth: mtabLabel.implicitWidth + 14
                    radius: 4
                    color: (mtabMa.containsMouse || (menuObj && menuObj.visible)) ? "{s200}" : "transparent"
                    Text {{ id: mtabLabel; anchors.centerIn: parent; text: mtab.label; color: "{text}"; font.family: root.uiFont; font.pixelSize: {menu_fs} }}
                    MouseArea {{
                        id: mtabMa; anchors.fill: parent; hoverEnabled: true
                        onClicked: if (mtab.menuObj) mtab.menuObj.popup(mtab, 0, mtab.height)
                    }}
                }}

                Menu {{
                    id: fileMenu
                    background: Rectangle {{ implicitWidth: 220; color: "{s200}"; border.width: 1; border.color: "{stroke}" }}
                    delegate: MenuItem {{
                        id: mi; implicitHeight: 28
                        contentItem: Text {{ leftPadding: 10; text: mi.text; color: mi.enabled ? "{text}" : "{text_muted}"; font.family: root.uiFont; font.pixelSize: 12; verticalAlignment: Text.AlignVCenter }}
                        background: Rectangle {{ color: mi.highlighted ? "{hover}" : "transparent" }}
                    }}
                    MenuItem {{ text: "New"; enabled: false }}
                    MenuItem {{ text: "Open from File..."; enabled: false }}
                    MenuItem {{ text: "Open from Roblox..."; enabled: false }}
                    MenuSeparator {{ contentItem: Rectangle {{ implicitHeight: 1; color: "{stroke}" }} }}
                    MenuItem {{ text: "Save to File"; enabled: false }}
                    MenuItem {{ text: "Save to File As..."; enabled: false }}
                    MenuItem {{ text: "Publish to Roblox"; enabled: false }}
                    MenuSeparator {{ contentItem: Rectangle {{ implicitHeight: 1; color: "{stroke}" }} }}
                    MenuItem {{ text: "Studio Settings..."; enabled: true; onTriggered: root.openSettings() }}
                    MenuItem {{ text: "Close Place"; enabled: true; onTriggered: {{ root.startNav = "Home"; root.showStartPage = true }} }}
                }}
                Menu {{
                    id: editMenu
                    background: Rectangle {{ implicitWidth: 240; color: "{s200}"; border.width: 1; border.color: "{stroke}" }}
                    delegate: MenuItem {{
                        id: mi2; implicitHeight: 28
                        contentItem: Text {{ leftPadding: 10; text: mi2.text; color: mi2.enabled ? "{text}" : "{text_muted}"; font.family: root.uiFont; font.pixelSize: 12; verticalAlignment: Text.AlignVCenter }}
                        background: Rectangle {{ color: mi2.highlighted ? "{hover}" : "transparent" }}
                    }}
                    MenuItem {{ text: "Undo"; enabled: false }}
                    MenuItem {{ text: "Redo"; enabled: false }}
                    MenuSeparator {{ contentItem: Rectangle {{ implicitHeight: 1; color: "{stroke}" }} }}
                    MenuItem {{ text: "Cut\tCtrl+X"; enabled: root.explorerSelected !== ""; onTriggered: root.cutSelected() }}
                    MenuItem {{ text: "Copy\tCtrl+C"; enabled: root.explorerSelected !== ""; onTriggered: root.copySelected() }}
                    MenuItem {{ text: "Paste\tCtrl+V"; enabled: root.explorerClipboard !== null; onTriggered: root.pasteClipboard() }}
                    MenuItem {{ text: "Duplicate\tCtrl+D"; enabled: root.explorerSelected !== ""; onTriggered: root.duplicateSelected() }}
                    MenuItem {{ text: "Delete\tDel"; enabled: root.explorerSelected !== ""; onTriggered: root.deleteSelected() }}
                    MenuSeparator {{ contentItem: Rectangle {{ implicitHeight: 1; color: "{stroke}" }} }}
                    MenuItem {{ text: "Select All"; enabled: false }}
                }}
                Menu {{
                    id: viewMenu
                    background: Rectangle {{ implicitWidth: 220; color: "{s200}"; border.width: 1; border.color: "{stroke}" }}
                    delegate: MenuItem {{
                        id: mi3; implicitHeight: 28
                        contentItem: Text {{ leftPadding: 10; text: mi3.text; color: mi3.enabled ? "{text}" : "{text_muted}"; font.family: root.uiFont; font.pixelSize: 12; verticalAlignment: Text.AlignVCenter }}
                        background: Rectangle {{ color: mi3.highlighted ? "{hover}" : "transparent" }}
                    }}
                    MenuItem {{ text: "Explorer"; enabled: false }}
                    MenuItem {{ text: "Properties"; enabled: false }}
                    MenuItem {{ text: "Output"; enabled: false }}
                    MenuItem {{ text: "Toolbox"; enabled: false }}
                    MenuItem {{ text: "Command Bar"; enabled: false }}
                    MenuSeparator {{ contentItem: Rectangle {{ implicitHeight: 1; color: "{stroke}" }} }}
                    MenuItem {{ text: "View Selector"; enabled: false }}
                }}
                Menu {{
                    id: pluginsMenu
                    background: Rectangle {{ implicitWidth: 220; color: "{s200}"; border.width: 1; border.color: "{stroke}" }}
                    delegate: MenuItem {{
                        id: mi4; implicitHeight: 28
                        contentItem: Text {{ leftPadding: 10; text: mi4.text; color: mi4.enabled ? "{text}" : "{text_muted}"; font.family: root.uiFont; font.pixelSize: 12; verticalAlignment: Text.AlignVCenter }}
                        background: Rectangle {{ color: mi4.highlighted ? "{hover}" : "transparent" }}
                    }}
                    MenuItem {{ text: "Manage Plugins"; enabled: false }}
                    MenuItem {{ text: "Plugins Folder"; enabled: false }}
                }}
                Menu {{
                    id: testMenu
                    background: Rectangle {{ implicitWidth: 220; color: "{s200}"; border.width: 1; border.color: "{stroke}" }}
                    delegate: MenuItem {{
                        id: mi5; implicitHeight: 28
                        contentItem: Text {{ leftPadding: 10; text: mi5.text; color: mi5.enabled ? "{text}" : "{text_muted}"; font.family: root.uiFont; font.pixelSize: 12; verticalAlignment: Text.AlignVCenter }}
                        background: Rectangle {{ color: mi5.highlighted ? "{hover}" : "transparent" }}
                    }}
                    MenuItem {{ text: "Play"; enabled: !studio.playing; onTriggered: studio.set_playing(true) }}
                    MenuItem {{ text: "Play Here"; enabled: false }}
                    MenuItem {{ text: "Run"; enabled: false }}
                    MenuItem {{ text: "Stop"; enabled: studio.playing; onTriggered: studio.set_playing(false) }}
                    MenuSeparator {{ contentItem: Rectangle {{ implicitHeight: 1; color: "{stroke}" }} }}
                    MenuItem {{ text: "Test Service"; enabled: false }}
                }}
                Menu {{
                    id: windowMenu
                    background: Rectangle {{ implicitWidth: 220; color: "{s200}"; border.width: 1; border.color: "{stroke}" }}
                    delegate: MenuItem {{
                        id: mi6; implicitHeight: 28
                        contentItem: Text {{ leftPadding: 10; text: mi6.text; color: mi6.enabled ? "{text}" : "{text_muted}"; font.family: root.uiFont; font.pixelSize: 12; verticalAlignment: Text.AlignVCenter }}
                        background: Rectangle {{ color: mi6.highlighted ? "{hover}" : "transparent" }}
                    }}
                    MenuItem {{ text: "Reset View"; enabled: false }}
                    MenuSeparator {{ contentItem: Rectangle {{ implicitHeight: 1; color: "{stroke}" }} }}
                    MenuItem {{ text: "Start Page"; enabled: true; onTriggered: {{ root.showStartPage = true }} }}
                }}
                Menu {{
                    id: helpMenu
                    background: Rectangle {{ implicitWidth: 220; color: "{s200}"; border.width: 1; border.color: "{stroke}" }}
                    delegate: MenuItem {{
                        id: mi7; implicitHeight: 28
                        contentItem: Text {{ leftPadding: 10; text: mi7.text; color: mi7.enabled ? "{text}" : "{text_muted}"; font.family: root.uiFont; font.pixelSize: 12; verticalAlignment: Text.AlignVCenter }}
                        background: Rectangle {{ color: mi7.highlighted ? "{hover}" : "transparent" }}
                    }}
                    MenuItem {{ text: "Documentation"; enabled: false }}
                    MenuItem {{ text: "About Rustblox Studio"; enabled: false }}
                }}

                MenuTab {{ label: "File"; menuObj: fileMenu }}
                MenuTab {{ label: "Edit"; menuObj: editMenu }}
                MenuTab {{ label: "View"; menuObj: viewMenu }}
                MenuTab {{ label: "Plugins"; menuObj: pluginsMenu }}
                MenuTab {{ label: "Test"; menuObj: testMenu }}
                MenuTab {{ label: "Window"; menuObj: windowMenu }}
                MenuTab {{ label: "Help"; menuObj: helpMenu }}

                Item {{ Layout.fillWidth: true }}
                Text {{ text: root.placeName + " - Rustblox Studio"; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 12 }}
                Item {{ width: 6 }}
            }}
            Rectangle {{ anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom; height: {stroke_w}; color: "{stroke}" }}
        }}

        // Ribbon tab strip
        Rectangle {{
            Layout.fillWidth: true
            Layout.preferredHeight: 34
            color: "{s100}"
            RowLayout {{
                anchors.fill: parent
                anchors.leftMargin: 8
                anchors.bottomMargin: 0
                spacing: 0
                Repeater {{
                    model: ribbonDef.tabOrder || []
                    Item {{
                        property bool active: root.activeTab === modelData
                        property var tab: root.tabById(modelData)
                        Layout.preferredHeight: 34
                        Layout.preferredWidth: tabLabel.implicitWidth + 22
                        Rectangle {{
                            anchors.fill: parent
                            anchors.topMargin: 4
                            anchors.bottomMargin: active ? 0 : 4
                            radius: 4
                            color: active ? "{s100}" : (tabMa.containsMouse ? "{hover}" : "transparent")
                            border.width: active ? {stroke_w} : 0
                            border.color: "{stroke}"
                            // Cover bottom border of active tab so it blends into ribbon content
                            Rectangle {{
                                visible: active
                                anchors.left: parent.left; anchors.right: parent.right
                                anchors.bottom: parent.bottom; anchors.bottomMargin: -1
                                height: 2
                                color: "{s100}"
                            }}
                            Text {{
                                id: tabLabel
                                anchors.centerIn: parent
                                anchors.verticalCenterOffset: active ? 0 : 0
                                text: tab.label || modelData
                                color: active ? "{text}" : "{text_muted}"
                                font.family: active ? root.uiFontMedium : root.uiFont
                                font.pixelSize: {tab_fs}
                            }}
                            MouseArea {{ id: tabMa; anchors.fill: parent; hoverEnabled: true; onClicked: {{ root.activeTab = modelData; studio.activeTab = modelData }} }}
                        }}
                    }}
                }}
                Item {{ Layout.fillWidth: true }}
            }}
            Rectangle {{ anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom; height: {stroke_w}; color: "{stroke}"; z: -1 }}
        }}

        // Ribbon content
        Rectangle {{
            id: ribbonArea
            Layout.fillWidth: true
            Layout.preferredHeight: 96
            color: "{s100}"
            property var activeTabDef: root.tabById(root.activeTab)

            RowLayout {{
                anchors.fill: parent
                anchors.leftMargin: 4
                anchors.rightMargin: 4
                anchors.topMargin: 2
                anchors.bottomMargin: 2
                spacing: 0

                Repeater {{
                    model: ribbonArea.activeTabDef.groups || []
                    delegate: RowLayout {{
                        spacing: 0
                        Layout.fillHeight: true
                        property var group: modelData
                        // Group controls
                        Row {{
                            spacing: 1
                            Layout.alignment: Qt.AlignVCenter
                            Layout.leftMargin: 4
                            Layout.rightMargin: 4
                            Repeater {{
                                model: group.controls || []
                                delegate: Rectangle {{
                                    property var ctl: modelData
                                    width: Math.max(52, ctlLabel.implicitWidth + 20)
                                    height: 86
                                    radius: 4
                                    color: ctlMa.containsMouse ? "{s200}" : "transparent"
                                    border.width: 0
                                    Column {{
                                        anchors.horizontalCenter: parent.horizontalCenter
                                        anchors.verticalCenter: parent.verticalCenter
                                        width: parent.width - 8
                                        spacing: 3
                                        Item {{
                                            width: 32; height: 32
                                            anchors.horizontalCenter: parent.horizontalCenter
                                            Image {{
                                                anchors.centerIn: parent
                                                width: 24; height: 24
                                                source: ctl.icon || ""
                                                fillMode: Image.PreserveAspectFit
                                                smooth: true
                                                mipmap: true
                                                visible: (ctl.icon || "") !== ""
                                            }}
                                            Rectangle {{
                                                anchors.centerIn: parent; width: 22; height: 22; radius: 4
                                                color: "{s300}"; visible: !(ctl.icon || "")
                                                Text {{ anchors.centerIn: parent; text: (ctl.label || "?").charAt(0); color: "{text_muted}"; font.pixelSize: 10 }}
                                            }}
                                        }}
                                        Text {{
                                            id: ctlLabel
                                            width: parent.width
                                            horizontalAlignment: Text.AlignHCenter
                                            text: ctl.label || ctl.id
                                            color: "{text}"
                                            font.family: root.uiFont
                                            font.pixelSize: {ribbon_fs}
                                            wrapMode: Text.NoWrap
                                            elide: Text.ElideRight
                                        }}
                                        Image {{
                                            visible: (ctl.kind === "split_button" || ctl.kind === "dropdown" || ctl.kind === "picker") && "{dropdown_icon}" !== ""
                                            anchors.horizontalCenter: parent.horizontalCenter
                                            width: 8
                                            height: 8
                                            source: "{dropdown_icon}"
                                            fillMode: Image.PreserveAspectFit
                                            smooth: true
                                            opacity: 0.9
                                        }}
                                    }}
                                    MouseArea {{ id: ctlMa; anchors.fill: parent; hoverEnabled: true; cursorShape: Qt.PointingHandCursor; onClicked: root.ribbonControl(ctl.id) }}
                                }}
                            }}
                        }}
                        // Increment changer near Geometric (transform options)
                        Row {{
                            visible: (group.id || "") === "transform_options"
                            spacing: 8
                            Layout.alignment: Qt.AlignVCenter
                            Layout.leftMargin: 4
                            Layout.rightMargin: 6
                            Column {{
                                spacing: 4
                                anchors.verticalCenter: parent.verticalCenter
                                Row {{
                                    spacing: 4
                                    Text {{ text: "Move"; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 10; width: 34; verticalAlignment: Text.AlignVCenter; anchors.verticalCenter: parent.verticalCenter }}
                                    Rectangle {{
                                        width: 54; height: 20; radius: 3; color: "{s200}"; border.width: 1; border.color: "{stroke}"
                                        anchors.verticalCenter: parent.verticalCenter
                                        TextInput {{
                                            anchors.fill: parent; anchors.leftMargin: 4; anchors.rightMargin: 4
                                            verticalAlignment: Text.AlignVCenter
                                            color: "{text}"; selectionColor: "{accent}"; selectedTextColor: "#ffffff"
                                            font.family: root.uiFont; font.pixelSize: 11
                                            selectByMouse: true; clip: true
                                            text: Number(root.moveIncrement).toFixed(3).replace(/\.?0+$/,"")
                                            onEditingFinished: {{
                                                var v = parseFloat(text);
                                                if (!isNaN(v) && v > 0) {{ root.moveIncrement = v; root.scaleIncrement = v; }}
                                                text = Number(root.moveIncrement).toFixed(3).replace(/\.?0+$/,"");
                                                studio.set_tool_snap(root.snapToGrid, root.moveIncrement, root.rotateIncrement, root.scaleIncrement);
                                            }}
                                        }}
                                    }}
                                }}
                                Row {{
                                    spacing: 4
                                    Text {{ text: "Rotate"; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 10; width: 34; verticalAlignment: Text.AlignVCenter; anchors.verticalCenter: parent.verticalCenter }}
                                    Rectangle {{
                                        width: 54; height: 20; radius: 3; color: "{s200}"; border.width: 1; border.color: "{stroke}"
                                        anchors.verticalCenter: parent.verticalCenter
                                        TextInput {{
                                            anchors.fill: parent; anchors.leftMargin: 4; anchors.rightMargin: 4
                                            verticalAlignment: Text.AlignVCenter
                                            color: "{text}"; selectionColor: "{accent}"; selectedTextColor: "#ffffff"
                                            font.family: root.uiFont; font.pixelSize: 11
                                            selectByMouse: true; clip: true
                                            text: Number(root.rotateIncrement).toFixed(3).replace(/\.?0+$/,"")
                                            onEditingFinished: {{
                                                var v = parseFloat(text);
                                                if (!isNaN(v) && v > 0) {{ root.rotateIncrement = v; }}
                                                text = Number(root.rotateIncrement).toFixed(3).replace(/\.?0+$/,"");
                                                studio.set_tool_snap(root.snapToGrid, root.moveIncrement, root.rotateIncrement, root.scaleIncrement);
                                            }}
                                        }}
                                    }}
                                }}
                            }}
                        }}
                        // Group divider
                        Rectangle {{
                            visible: index < ((ribbonArea.activeTabDef.groups || []).length - 1)
                            Layout.preferredWidth: 1
                            Layout.fillHeight: true
                            Layout.topMargin: 8
                            Layout.bottomMargin: 8
                            color: "{stroke}"
                        }}
                    }}
                }}

                Item {{ Layout.fillWidth: true }}
                // Dynamic/plugins placeholder
                Text {{
                    visible: ribbonArea.activeTabDef.dynamic === true
                    text: "Plugins (dynamic - user plugins are not hardcoded)"
                    color: "{text_muted}"
                    font.family: root.uiFont
                    font.pixelSize: 12
                    Layout.alignment: Qt.AlignVCenter
                    Layout.rightMargin: 12
                }}
            }}
            Rectangle {{ anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom; height: {stroke_w}; color: "{stroke}" }}
        }}

        // Main dock area (resizable, modeled after edit.xml: Toolbox left, Explorer+Properties right)
        Rectangle {{
            Layout.fillWidth: true
            Layout.fillHeight: true
            color: "{bg}"
            SplitView {{
                id: mainSplit
                anchors.fill: parent
                anchors.margins: 1
                orientation: Qt.Horizontal
                handle: Rectangle {{
                    implicitWidth: 4
                    implicitHeight: 4
                    color: SplitHandle.hovered || SplitHandle.pressed ? "{accent}" : "{bg}"
                }}

                Rectangle {{
                    id: toolboxDock
                    SplitView.preferredWidth: 300
                    SplitView.minimumWidth: 230
                    SplitView.fillHeight: true
                    color: "{s100}"

                    ColumnLayout {{
                        anchors.fill: parent
                        spacing: 0

                        // Title bar + account chip
                        Rectangle {{
                            Layout.fillWidth: true
                            Layout.preferredHeight: 28
                            color: "{s200}"
                            RowLayout {{
                                anchors.fill: parent
                                anchors.leftMargin: 10
                                anchors.rightMargin: 6
                                spacing: 6
                                Text {{
                                    text: "Toolbox"
                                    color: "{text}"
                                    font.family: root.uiFontMedium
                                    font.pixelSize: 12
                                }}
                                Item {{ Layout.fillWidth: true }}
                                Rectangle {{
                                    implicitHeight: 20
                                    implicitWidth: accountRow.implicitWidth + 16
                                    radius: 10
                                    color: accountMa.containsMouse ? "{s300}" : "transparent"
                                    border.width: 1
                                    border.color: studio.login_user !== "" ? "#3F7A46" : "{stroke}"
                                    Row {{
                                        id: accountRow
                                        anchors.centerIn: parent
                                        spacing: 5
                                        Rectangle {{
                                            width: 6
                                            height: 6
                                            radius: 3
                                            anchors.verticalCenter: parent.verticalCenter
                                            color: studio.login_busy ? "#FBBF24" : (studio.login_user !== "" ? "#7ED957" : "{text_muted}")
                                        }}
                                        Text {{
                                            anchors.verticalCenter: parent.verticalCenter
                                            text: studio.login_busy ? "Signing in..." : (studio.login_user !== "" ? studio.login_user : "Sign in")
                                            color: studio.login_user !== "" ? "{text}" : "{text_muted}"
                                            font.family: root.uiFont
                                            font.pixelSize: 11
                                        }}
                                    }}
                                    MouseArea {{
                                        id: accountMa
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: {{
                                            if (studio.login_user !== "") studio.sign_out();
                                            else studio.login_roblox();
                                        }}
                                    }}
                                }}
                            }}
                        }}

                        // Category tabs (ids are toolbox-service categories)
                        Rectangle {{
                            Layout.fillWidth: true
                            Layout.preferredHeight: 26
                            color: "{s100}"
                            RowLayout {{
                                anchors.fill: parent
                                anchors.leftMargin: 4
                                anchors.rightMargin: 4
                                spacing: 1
                                Repeater {{
                                    model: root.toolboxCategories
                                    Rectangle {{
                                        property var cat: modelData
                                        Layout.fillWidth: true
                                        Layout.preferredHeight: 22
                                        radius: 3
                                        color: root.toolboxCategory === cat.id ? "{s300}" : (catMa.containsMouse ? "{hover}" : "transparent")
                                        Text {{
                                            anchors.centerIn: parent
                                            text: cat.label
                                            color: root.toolboxCategory === cat.id ? "{text}" : "{text_muted}"
                                            font.family: root.toolboxCategory === cat.id ? root.uiFontMedium : root.uiFont
                                            font.pixelSize: 10
                                        }}
                                        MouseArea {{
                                            id: catMa
                                            anchors.fill: parent
                                            hoverEnabled: true
                                            cursorShape: Qt.PointingHandCursor
                                            onClicked: {{
                                                root.toolboxCategory = cat.id;
                                                root.toolboxCurrent = null;
                                                studio.toolbox_search(cat.id, toolboxSearch.text);
                                            }}
                                        }}
                                    }}
                                }}
                            }}
                        }}

                        // Search field
                        Rectangle {{
                            Layout.fillWidth: true
                            Layout.preferredHeight: 34
                            color: "transparent"
                            Rectangle {{
                                anchors.fill: parent
                                anchors.margins: 6
                                radius: 4
                                color: "{s200}"
                                border.width: 1
                                border.color: toolboxSearch.activeFocus ? "{accent}" : "{stroke}"
                                TextInput {{
                                    id: toolboxSearch
                                    anchors.fill: parent
                                    anchors.leftMargin: 8
                                    anchors.rightMargin: 8
                                    verticalAlignment: TextInput.AlignVCenter
                                    color: "{text}"
                                    font.family: root.uiFont
                                    font.pixelSize: 12
                                    clip: true
                                    selectByMouse: true
                                    onAccepted: studio.toolbox_search(root.toolboxCategory, text)
                                }}
                                Text {{
                                    anchors.fill: parent
                                    anchors.leftMargin: 8
                                    verticalAlignment: Text.AlignVCenter
                                    visible: toolboxSearch.text.length === 0
                                    text: "Search " + root.categoryLabel()
                                    color: "{text_muted}"
                                    font.family: root.uiFont
                                    font.pixelSize: 12
                                }}
                            }}
                        }}

                        // Result count / errors
                        Text {{
                            Layout.fillWidth: true
                            Layout.leftMargin: 9
                            Layout.rightMargin: 9
                            Layout.preferredHeight: 16
                            visible: text.length > 0
                            text: studio.toolbox_status
                            color: studio.toolbox_status.indexOf("failed") >= 0 ? "#F87171" : "{text_muted}"
                            font.family: root.uiFont
                            font.pixelSize: 10
                            elide: Text.ElideRight
                            verticalAlignment: Text.AlignVCenter
                        }}

                        // Sign-in problems (shown until a user is restored)
                        Text {{
                            Layout.fillWidth: true
                            Layout.leftMargin: 9
                            Layout.rightMargin: 9
                            Layout.preferredHeight: visible ? 16 : 0
                            visible: studio.login_status.length > 0 && studio.login_user === ""
                            text: studio.login_status
                            color: studio.login_status.indexOf("failed") >= 0 || studio.login_status.indexOf("timed out") >= 0 || studio.login_status.indexOf("expired") >= 0 ? "#F87171" : "{text_muted}"
                            font.family: root.uiFont
                            font.pixelSize: 10
                            elide: Text.ElideRight
                            verticalAlignment: Text.AlignVCenter
                        }}

                        Rectangle {{
                            Layout.fillWidth: true
                            Layout.preferredHeight: 2
                            color: studio.toolbox_busy ? "{accent}" : "transparent"
                        }}

                        // Results grid
                        GridView {{
                            id: toolboxGrid
                            Layout.fillWidth: true
                            Layout.fillHeight: true
                            clip: true
                            cellWidth: 108
                            cellHeight: 118
                            topMargin: 4
                            leftMargin: 4
                            model: root.toolboxItems
                            ScrollBar.vertical: ScrollBar {{ width: 8; policy: ScrollBar.AsNeeded }}
                            delegate: Rectangle {{
                                width: 104
                                height: 114
                                radius: 4
                                color: cardMa.containsMouse ? "{hover}" : "transparent"
                                border.width: (root.toolboxCurrent && root.toolboxCurrent.id === modelData.id) ? 1 : 0
                                border.color: "{accent}"
                                Column {{
                                    anchors.fill: parent
                                    anchors.margins: 4
                                    spacing: 2
                                    Rectangle {{
                                        width: 96
                                        height: 72
                                        radius: 3
                                        color: "{s200}"
                                        Image {{
                                            anchors.fill: parent
                                            anchors.margins: 2
                                            source: modelData.thumbnail
                                            asynchronous: true
                                            cache: true
                                            smooth: true
                                            fillMode: Image.PreserveAspectFit
                                        }}
                                    }}
                                    Text {{
                                        width: parent.width
                                        text: modelData.name
                                        color: "{text}"
                                        font.family: root.uiFont
                                        font.pixelSize: 10
                                        elide: Text.ElideRight
                                    }}
                                    Text {{
                                        width: parent.width
                                        text: modelData.creator
                                        color: "{text_muted}"
                                        font.family: root.uiFont
                                        font.pixelSize: 9
                                        elide: Text.ElideRight
                                    }}
                                }}
                                MouseArea {{
                                    id: cardMa
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: root.toolboxCurrent = modelData
                                }}
                            }}
                        }}

                        // Insert progress / result
                        Text {{
                            Layout.fillWidth: true
                            Layout.leftMargin: 9
                            Layout.rightMargin: 9
                            Layout.preferredHeight: visible ? 16 : 0
                            visible: studio.insert_status.length > 0
                            text: studio.insert_status
                            color: studio.insert_status.indexOf("failed") >= 0 ? "#F87171" : "{text_muted}"
                            font.family: root.uiFont
                            font.pixelSize: 10
                            elide: Text.ElideRight
                            verticalAlignment: Text.AlignVCenter
                        }}

                        // Selected item footer
                        Rectangle {{
                            Layout.fillWidth: true
                            Layout.preferredHeight: root.toolboxCurrent ? 46 : 0
                            visible: root.toolboxCurrent !== null
                            color: "{s200}"
                            RowLayout {{
                                anchors.fill: parent
                                anchors.leftMargin: 8
                                anchors.rightMargin: 8
                                spacing: 8
                                Column {{
                                    Layout.fillWidth: true
                                    spacing: 1
                                    Text {{
                                        width: parent.width
                                        text: root.toolboxCurrent ? root.toolboxCurrent.name : ""
                                        color: "{text}"
                                        font.family: root.uiFontMedium
                                        font.pixelSize: 11
                                        elide: Text.ElideRight
                                    }}
                                    Text {{
                                        width: parent.width
                                        text: root.toolboxCurrent ? ("by " + root.toolboxCurrent.creator + "  |  ID " + root.toolboxCurrent.id) : ""
                                        color: "{text_muted}"
                                        font.family: root.uiFont
                                        font.pixelSize: 9
                                        elide: Text.ElideRight
                                    }}
                                }}
                                Rectangle {{
                                    implicitWidth: 62
                                    implicitHeight: 22
                                    radius: 3
                                    color: studio.insert_busy
                                        ? "{s300}"
                                        : (insertMa.containsMouse ? "#3D7BFF" : "{accent}")
                                    Text {{
                                        anchors.centerIn: parent
                                        text: studio.insert_busy ? "…" : "Insert"
                                        color: studio.insert_busy ? "{text_muted}" : "white"
                                        font.family: root.uiFontMedium
                                        font.pixelSize: 11
                                    }}
                                    MouseArea {{
                                        id: insertMa
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        enabled: !studio.insert_busy
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: {{
                                            if (root.toolboxCurrent) studio.insert_asset(root.toolboxCurrent.id, root.toolboxCurrent.name, root.toolboxCurrent.asset_type || 10);
                                        }}
                                    }}
                                }}
                            }}
                        }}
                    }}
                }}

                SplitView {{
                    id: centerSplit
                    SplitView.fillWidth: true
                    SplitView.minimumWidth: 320
                    SplitView.fillHeight: true
                    orientation: Qt.Vertical
                    handle: Rectangle {{
                        implicitWidth: 4
                        implicitHeight: 4
                        color: SplitHandle.hovered || SplitHandle.pressed ? "{accent}" : "{bg}"
                    }}

                    Rectangle {{
                        id: centerDock
                        SplitView.fillWidth: true
                        SplitView.fillHeight: true
                        SplitView.minimumHeight: 180
                        color: "{s200}"
                        ColumnLayout {{
                            anchors.fill: parent; spacing: 0

                            // Document tab bar (viewport + script tabs)
                            Rectangle {{
                                Layout.fillWidth: true
                                Layout.preferredHeight: 28
                                color: "{s100}"
                                RowLayout {{
                                    anchors.fill: parent
                                    anchors.leftMargin: 4
                                    spacing: 1
                                    Repeater {{
                                        model: root.docTabs || []
                                        Rectangle {{
                                            property var dtab: modelData
                                            property bool active: root.activeDocTab === dtab.id
                                            Layout.preferredHeight: 26
                                            Layout.preferredWidth: dtabRow.implicitWidth + 18
                                            color: active ? "{s200}" : (dtabMa.containsMouse ? "{hover}" : "transparent")
                                            border.width: active ? 1 : 0
                                            border.color: "{stroke}"
                                            Rectangle {{
                                                visible: active
                                                anchors.left: parent.left; anchors.right: parent.right
                                                anchors.bottom: parent.bottom; height: 2
                                                color: "{s200}"
                                            }}
                                            Row {{
                                                id: dtabRow
                                                anchors.centerIn: parent
                                                spacing: 6
                                                Image {{
                                                    width: 14; height: 14
                                                    anchors.verticalCenter: parent.verticalCenter
                                                    source: dtab.icon || ""
                                                    visible: (dtab.icon || "") !== ""
                                                    fillMode: Image.PreserveAspectFit; smooth: true
                                                }}
                                                Text {{
                                                    anchors.verticalCenter: parent.verticalCenter
                                                    text: dtab.title || dtab.id
                                                    color: active ? "{text}" : "{text_muted}"
                                                    font.family: active ? root.uiFontMedium : root.uiFont
                                                    font.pixelSize: 12
                                                }}
                                            }}
                                            MouseArea {{ id: dtabMa; anchors.fill: parent; hoverEnabled: true; onClicked: root.activeDocTab = dtab.id }}
                                        }}
                                    }}
                                    Item {{ Layout.fillWidth: true }}
                                }}
                                Rectangle {{ anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom; height: 1; color: "{stroke}" }}
                            }}

                            // Document content
                            Item {{
                                id: docContent
                                Layout.fillWidth: true
                                Layout.fillHeight: true

                                property var activeTabObj: {{
                                    var tabs = root.docTabs || [];
                                    for (var i = 0; i < tabs.length; i++) {{ if (tabs[i].id === root.activeDocTab) return tabs[i]; }}
                                    return tabs.length ? tabs[0] : null;
                                }}

                                // 3D Viewport tab (wgpu renderer)
                                Rectangle {{
                                    id: viewportRoot
                                    anchors.fill: parent
                                    visible: docContent.activeTabObj && docContent.activeTabObj.kind === "viewport"
                                    color: "#1C1D21"
                                    property real lastMouseX: 0
                                    property real lastMouseY: 0
                                    property bool draggingOrbit: false
                                    property bool draggingPan: false
                                    property bool flying: false
                                    property bool pointerOver: false
                                    property bool embedded: studio.viewport_embedded
                                    property bool covered: root.showStartPage || root.showSettings
                                    // The embedded child window floats above QML, so it must
                                    // never sit under an overlay (start page / settings).
                                    onCoveredChanged: studio.viewport_show(visible && !covered && embedded && width > 8 && height > 8)
                                    onVisibleChanged: studio.viewport_show(visible && !covered && embedded && width > 8 && height > 8)

                                    // The native child window is placed over this item; the
                                    // 34px strip above it keeps the chips row clickable.
                                    Item {{
                                        id: viewport3d
                                        anchors.fill: parent
                                        anchors.topMargin: viewportRoot.embedded ? 34 : 0

                                    Image {{
                                        id: viewportImage
                                        anchors.fill: parent
                                        visible: !viewportRoot.embedded
                                        source: studio.viewport_frame
                                        fillMode: Image.Stretch
                                        cache: false
                                        asynchronous: true
                                        smooth: true
                                        mipmap: true
                                    }}

                                    // fallback while first frame renders / on error
                                    Rectangle {{
                                        anchors.fill: parent
                                        color: "transparent"
                                        visible: !viewportRoot.embedded && viewportImage.status !== Image.Ready
                                        Canvas {{
                                            anchors.fill: parent
                                            onPaint: {{
                                                var ctx = getContext("2d");
                                                ctx.clearRect(0, 0, width, height);
                                                ctx.fillStyle = "#1C1D21"; ctx.fillRect(0,0,width,height);
                                                ctx.strokeStyle = "#2A2C31"; ctx.lineWidth = 1; var step = 32;
                                                for (var x = 0; x < width; x += step) {{ ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, height); ctx.stroke(); }}
                                                for (var y = 0; y < height; y += step) {{ ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(width, y); ctx.stroke(); }}
                                            }}
                                            onWidthChanged: requestPaint(); onHeightChanged: requestPaint()
                                        }}
                                        Text {{
                                            anchors.centerIn: parent
                                            text: studio.viewport_status && studio.viewport_status.length > 0 ? studio.viewport_status : "Initializing 3D viewport (wgpu)..."
                                            color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 12
                                        }}
                                    }}
                                    }}

                                    MouseArea {{
                                        id: viewportMouse
                                        anchors.fill: parent
                                        // The embedded child window receives its own input.
                                        enabled: !viewportRoot.embedded
                                        acceptedButtons: Qt.LeftButton | Qt.RightButton | Qt.MiddleButton
                                        hoverEnabled: true
                                        onPressed: function(mouse) {{
                                            studio.begin_interaction();
                                            viewportRoot.lastMouseX = mouse.x; viewportRoot.lastMouseY = mouse.y;
                                            viewportRoot.draggingOrbit = (mouse.buttons & Qt.RightButton) || (mouse.buttons & Qt.LeftButton);
                                            viewportRoot.draggingPan = (mouse.buttons & Qt.MiddleButton) || ((mouse.buttons & Qt.LeftButton) && (mouse.modifiers & Qt.ShiftModifier));
                                            viewportRoot.draggingOrbit = viewportRoot.draggingOrbit && !viewportRoot.draggingPan;
                                            viewportRoot.flying = true;
                                        }}
                                        onPositionChanged: function(mouse) {{
                                            var dx = mouse.x - viewportRoot.lastMouseX;
                                            var dy = mouse.y - viewportRoot.lastMouseY;
                                            viewportRoot.lastMouseX = mouse.x; viewportRoot.lastMouseY = mouse.y;
                                            if (viewportRoot.draggingPan) studio.pan_camera(dx, dy);
                                            else if (viewportRoot.draggingOrbit && (mouse.buttons & (Qt.LeftButton|Qt.RightButton))) studio.orbit_camera(dx, dy);
                                        }}
                                        onReleased: {{ viewportRoot.draggingOrbit = false; viewportRoot.draggingPan = false; viewportRoot.flying = false; studio.end_interaction(); }}
                                        onHoveredChanged: viewportRoot.pointerOver = hovered
                                        onWheel: function(wheel) {{ studio.begin_interaction(); studio.zoom_camera(wheel.angleDelta.y); wheel.accepted = true; studio.end_interaction(); }}
                                    }}

                                    // WASD / Q-E camera flight while the pointer is over the
                                    // viewport (the embedded child window polls the OS itself).
                                    Timer {{
                                        id: flyTimer
                                        interval: 16
                                        repeat: true
                                        running: viewportRoot.visible && !viewportRoot.embedded && (viewportRoot.pointerOver || viewportRoot.flying)
                                        onTriggered: studio.fly_camera(0.016)
                                    }}

                                    Timer {{
                                        id: viewportTimer
                                        // Readback fallback only: the embedded viewport has its
                                        // own swapchain and never needs a QML image frame.
                                        interval: 66
                                        running: viewportRoot.visible && !viewportRoot.embedded
                                        repeat: true
                                        triggeredOnStart: true
                                        onTriggered: {{
                                            if (viewportRoot.width > 8 && viewportRoot.height > 8)
                                                studio.render_viewport(Math.round(viewportRoot.width), Math.round(viewportRoot.height));
                                        }}
                                    }}

                                    // Attaches the native child window once the Qt window exists.
                                    Timer {{
                                        id: embedAttachTimer
                                        interval: 400; repeat: true
                                        running: viewportRoot.visible && !viewportRoot.covered && !viewportRoot.embedded
                                        triggeredOnStart: true
                                        onTriggered: studio.viewport_attach()
                                    }}

                                    // Keeps the child window aligned with the dock as the
                                    // window resizes or the docks are dragged.
                                    Timer {{
                                        id: embedRectTimer
                                        interval: 40; repeat: true
                                        running: viewportRoot.visible && viewportRoot.embedded
                                        onTriggered: {{
                                            var covered = viewportRoot.covered;
                                            studio.viewport_show(!covered);
                                            if (!covered && viewport3d.width > 8 && viewport3d.height > 8) {{
                                                var p = viewport3d.mapToItem(null, 0, 0);
                                                studio.viewport_set_rect(p.x, p.y, viewport3d.width, viewport3d.height);
                                            }}
                                        }}
                                    }}

                                    Row {{
                                        anchors.top: parent.top; anchors.left: parent.left; anchors.margins: 8
                                        spacing: 6
                                        Rectangle {{
                                            radius: 4; color: "#2D5A27CC"; border.width: 1; border.color: "#4A8A42"
                                            implicitHeight: 24; implicitWidth: nativeLabel.implicitWidth + 16
                                            Text {{ id: nativeLabel; anchors.centerIn: parent; text: "Detach 3D"; color: "#E8F5E8"; font.family: root.uiFont; font.pixelSize: 11 }}
                                            MouseArea {{
                                                anchors.fill: parent
                                                hoverEnabled: true
                                                onClicked: studio.open_native_viewport()
                                            }}
                                        }}
                                        Rectangle {{
                                            radius: 4; color: "#15161ACC"; border.width: 1; border.color: "{stroke}"
                                            implicitHeight: 24; implicitWidth: techLabel.implicitWidth + 16
                                            Text {{ id: techLabel; anchors.centerIn: parent; text: "Lighting: " + (studio.lighting_technology && studio.lighting_technology.length ? studio.lighting_technology : "Future"); color: "{text}"; font.family: root.uiFont; font.pixelSize: 11 }}
                                            MouseArea {{
                                                anchors.fill: parent
                                                onClicked: {{
                                                    var modes = ["Future","ShadowMap","Compatibility","Voxel","Legacy"];
                                                    var cur = studio.lighting_technology || "Future";
                                                    var idx = modes.indexOf(cur); if (idx < 0) idx = 0;
                                                    studio.lighting_technology = modes[(idx + 1) % modes.length];
                                                }}
                                            }}
                                        }}
                                        Rectangle {{
                                            radius: 4; color: "#15161ACC"; border.width: 1; border.color: "{stroke}"
                                            implicitHeight: 24; implicitWidth: aaLabel.implicitWidth + 16
                                            Text {{ id: aaLabel; anchors.centerIn: parent; text: (studio.aa_mode && studio.aa_mode.length ? studio.aa_mode : "AA Off"); color: "{text}"; font.family: root.uiFont; font.pixelSize: 11 }}
                                            MouseArea {{
                                                anchors.fill: parent
                                                onClicked: {{
                                                    var modes = ["AA Off","MSAA 4x"];
                                                    var cur = studio.aa_mode || "AA Off";
                                                    var idx = modes.indexOf(cur); if (idx < 0) idx = 0;
                                                    studio.aa_mode = modes[(idx + 1) % modes.length];
                                                }}
                                            }}
                                        }}
                                        Rectangle {{
                                            radius: 4; color: "#15161ACC"; border.width: 1; border.color: "{stroke}"
                                            implicitHeight: 24; implicitWidth: flyHint.implicitWidth + 16
                                            Text {{ id: flyHint; anchors.centerIn: parent; text: "WASD + Q/E fly"; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 10 }}
                                        }}
                                        Rectangle {{
                                            radius: 4; color: "#15161ACC"; border.width: 1; border.color: "{stroke}"
                                            implicitHeight: 24; implicitWidth: vpStatus.implicitWidth + 16
                                            visible: vpStatus.text.length > 0
                                            Text {{ id: vpStatus; anchors.centerIn: parent; text: studio.viewport_status || ""; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 10 }}
                                        }}
                                    }}
                                }}

                                // Script tab
                                Rectangle {{
                                    anchors.fill: parent
                                    visible: docContent.activeTabObj && docContent.activeTabObj.kind === "script"
                                    color: "#202227"
                                    ColumnLayout {{
                                        anchors.fill: parent; spacing: 0
                                        Rectangle {{
                                            Layout.fillWidth: true; Layout.preferredHeight: 22; color: "{s100}"
                                            Text {{
                                                anchors.left: parent.left; anchors.leftMargin: 10
                                                anchors.verticalCenter: parent.verticalCenter
                                                text: (docContent.activeTabObj ? docContent.activeTabObj.title : "Script") + " — Luau"
                                                color: "#6A6F81"; font.family: root.uiFont; font.pixelSize: 11
                                            }}
                                        }}
                                        Rectangle {{
                                            Layout.fillWidth: true; Layout.fillHeight: true
                                            color: "#202227"
                                            Flickable {{
                                                id: scriptFlick
                                                anchors.fill: parent
                                                clip: true
                                                contentWidth: scriptRow.implicitWidth
                                                contentHeight: scriptRow.implicitHeight
                                                boundsBehavior: Flickable.StopAtBounds
                                                ScrollBar.vertical: ScrollBar {{ policy: ScrollBar.AsNeeded; width: 10 }}
                                                ScrollBar.horizontal: ScrollBar {{ policy: ScrollBar.AsNeeded; height: 10 }}

                                                Row {{
                                                    id: scriptRow
                                                    spacing: 0
                                                    Rectangle {{
                                                        id: lineGutter
                                                        width: 46
                                                        height: Math.max(scriptFlick.height, scriptEditor.contentHeight + 20)
                                                        color: "#202227"
                                                        Text {{
                                                            id: lineNumbers
                                                            anchors.right: parent.right
                                                            anchors.rightMargin: 8
                                                            y: scriptEditor.topPadding
                                                            width: parent.width - 10
                                                            horizontalAlignment: Text.AlignRight
                                                            color: "#6A6F81"
                                                            font: scriptEditor.font
                                                            lineHeight: scriptEditor.lineCount > 0 ? (scriptEditor.contentHeight / scriptEditor.lineCount) : 1
                                                            lineHeightMode: Text.FixedHeight
                                                            text: {{
                                                                var n = Math.max(1, scriptEditor.lineCount);
                                                                var s = [];
                                                                for (var i = 1; i <= n; i++) s.push(i);
                                                                return s.join("\n");
                                                            }}
                                                        }}
                                                        Rectangle {{ anchors.top: parent.top; anchors.bottom: parent.bottom; anchors.right: parent.right; width: 1; color: "#353741"; opacity: 0.8 }}
                                                    }}
                                                    Item {{
                                                        id: editorStack
                                                        width: Math.max(scriptFlick.width - lineGutter.width, scriptEditor.contentWidth + scriptEditor.leftPadding + scriptEditor.rightPadding + 20)
                                                        height: Math.max(scriptFlick.height, scriptEditor.contentHeight + scriptEditor.topPadding + scriptEditor.bottomPadding + 20)
                                                        Text {{
                                                            id: syntaxHighlight
                                                            x: scriptEditor.leftPadding
                                                            y: scriptEditor.topPadding
                                                            width: Math.max(0, editorStack.width - scriptEditor.leftPadding - scriptEditor.rightPadding)
                                                            text: root.highlightLuau(scriptEditor.text)
                                                            textFormat: Text.RichText
                                                            font.family: root.monoFont
                                                            font.pixelSize: 12
                                                            wrapMode: Text.NoWrap
                                                        }}
                                                        TextArea {{
                                                            id: scriptEditor
                                                            anchors.fill: parent
                                                            text: docContent.activeTabObj ? (docContent.activeTabObj.source || "") : ""
                                                            color: "transparent"
                                                            selectedTextColor: "#FFFFFF"
                                                            selectionColor: "#13235D"
                                                            font.family: root.monoFont
                                                            font.pixelSize: 12
                                                            placeholderText: "-- Luau script"
                                                            placeholderTextColor: "#6A6F81"
                                                            background: Rectangle {{ color: "transparent" }}
                                                            selectByMouse: true
                                                            wrapMode: TextEdit.NoWrap
                                                            leftPadding: 12; topPadding: 10; rightPadding: 12; bottomPadding: 10
                                                            tabStopDistance: 28
                                                        }}
                                                        Rectangle {{
                                                            visible: scriptEditor.activeFocus
                                                            x: scriptEditor.cursorRectangle.x
                                                            y: scriptEditor.cursorRectangle.y
                                                            width: 2
                                                            height: scriptEditor.cursorRectangle.height
                                                            color: "#BCBEC8"
                                                        }}
                                                    }}
                                                }}
                                            }}
                                        }}
                                    }}
                                }}
                            }}
                        }}
                    }}

                    Rectangle {{
                        id: consoleDock
                        SplitView.preferredHeight: 180
                        SplitView.minimumHeight: 90
                        SplitView.fillWidth: true
                        color: "{s100}"
                        ColumnLayout {{
                            anchors.fill: parent; spacing: 0
                            Rectangle {{
                                Layout.fillWidth: true; Layout.preferredHeight: 28; color: "{s200}"
                                RowLayout {{
                                    anchors.fill: parent; anchors.leftMargin: 10; anchors.rightMargin: 8; spacing: 8
                                    Text {{ text: "Output"; color: "{text}"; font.family: root.uiFontMedium; font.pixelSize: 12 }}
                                    Rectangle {{ width: 1; height: 14; color: "{stroke}" }}
                                    Text {{ text: "All"; color: "{text}"; font.family: root.uiFont; font.pixelSize: 11 }}
                                    Text {{ text: "Info"; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 11 }}
                                    Text {{ text: "Warnings"; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 11 }}
                                    Text {{ text: "Errors"; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 11 }}
                                    Item {{ Layout.fillWidth: true }}
                                    Rectangle {{
                                        width: clearLabel.implicitWidth + 12; height: 20; radius: 3
                                        color: clearMa.containsMouse ? "{hover}" : "transparent"
                                        Text {{ id: clearLabel; anchors.centerIn: parent; text: "Clear"; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 11 }}
                                        MouseArea {{ id: clearMa; anchors.fill: parent; hoverEnabled: true; onClicked: root.consoleLines = [] }}
                                    }}
                                }}
                            }}
                            ListView {{
                                id: consoleList
                                Layout.fillWidth: true
                                Layout.fillHeight: true
                                clip: true
                                model: root.consoleLines
                                boundsBehavior: Flickable.StopAtBounds
                                ScrollBar.vertical: ScrollBar {{ policy: ScrollBar.AsNeeded; width: 10 }}
                                delegate: Rectangle {{
                                    width: consoleList.width
                                    height: Math.max(20, msgRow.implicitHeight + 6)
                                    color: index % 2 === 0 ? "transparent" : "#15161B"
                                    property var line: modelData
                                    Row {{
                                        id: msgRow
                                        anchors.left: parent.left; anchors.right: parent.right
                                        anchors.verticalCenter: parent.verticalCenter
                                        spacing: 0
                                        Rectangle {{
                                            width: 40; height: parent.height
                                            color: "transparent"
                                            Text {{
                                                anchors.right: parent.right; anchors.rightMargin: 8
                                                anchors.verticalCenter: parent.verticalCenter
                                                text: (index + 1)
                                                color: "{text_muted}"
                                                font.family: root.monoFont; font.pixelSize: 12
                                                horizontalAlignment: Text.AlignRight
                                            }}
                                        }}
                                        Rectangle {{ width: 1; height: Math.max(16, msgText.implicitHeight); color: "{stroke}"; opacity: 0.4 }}
                                        Text {{
                                            id: msgText
                                            width: msgRow.width - 49
                                            leftPadding: 8; rightPadding: 8
                                            text: (line.time ? ("[" + line.time + "] ") : "") + (line.text || "")
                                            color: line.level === "error" ? "#F87171" : (line.level === "warn" ? "#FBBF24" : (line.level === "info" ? "#60A5FA" : "{text}"))
                                            font.family: root.monoFont
                                            font.pixelSize: 12
                                            wrapMode: Text.WrapAnywhere
                                        }}
                                    }}
                                }}
                            }}
                            Rectangle {{ Layout.fillWidth: true; height: 1; color: "{stroke}" }}
                            Rectangle {{
                                Layout.fillWidth: true; Layout.preferredHeight: 26; color: "{bg}"
                                RowLayout {{
                                    anchors.fill: parent; anchors.leftMargin: 8; anchors.rightMargin: 8; spacing: 6
                                    Text {{ text: ">"; color: "{text_muted}"; font.family: root.monoFont; font.pixelSize: 12 }}
                                    TextInput {{
                                        id: consoleInput
                                        Layout.fillWidth: true
                                        Layout.fillHeight: true
                                        verticalAlignment: Text.AlignVCenter
                                        color: "{text}"
                                        selectionColor: "{accent}"
                                        selectedTextColor: "white"
                                        font.family: root.monoFont; font.pixelSize: 12
                                        clip: true
                                        selectByMouse: true
                                        Keys.onReturnPressed: {{
                                            var t = text.trim();
                                            if (t.length > 0) {{
                                                text = "";
                                                studio.run_luau(t);
                                                consoleList.positionViewAtEnd();
                                            }}
                                        }}
                                        Text {{
                                            anchors.fill: parent; verticalAlignment: Text.AlignVCenter
                                            text: "Run a command (Luau)"; color: "{text_muted}"; font.family: root.monoFont; font.pixelSize: 12
                                            visible: !consoleInput.text && !consoleInput.activeFocus
                                        }}
                                    }}
                                }}
                            }}
                        }}
                    }}
                }}

                SplitView {{
                    id: rightSplit
                    SplitView.preferredWidth: 330
                    SplitView.minimumWidth: 180
                    SplitView.fillHeight: true
                    orientation: Qt.Vertical
                    handle: Rectangle {{
                        implicitWidth: 4
                        implicitHeight: 4
                        color: SplitHandle.hovered || SplitHandle.pressed ? "{accent}" : "{bg}"
                    }}

                    Rectangle {{
                        id: explorerDock
                        SplitView.preferredHeight: 420
                        SplitView.minimumHeight: 120
                        SplitView.fillWidth: true
                        color: "{s100}"
                        ColumnLayout {{
                            anchors.fill: parent; spacing: 0
                            Rectangle {{
                                Layout.fillWidth: true; Layout.preferredHeight: 28; color: "{s200}"
                                Text {{ anchors.left: parent.left; anchors.leftMargin: 10; anchors.verticalCenter: parent.verticalCenter; text: "Explorer"; color: "{text}"; font.family: root.uiFontMedium; font.pixelSize: 12 }}
                            }}
                            Rectangle {{
                                Layout.fillWidth: true; Layout.preferredHeight: 28; color: "{s100}"
                                RowLayout {{
                                    anchors.fill: parent; anchors.margins: 4; spacing: 4
                                    Rectangle {{
                                        Layout.fillWidth: true
                                        Layout.preferredHeight: 20
                                        radius: 3; color: "{bg}"
                                        border.width: 1
                                        border.color: explorerSearch.activeFocus ? "{accent}" : "{stroke}"
                                        TextInput {{
                                            id: explorerSearch
                                            anchors.fill: parent
                                            anchors.leftMargin: 6; anchors.rightMargin: 6
                                            verticalAlignment: TextInput.AlignVCenter
                                            color: "{text}"
                                            selectionColor: "{accent}"
                                            selectedTextColor: "#ffffff"
                                            font.family: root.uiFont; font.pixelSize: 11
                                            selectByMouse: true
                                            clip: true
                                            onTextChanged: root.explorerFilter = text
                                            Text {{
                                                anchors.fill: parent
                                                verticalAlignment: Text.AlignVCenter
                                                text: "Search"
                                                visible: !parent.text && !parent.activeFocus
                                                color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 11
                                            }}
                                        }}
                                    }}
                                    Rectangle {{
                                        id: explorerNewButton
                                        Layout.preferredWidth: 24
                                        Layout.preferredHeight: 20
                                        radius: 3
                                        color: explorerNewMa.containsMouse ? "{s200}" : "transparent"
                                        border.width: 1; border.color: "{stroke}"
                                        Image {{
                                            anchors.centerIn: parent
                                            width: 12; height: 12
                                            source: "{sp_plus}"
                                            fillMode: Image.PreserveAspectFit
                                            smooth: true
                                            visible: "{sp_plus}" !== ""
                                        }}
                                        Text {{
                                            visible: "{sp_plus}" === ""
                                            anchors.centerIn: parent
                                            text: "+"; color: "{text}"; font.pixelSize: 14; font.bold: true
                                        }}
                                        MouseArea {{
                                            id: explorerNewMa
                                            anchors.fill: parent
                                            hoverEnabled: true
                                            onClicked: {{
                                                var p = explorerNewButton.mapToItem(null, 0, 0);
                                                var w = 240;
                                                explorerInsertMenu.width = w;
                                                explorerInsertMenu.x = Math.min(Math.max(4, p.x + explorerNewButton.width - w), root.width - w - 6);
                                                explorerInsertMenu.y = p.y + explorerNewButton.height + 2;
                                                explorerInsertMenu.open();
                                            }}
                                        }}
                                    }}
                                }}
                            }}
                            Rectangle {{ Layout.fillWidth: true; height: 1; color: "{stroke}" }}
                            Flickable {{
                                id: explorerFlick
                                Layout.fillWidth: true
                                Layout.fillHeight: true
                                clip: true
                                contentWidth: width
                                contentHeight: explorerColumn.implicitHeight
                                boundsBehavior: Flickable.StopAtBounds
                                ScrollBar.vertical: ScrollBar {{ policy: ScrollBar.AsNeeded; width: 10 }}

                                Column {{
                                    id: explorerColumn
                                    width: explorerFlick.width
                                    spacing: 0
                                    Repeater {{
                                        model: root.visibleExplorerRoots()
                                        delegate: Loader {{
                                            width: explorerColumn.width
                                            property var nodeData: modelData
                                            property string parentPath: ""
                                            property int nodeDepth: 0
                                            sourceComponent: explorerNodeDelegate
                                        }}
                                    }}
                                }}
                            }}
                        }}
                    }}

                    Rectangle {{
                        id: propertiesDock
                        SplitView.fillHeight: true
                        SplitView.minimumHeight: 120
                        SplitView.fillWidth: true
                        color: "{s100}"
                        ColumnLayout {{
                            anchors.fill: parent; spacing: 0
                            Rectangle {{
                                Layout.fillWidth: true; Layout.preferredHeight: 28; color: "{s200}"
                                Text {{ anchors.left: parent.left; anchors.leftMargin: 10; anchors.verticalCenter: parent.verticalCenter; text: "Properties"; color: "{text}"; font.family: root.uiFontMedium; font.pixelSize: 12 }}
                            }}
                            // Selection header
                            Rectangle {{
                                Layout.fillWidth: true; Layout.preferredHeight: 30; color: "{s100}"
                                visible: root.selectedNode !== null
                                Row {{
                                    anchors.left: parent.left; anchors.leftMargin: 8
                                    anchors.verticalCenter: parent.verticalCenter; spacing: 6
                                    Image {{
                                        width: 16; height: 16; anchors.verticalCenter: parent.verticalCenter
                                        source: root.selectedNode ? (root.selectedNode.icon || "") : ""
                                        visible: root.selectedNode && (root.selectedNode.icon || "") !== ""
                                        fillMode: Image.PreserveAspectFit; smooth: true
                                    }}
                                    Text {{
                                        anchors.verticalCenter: parent.verticalCenter
                                        text: root.selectedNode ? root.selectedNode.name : ""
                                        color: "{text}"; font.family: root.uiFontMedium; font.pixelSize: 12
                                        elide: Text.ElideRight
                                    }}
                                }}
                                Rectangle {{ anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom; height: 1; color: "{stroke}" }}
                            }}
                            // Filter
                            Rectangle {{
                                Layout.fillWidth: true; Layout.preferredHeight: 26; color: "{s100}"
                                Rectangle {{
                                    anchors.fill: parent; anchors.margins: 4
                                    radius: 3; color: "{bg}"; border.width: 1; border.color: "{stroke}"
                                    Text {{
                                        anchors.left: parent.left; anchors.leftMargin: 8
                                        anchors.verticalCenter: parent.verticalCenter
                                        text: "Filter Properties (Ctrl+Shift+P)"
                                        color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 11
                                        elide: Text.ElideRight; width: parent.width - 16
                                    }}
                                }}
                            }}
                            Rectangle {{ Layout.fillWidth: true; height: 1; color: "{stroke}" }}

                            Item {{
                                Layout.fillWidth: true; Layout.fillHeight: true
                                Text {{
                                    anchors.centerIn: parent
                                    visible: root.selectedNode === null
                                    text: "Select an instance to view its properties"
                                    color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 12
                                }}
                                Flickable {{
                                    id: propsFlick
                                    anchors.fill: parent
                                    visible: root.selectedNode !== null
                                    clip: true
                                    contentWidth: width
                                    contentHeight: propsCol.implicitHeight
                                    boundsBehavior: Flickable.StopAtBounds
                                    ScrollBar.vertical: ScrollBar {{ policy: ScrollBar.AsNeeded; width: 10 }}

                                    Column {{
                                        id: propsCol
                                        width: propsFlick.width
                                        spacing: 0
                                        Repeater {{
                                            model: root.selectedNode ? root.propCategories(root.propertiesForSelected()) : []
                                            delegate: Column {{
                                                width: propsCol.width
                                                property var cat: modelData
                                                Rectangle {{
                                                    width: parent.width; height: 22; color: "{s200}"
                                                    Text {{
                                                        anchors.left: parent.left; anchors.leftMargin: 8
                                                        anchors.verticalCenter: parent.verticalCenter
                                                        text: cat.name
                                                        color: "{text}"; font.family: root.uiFontMedium; font.pixelSize: 11
                                                    }}
                                                    Rectangle {{ anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom; height: 1; color: "{stroke}" }}
                                                }}
                                                Repeater {{
                                                    model: cat.props || []
                                                    delegate: Rectangle {{
                                                        id: propRow
                                                        width: propsCol.width
                                                        height: 24
                                                        color: index % 2 === 0 ? "{s100}" : "#15161B"
                                                        opacity: (prop.deprecated || prop.hidden || prop.notBrowsable) ? 0.72 : 1.0
                                                        property var prop: modelData
                                                        property bool isBool: prop.typeCategory === "Primitive" && prop.typeName === "bool"
                                                        property bool isEnum: prop.typeCategory === "Enum"
                                                        property bool isClass: prop.typeCategory === "Class"
                                                        property bool isColor: prop.typeCategory === "DataType" && (prop.typeName === "Color3" || prop.typeName === "Color3uint8")
                                                        Row {{
                                                            anchors.fill: parent
                                                            anchors.leftMargin: 8
                                                            anchors.rightMargin: 4
                                                            spacing: 3
                                                            // Property name + flags
                                                            Item {{
                                                                width: Math.max(90, parent.width * 0.44)
                                                                height: parent.height
                                                                Row {{
                                                                    anchors.verticalCenter: parent.verticalCenter
                                                                    anchors.left: parent.left
                                                                    width: parent.width
                                                                    spacing: 4
                                                                    Text {{
                                                                        width: Math.max(20, parent.width - flagRow.width - 4)
                                                                        text: prop.name
                                                                        color: prop.readOnly ? "{text_muted}" : "{text}"
                                                                        font.family: root.uiFont
                                                                        font.pixelSize: 11
                                                                        font.italic: prop.deprecated
                                                                        elide: Text.ElideRight
                                                                    }}
                                                                    Row {{
                                                                        id: flagRow
                                                                        spacing: 2
                                                                        anchors.verticalCenter: parent.verticalCenter
                                                                        Rectangle {{ visible: prop.readOnly; width: 10; height: 10; radius: 2; color: "transparent"; border.width: 1; border.color: "{text_muted}"; Text {{ anchors.centerIn: parent; text: "R"; color: "{text_muted}"; font.pixelSize: 7 }} }}
                                                                        Rectangle {{ visible: prop.deprecated; width: 10; height: 10; radius: 2; color: "transparent"; border.width: 1; border.color: "#C9A227"; Text {{ anchors.centerIn: parent; text: "D"; color: "#C9A227"; font.pixelSize: 7 }} }}
                                                                    }}
                                                                }}
                                                            }}
                                                            Rectangle {{ width: 1; height: parent.height; color: "{stroke}"; opacity: 0.6 }}
                                                            // Value editor (all property types)
                                                            Item {{
                                                                width: parent.width - Math.max(90, parent.width * 0.44) - 8
                                                                height: parent.height

                                                                // Bool
                                                                Rectangle {{
                                                                    visible: isBool
                                                                    width: 13; height: 13; radius: 2
                                                                    anchors.left: parent.left; anchors.leftMargin: 4
                                                                    anchors.verticalCenter: parent.verticalCenter
                                                                    color: (String(prop.value) === "true") ? "{accent}" : "{bg}"
                                                                    border.width: 1; border.color: (String(prop.value) === "true") ? "{accent}" : "{stroke}"
                                                                    opacity: prop.readOnly ? 0.6 : 1.0
                                                                    Text {{ anchors.centerIn: parent; visible: String(prop.value) === "true"; text: "\u2713"; color: "white"; font.pixelSize: 9 }}
                                                                    MouseArea {{
                                                                        anchors.fill: parent
                                                                        enabled: !prop.readOnly
                                                                        onClicked: root.setSelectedProperty(prop.name, (String(prop.value) === "true") ? "false" : "true")
                                                                    }}
                                                                }}

                                                                // Enum (includes every enum item via allEnums)
                                                                Rectangle {{
                                                                    visible: isEnum
                                                                    anchors.fill: parent
                                                                    anchors.margins: 2
                                                                    radius: 2
                                                                    color: enumMa.containsMouse ? "{s200}" : "transparent"
                                                                    border.width: enumMa.containsMouse ? 1 : 0
                                                                    border.color: "{stroke}"
                                                                    Text {{
                                                                        anchors.left: parent.left; anchors.leftMargin: 4
                                                                        anchors.right: enumHint.left; anchors.rightMargin: 2
                                                                        anchors.verticalCenter: parent.verticalCenter
                                                                        text: String(prop.value)
                                                                        color: prop.readOnly ? "{text_muted}" : "{text}"
                                                                        font.family: root.uiFont; font.pixelSize: 11
                                                                        elide: Text.ElideRight
                                                                    }}
                                                                    Image {{
                                                                        id: enumHint
                                                                        anchors.right: parent.right; anchors.rightMargin: 4
                                                                        anchors.verticalCenter: parent.verticalCenter
                                                                        width: 10; height: 10
                                                                        source: "{dropdown_icon}"
                                                                        visible: "{dropdown_icon}" !== ""
                                                                        fillMode: Image.PreserveAspectFit
                                                                        smooth: true
                                                                    }}
                                                                    Text {{
                                                                        anchors.right: parent.right; anchors.rightMargin: 4
                                                                        anchors.verticalCenter: parent.verticalCenter
                                                                        text: "?"
                                                                        visible: "{dropdown_icon}" === ""
                                                                        color: "{text_muted}"; font.pixelSize: 9
                                                                    }}
                                                                    MouseArea {{
                                                                        id: enumMa
                                                                        anchors.fill: parent; hoverEnabled: true
                                                                        enabled: !prop.readOnly
                                                                        onClicked: {{
                                                                            var items = root.enumItems(prop.typeName);
                                                                            enumPopup.items = items;
                                                                            enumPopup.currentValue = String(prop.value);
                                                                            enumPopup.propName = prop.name;
                                                                            var w = Math.max(200, Math.min(300, enumMa.width + 40));
                                                                            var h = Math.min(340, Math.max(30, items.length * 20 + 12));
                                                                            enumPopup.width = w;
                                                                            enumPopup.height = h;
                                                                            var p = enumMa.mapToItem(null, 0, 0);
                                                                            var px = Math.min(Math.max(4, p.x + enumMa.width - w), root.width - w - 6);
                                                                            var py = p.y + enumMa.height + 2;
                                                                            if (py + h > root.height - 6) {{
                                                                                var above = p.y - h - 2;
                                                                                py = above > 6 ? above : Math.max(6, root.height - h - 6);
                                                                            }}
                                                                            enumPopup.x = px;
                                                                            enumPopup.y = py;
                                                                            enumPopup.open();
                                                                        }}
                                                                    }}
                                                                }}

                                                                // Class reference
                                                                Text {{
                                                                    visible: isClass
                                                                    anchors.verticalCenter: parent.verticalCenter
                                                                    anchors.left: parent.left; anchors.leftMargin: 4
                                                                    width: parent.width - 8
                                                                    text: String(prop.value)
                                                                    color: "{accent}"
                                                                    font.family: root.uiFont; font.pixelSize: 11
                                                                    elide: Text.ElideRight
                                                                }}

                                                                // Color3 swatch + hex (opens the picker)
                                                                Item {{
                                                                    visible: isColor
                                                                    anchors.fill: parent
                                                                    Rectangle {{
                                                                        id: colorSwatch
                                                                        width: 15; height: 15; radius: 2
                                                                        anchors.left: parent.left; anchors.leftMargin: 4
                                                                        anchors.verticalCenter: parent.verticalCenter
                                                                        color: root.hexFromValue(prop.value)
                                                                        border.width: 1; border.color: "{stroke}"
                                                                        opacity: prop.readOnly ? 0.6 : 1.0
                                                                    }}
                                                                    Text {{
                                                                        anchors.left: colorSwatch.right; anchors.leftMargin: 6
                                                                        anchors.right: parent.right; anchors.rightMargin: 4
                                                                        anchors.verticalCenter: parent.verticalCenter
                                                                        text: root.hexFromValue(prop.value)
                                                                        color: prop.readOnly ? "{text_muted}" : "{text}"
                                                                        font.family: root.uiFont; font.pixelSize: 11
                                                                        elide: Text.ElideRight
                                                                    }}
                                                                    MouseArea {{
                                                                        anchors.fill: parent
                                                                        enabled: !prop.readOnly
                                                                        onClicked: root.openColorPicker(prop.name, prop.value, colorSwatch)
                                                                    }}
                                                                }}

                                                                // All other types (Primitive numbers/strings, all DataTypes)
                                                                Item {{
                                                                    visible: !isBool && !isEnum && !isClass && !isColor
                                                                    anchors.fill: parent
                                                                    Text {{
                                                                        visible: prop.readOnly
                                                                        anchors.verticalCenter: parent.verticalCenter
                                                                        anchors.left: parent.left; anchors.leftMargin: 4
                                                                        width: parent.width - 8
                                                                        text: String(prop.value)
                                                                        color: "{text_muted}"
                                                                        font.family: root.uiFont; font.pixelSize: 11
                                                                        elide: Text.ElideRight
                                                                    }}
                                                                    Rectangle {{
                                                                        visible: !prop.readOnly
                                                                        anchors.fill: parent
                                                                        anchors.margins: 2
                                                                        radius: 2
                                                                        color: propEdit.activeFocus ? "{bg}" : "transparent"
                                                                        border.width: propEdit.activeFocus ? 1 : 0
                                                                        border.color: "{accent}"
                                                                        TextInput {{
                                                                            id: propEdit
                                                                            anchors.fill: parent
                                                                            anchors.leftMargin: 4; anchors.rightMargin: 4
                                                                            verticalAlignment: TextInput.AlignVCenter
                                                                            text: String(prop.value)
                                                                            color: "{text}"
                                                                            selectionColor: "{accent}"
                                                                            selectedTextColor: "#ffffff"
                                                                            font.family: root.uiFont; font.pixelSize: 11
                                                                            selectByMouse: true
                                                                            clip: true
                                                                            onEditingFinished: {{
                                                                                if (text !== String(prop.value)) root.setSelectedProperty(prop.name, text)
                                                                            }}
                                                                        }}
                                                                    }}
                                                                }}
                                                            }}
                                                        }}
                                                        Rectangle {{ anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom; height: 1; color: "{stroke}"; opacity: 0.35 }}
                                                    }}
                                                }}
                                            }}
                                        }}
                                    }}
                                }}
                            }}
                        }}
                    }}
                }}
            }}
        }}

        // Status bar
        Rectangle {{
            Layout.fillWidth: true
            Layout.preferredHeight: 24
            color: "{s100}"
            Rectangle {{ anchors.left: parent.left; anchors.right: parent.right; anchors.top: parent.top; height: {stroke_w}; color: "{stroke}" }}
            Text {{ anchors.left: parent.left; anchors.leftMargin: 10; anchors.verticalCenter: parent.verticalCenter; text: "Ready  |  Edit  |  API dump: properties + all enums loaded"; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 12 }}
        }}
    }}

    Popup {{
        id: enumPopup
        property var items: []
        property string currentValue: ""
        property string propName: ""
        modal: false
        focus: true
        width: 240
        height: 200
        padding: 6
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
        background: Rectangle {{ color: "{s200}"; border.width: 1; border.color: "{stroke}"; radius: 3 }}
        ListView {{
            id: enumList
            anchors.fill: parent
            clip: true
            model: enumPopup.items || []
            boundsBehavior: Flickable.StopAtBounds
            ScrollBar.vertical: ScrollBar {{ policy: ScrollBar.AsNeeded; width: 8 }}
            delegate: Rectangle {{
                width: enumList.width
                height: 20
                color: (modelData && modelData.name === enumPopup.currentValue) ? "{accent}" : (enumMa2.containsMouse ? "{hover}" : "transparent")
                opacity: (modelData && modelData.name === enumPopup.currentValue) ? 0.28 : 1.0
                Text {{
                    anchors.left: parent.left; anchors.leftMargin: 6
                    anchors.right: parent.right; anchors.rightMargin: 6
                    anchors.verticalCenter: parent.verticalCenter
                    text: modelData ? modelData.name : ""
                    color: "{text}"; font.family: root.uiFont; font.pixelSize: 11
                    elide: Text.ElideRight
                }}
                MouseArea {{
                    id: enumMa2; anchors.fill: parent; hoverEnabled: true
                    onClicked: {{
                        if (modelData && enumPopup.propName !== "") {{
                            root.setSelectedProperty(enumPopup.propName, modelData.name);
                        }}
                        enumPopup.close();
                    }}
                }}
            }}
        }}
    }}

    // Color3 picker: palette + hex + RGB, anchored under the property swatch.
    Popup {{
        id: colorPopup
        property string propName: ""
        property int r: 163
        property int g: 162
        property int b: 165
        property var palette: [
            "#FFFFFF", "#E8E8E8", "#C7C7C7", "#A3A2A5", "#7C7C7C", "#636263", "#4B4B4B", "#383838", "#232323", "#0A0A0A",
            "#FF8A8A", "#FF5C5C", "#FF2D2D", "#E01B1B", "#C62828", "#A31D1D", "#7A1717", "#5C1010", "#3D0A0A", "#260505",
            "#FFE0A3", "#FFC46B", "#FFA726", "#FB8C00", "#EF6C00", "#D97706", "#B45309", "#8C3D06", "#5C2803", "#3A1701",
            "#D9F2C4", "#A8E063", "#66BB6A", "#43A047", "#2E7D32", "#1B5E20", "#0E3D14", "#7ED957", "#00E676", "#00C853",
            "#B3E5FC", "#4FC3F7", "#29B6F6", "#039BE5", "#1E88E5", "#1565C0", "#0D47A1", "#0A2F66", "#6A7BFF", "#2929FF",
            "#E1BEE7", "#BA68C8", "#9C27B0", "#7B1FA2", "#6A1B9A", "#4A148C", "#FF80AB", "#E91E63", "#C2185B", "#880E4F",
            "#C8B8A0", "#A1887F", "#8D6E63", "#6D4C41", "#5D4037", "#4E342E", "#3E2723", "#D2B48C", "#C19A6B", "#8B5A2B",
            "#00FFFF", "#FF00FF", "#FFFF00", "#FF0000", "#00FF00", "#0000FF", "#FF5722", "#795548", "#607D8B", "#009688"
        ]
        modal: false
        focus: true
        width: 268
        height: 300
        padding: 10
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
        background: Rectangle {{ color: "{s200}"; border.width: 1; border.color: "{stroke}"; radius: 4 }}
        Column {{
            id: colorPopupCol
            width: parent.width
            spacing: 8
            Row {{
                width: parent.width
                spacing: 6
                Rectangle {{
                    width: 40; height: 24; radius: 2
                    color: root.rgbHex(colorPopup.r, colorPopup.g, colorPopup.b)
                    border.width: 1; border.color: "{stroke}"
                }}
                Rectangle {{
                    width: parent.width - 46; height: 24; radius: 3
                    color: "{bg}"
                    border.width: 1; border.color: colorHexInput.activeFocus ? "{accent}" : "{stroke}"
                    TextInput {{
                        id: colorHexInput
                        anchors.fill: parent; anchors.leftMargin: 6; anchors.rightMargin: 6
                        verticalAlignment: TextInput.AlignVCenter
                        text: root.rgbHex(colorPopup.r, colorPopup.g, colorPopup.b)
                        color: "{text}"; selectionColor: "{accent}"; selectedTextColor: "#ffffff"
                        font.family: root.monoFont; font.pixelSize: 12
                        selectByMouse: true; clip: true
                        onEditingFinished: root.applyHexField(text)
                    }}
                }}
            }}
            RowLayout {{
                width: parent.width; spacing: 4
                Repeater {{
                    id: rgbRepeater
                    model: [ {{ label: "R", key: "r" }}, {{ label: "G", key: "g" }}, {{ label: "B", key: "b" }} ]
                    RowLayout {{
                        Layout.fillWidth: true; spacing: 3
                        Text {{
                            text: modelData.label
                            color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 11
                            Layout.preferredWidth: 10
                        }}
                        Rectangle {{
                            Layout.fillWidth: true; Layout.preferredHeight: 22; radius: 3
                            color: "{bg}"; border.width: 1; border.color: "{stroke}"
                            TextInput {{
                                id: rgbInput
                                anchors.fill: parent; anchors.leftMargin: 4; anchors.rightMargin: 4
                                verticalAlignment: TextInput.AlignVCenter
                                text: String(colorPopup[modelData.key])
                                color: "{text}"; selectionColor: "{accent}"; selectedTextColor: "#ffffff"
                                font.family: root.uiFont; font.pixelSize: 11
                                selectByMouse: true; clip: true
                                onEditingFinished: {{
                                    colorPopup[modelData.key] = root.clampByte(text);
                                    root.commitColorFields();
                                }}
                            }}
                        }}
                    }}
                }}
            }}
            Rectangle {{ width: parent.width; height: 1; color: "{stroke}" }}
            Grid {{
                width: parent.width
                columns: 10
                spacing: 2
                Repeater {{
                    model: colorPopup.palette
                    Rectangle {{
                        width: (colorPopup.width - 20 - 18) / 10
                        height: 18
                        radius: 2
                        color: modelData
                        border.width: root.rgbHex(colorPopup.r, colorPopup.g, colorPopup.b).toLowerCase() === String(modelData).toLowerCase() ? 2 : 1
                        border.color: root.rgbHex(colorPopup.r, colorPopup.g, colorPopup.b).toLowerCase() === String(modelData).toLowerCase() ? "{text}" : "{stroke}"
                        MouseArea {{
                            anchors.fill: parent
                            hoverEnabled: true
                            onClicked: {{
                                var h = String(modelData).replace("#", "");
                                colorPopup.r = parseInt(h.substr(0, 2), 16);
                                colorPopup.g = parseInt(h.substr(2, 2), 16);
                                colorPopup.b = parseInt(h.substr(4, 2), 16);
                                root.commitColorFields();
                            }}
                        }}
                    }}
                }}
            }}
            Text {{
                width: parent.width
                text: "Click a swatch or type a hex / RGB value"
                color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 10
                elide: Text.ElideRight
            }}
        }}
    }}

    // Explorer "New" (+) popup: searchable insert-object list.
    Popup {{
        id: explorerInsertMenu
        property string filter: ""
        modal: false
        focus: true
        width: 240
        height: 340
        padding: 6
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
        background: Rectangle {{ color: "{s200}"; border.width: 1; border.color: "{stroke}"; radius: 4 }}
        onOpened: {{
            filter = "";
            if (insertSearchInput) {{
                insertSearchInput.text = "";
                insertSearchInput.forceActiveFocus();
            }}
        }}
        Column {{
            width: parent.width
            spacing: 6
            Rectangle {{
                width: parent.width
                height: 24
                radius: 3
                color: "{bg}"
                border.width: 1
                border.color: insertSearchInput.activeFocus ? "{accent}" : "{stroke}"
                TextInput {{
                    id: insertSearchInput
                    anchors.fill: parent; anchors.leftMargin: 6; anchors.rightMargin: 6
                    verticalAlignment: TextInput.AlignVCenter
                    color: "{text}"
                    selectionColor: "{accent}"; selectedTextColor: "#ffffff"
                    font.family: root.uiFont; font.pixelSize: 11
                    selectByMouse: true; clip: true
                    onTextChanged: explorerInsertMenu.filter = text
                    Keys.onEscapePressed: explorerInsertMenu.close()
                    Keys.onReturnPressed: {{
                        if (insertList.count > 0) {{
                            root.insertObject(insertList.model[0]);
                            explorerInsertMenu.close();
                        }}
                    }}
                    Text {{
                        anchors.fill: parent
                        verticalAlignment: Text.AlignVCenter
                        text: "Search objects"
                        visible: !parent.text && !parent.activeFocus
                        color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 11
                    }}
                }}
            }}
            Rectangle {{ width: parent.width; height: 1; color: "{stroke}" }}
            ListView {{
                id: insertList
                width: parent.width
                height: explorerInsertMenu.height - 48
                clip: true
                model: root.filteredInsertables(explorerInsertMenu.filter)
                boundsBehavior: Flickable.StopAtBounds
                ScrollBar.vertical: ScrollBar {{ policy: ScrollBar.AsNeeded; width: 8 }}
                delegate: Rectangle {{
                    width: insertList.width
                    height: 24
                    color: insertItemMa.containsMouse ? "{hover}" : "transparent"
                    Row {{
                        anchors.left: parent.left; anchors.leftMargin: 4
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 6
                        Image {{
                            width: 14; height: 14
                            anchors.verticalCenter: parent.verticalCenter
                            source: (root.classIcons && root.classIcons[modelData]) ? root.classIcons[modelData] : ""
                            visible: source !== ""
                            fillMode: Image.PreserveAspectFit
                            smooth: true
                        }}
                        Text {{
                            anchors.verticalCenter: parent.verticalCenter
                            text: modelData
                            color: "{text}"; font.family: root.uiFont; font.pixelSize: 12
                            elide: Text.ElideRight
                        }}
                    }}
                    MouseArea {{
                        id: insertItemMa
                        anchors.fill: parent
                        hoverEnabled: true
                        onClicked: {{
                            root.insertObject(modelData);
                            explorerInsertMenu.close();
                        }}
                    }}
                }}
            }}
        }}
    }}

    Menu {{
        id: explorerContextMenu
        background: Rectangle {{ implicitWidth: 220; color: "{s200}"; border.width: 1; border.color: "{stroke}" }}
        delegate: MenuItem {{
            id: exMi; implicitHeight: 26
            contentItem: Text {{ leftPadding: 10; text: exMi.text; color: exMi.enabled ? "{text}" : "{text_muted}"; font.family: root.uiFont; font.pixelSize: 12; verticalAlignment: Text.AlignVCenter }}
            background: Rectangle {{ color: exMi.highlighted ? "{hover}" : "transparent" }}
        }}
        MenuItem {{ text: "Cut"; enabled: root.explorerSelected !== ""; onTriggered: root.cutSelected() }}
        MenuItem {{ text: "Copy"; enabled: root.explorerSelected !== ""; onTriggered: root.copySelected() }}
        MenuItem {{ text: "Paste Into"; enabled: root.explorerClipboard !== null; onTriggered: root.pasteClipboard() }}
        MenuItem {{ text: "Duplicate"; enabled: root.explorerSelected !== ""; onTriggered: root.duplicateSelected() }}
        MenuItem {{ text: "Delete"; enabled: root.explorerSelected !== ""; onTriggered: root.deleteSelected() }}
    }}

    // Studio Settings (modal, opened from File > Studio Settings... or Avatar > Settings)
    Rectangle {{
        id: settingsScrim
        anchors.fill: parent
        z: 400
        visible: root.showSettings
        color: "#99000000"

        MouseArea {{ anchors.fill: parent; onClicked: root.closeSettings() }}

        Rectangle {{
            id: settingsPanel
            width: 620
            height: Math.max(380, Math.min(root.height - 80, settingsBody.implicitHeight + 74))
            radius: 6
            color: "{s100}"
            border.width: 1
            border.color: "{stroke}"
            anchors.centerIn: parent

            // Swallow clicks so the scrim handler doesn't close the dialog.
            MouseArea {{ anchors.fill: parent }}

            ColumnLayout {{
                anchors.fill: parent
                spacing: 0

                // Title bar
                Rectangle {{
                    Layout.fillWidth: true
                    Layout.preferredHeight: 38
                    color: "{s200}"
                    Text {{
                        anchors.left: parent.left
                        anchors.leftMargin: 14
                        anchors.verticalCenter: parent.verticalCenter
                        text: "Studio Settings"
                        color: "{text}"
                        font.family: root.uiFontMedium
                        font.pixelSize: 13
                    }}
                    Rectangle {{
                        width: 28; height: 28; radius: 4
                        anchors.right: parent.right
                        anchors.rightMargin: 5
                        anchors.verticalCenter: parent.verticalCenter
                        color: closeSettingsMa.containsMouse ? "{s300}" : "transparent"
                        Text {{
                            anchors.centerIn: parent
                            text: "\u2715"
                            color: "{text_muted}"
                            font.family: root.uiFont
                            font.pixelSize: 12
                        }}
                        MouseArea {{
                            id: closeSettingsMa
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.closeSettings()
                        }}
                    }}
                    Rectangle {{
                        anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom
                        height: 1; color: "{stroke}"
                    }}
                }}

                // Security & Accounts
                ColumnLayout {{
                    id: settingsBody
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    Layout.margins: 18
                    spacing: 12

                    Text {{
                        text: "Security & Accounts"
                        color: "{text}"
                        font.family: root.uiFontMedium
                        font.pixelSize: 15
                    }}
                    Rectangle {{ Layout.fillWidth: true; Layout.preferredHeight: 1; color: "{stroke}" }}

                    // Open Cloud key header + link state
                    RowLayout {{
                        Layout.fillWidth: true
                        spacing: 8
                        Text {{
                            text: "Open Cloud API key"
                            color: "{text}"
                            font.family: root.uiFontMedium
                            font.pixelSize: 12
                        }}
                        Item {{ Layout.fillWidth: true }}
                        Rectangle {{
                            implicitWidth: ocStateRow.implicitWidth + 16
                            implicitHeight: 20
                            radius: 10
                            color: studio.opencloud_linked ? "#1E3A24" : "{s200}"
                            border.width: 1
                            border.color: studio.opencloud_linked ? "#3F7A46" : "{stroke}"
                            Row {{
                                id: ocStateRow
                                anchors.centerIn: parent
                                spacing: 5
                                Rectangle {{
                                    width: 6; height: 6; radius: 3
                                    anchors.verticalCenter: parent.verticalCenter
                                    color: studio.opencloud_linked ? "#7ED957" : "{text_muted}"
                                }}
                                Text {{
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: studio.opencloud_linked ? "Linked" : "Not linked"
                                    color: studio.opencloud_linked ? "{text}" : "{text_muted}"
                                    font.family: root.uiFont
                                    font.pixelSize: 10
                                }}
                            }}
                        }}
                    }}

                    Text {{
                        Layout.fillWidth: true
                        text: "Used for downloading asset content (Insert from the Toolbox). The key is stored in the Windows Credential Manager for this Windows account \u2014 it is never written into place files, exported games, or logs."
                        color: "{text_muted}"
                        font.family: root.uiFont
                        font.pixelSize: 11
                        wrapMode: Text.WordWrap
                    }}

                    RowLayout {{
                        Layout.fillWidth: true
                        spacing: 8

                        Text {{
                            visible: studio.opencloud_linked
                            text: studio.opencloud_mask
                            color: "{text_muted}"
                            font.family: root.monoFont
                            font.pixelSize: 11
                        }}
                        Item {{ Layout.fillWidth: true }}

                        Rectangle {{
                            Layout.preferredWidth: linkOpenCloudLabel.implicitWidth + 24
                            Layout.preferredHeight: 28
                            radius: 4
                            color: linkOpenCloudMa.containsMouse ? "#3D7BFF" : "{accent}"
                            Text {{
                                id: linkOpenCloudLabel
                                anchors.centerIn: parent
                                text: studio.opencloud_linked ? "Replace key" : "Link OpenCloud"
                                color: "white"
                                font.family: root.uiFontMedium
                                font.pixelSize: 11
                            }}
                            MouseArea {{
                                id: linkOpenCloudMa
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: root.openOpenCloudDialog()
                            }}
                        }}

                        Rectangle {{
                            visible: studio.opencloud_linked
                            Layout.preferredWidth: 74
                            Layout.preferredHeight: 28
                            radius: 4
                            color: unlinkMa.containsMouse ? "#3A2020" : "transparent"
                            border.width: 1
                            border.color: unlinkMa.containsMouse ? "#7A3B3B" : "{stroke}"
                            Text {{
                                anchors.centerIn: parent
                                text: "Unlink"
                                color: "#F87171"
                                font.family: root.uiFont
                                font.pixelSize: 11
                            }}
                            MouseArea {{
                                id: unlinkMa
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: studio.opencloud_unlink()
                            }}
                        }}
                    }}

                    Text {{
                        Layout.fillWidth: true
                        Layout.preferredHeight: visible ? 16 : 0
                        visible: studio.opencloud_status.length > 0
                        text: studio.opencloud_status
                        color: (studio.opencloud_status.indexOf("Could not") >= 0) ? "#F87171" : "{text_muted}"
                        font.family: root.uiFont
                        font.pixelSize: 10
                        elide: Text.ElideRight
                    }}

                    Item {{ Layout.fillHeight: true }}

                    Text {{
                        Layout.fillWidth: true
                        text: "Create a key at create.roblox.com \u203a Credentials \u203a API Keys with the Legacy Assets \u203a Manage permission (legacy-asset:manage) \u2014 asset:read only grants metadata. The key must belong to your user account; group-owned keys cannot download content."
                        color: "{text_muted}"
                        font.family: root.uiFont
                        font.pixelSize: 10
                        wrapMode: Text.WordWrap
                        opacity: 0.85
                    }}
                }}
            }}

            // Nested "Link OpenCloud" dialog
            Rectangle {{
                id: openCloudDialog
                anchors.fill: parent
                visible: root.showOpenCloudDialog
                color: "#AA000000"
                z: 2

                MouseArea {{
                    anchors.fill: parent
                    onClicked: root.closeOpenCloudDialog()
                }}

                Rectangle {{
                    width: 540
                    height: 256
                    radius: 6
                    color: "{s200}"
                    border.width: 1
                    border.color: "{stroke}"
                    anchors.centerIn: parent

                    MouseArea {{ anchors.fill: parent }}

                    ColumnLayout {{
                        anchors.fill: parent
                        anchors.margins: 18
                        spacing: 10

                        Text {{
                            text: "Link OpenCloud"
                            color: "{text}"
                            font.family: root.uiFontMedium
                            font.pixelSize: 14
                        }}
                        Text {{
                            Layout.fillWidth: true
                            text: "Paste an Open Cloud API key. It is kept in the Windows Credential Manager for this account only \u2014 never in the place file, an exported game, or logs."
                            color: "{text_muted}"
                            font.family: root.uiFont
                            font.pixelSize: 11
                            wrapMode: Text.WordWrap
                        }}

                        // Key input
                        Rectangle {{
                            Layout.fillWidth: true
                            Layout.preferredHeight: 36
                            radius: 4
                            color: "{s100}"
                            border.width: 1
                            border.color: openCloudInput.activeFocus ? "{accent}" : "{stroke}"

                            Text {{
                                anchors.fill: parent
                                anchors.leftMargin: 10
                                anchors.rightMargin: 64
                                verticalAlignment: Text.AlignVCenter
                                visible: openCloudInput.text.length === 0
                                text: "Paste API key"
                                color: "{text_muted}"
                                font.family: root.uiFont
                                font.pixelSize: 12
                            }}
                            TextInput {{
                                id: openCloudInput
                                anchors.fill: parent
                                anchors.leftMargin: 10
                                anchors.rightMargin: 64
                                verticalAlignment: TextInput.AlignVCenter
                                color: "{text}"
                                font.family: root.monoFont
                                font.pixelSize: 12
                                clip: true
                                selectByMouse: true
                                echoMode: root.openCloudReveal ? TextInput.Normal : TextInput.Password
                                onTextChanged: root.openCloudKey = text
                                onAccepted: root.submitOpenCloudKey()
                            }}
                            Rectangle {{
                                width: 54
                                height: 22
                                radius: 3
                                anchors.right: parent.right
                                anchors.rightMargin: 6
                                anchors.verticalCenter: parent.verticalCenter
                                color: revealMa.containsMouse ? "{s300}" : "transparent"
                                Text {{
                                    anchors.centerIn: parent
                                    text: root.openCloudReveal ? "Hide" : "Show"
                                    color: "{text_muted}"
                                    font.family: root.uiFont
                                    font.pixelSize: 9
                                }}
                                MouseArea {{
                                    id: revealMa
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: root.openCloudReveal = !root.openCloudReveal
                                }}
                            }}
                        }}

                        Text {{
                            Layout.fillWidth: true
                            Layout.preferredHeight: visible ? 14 : 0
                            visible: root.openCloudError.length > 0
                            text: root.openCloudError
                            color: "#F87171"
                            font.family: root.uiFont
                            font.pixelSize: 10
                            elide: Text.ElideRight
                        }}

                        Item {{ Layout.fillHeight: true }}

                        RowLayout {{
                            Layout.fillWidth: true
                            spacing: 8
                            Item {{ Layout.fillWidth: true }}

                            Rectangle {{
                                Layout.preferredWidth: 76
                                Layout.preferredHeight: 28
                                radius: 4
                                color: cancelKeyMa.containsMouse ? "{s300}" : "transparent"
                                border.width: 1
                                border.color: "{stroke}"
                                Text {{
                                    anchors.centerIn: parent
                                    text: "Cancel"
                                    color: "{text}"
                                    font.family: root.uiFont
                                    font.pixelSize: 11
                                }}
                                MouseArea {{
                                    id: cancelKeyMa
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: root.closeOpenCloudDialog()
                                }}
                            }}

                            Rectangle {{
                                Layout.preferredWidth: 76
                                Layout.preferredHeight: 28
                                radius: 4
                                property bool canSubmit: openCloudInput.text.trim().length > 0
                                color: !canSubmit ? "{s300}" : (saveKeyMa.containsMouse ? "#3D7BFF" : "{accent}")
                                Text {{
                                    anchors.centerIn: parent
                                    text: "Link"
                                    color: parent.canSubmit ? "white" : "{text_muted}"
                                    font.family: root.uiFontMedium
                                    font.pixelSize: 11
                                }}
                                MouseArea {{
                                    id: saveKeyMa
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    enabled: parent.canSubmit
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: root.submitOpenCloudKey()
                                }}
                            }}
                        }}
                    }}
                }}

                onVisibleChanged: if (visible) openCloudInput.forceActiveFocus()
            }}
        }}
    }}

    // Esc closes the key dialog first, then Settings.
    Shortcut {{
        sequence: "Escape"
        enabled: root.showSettings
        onActivated: {{
            if (root.showOpenCloudDialog) {{
                root.closeOpenCloudDialog();
            }} else {{
                root.closeSettings();
            }}
        }}
    }}

    Shortcut {{ sequence: "Ctrl+C"; enabled: !root.showStartPage && !root.showSettings && root.explorerSelected !== ""; onActivated: root.copySelected() }}
    Shortcut {{ sequence: "Ctrl+X"; enabled: !root.showStartPage && !root.showSettings && root.explorerSelected !== ""; onActivated: root.cutSelected() }}
    Shortcut {{ sequence: "Ctrl+V"; enabled: !root.showStartPage && !root.showSettings && root.explorerClipboard !== null; onActivated: root.pasteClipboard() }}
    Shortcut {{ sequence: "Ctrl+D"; enabled: !root.showStartPage && !root.showSettings && root.explorerSelected !== ""; onActivated: root.duplicateSelected() }}
    Shortcut {{ sequence: "Delete"; enabled: !root.showStartPage && !root.showSettings && root.explorerSelected !== ""; onActivated: root.deleteSelected() }}

        // Start page overlay (shown on launch; exact-to-Roblox start page structure)
    Rectangle {{
        id: startPage
        anchors.fill: parent
        anchors.topMargin: ribbonArea.mapToItem(null, 0, 0).y + ribbonArea.height
        anchors.bottomMargin: 24
        visible: root.showStartPage
        z: 200
        color: "{bg}"

        Row {{
            anchors.fill: parent
            spacing: 0

            // Left sidebar
            Rectangle {{
                width: 240
                height: parent.height
                color: "{s100}"
                Column {{
                    anchors.fill: parent
                    spacing: 0

                    // Branding header
                    Item {{
                        width: parent.width; height: 64
                        Row {{
                            anchors.left: parent.left; anchors.leftMargin: 16
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 10
                            Image {{
                                width: 28; height: 28
                                source: "{studio_logo}"
                                fillMode: Image.PreserveAspectFit; smooth: true; mipmap: true
                                visible: "{studio_logo}" !== ""
                            }}
                            Text {{
                                anchors.verticalCenter: parent.verticalCenter
                                text: "Rustblox Studio"
                                color: "{text}"; font.family: root.uiFontBold; font.pixelSize: 16
                            }}
                        }}
                    }}

                    Rectangle {{ width: parent.width - 24; height: 1; x: 12; color: "{stroke}"; opacity: 0.7 }}

                    Item {{ width: 1; height: 10 }}

                    // Nav items
                    Repeater {{
                        model: [
                            {{id:"Home", label:"Home", icon:"{sp_home}", iconSel:"{sp_home_f}"}},
                            {{id:"Experiences", label:"Experiences", icon:"{sp_exp}", iconSel:"{sp_exp_f}"}},
                            {{id:"Templates", label:"Templates", icon:"{sp_tpl}", iconSel:"{sp_tpl_f}"}},
                            {{id:"Recent", label:"Recents", icon:"{sp_recent}", iconSel:"{sp_recent_f}"}}
                        ]
                        delegate: Rectangle {{
                            width: startPage.width ? 240 : 240
                            height: 40
                            property bool sel: root.startNav === modelData.id
                            color: sel ? "{hover}" : (navMa.containsMouse ? "{s200}" : "transparent")
                            Rectangle {{
                                visible: sel
                                width: 3; height: 20; radius: 2
                                anchors.left: parent.left; anchors.leftMargin: 6
                                anchors.verticalCenter: parent.verticalCenter
                                color: "{accent}"
                            }}
                            Row {{
                                anchors.left: parent.left; anchors.leftMargin: 18
                                anchors.verticalCenter: parent.verticalCenter
                                spacing: 12
                                Image {{
                                    width: 20; height: 20
                                    source: sel ? (modelData.iconSel || modelData.icon) : modelData.icon
                                    fillMode: Image.PreserveAspectFit; smooth: true
                                    opacity: sel ? 1.0 : 0.85
                                }}
                                Text {{
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: modelData.label
                                    color: sel ? "{text}" : "{text_muted}"
                                    font.family: sel ? root.uiFontMedium : root.uiFont
                                    font.pixelSize: 14
                                }}
                            }}
                            MouseArea {{ id: navMa; anchors.fill: parent; hoverEnabled: true; onClicked: {{
                                        root.startNav = modelData.id;
                                        if (modelData.id === "Experiences" && studio.login_user !== "") studio.refresh_experiences();
                                    }} }}
                        }}
                    }}

                    Item {{ width: 1; Layout.fillHeight: true; height: 20 }}

                    Rectangle {{ width: parent.width - 24; height: 1; x: 12; color: "{stroke}"; opacity: 0.7 }}
                    Item {{
                        width: parent.width; height: 44
                        Row {{
                            anchors.left: parent.left; anchors.leftMargin: 18
                            anchors.verticalCenter: parent.verticalCenter; spacing: 10
                            Image {{ width: 18; height: 18; source: "{sp_archive}"; fillMode: Image.PreserveAspectFit; opacity: 0.8 }}
                            Text {{ text: "Archive"; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 13 }}
                        }}
                    }}
                }}
                Rectangle {{ anchors.top: parent.top; anchors.bottom: parent.bottom; anchors.right: parent.right; width: 1; color: "{stroke}" }}
            }}

            // Main content
            Rectangle {{
                width: parent.width - 240
                height: parent.height
                color: "{bg}"
                Column {{
                    anchors.fill: parent
                    anchors.leftMargin: 32
                    anchors.rightMargin: 32
                    anchors.topMargin: 24
                    spacing: 18

                    // Header row
                    Row {{
                        width: parent.width
                        height: 40
                        spacing: 12
                        Text {{
                            anchors.verticalCenter: parent.verticalCenter
                            text: root.startNav === "Recent" ? "Recents" : root.startNav
                            color: "{text}"; font.family: root.uiFontBold; font.pixelSize: 28
                        }}
                        Item {{ width: 1; height: 1 }} // spacer handled by layout below
                    }}

                    // Search + actions
                    Row {{
                        width: parent.width
                        height: 36
                        spacing: 10
                        Rectangle {{
                            width: Math.min(420, parent.width - 140); height: 34; radius: 4
                            color: "{s100}"; border.width: 1; border.color: "{stroke}"
                            Row {{
                                anchors.left: parent.left; anchors.leftMargin: 10
                                anchors.verticalCenter: parent.verticalCenter; spacing: 8
                                Image {{ width: 16; height: 16; source: "{sp_search}"; fillMode: Image.PreserveAspectFit; opacity: 0.7 }}
                                Text {{ text: "Search"; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 13 }}
                            }}
                        }}
                        Item {{ width: Math.max(0, parent.width - Math.min(420, parent.width - 140) - 150); height: 1 }}
                        Rectangle {{
                            width: 120; height: 34; radius: 4
                            color: newMa.containsMouse ? "#3D7BFF" : "{accent}"
                            Row {{
                                anchors.centerIn: parent; spacing: 6
                                Image {{ width: 14; height: 14; source: "{sp_plus}"; fillMode: Image.PreserveAspectFit; visible: "{sp_plus}" !== "" }}
                                Text {{ text: "New"; color: "white"; font.family: root.uiFontMedium; font.pixelSize: 13 }}
                            }}
                            MouseArea {{ id: newMa; anchors.fill: parent; hoverEnabled: true; onClicked: root.showStartPage = false }}
                        }}
                    }}

                    // Content
                    Flickable {{
                        width: parent.width
                        height: parent.height - 140
                        clip: true
                        contentWidth: width
                        contentHeight: contentCol.implicitHeight
                        boundsBehavior: Flickable.StopAtBounds
                        ScrollBar.vertical: ScrollBar {{ policy: ScrollBar.AsNeeded; width: 10 }}

                        Column {{
                            id: contentCol
                            width: parent.width
                            spacing: 24

                            // Home / Templates grid
                            Column {{
                                width: parent.width
                                spacing: 12
                                visible: root.startNav === "Home" || root.startNav === "Templates"
                                Text {{
                                    text: root.startNav === "Home" ? "All templates" : "Templates"
                                    color: "{text}"; font.family: root.uiFontMedium; font.pixelSize: 18
                                }}
                                Flow {{
                                    width: parent.width
                                    spacing: 14
                                    Repeater {{
                                        model: [
                                            {{name:"Baseplate", desc:"Classic baseplate"}},
                                            {{name:"Flat Terrain", desc:"Empty flat world"}},
                                            {{name:"Village", desc:"Village template"}},
                                            {{name:"City", desc:"Urban environment"}},
                                            {{name:"Racing", desc:"Racing starter"}},
                                            {{name:"Obby", desc:"Obstacle course"}},
                                            {{name:"Combat", desc:"Combat starter"}},
                                            {{name:"Platformer", desc:"Platformer starter"}}
                                        ]
                                        delegate: Rectangle {{
                                            width: 200; height: 150; radius: 6
                                            color: tplMa.containsMouse ? "{s300}" : "{s200}"
                                            border.width: 1; border.color: tplMa.containsMouse ? "{accent}" : "{stroke}"
                                            Column {{
                                                anchors.fill: parent; anchors.margins: 0; spacing: 0
                                                Rectangle {{
                                                    width: parent.width; height: 100
                                                    radius: 6; color: "{s100}"
                                                    // square off bottom corners visually by overlay
                                                    Rectangle {{ anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom; height: 6; color: "{s100}" }}
                                                    Image {{
                                                        anchors.centerIn: parent; width: 28; height: 28
                                                        source: "{sp_tpl}"; fillMode: Image.PreserveAspectFit; opacity: 0.35
                                                    }}
                                                }}
                                                Item {{
                                                    width: parent.width; height: 50
                                                    Column {{
                                                        anchors.left: parent.left; anchors.leftMargin: 10
                                                        anchors.verticalCenter: parent.verticalCenter; spacing: 2
                                                        Text {{ text: modelData.name; color: "{text}"; font.family: root.uiFontMedium; font.pixelSize: 13 }}
                                                        Text {{ text: modelData.desc; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 11 }}
                                                    }}
                                                }}
                                            }}
                                            MouseArea {{ id: tplMa; anchors.fill: parent; hoverEnabled: true; onClicked: root.showStartPage = false }}
                                        }}
                                    }}
                                }}
                            }}

                            // Experiences
                            Column {{
                                width: parent.width; spacing: 10
                                visible: root.startNav === "Experiences"

                                Row {{
                                    width: parent.width; height: 32; spacing: 10
                                    Text {{
                                        anchors.verticalCenter: parent.verticalCenter
                                        text: "Your experiences"; color: "{text}"
                                        font.family: root.uiFontBold; font.pixelSize: 24
                                    }}
                                    Item {{ width: 1; height: 1; Layout.fillWidth: true }}
                                    Rectangle {{
                                        visible: studio.login_user !== ""
                                        width: 96; height: 28; radius: 4
                                        color: expRefMa.containsMouse ? "{hover}" : "{s200}"
                                        border.width: 1; border.color: "{stroke}"
                                        anchors.verticalCenter: parent.verticalCenter
                                        Row {{
                                            anchors.centerIn: parent; spacing: 6
                                            Text {{ text: studio.experiences_busy ? "…" : "Refresh"; color: "{text}"; font.family: root.uiFont; font.pixelSize: 12 }}
                                        }}
                                        MouseArea {{
                                            id: expRefMa; anchors.fill: parent; hoverEnabled: true
                                            enabled: !studio.experiences_busy
                                            onClicked: studio.refresh_experiences()
                                        }}
                                    }}
                                }}

                                Text {{
                                    width: parent.width; wrapMode: Text.Wrap
                                    visible: studio.place_busy || (studio.place_status || "") !== "" || studio.experiences_busy || (root.experiencesStatus || "") !== ""
                                    text: studio.place_busy ? (studio.place_status || "Opening place…") : ((studio.place_status || "") !== "" ? studio.place_status : (root.experiencesStatus || ""))
                                    color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 12
                                }}

                                Rectangle {{
                                    visible: studio.login_user === ""
                                    width: parent.width; height: 140; radius: 6; color: "{s100}"; border.width: 1; border.color: "{stroke}"
                                    Column {{
                                        anchors.centerIn: parent; spacing: 8
                                        Image {{ anchors.horizontalCenter: parent.horizontalCenter; width: 28; height: 28; source: "{sp_exp}"; opacity: 0.5 }}
                                        Text {{ text: "Sign in to Roblox to see experiences you can edit."; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 13 }}
                                        Rectangle {{
                                            width: 140; height: 30; radius: 4; color: "{accent}"
                                            anchors.horizontalCenter: parent.horizontalCenter
                                            Text {{ anchors.centerIn: parent; text: "Sign in"; color: "#FFFFFF"; font.family: root.uiFontMedium; font.pixelSize: 12 }}
                                            MouseArea {{ anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: studio.login_roblox() }}
                                        }}
                                    }}
                                }}

                                Rectangle {{
                                    visible: studio.login_user !== "" && !studio.experiences_busy && (root.experiences || []).length === 0 && (root.experiencesStatus || "") !== ""
                                    width: parent.width; height: 100; radius: 6; color: "{s100}"; border.width: 1; border.color: "{stroke}"
                                    Text {{
                                        anchors.centerIn: parent; width: parent.width - 32; horizontalAlignment: Text.AlignHCenter
                                        wrapMode: Text.Wrap; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 13
                                        text: root.experiencesStatus
                                    }}
                                }}

                                Flow {{
                                    visible: studio.login_user !== ""
                                    width: parent.width
                                    spacing: 14
                                    Repeater {{
                                        model: root.experiences || []
                                        delegate: Rectangle {{
                                            width: 240; height: 190; radius: 6
                                            color: expMa.containsMouse ? "{s300}" : "{s200}"
                                            border.width: 1; border.color: expMa.containsMouse ? "{accent}" : "{stroke}"
                                            clip: true
                                            Column {{
                                                anchors.fill: parent
                                                spacing: 0
                                                Rectangle {{
                                                    width: parent.width; height: 126; color: "{s100}"
                                                    Image {{
                                                        anchors.fill: parent
                                                        source: modelData.thumbnail || ""
                                                        fillMode: Image.PreserveAspectCrop
                                                        asynchronous: true
                                                        visible: (modelData.thumbnail || "") !== ""
                                                    }}
                                                    Image {{
                                                        anchors.centerIn: parent; width: 32; height: 32
                                                        source: "{sp_exp}"; opacity: 0.35
                                                        visible: (modelData.thumbnail || "") === ""
                                                    }}
                                                }}
                                                Item {{
                                                    width: parent.width; height: 64
                                                    Column {{
                                                        anchors.fill: parent; anchors.margins: 10; spacing: 2
                                                        Text {{
                                                            width: parent.width
                                                            text: modelData.name || "Experience"
                                                            color: "{text}"; font.family: root.uiFontMedium; font.pixelSize: 13
                                                            elide: Text.ElideRight
                                                        }}
                                                        Text {{
                                                            width: parent.width
                                                            text: "Universe " + (modelData.universe_id || "") + " · Place " + (modelData.root_place_id || "")
                                                            color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 10
                                                            elide: Text.ElideRight
                                                        }}
                                                    }}
                                                }}
                                            }}
                                            MouseArea {{
                                                id: expMa
                                                anchors.fill: parent; hoverEnabled: true
                                                cursorShape: Qt.PointingHandCursor
                                                enabled: !studio.place_busy
                                                onClicked: studio.open_experience(modelData.universe_id || 0, modelData.root_place_id || 0, modelData.name || "Place")
                                            }}
                                        }}
                                    }}
                                }}
                            }}

                            // Recent
                            Column {{
                                width: parent.width; spacing: 8
                                visible: root.startNav === "Recent"
                                Text {{ text: "Recent files"; color: "{text}"; font.family: root.uiFontMedium; font.pixelSize: 18 }}
                                Rectangle {{
                                    width: parent.width; height: 120; radius: 6; color: "{s100}"; border.width: 1; border.color: "{stroke}"
                                    Column {{
                                        anchors.centerIn: parent; spacing: 8
                                        Image {{ anchors.horizontalCenter: parent.horizontalCenter; width: 28; height: 28; source: "{sp_recent}"; opacity: 0.5 }}
                                        Text {{ text: "No recent files"; color: "{text_muted}"; font.family: root.uiFont; font.pixelSize: 13 }}
                                    }}
                                }}
                            }}
                        }}
                    }}
                }}
            }}
        }}
    }}
}}
"##,
        place_name = place_name,
        ribbon_json = ribbon_json,
        explorer_json = explorer_json,
        all_enums_json = all_enums_json_str,
        class_properties_json = class_properties_json_str,
        class_icons_json = class_icons_json,
        insertable_classes_json = insertable_classes_json,
        toolbox_categories_json = toolbox_categories_json,
        chevron_right_icon = chevron_right_icon,
        chevron_down_icon = chevron_down_icon,
        sp_home = sp_home,
        sp_home_f = sp_home_f,
        sp_exp = sp_exp,
        sp_exp_f = sp_exp_f,
        sp_tpl = sp_tpl,
        sp_tpl_f = sp_tpl_f,
        sp_recent = sp_recent,
        sp_recent_f = sp_recent_f,
        sp_search = sp_search,
        sp_archive = sp_archive,
        sp_plus = sp_plus,
        studio_logo = studio_logo,
        icon_script = icon_script,
        icon_local_script = icon_local_script,
        icon_viewport = icon_viewport,
        active_tab = active_tab,
        font_regular = font_regular,
        font_medium = font_medium,
        font_bold = font_bold,
        font_mono = font_mono,
        bg = bg,
        s100 = s100,
        s200 = s200,
        s300 = s300,
        stroke = stroke,
        text = text,
        text_muted = text_muted,
        accent = accent,
        hover = hover,
        stroke_w = stroke_w.max(1.0) as i32,
        menu_fs = menu_fs as i32,
        tab_fs = tab_fs as i32,
        ribbon_fs = ribbon_fs as i32,
    )
}

fn main() -> Result<()> {
    let tokens = DesignTokens::bundled(ThemeName::Default, ColorMode::Dark)?;
    let ribbon = RibbonDefinition::bundled()?;
    let strings = Strings::bundled("ribbon").ok();
    let root = workspace_root();

    let place_name = "Place1".to_string();
    let window_title = format!("{place_name} - Rustblox Studio");
    let icon_path = root.join("assets/studio/branding/RustbloxStudio.ico");

    let mut engine = QmlEngine::new();
    let app = QObjectBox::new(StudioApp::default());
    engine.set_object_property("studio".into(), app.pinned());

    let qml = build_qml(
        &tokens,
        &ribbon,
        &root,
        strings.as_ref(),
        &place_name,
        &file_url(&icon_path),
    );
    engine.load_data(qml.into());

    #[cfg(windows)]
    {
        let title = window_title.clone();
        let icon = icon_path.clone();
        std::thread::spawn(move || set_window_icon_windows(&title, &icon));
    }

    engine.exec();
    Ok(())
}

#[cfg(windows)]
fn set_window_icon_windows(window_title: &str, icon_path: &Path) {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTSIZE,
        LR_LOADFROMFILE, WM_SETICON,
    };

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(once(0)).collect()
    }

    // The QML window may not exist yet at this point; try a few times briefly.
    let title_w = wide(OsStr::new(window_title));
    let mut hwnd: HWND = std::ptr::null_mut();
    for _ in 0..50 {
        hwnd = unsafe { FindWindowW(std::ptr::null(), title_w.as_ptr()) };
        if !hwnd.is_null() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    if hwnd.is_null() {
        return;
    }

    let icon_w = wide(icon_path.as_os_str());
    let big = unsafe {
        LoadImageW(
            std::ptr::null_mut(),
            icon_w.as_ptr(),
            IMAGE_ICON,
            0,
            0,
            LR_LOADFROMFILE | LR_DEFAULTSIZE,
        )
    };
    let small = unsafe {
        LoadImageW(
            std::ptr::null_mut(),
            icon_w.as_ptr(),
            IMAGE_ICON,
            16,
            16,
            LR_LOADFROMFILE,
        )
    };
    if !big.is_null() {
        unsafe { SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, big as isize) };
    }
    if !small.is_null() {
        unsafe { SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, small as isize) };
    }
}
