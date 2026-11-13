//! Execution contexts (`Script` = server, `LocalScript` = client).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ExecContext {
    /// `Script`, `ServerScriptService`, core scripts.
    #[default]
    Server,
    /// `LocalScript`, player scripts.
    Client,
}

impl ExecContext {
    pub fn is_server(self) -> bool {
        matches!(self, Self::Server)
    }

    pub fn is_client(self) -> bool {
        matches!(self, Self::Client)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Server => "Server",
            Self::Client => "Client",
        }
    }

    /// Steam API access is server-only by design (clients must not self-grant gains).
    pub fn can_use_steam(self) -> bool {
        self.is_server()
    }

    /// Settings writes are server-only; reads are allowed anywhere.
    pub fn can_write_settings(self) -> bool {
        self.is_server()
    }
}

impl std::fmt::Display for ExecContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}
