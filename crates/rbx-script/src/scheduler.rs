//! Cooperative task scheduler (`task.wait`, `task.delay`, `task.spawn`, `task.defer`).
//!
//! Scripts run on Luau coroutines created by the host. Yielding happens inside
//! Luau (`coroutine.yield()`), so a suspended script is a `Thread` plus the host
//! time at which it should be resumed. `step()` is driven by the Studio frame
//! timer (or the player loop).

use mlua::Thread;

/// A suspended script waiting for host time to reach `resume_at`.
pub struct Scheduled {
    pub thread: Thread,
    pub resume_at: f64,
    /// Script that owns the thread, for error messages.
    pub script: Option<String>,
}

#[derive(Default)]
pub struct Scheduler {
    tasks: Vec<Scheduled>,
    /// Simulated host time in seconds (starts at 0).
    clock: f64,
    /// Frames stepped so far.
    pub frame: u64,
}

impl Scheduler {
    pub fn now(&self) -> f64 {
        self.clock
    }

    /// Advances the clock (called from `step`). Does not run anything.
    pub fn advance(&mut self, dt: f64) {
        self.clock += dt.max(0.0);
        self.frame += 1;
    }

    pub fn spawn(&mut self, thread: Thread, delay: f64, script: Option<String>) {
        self.tasks.push(Scheduled {
            thread,
            resume_at: self.clock + delay.max(0.0),
            script,
        });
    }

    pub fn cancel(&mut self, pointer: *const std::ffi::c_void) {
        self.tasks.retain(|t| t.thread.to_pointer() != pointer);
    }

    /// Removes and returns tasks whose deadline has passed.
    pub fn take_due(&mut self) -> Vec<Scheduled> {
        let now = self.clock;
        let mut due = Vec::new();
        let mut i = 0;
        while i < self.tasks.len() {
            if self.tasks[i].resume_at <= now {
                due.push(self.tasks.swap_remove(i));
            } else {
                i += 1;
            }
        }
        due
    }

    pub fn pending(&self) -> usize {
        self.tasks.len()
    }

    pub fn clear(&mut self) {
        self.tasks.clear();
    }
}
