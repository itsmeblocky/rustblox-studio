//! Console/log capture for scripts, the command bar, and the Output panel.

use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Print,
    Info,
    Warn,
    Error,
    Command,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Print => "print",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Command => "command",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub level: LogLevel,
    pub message: String,
    /// `Workspace.Rig.Script:12` when the message came from a script.
    pub source: Option<String>,
    /// Host time (seconds) when the line was produced.
    pub time: f64,
}

/// Shared ring of log lines. Scripts append, the Qt console drains.
#[derive(Clone, Default)]
pub struct LogSink {
    lines: Rc<RefCell<Vec<LogLine>>>,
}

impl LogSink {
    pub fn push(&self, level: LogLevel, message: impl Into<String>, source: Option<String>, time: f64) {
        let mut lines = self.lines.borrow_mut();
        if lines.len() > 4000 {
            lines.drain(..1000);
        }
        lines.push(LogLine {
            level,
            message: message.into(),
            source,
            time,
        });
    }

    pub fn lines(&self) -> Vec<LogLine> {
        self.lines.borrow().clone()
    }

    pub fn drain(&self) -> Vec<LogLine> {
        std::mem::take(&mut *self.lines.borrow_mut())
    }

    pub fn clear(&self) {
        self.lines.borrow_mut().clear();
    }

    pub fn len(&self) -> usize {
        self.lines.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
