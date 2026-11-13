use glam::Vec3;
use rbx_core::{AtmosphereSettings, LightingSettings, PostEffectsSettings, SkySettings};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LightingTechnology {
    Legacy,
    Voxel,
    Compatibility,
    ShadowMap,
    Future,
}

impl LightingTechnology {
    pub fn from_str(s: &str) -> Self {
        match s {
            "Legacy" => Self::Legacy,
            "Voxel" => Self::Voxel,
            "Compatibility" => Self::Compatibility,
            "ShadowMap" => Self::ShadowMap,
            "Future" => Self::Future,
            _ => Self::Future,
        }
    }
    pub fn as_u32(self) -> u32 {
        match self {
            Self::Legacy => 0,
            Self::Voxel => 1,
            Self::Compatibility => 2,
            Self::ShadowMap => 3,
            Self::Future => 4,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LightingState {
    pub technology: LightingTechnology,
    pub ambient: Vec3,
    pub outdoor_ambient: Vec3,
    pub brightness: f32,
    pub clock_time: f32,
    pub geographic_latitude: f32,
    pub color_shift_top: Vec3,
    pub color_shift_bottom: Vec3,
    pub environment_diffuse_scale: f32,
    pub environment_specular_scale: f32,
    pub exposure_compensation: f32,
    pub global_shadows: bool,
    pub shadow_softness: f32,
    pub fog_color: Vec3,
    pub fog_start: f32,
    pub fog_end: f32,
    pub sun_direction: Vec3, // points toward sun
    pub sky: Option<SkySettings>,
    pub atmosphere: Option<AtmosphereSettings>,
    pub post: PostEffectsSettings,
}

impl Default for LightingState {
    fn default() -> Self {
        Self::from_settings(&LightingSettings {
            technology: "Future".to_string(),
            ambient: [0.0, 0.0, 0.0],
            outdoor_ambient: [0.5, 0.5, 0.5],
            brightness: 2.0,
            clock_time: 14.5,
            geographic_latitude: 0.0,
            color_shift_top: [0.0, 0.0, 0.0],
            color_shift_bottom: [0.0, 0.0, 0.0],
            environment_diffuse_scale: 1.0,
            environment_specular_scale: 1.0,
            exposure_compensation: 1.0, // will be used as EV-ish; tuned
            global_shadows: true,
            shadow_softness: 0.2,
            fog_color: [0.75, 0.75, 0.75],
            fog_start: 0.0,
            fog_end: 100000.0,
        }, None, None, &PostEffectsSettings::default())
    }
}

impl LightingState {
    pub fn from_settings(
        l: &LightingSettings,
        sky: Option<&SkySettings>,
        atmosphere: Option<&AtmosphereSettings>,
        post: &PostEffectsSettings,
    ) -> Self {
        let technology = LightingTechnology::from_str(&l.technology);
        let sun_direction = sun_direction_from_clock(l.clock_time, l.geographic_latitude);
        Self {
            technology,
            ambient: Vec3::from_array(l.ambient),
            outdoor_ambient: Vec3::from_array(l.outdoor_ambient),
            brightness: l.brightness,
            clock_time: l.clock_time,
            geographic_latitude: l.geographic_latitude,
            color_shift_top: Vec3::from_array(l.color_shift_top),
            color_shift_bottom: Vec3::from_array(l.color_shift_bottom),
            environment_diffuse_scale: l.environment_diffuse_scale,
            environment_specular_scale: l.environment_specular_scale,
            exposure_compensation: l.exposure_compensation,
            global_shadows: l.global_shadows,
            shadow_softness: l.shadow_softness,
            fog_color: Vec3::from_array(l.fog_color),
            fog_start: l.fog_start,
            fog_end: l.fog_end,
            sun_direction,
            sky: sky.cloned(),
            atmosphere: atmosphere.cloned(),
            post: post.clone(),
        }
    }

    /// Effective exposure multiplier (approx). Roblox uses ExposureCompensation in stops-like fashion.
    pub fn exposure_mul(&self) -> f32 {
        2.0_f32.powf(self.exposure_compensation)
    }
}

/// Roblox-style sun direction from ClockTime (hours, 0-24) and GeographicLatitude (degrees).
/// 6:00 sunrise (east-ish), 12:00 overhead, 18:00 sunset. Latitude tilts the arc.
pub fn sun_direction_from_clock(clock_time: f32, latitude_deg: f32) -> Vec3 {
    let minutes = (clock_time.rem_euclid(24.0)) * 60.0;
    let day_angle = (minutes / (24.0 * 60.0)) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
    // base: sun moves in X/Y plane (east-up-west)
    let sun_y = day_angle.sin();
    let sun_x = day_angle.cos();
    let lat = latitude_deg.to_radians();
    // tilt by latitude around X to affect north/south component
    let (sl, cl) = lat.sin_cos();
    let dir = Vec3::new(sun_x * cl, sun_y, sun_x * sl).normalize_or_zero();
    if dir.length_squared() < 1e-6 { Vec3::Y } else { dir }
}
