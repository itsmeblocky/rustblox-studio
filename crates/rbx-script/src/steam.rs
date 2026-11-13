//! Steamworks bridge used by `SteamworksService`.
//!
//! The real implementation lives in `rbx-player` (behind the `steamworks` crate);
//! the trait keeps this crate free of that dependency and lets Studio run with no
//! Steam client attached.

use std::cell::RefCell;
use std::collections::BTreeMap;

/// Server-side Steam API surface exposed to Luau.
pub trait SteamBackend {
    fn init(&self) -> bool;
    fn is_overlay_enabled(&self) -> bool;
    fn request_current_stats(&self) -> bool;
    fn unlock_achievement(&self, name: &str) -> bool;
    fn clear_achievement(&self, name: &str) -> bool;
    fn get_achievement(&self, name: &str) -> bool;
    fn set_stat(&self, name: &str, value: i64) -> bool;
    fn get_stat(&self, name: &str) -> i64;
    fn store_stats(&self) -> bool;
    fn file_write(&self, name: &str, data: &[u8]) -> bool;
    fn file_read(&self, name: &str) -> Option<Vec<u8>>;
}

/// Offline stand-in: keeps achievements/stats in memory so scripts and saves still
/// behave, and reports that the Steam overlay is unavailable.
#[derive(Default)]
pub struct NoopSteam {
    achievements: RefCell<BTreeMap<String, bool>>,
    stats: RefCell<BTreeMap<String, i64>>,
    files: RefCell<BTreeMap<String, Vec<u8>>>,
}

impl SteamBackend for NoopSteam {
    fn init(&self) -> bool {
        false
    }

    fn is_overlay_enabled(&self) -> bool {
        false
    }

    fn request_current_stats(&self) -> bool {
        false
    }

    fn unlock_achievement(&self, name: &str) -> bool {
        self.achievements.borrow_mut().insert(name.to_string(), true);
        true
    }

    fn clear_achievement(&self, name: &str) -> bool {
        self.achievements.borrow_mut().remove(name);
        true
    }

    fn get_achievement(&self, name: &str) -> bool {
        self.achievements.borrow().get(name).copied().unwrap_or(true)
    }

    fn set_stat(&self, name: &str, value: i64) -> bool {
        self.stats.borrow_mut().insert(name.to_string(), value);
        true
    }

    fn get_stat(&self, name: &str) -> i64 {
        self.stats.borrow().get(name).copied().unwrap_or(0)
    }

    fn store_stats(&self) -> bool {
        true
    }

    fn file_write(&self, name: &str, data: &[u8]) -> bool {
        self.files.borrow_mut().insert(name.to_string(), data.to_vec());
        true
    }

    fn file_read(&self, name: &str) -> Option<Vec<u8>> {
        self.files.borrow().get(name).cloned()
    }
}
