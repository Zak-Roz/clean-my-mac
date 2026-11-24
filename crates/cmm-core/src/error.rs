//! Error type shared by all of cmm-core.

/// Each variant carries a human-readable message; modules map their library errors into these
/// (e.g. `rusqlite::Error` -> `CoreError::History(e.to_string())`).
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Invalid rule catalog / config, or a detector failing on a rule.
    #[error("rule error: {0}")]
    Rule(String),
    /// The path guard refused a target (ARCHITECTURE.md §6.2).
    #[error("refused by path guard: {0}")]
    Guard(String),
    /// An external command failed or is not allowlisted.
    #[error("command error: {0}")]
    Command(String),
    #[error("history error: {0}")]
    History(String),
    #[error("not found: {0}")]
    NotFound(String),
}
