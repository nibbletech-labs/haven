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

    /// A content file isn't on this machine, but a synced copy exists in cloud
    /// Storage (`remote_path`). Front-ends with sync configured catch this and
    /// lazy-download (SPEC §5); otherwise it surfaces with enough context to
    /// run `haven sync` / fetch by hand.
    #[error("content file {rel_path} not present locally; remote copy at {remote_path}")]
    ContentNotLocal {
        /// Project key (content lives under `<root>/<project>/<rel_path>`).
        project: String,
        /// The artifact's `path`, relative to the project content dir.
        rel_path: String,
        /// The Storage object key holding the synced bytes.
        remote_path: String,
        /// `content_hash` recorded on the row, to verify the download.
        content_hash: Option<String>,
    },

    /// A structural rule was violated (e.g. a cycle in the decomposition or
    /// dependency DAG, or an evolve op on an already-superseded node).
    #[error("graph rule violated: {0}")]
    GraphRule(String),

    /// The local DB has been migrated by a newer Haven binary. Refuse to open it
    /// rather than attempting an unsafe downgrade or surfacing a low-level
    /// migration error.
    #[error(
        "Haven store at {path} uses schema migration {db_version}, but this binary only supports up to {supported_version}. Upgrade or reinstall `haven` before using this store."
    )]
    StoreTooNew {
        path: String,
        db_version: i64,
        supported_version: i64,
    },

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
            HavenError::ContentNotLocal { .. } => "content_not_local",
            HavenError::GraphRule(_) => "graph_rule",
            HavenError::StoreTooNew { .. } => "store_too_new",
            HavenError::Db(_) => "db",
            HavenError::Migration(_) => "migration",
            HavenError::Serde(_) => "serde",
            HavenError::Io(_) => "io",
        }
    }
}

pub type Result<T> = std::result::Result<T, HavenError>;
