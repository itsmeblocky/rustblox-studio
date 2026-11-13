//! Diagnose extracted Studio localization bundles.
//!
//! ```text
//! cargo run -p rbx-studio --bin dump-strings -- [ribbon|explorer|properties|toolbox] [pattern]
//! ```

use anyhow::Result;
use rbx_studio::strings::Strings;

fn main() -> Result<()> {
    let mut bundle = "ribbon".to_string();
    let mut pattern: Option<String> = None;
    for (i, arg) in std::env::args().skip(1).enumerate() {
        if i == 0 && matches!(arg.as_str(), "ribbon" | "explorer" | "properties" | "toolbox") {
            bundle = arg;
        } else {
            pattern = Some(arg);
        }
    }

    let strings = Strings::bundled(&bundle)?;
    println!("Bundle: {}  keys={}", strings.plugin(), strings.len());
    let needle = pattern.as_deref().map(str::to_ascii_lowercase);
    let mut count = 0usize;
    for key in strings.keys() {
        let Some(value) = strings.get(key) else { continue };
        if let Some(ref n) = needle {
            let hay = format!("{key} {value}").to_ascii_lowercase();
            if !hay.contains(n) {
                continue;
            }
        }
        println!("{key} = {value}");
        count += 1;
        if needle.is_none() && count >= 80 {
            println!("... ({} more; pass a pattern to filter)", strings.len() - count);
            break;
        }
    }
    if needle.is_some() {
        println!("-- {count} matches --");
    }
    Ok(())
}
