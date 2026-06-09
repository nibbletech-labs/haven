//! Error type for the Haven service layer.
//!
//! The CLI maps these to the `{"error": {...}}` envelope (SPEC §2); the MCP
//! server maps them to JSON-RPC errors. Each variant carries a stable `code`
//! so clients can branch without string-matching messages.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum HavenError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid argument: {0}")]
    Invalid(String),

    #[error("conflict: {0}")]
    Conflict(String),

    /// A structural rule was violated (e.g. a cycle in the decomposition or
    /// dependency DAG, or an evolve op on an already-superseded node).
    #[error("graph rule violated: {0}")]
    GraphRule(String),

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl HavenError {
    /// Stable machine-readable code for the error envelope.
    pub fn code(&self) -> &'static str {
        match self {
            HavenError::NotFound(_) => "not_found",
            HavenError::Invalid(_) => "invalid",
            HavenError::Conflict(_) => "conflict",
            HavenError::GraphRule(_) => "graph_rule",
            HavenError::Db(_) => "db",
            HavenError::Migration(_) => "migration",
            HavenError::Serde(_) => "serde",
            HavenError::Io(_) => "io",
        }
    }
}

pub type Result<T> = std::result::Result<T, HavenError>;
