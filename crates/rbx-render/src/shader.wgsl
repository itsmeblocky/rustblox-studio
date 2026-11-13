// Rustblox viewport shader - Roblox Lighting-parameter-faithful, per-Technology switching.
// Calibration target: Roblox Studio/Client visual response. Full-stack hooks included.

struct CameraUniform {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: CameraUniform;

struct LightingUniform {
    sun_dir: vec4<f32>,            // xyz toward sun, w = brightness
    ambient: vec4<f32>,            // rgb ambient, w = exposureCompensation (stops)
    outdoor_ambient: vec4<f32>,    // rgb, w = environmentDiffuseScale
    fog_color: vec4<f32>,          // rgb, w = fogStart
    fog_params: vec4<f32>,         // x=fogEnd, y=environmentSpecularScale, z=shadowSoftness, w=globalShadows(0/1)
    tech_params: vec4<f32>,        // x=technology (0 Legacy,1 Voxel,2 Compat,3 ShadowMap,4 Future), y=clockTime, z=geoLat, w=hasAtmosphere
    color_shift: vec4<f32>,        // rgb top shift, w unused
    color_shift_bottom: vec4<f32>, // rgb bottom shift, w unused
    atmo: vec4<f32>,               // density, offset, glare, haze
    atmo_color: vec4<f32>,         // rgb color, w unused
    atmo_decay: vec4<f32>,         // rgb decay, w unused
};
@group(0) @binding(1) var<uniform> light: LightingUniform;
// Layer 0..N: material albedo/detail; above that: decal images.
@group(0) @binding(2) var material_textures: texture_2d_array<f32>;
@group(0) @binding(3) var material_sampler: sampler;
@group(0) @binding(4) var material_metal_textures: texture_2d_array<f32>;
@group(0) @binding(5) var material_rough_textures: texture_2d_array<f32>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) material: vec4<f32>, // metallic, roughness, reflectance, kind
    @location(4) uv: vec2<f32>,
    @location(5) layers: vec2<f32>, // x = material layer, y = decal layer (-1 = none)
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) material: vec4<f32>,
    @location(4) uv: vec2<f32>,
    @location(5) layers: vec2<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world = vec4<f32>(in.position, 1.0);
    out.clip_position = camera.view_proj * world;
    out.world_pos = in.position;
    out.normal = normalize(in.normal);
    out.color = in.color;
    out.material = in.material;
    out.uv = in.uv;
    out.layers = in.layers;
    return out;
}

/// Tiling scale: one texture tile covers 4 studs (stud spacing ≈ 1 stud).
const MATERIAL_UV_SCALE: f32 = 0.25;

/// Samples the material layer triplanar-style, so boxes, spheres, cylinders and
/// imported meshes all get consistent surface detail without UV unwrapping.
fn sample_material(world_pos: vec3<f32>, n: vec3<f32>, layer: i32) -> vec3<f32> {
    let w = pow(abs(n), vec3<f32>(4.0));
    let wsum = max(w.x + w.y + w.z, 1e-4);
    let p = world_pos * MATERIAL_UV_SCALE;
    let sx = textureSample(material_textures, material_sampler, p.zy, layer).rgb;
    let sy = textureSample(material_textures, material_sampler, p.xz, layer).rgb;
    let sz = textureSample(material_textures, material_sampler, p.xy, layer).rgb;
    return (sx * w.x + sy * w.y + sz * w.z) / wsum;
}

fn sample_material_scalar(tex: texture_2d_array<f32>, world_pos: vec3<f32>, n: vec3<f32>, layer: i32) -> f32 {
    let w = pow(abs(n), vec3<f32>(4.0));
    let wsum = max(w.x + w.y + w.z, 1e-4);
    let p = world_pos * MATERIAL_UV_SCALE;
    let sx = textureSample(tex, material_sampler, p.zy, layer).r;
    let sy = textureSample(tex, material_sampler, p.xz, layer).r;
    let sz = textureSample(tex, material_sampler, p.xy, layer).r;
    return (sx * w.x + sy * w.y + sz * w.z) / wsum;
}

fn saturate(x: f32) -> f32 { return clamp(x, 0.0, 1.0); }
fn saturate3(v: vec3<f32>) -> vec3<f32> { return clamp(v, vec3<f32>(0.0), vec3<f32>(1.0)); }

fn distribution_ggx(nh: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let d = nh * nh * (a2 - 1.0) + 1.0;
    return a2 / max(3.14159265 * d * d, 1e-5);
}
fn geometry_schlick_ggx(nv: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    return nv / (nv * (1.0 - k) + k);
}
fn geometry_smith(nv: f32, nl: f32, roughness: f32) -> f32 {
    return geometry_schlick_ggx(nv, roughness) * geometry_schlick_ggx(nl, roughness);
}
fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (1.0 - f0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

// Simple sky gradient used when no Sky cubemap IBL is bound (full IBL path can replace this).
fn sky_color(dir: vec3<f32>, top_shift: vec3<f32>, bottom_shift: vec3<f32>) -> vec3<f32> {
    let t = saturate(dir.y * 0.5 + 0.5);
    let horizon = vec3<f32>(0.62, 0.72, 0.82);
    let zenith = vec3<f32>(0.24, 0.45, 0.85);
    let ground = vec3<f32>(0.35, 0.34, 0.33);
    var c = mix(horizon, zenith, pow(t, 0.65));
    if (dir.y < 0.0) { c = mix(horizon, ground, pow(saturate(-dir.y), 0.5)); }
    c = c + top_shift * max(dir.y, 0.0) + bottom_shift * max(-dir.y, 0.0);
    return max(c, vec3<f32>(0.0));
}

// Approximate atmosphere inscatter/extinction (Hillaire/Bruneton-inspired simplified; tunable).
fn apply_atmosphere(color: vec3<f32>, world_pos: vec3<f32>, cam_pos: vec3<f32>, sun_dir: vec3<f32>) -> vec3<f32> {
    let density = light.atmo.x;
    if (density <= 0.0001) { return color; }
    let dist = length(world_pos - cam_pos);
    let height = max(world_pos.y, 0.0);
    let ray_dir = normalize(world_pos - cam_pos);
    let optical = dist * density * exp(-height * 0.01 * (1.0 + light.atmo.y));
    let trans = exp(-optical * 0.5);
    let sun_amt = pow(saturate(dot(ray_dir, sun_dir)), 8.0) * light.atmo.z;
    let scatter = light.atmo_color.rgb * (1.0 - trans) * (0.6 + 0.4 * saturate(sun_dir.y + 0.1));
    let haze = light.atmo.w * (1.0 - trans);
    return color * trans + scatter + vec3<f32>(sun_amt) * light.atmo_decay.rgb * (1.0 - trans) + haze * light.atmo_color.rgb;
}

fn tonemap_aces(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51; let b = 0.03; let c = 2.43; let d = 0.59; let e = 0.14;
    return saturate3((x*(a*x+b))/(x*(c*x+d)+e));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tech = i32(light.tech_params.x + 0.5);
    let N = normalize(in.normal);
    let V = normalize(camera.camera_pos.xyz - in.world_pos);
    let L = normalize(light.sun_dir.xyz);
    let H = normalize(V + L);

    let base_flat = in.color.rgb;
    let alpha = in.color.a;
    if (alpha < 0.01) { discard; }

    // Surface detail from the material texture array, plus an optional decal.
    let material_layer = i32(in.layers.x + 0.5);
    let detail = sample_material(in.world_pos, N, material_layer);
    var base = base_flat * detail;
    let decal_on = select(0.0, 1.0, in.layers.y > 0.5);
    let decal_layer = select(0, i32(in.layers.y + 0.5), in.layers.y > 0.5);
    let decal_sample = textureSample(material_textures, material_sampler, in.uv, decal_layer);
    base = mix(base, decal_sample.rgb, decal_sample.a * decal_on);

    // Metalness/roughness maps (0.5 = neutral/no override) modulate vertex constants.
    let m_map = sample_material_scalar(material_metal_textures, in.world_pos, N, material_layer);
    let r_map = sample_material_scalar(material_rough_textures, in.world_pos, N, material_layer);
    let m_mul = clamp(m_map * 2.0, 0.0, 2.0);
    let r_mul = clamp(r_map * 2.0, 0.0, 2.0);
    let metallic = saturate(in.material.x * m_mul);
    let roughness = clamp(in.material.y * r_mul, 0.04, 1.0);
    let reflectance = saturate(in.material.z);
    let kind = in.material.w; // 0 standard,1 neon,2 glass,3 forcefield

    let nl = max(dot(N, L), 0.0);
    let nv = max(dot(N, V), 0.0);
    let nh = max(dot(N, H), 0.0);
    let vh = max(dot(V, H), 0.0);

    let sun_brightness = max(light.sun_dir.w, 0.0);
    let sun_vis = select(0.0, nl, light.sun_dir.y > -0.05); // below horizon dims

    // PBR-ish specular for Future/ShadowMap/Compat; simplified for Legacy/Voxel
    let f0_dielectric = vec3<f32>(0.16 * reflectance * reflectance);
    let f0 = mix(f0_dielectric, base, metallic);
    let NDF = distribution_ggx(nh, roughness);
    let G = geometry_smith(nv, nl, roughness);
    let F = fresnel_schlick(vh, f0);
    let spec = (NDF * G * F) / max(4.0 * nv * nl, 1e-4);
    let kS = F;
    var kD = (vec3<f32>(1.0) - kS) * (1.0 - metallic);

    let use_pbr = (tech >= 2); // Compatibility/ShadowMap/Future get stronger specular/IBL model
    let spec_strength = select(0.15, 1.0, use_pbr) * light.fog_params.y; // envSpecularScale
    let direct_spec = spec * sun_brightness * sun_vis * spec_strength;
    let direct_diff = (kD * base / 3.14159265) * sun_brightness * sun_vis;

    // Ambient/IBL by technology
    let up = saturate(N.y * 0.5 + 0.5);
    var env_diff = sky_color(N, light.color_shift.rgb, light.color_shift_bottom.rgb);
    var env_spec = sky_color(reflect(-V, N), light.color_shift.rgb, light.color_shift_bottom.rgb);
    let env_diff_scale = light.outdoor_ambient.w; // environmentDiffuseScale
    let env_spec_scale = light.fog_params.y;

    var ambient_term: vec3<f32>;
    if (tech == 0) { // Legacy
        ambient_term = light.ambient.rgb + light.outdoor_ambient.rgb * up;
    } else if (tech == 1) { // Voxel
        ambient_term = light.ambient.rgb + light.outdoor_ambient.rgb * (0.55 + 0.45 * up);
    } else if (tech == 2) { // Compatibility
        ambient_term = light.ambient.rgb + light.outdoor_ambient.rgb * env_diff * env_diff_scale * 0.5;
    } else { // ShadowMap / Future
        let ibl = select(0.35, 1.0, tech == 4);
        ambient_term = light.ambient.rgb + light.outdoor_ambient.rgb * mix(vec3<f32>(up), env_diff, ibl) * env_diff_scale;
    }

    var ibl_spec = vec3<f32>(0.0);
    if (use_pbr) {
        let rough = roughness * roughness;
        ibl_spec = env_spec * env_spec_scale * F * (1.0 - rough) * 0.35;
    }

    var lit = base * ambient_term + direct_diff + direct_spec + ibl_spec;

    // Neon emits (approx)
    if (kind > 0.5 && kind < 1.5) {
        lit = base * (1.5 + sun_brightness * 0.2);
    }
    // Glass/transparency approximation
    if (kind > 1.5 && kind < 2.5) {
        lit = mix(lit, env_spec * 0.6 + direct_spec, 0.35);
    }

    // Fog (linear + Roblox FogStart/FogEnd)
    let cam_pos = camera.camera_pos.xyz;
    let dist = length(in.world_pos - cam_pos);
    let fog_start = light.fog_color.w;
    let fog_end = max(light.fog_params.x, fog_start + 1.0);
    let fog_f = saturate((dist - fog_start) / (fog_end - fog_start));
    lit = mix(lit, light.fog_color.rgb, fog_f);

    // Atmosphere (full-stack hook)
    if (light.tech_params.w > 0.5) {
        lit = apply_atmosphere(lit, in.world_pos, cam_pos, L);
    }

    // Exposure compensation (stops-like) + tonemap
    let exposure = pow(2.0, light.ambient.w);
    lit = tonemap_aces(max(lit * exposure, vec3<f32>(0.0)));

    // Gamma to sRGB-ish output (swapchain format dependent; common path)
    lit = pow(lit, vec3<f32>(1.0 / 2.2));

    return vec4<f32>(lit, alpha);
}
