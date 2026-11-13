//! Roblox material metadata: default colors, texture asset IDs, and physical
//! properties (as provided by the Material/PhysicalProperties tables).

/// A PBR texture set for a material. Asset IDs are Roblox image asset ids
/// (downloadable via the asset delivery API as `rbxassetid://{id}`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaterialTextures {
    pub color: Option<u64>,
    pub normal: Option<u64>,
    pub metalness: Option<u64>,
    pub roughness: Option<u64>,
}

/// Default `PhysicalProperties` values for a material.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicalProperties {
    pub density: f32,
    pub elasticity: f32,
    pub elasticity_weight: f32,
    pub friction: f32,
    pub friction_weight: f32,
}

impl PhysicalProperties {
    pub const fn new(
        density: f32,
        elasticity: f32,
        elasticity_weight: f32,
        friction: f32,
        friction_weight: f32,
    ) -> Self {
        Self {
            density,
            elasticity,
            elasticity_weight,
            friction,
            friction_weight,
        }
    }
}

/// Full metadata for a Roblox material.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialInfo {
    pub name: &'static str,
    /// Default part color as linear-ish RGB in 0..1 (from Roblox 0..255 defaults).
    pub color: [f32; 3],
    pub textures: MaterialTextures,
    pub physical: Option<PhysicalProperties>,
}

const fn rgb(r: u8, g: u8, b: u8) -> [f32; 3] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
}

const fn tex(color: Option<u64>, normal: Option<u64>, metalness: Option<u64>, roughness: Option<u64>) -> MaterialTextures {
    MaterialTextures { color, normal, metalness, roughness }
}

/// All known Roblox materials with defaults/texture IDs/physical properties.
/// Texture IDs and property values match the tables supplied for this project.
pub static MATERIALS: &[MaterialInfo] = &[
    MaterialInfo { name: "Asphalt", color: rgb(80, 84, 84), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(2.36, 0.2, 1.0, 0.8, 0.3)) },
    MaterialInfo { name: "Basalt", color: rgb(75, 74, 74), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(2.691, 0.15, 1.0, 0.7, 0.3)) },
    MaterialInfo { name: "Brick", color: rgb(138, 97, 73), textures: tex(Some(7_546_648_254), Some(7_546_649_654), None, Some(7_546_650_017)), physical: Some(PhysicalProperties::new(1.922, 0.15, 1.0, 0.8, 0.3)) },
    MaterialInfo { name: "Cardboard", color: rgb(255, 206, 152), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(0.7, 0.05, 2.0, 0.5, 1.0)) },
    MaterialInfo { name: "Carpet", color: rgb(163, 162, 165), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(1.1, 0.25, 2.0, 0.4, 1.0)) },
    MaterialInfo { name: "CeramicTiles", color: rgb(181, 173, 156), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(2.4, 0.2, 1.0, 0.51, 1.0)) },
    MaterialInfo { name: "ClayRoofTiles", color: rgb(255, 142, 87), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(2.0, 0.2, 1.0, 0.51, 1.0)) },
    MaterialInfo { name: "Cobblestone", color: rgb(134, 134, 118), textures: tex(Some(7_546_651_802), Some(7_546_652_689), None, Some(7_546_652_892)), physical: Some(PhysicalProperties::new(2.691, 0.17, 1.0, 0.5, 1.0)) },
    MaterialInfo { name: "Concrete", color: rgb(152, 152, 152), textures: tex(Some(7_546_653_328), Some(7_546_653_707), None, Some(7_546_653_868)), physical: Some(PhysicalProperties::new(2.403, 0.2, 1.0, 0.7, 0.3)) },
    MaterialInfo { name: "CorrodedMetal", color: rgb(104, 140, 173), textures: tex(Some(7_547_183_598), Some(7_547_181_182), Some(7_547_184_321), Some(7_547_184_588)), physical: Some(PhysicalProperties::new(7.85, 0.2, 1.0, 0.7, 1.0)) },
    MaterialInfo { name: "CrackedLava", color: rgb(255, 24, 67), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(2.691, 0.15, 1.0, 0.65, 1.0)) },
    MaterialInfo { name: "DiamondPlate", color: rgb(168, 175, 176), textures: tex(Some(7_546_654_401), Some(7_546_654_536), Some(7_547_162_002), Some(7_547_162_137)), physical: Some(PhysicalProperties::new(7.85, 0.25, 1.0, 0.35, 1.0)) },
    MaterialInfo { name: "Fabric", color: rgb(194, 193, 168), textures: tex(Some(7_547_100_606), Some(7_547_100_915), None, Some(7_547_101_072)), physical: Some(PhysicalProperties::new(0.7, 0.05, 1.0, 0.35, 1.0)) },
    MaterialInfo { name: "Foil", color: rgb(168, 175, 176), textures: tex(Some(7_546_644_642), Some(7_546_644_903), Some(7_546_644_642), Some(7_546_644_963)), physical: Some(PhysicalProperties::new(2.7, 0.25, 1.0, 0.4, 1.0)) },
    MaterialInfo { name: "ForceField", color: rgb(163, 162, 165), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(2.403, 0.2, 1.0, 0.25, 1.0)) },
    MaterialInfo { name: "Glacier", color: rgb(221, 228, 229), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(0.919, 0.15, 1.0, 0.05, 2.0)) },
    MaterialInfo { name: "Glass", color: rgb(138, 167, 168), textures: tex(Some(7_547_304_577), Some(7_547_304_785), None, Some(7_547_304_892)), physical: Some(PhysicalProperties::new(2.403, 0.2, 1.0, 0.25, 1.0)) },
    MaterialInfo { name: "Granite", color: rgb(149, 146, 139), textures: tex(Some(7_547_164_400), Some(7_546_654_648), None, Some(7_547_164_660)), physical: Some(PhysicalProperties::new(2.691, 0.2, 1.0, 0.4, 1.0)) },
    MaterialInfo { name: "Grass", color: rgb(111, 126, 62), textures: tex(Some(7_547_167_347), Some(7_547_168_653), None, Some(7_547_169_207)), physical: Some(PhysicalProperties::new(0.9, 0.1, 1.5, 0.4, 1.0)) },
    MaterialInfo { name: "Ground", color: rgb(140, 130, 104), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(0.9, 0.1, 1.0, 0.45, 1.0)) },
    MaterialInfo { name: "Ice", color: rgb(204, 210, 223), textures: tex(Some(7_546_644_642), Some(7_547_171_198), None, Some(7_547_171_276)), physical: Some(PhysicalProperties::new(0.919, 0.15, 1.0, 0.02, 3.0)) },
    MaterialInfo { name: "LeafyGrass", color: rgb(106, 134, 64), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(0.9, 0.1, 2.0, 0.4, 2.0)) },
    MaterialInfo { name: "Leather", color: rgb(110, 73, 53), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(0.86, 0.25, 1.0, 0.35, 1.0)) },
    MaterialInfo { name: "Limestone", color: rgb(255, 243, 192), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(2.691, 0.15, 1.0, 0.5, 1.0)) },
    MaterialInfo { name: "Marble", color: rgb(122, 122, 122), textures: tex(Some(7_547_174_345), Some(7_547_176_060), None, Some(7_547_177_213)), physical: Some(PhysicalProperties::new(2.563, 0.17, 1.0, 0.2, 1.0)) },
    MaterialInfo { name: "Metal", color: rgb(168, 175, 176), textures: tex(Some(7_547_178_395), Some(7_547_287_997), Some(7_547_288_112), Some(7_547_179_082)), physical: Some(PhysicalProperties::new(7.85, 0.25, 1.0, 0.4, 1.0)) },
    MaterialInfo { name: "Mud", color: rgb(121, 112, 98), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(0.9, 0.07, 4.0, 0.3, 3.0)) },
    MaterialInfo { name: "Neon", color: rgb(163, 162, 165), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(0.7, 0.2, 1.0, 0.3, 1.0)) },
    MaterialInfo { name: "Pavement", color: rgb(143, 144, 135), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(2.691, 0.17, 1.0, 0.5, 0.3)) },
    MaterialInfo { name: "Pebble", color: rgb(122, 122, 118), textures: tex(Some(7_547_291_174), Some(7_546_645_052), None, Some(7_547_291_306)), physical: Some(PhysicalProperties::new(2.403, 0.17, 1.5, 0.4, 1.0)) },
    MaterialInfo { name: "Plaster", color: rgb(204, 142, 105), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(0.75, 0.2, 1.0, 0.6, 0.3)) },
    MaterialInfo { name: "Plastic", color: rgb(163, 162, 165), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(0.7, 0.5, 1.0, 0.3, 1.0)) },
    MaterialInfo { name: "Rock", color: rgb(99, 100, 102), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(2.691, 0.17, 1.0, 0.5, 1.0)) },
    MaterialInfo { name: "RoofShingles", color: rgb(66, 66, 66), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(2.36, 0.2, 1.0, 0.8, 0.3)) },
    MaterialInfo { name: "Rubber", color: rgb(32, 32, 32), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(1.3, 0.95, 2.0, 1.5, 3.0)) },
    MaterialInfo { name: "Salt", color: rgb(255, 255, 254), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(2.165, 0.05, 1.0, 0.5, 1.0)) },
    MaterialInfo { name: "Sand", color: rgb(207, 203, 167), textures: tex(Some(7_547_294_684), Some(7_547_294_810), None, Some(7_547_295_087)), physical: Some(PhysicalProperties::new(1.602, 0.05, 2.5, 0.5, 5.0)) },
    MaterialInfo { name: "Sandstone", color: rgb(148, 124, 95), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(2.691, 0.15, 1.0, 0.5, 5.0)) },
    MaterialInfo { name: "Slate", color: rgb(88, 89, 86), textures: tex(Some(7_547_297_050), Some(7_547_297_808), None, Some(7_547_298_051)), physical: Some(PhysicalProperties::new(2.691, 0.2, 1.0, 0.4, 1.0)) },
    MaterialInfo { name: "SmoothPlastic", color: rgb(163, 162, 165), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(0.7, 0.5, 1.0, 0.2, 1.0)) },
    MaterialInfo { name: "Snow", color: rgb(235, 253, 255), textures: tex(None, None, None, None), physical: Some(PhysicalProperties::new(0.9, 0.03, 4.0, 0.3, 3.0)) },
    MaterialInfo { name: "Wood", color: rgb(172, 148, 108), textures: tex(Some(7_547_190_453), Some(7_547_190_548), Some(7_547_190_619), Some(7_547_303_147)), physical: Some(PhysicalProperties::new(0.35, 0.2, 1.0, 0.48, 1.0)) },
    MaterialInfo { name: "WoodPlanks", color: rgb(172, 148, 108), textures: tex(Some(7_547_301_709), Some(7_547_188_159), Some(7_547_188_891), Some(7_547_332_869)), physical: Some(PhysicalProperties::new(0.35, 0.2, 1.0, 0.48, 1.0)) },
];

/// Look up material metadata by Roblox material enum name (case-sensitive as in Enum.Material).
pub fn material_info(name: &str) -> Option<&'static MaterialInfo> {
    MATERIALS.iter().find(|m| m.name == name)
}

/// Default part color for a material (0..1 RGB). Falls back to Plastic gray.
pub fn default_color(name: &str) -> [f32; 3] {
    material_info(name).map(|m| m.color).unwrap_or(rgb(163, 162, 165))
}

/// Default physical properties for a material, if known.
pub fn default_physical(name: &str) -> Option<PhysicalProperties> {
    material_info(name).and_then(|m| m.physical)
}

/// Color texture asset id for a material, if it has one.
pub fn color_texture_id(name: &str) -> Option<u64> {
    material_info(name).and_then(|m| m.textures.color)
}
