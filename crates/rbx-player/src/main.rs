use anyhow::Result;
use rbx_core::import_rbxl;
use std::env;
use std::fs;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let input = args.get(1).cloned().unwrap_or_else(|| "game.rbxl".to_string());
    let bytes = fs::read(&input)?;
    let scene = import_rbxl(&bytes)?;
    println!("Loaded scene: {} ({} parts)", scene.name, scene.parts.len());
    for p in &scene.parts {
        println!("  Part {} @ ({},{},{}) size ({},{},{})",
            p.name, p.position.x, p.position.y, p.position.z,
            p.size.x, p.size.y, p.size.z);
    }
    // Write converted format for the Steam build pipeline.
    let out = input.replace(".rbxl", ".rustblox.json");
    fs::write(&out, serde_json::to_string_pretty(&scene)?)?;
    println!("Wrote converted scene to {}", out);
    Ok(())
}
