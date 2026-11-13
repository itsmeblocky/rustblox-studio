use glam::{Mat4, Vec3};

#[derive(Debug, Clone)]
pub struct Camera {
    pub focus: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub fov_y_degrees: f32,
    pub near: f32,
    pub far: f32,
    /// WASD/QE acceleration ramp in `0..=1`, advanced by [`Camera::fly_input`].
    /// Resets to zero whenever movement stops, so each burst starts gently.
    pub fly_ramp: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            focus: Vec3::new(0.0, 0.0, 0.0),
            yaw: -0.6,
            pitch: 0.55,
            distance: 80.0,
            fov_y_degrees: 70.0,
            near: 0.1,
            far: 5000.0,
            fly_ramp: 0.0,
        }
    }
}

impl Camera {
    pub fn position(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let offset = Vec3::new(cy * cp, sp, sy * cp) * self.distance;
        self.focus + offset
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.position(), self.focus, Vec3::Y)
    }

    pub fn proj(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(self.fov_y_degrees.to_radians(), aspect.max(0.01), self.near, self.far)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.proj(aspect) * self.view()
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw += dx * 0.005;
        self.pitch = (self.pitch + dy * 0.005).clamp(-1.45, 1.45);
    }

    pub fn pan(&mut self, dx: f32, dy: f32) {
        let (sy, cy) = self.yaw.sin_cos();
        let right = Vec3::new(-sy, 0.0, cy);
        let up = Vec3::Y;
        let s = self.distance * 0.002;
        self.focus += (-right * dx + up * dy) * s;
    }

    pub fn zoom(&mut self, delta: f32) {
        let factor = (1.0 - delta * 0.001).clamp(0.5, 1.5);
        self.distance = (self.distance * factor).clamp(1.0, 5000.0);
    }

    /// Flight speed for WASD/QE, in studs per second. Scales with the zoom
    /// distance so a zoomed-out view still feels responsive (Studio-like).
    pub fn fly_speed(&self) -> f32 {
        (self.distance * 0.8).clamp(8.0, 320.0)
    }

    /// Moves the camera rig through space: `forward` along the view axis,
    /// `strafe` towards camera-right and `rise` along world up. Distances are in
    /// world studs, so callers multiply a speed by a delta time.
    ///
    /// The rig moves as a whole: [`Camera::position`] is derived from `focus`,
    /// so translating `focus` translates the eye and target together.
    pub fn fly(&mut self, forward: f32, strafe: f32, rise: f32) {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        // position() sits at focus + (cy*cp, sp, sy*cp)*distance, so the view
        // direction is the negation of that offset. Camera-right matches the
        // view matrix basis `cross(look, up)` so D really moves screen-right.
        let look = Vec3::new(-cy * cp, -sp, -sy * cp);
        let right = Vec3::new(sy, 0.0, -cy);
        self.focus += look * forward + right * strafe + Vec3::Y * rise;
    }

    /// Advances WASD/QE flight by `dt` seconds with acceleration: the step
    /// starts at ~10 % and ramps to the full [`Camera::fly_speed`] over
    /// [`FLY_RAMP_SECONDS`]. Calling with no input (keys released) resets the
    /// ramp, so the next burst starts slow again.
    pub fn fly_input(&mut self, forward: f32, strafe: f32, rise: f32, speed_mult: f32, dt: f32) {
        if forward == 0.0 && strafe == 0.0 && rise == 0.0 {
            self.fly_ramp = 0.0;
            return;
        }
        let dt = dt.max(0.0);
        self.fly_ramp = (self.fly_ramp + dt / FLY_RAMP_SECONDS).min(1.0);
        let t = self.fly_ramp;
        // Smoothstep: gentle start, no hard jolt at full speed.
        let ease = 0.1 + 0.9 * (t * t * (3.0 - 2.0 * t));
        let step = self.fly_speed() * speed_mult.max(0.0) * ease * dt;
        self.fly(forward * step, strafe * step, rise * step);
    }

    /// Drops the acceleration ramp (e.g. when the pointer leaves the viewport
    /// mid-flight, so the next burst does not start at full speed).
    pub fn reset_fly(&mut self) {
        self.fly_ramp = 0.0;
    }

    /// Generates a world-space ray from normalized device-like screen coordinates.
    /// `x`,`y` are in pixel space with origin at top-left; `w`,`h` are viewport size.
    pub fn screen_ray(&self, x: f32, y: f32, w: f32, h: f32, aspect: f32) -> (glam::Vec3, glam::Vec3) {
        let ndc_x = (x / w.max(1.0)) * 2.0 - 1.0;
        let ndc_y = 1.0 - (y / h.max(1.0)) * 2.0;
        let inv = self.view_proj(aspect).inverse();
        let near = inv.project_point3(glam::Vec3::new(ndc_x, ndc_y, 0.0));
        let far = inv.project_point3(glam::Vec3::new(ndc_x, ndc_y, 1.0));
        let dir = (far - near).normalize_or_zero();
        (near, dir)
    }
}

/// Seconds of held input for [`Camera::fly_input`] to reach full speed.
pub const FLY_RAMP_SECONDS: f32 = 0.6;

/// Keyboard flight axes read straight from the OS, so movement works even when
/// the viewport does not own keyboard focus (the embedded child window uses
/// `WS_EX_NOACTIVATE`, Qt keeps focus).
///
/// Returns `(forward, strafe, rise, speed_multiplier)` where `W/S` drive
/// forward/back, `A/D` strafe left/right, `Q/E` move down/up and `Shift`
/// multiplies the speed.
#[cfg(windows)]
pub fn keyboard_fly_axes() -> (f32, f32, f32, f32) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
    let down = |vk: i32| unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 };
    let axis = |pos: i32, neg: i32| -> f32 {
        (if down(pos) { 1.0 } else { 0.0 }) - (if down(neg) { 1.0 } else { 0.0 })
    };
    let forward = axis('W' as i32, 'S' as i32);
    let strafe = axis('D' as i32, 'A' as i32);
    let rise = axis('E' as i32, 'Q' as i32);
    let speed = if down(VK_SHIFT as i32) { 4.0 } else { 1.0 };
    (forward, strafe, rise, speed)
}

/// Non-Windows stub: no global key state, so flight is a no-op there.
#[cfg(not(windows))]
pub fn keyboard_fly_axes() -> (f32, f32, f32, f32) {
    (0.0, 0.0, 0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fly_moves_focus_along_view_axis() {
        let mut cam = Camera::default();
        let before = cam.focus;
        let pos_before = cam.position();
        let look = (cam.focus - cam.position()).normalize();
        cam.fly(10.0, 0.0, 0.0);
        let delta = cam.focus - before;
        assert!((delta - look * 10.0).length() < 1e-3, "forward = view axis");
        // The eye travels with the focus (rigid move).
        assert!((cam.position() - pos_before - look * 10.0).length() < 1e-3);
    }

    #[test]
    fn fly_strafe_is_horizontal_and_rise_is_world_up() {
        let mut cam = Camera::default();
        cam.pitch = 0.9; // steep pitch must not tilt the strafe axis
        let before = cam.focus;
        cam.fly(0.0, 5.0, 0.0);
        let strafe = cam.focus - before;
        assert!(strafe.y.abs() < 1e-5, "strafe stays horizontal: {strafe:?}");
        cam.focus = before;
        cam.fly(0.0, 0.0, 7.0);
        let rise = cam.focus - before;
        assert!(rise.y > 6.99 && rise.x.abs() < 1e-5 && rise.z.abs() < 1e-5);
    }

    #[test]
    fn fly_strafe_matches_camera_right() {
        let mut cam = Camera::default();
        cam.yaw = 0.7;
        cam.pitch = 0.3;
        let look = (cam.focus - cam.position()).normalize();
        let right = look.cross(Vec3::Y).normalize();
        let before = cam.focus;
        cam.fly(0.0, 3.0, 0.0);
        let delta = (cam.focus - before).normalize();
        assert!(
            (delta - right).length() < 1e-4,
            "D must strafe towards screen-right: {delta:?} vs {right:?}"
        );
        // A (negative strafe) goes the other way.
        cam.focus = before;
        cam.fly(0.0, -3.0, 0.0);
        assert!(((cam.focus - before).normalize() + right).length() < 1e-4);
    }

    #[test]
    fn fly_input_ramps_speed_and_resets_when_stopping() {
        let mut cam = Camera::default();
        cam.distance = 100.0;
        let dt = 1.0 / 60.0;
        let full_step = cam.fly_speed() * dt;

        let before = cam.focus;
        cam.fly_input(1.0, 0.0, 0.0, 1.0, dt);
        let first = (cam.focus - before).length();
        assert!(first < full_step * 0.4, "first frame is slow: {first} vs {full_step}");

        // Two seconds of held input: now at full speed.
        for _ in 0..120 {
            cam.fly_input(1.0, 0.0, 0.0, 1.0, dt);
        }
        let before = cam.focus;
        cam.fly_input(1.0, 0.0, 0.0, 1.0, dt);
        let settled = (cam.focus - before).length();
        assert!(settled > full_step * 0.95, "settled speed: {settled} vs {full_step}");

        // Releasing the key drops the ramp...
        cam.fly_input(0.0, 0.0, 0.0, 1.0, dt);
        assert_eq!(cam.fly_ramp, 0.0);
        // ...so the next tap starts slow again.
        let before = cam.focus;
        cam.fly_input(1.0, 0.0, 0.0, 1.0, dt);
        assert!((cam.focus - before).length() < full_step * 0.4);
    }

    #[test]
    fn fly_speed_scales_with_distance() {
        let mut near = Camera::default();
        let mut far = Camera::default();
        near.distance = 4.0;
        far.distance = 400.0;
        assert!(far.fly_speed() > near.fly_speed());
        assert!(near.fly_speed() >= 8.0);
    }
}
