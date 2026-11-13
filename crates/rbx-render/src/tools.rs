//! Viewport tool state (Select/Move/Rotate/Scale) and helpers.

use crate::culling::Aabb;
use glam::{Mat4, Vec3};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMode {
    Select,
    Move,
    Scale,
    Rotate,
    Transform,
}

impl ToolMode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "Move" => Self::Move,
            "Scale" => Self::Scale,
            "Rotate" => Self::Rotate,
            "Transform" => Self::Transform,
            _ => Self::Select,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

#[derive(Debug, Clone)]
pub struct SnapSettings {
    pub enabled: bool,
    pub move_step: f32,
    pub rotate_deg: f32,
    pub scale_step: f32,
}

impl Default for SnapSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            move_step: 1.0,
            rotate_deg: 15.0,
            scale_step: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DragState {
    pub axis: Axis,
    pub start_mouse: (f32, f32),
    pub start_pos: Vec3,
    pub start_size: Vec3,
    /// rotation matrix rows as stored in CFrameValue
    pub start_rot: [[f32; 3]; 3],
    pub origin: Vec3,
}

/// Ray vs AABB (slab). Returns distance t along ray if hit in front of origin.
pub fn ray_aabb(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let mut tmin = -f32::INFINITY;
    let mut tmax = f32::INFINITY;
    for i in 0..3 {
        let o = origin[i];
        let d = dir[i];
        let mn = min[i];
        let mx = max[i];
        if d.abs() < 1e-8 {
            if o < mn || o > mx {
                return None;
            }
        } else {
            let inv = 1.0 / d;
            let mut t1 = (mn - o) * inv;
            let mut t2 = (mx - o) * inv;
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
            }
            tmin = tmin.max(t1);
            tmax = tmax.min(t2);
            if tmin > tmax {
                return None;
            }
        }
    }
    let t = if tmin >= 0.0 { tmin } else { tmax };
    if t >= 0.0 { Some(t) } else { None }
}

/// Conservative AABB for a part transformed by its cframe and size.
pub fn part_world_aabb(part: &rbx_core::Part) -> Aabb {
    let model = part.cframe.to_mat4() * Mat4::from_scale(part.size.to_glam());
    Aabb::from_unit_cube(model)
}

pub fn axis_vec(axis: Axis) -> Vec3 {
    match axis {
        Axis::X => Vec3::X,
        Axis::Y => Vec3::Y,
        Axis::Z => Vec3::Z,
    }
}

pub fn axis_color(axis: Axis) -> [f32; 4] {
    match axis {
        Axis::X => [0.9, 0.2, 0.2, 1.0],
        Axis::Y => [0.2, 0.85, 0.2, 1.0],
        Axis::Z => [0.2, 0.4, 0.95, 1.0],
    }
}

pub fn snap_value(v: f32, step: f32, enabled: bool) -> f32 {
    if !enabled || step.abs() < 1e-6 {
        v
    } else {
        (v / step).round() * step
    }
}

pub fn snap_angle_deg(deg: f32, step: f32, enabled: bool) -> f32 {
    snap_value(deg, step, enabled)
}
