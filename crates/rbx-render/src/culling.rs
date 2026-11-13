use glam::{Mat4, Vec3, Vec4};

#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Option<Self> {
        let mut it = points.into_iter();
        let first = it.next()?;
        let mut min = first;
        let mut max = first;
        for p in it {
            min = min.min(p);
            max = max.max(p);
        }
        Some(Self { min, max })
    }

    /// Conservative world-space AABB for a unit cube [-0.5,0.5]^3 transformed by `model`.
    pub fn from_unit_cube(model: Mat4) -> Self {
        let corners = [
            Vec3::new(-0.5, -0.5, -0.5),
            Vec3::new( 0.5, -0.5, -0.5),
            Vec3::new(-0.5,  0.5, -0.5),
            Vec3::new( 0.5,  0.5, -0.5),
            Vec3::new(-0.5, -0.5,  0.5),
            Vec3::new( 0.5, -0.5,  0.5),
            Vec3::new(-0.5,  0.5,  0.5),
            Vec3::new( 0.5,  0.5,  0.5),
        ];
        let world = corners.into_iter().map(|c| model.transform_point3(c));
        Self::from_points(world).unwrap_or(Self { min: Vec3::ZERO, max: Vec3::ZERO })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Plane {
    pub normal: Vec3,
    pub d: f32,
}

impl Plane {
    pub fn from_vec4(v: Vec4) -> Self {
        let n = Vec3::new(v.x, v.y, v.z);
        let len = n.length();
        if len > 1e-6 {
            Self { normal: n / len, d: v.w / len }
        } else {
            Self { normal: Vec3::Y, d: 0.0 }
        }
    }
    pub fn distance(&self, p: Vec3) -> f32 {
        self.normal.dot(p) + self.d
    }
}

/// View frustum planes extracted from a view-projection matrix (row-major as in glam's to_cols_array gives column-major;
/// we use glam Mat4 methods). Planes point inward.
#[derive(Clone, Copy, Debug)]
pub struct Frustum {
    pub planes: [Plane; 6], // left, right, bottom, top, near, far
}

impl Frustum {
    pub fn from_view_proj(vp: Mat4) -> Self {
        // glam is column-major; extract rows for Gribb/Hartmann
        let r0 = vp.row(0);
        let r1 = vp.row(1);
        let r2 = vp.row(2);
        let r3 = vp.row(3);
        Self {
            planes: [
                Plane::from_vec4(r3 + r0), // left
                Plane::from_vec4(r3 - r0), // right
                Plane::from_vec4(r3 + r1), // bottom
                Plane::from_vec4(r3 - r1), // top
                Plane::from_vec4(r3 + r2), // near (wgpu/Vulkan/D3D depth [0,1] with glam perspective_rh? check)
                Plane::from_vec4(r3 - r2), // far
            ],
        }
    }

    /// Returns true if the AABB is (at least partially) inside or intersecting the frustum.
    pub fn intersects_aabb(&self, aabb: &Aabb) -> bool {
        for plane in &self.planes {
            // positive vertex relative to plane normal
            let p = Vec3::new(
                if plane.normal.x >= 0.0 { aabb.max.x } else { aabb.min.x },
                if plane.normal.y >= 0.0 { aabb.max.y } else { aabb.min.y },
                if plane.normal.z >= 0.0 { aabb.max.z } else { aabb.min.z },
            );
            if plane.distance(p) < 0.0 {
                return false; // completely outside this plane
            }
        }
        true
    }
}
