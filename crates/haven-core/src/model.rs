//! Domain types for the work-graph.
//!
//! The user-facing noun is **item** (SPEC §0); the table is `nodes` and the
//! model term is "node" — `Item` is the serialized view of a node row. Enum
//! string values are identical across JSON (serde) and SQL (rusqlite) by
//! construction (see the `sql_enum!` macro), so the CLI/MCP wire format and the
//! DB can never drift.

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

use crate::error::{HavenError, Result};

/// Generate an enum whose serde representation and SQL representation are the
/// same literal string for every variant — preventing wire/DB drift.
macro_rules! sql_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $lit:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $( #[serde(rename = $lit)] $variant ),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $( $name::$variant => $lit ),+ }
            }
            pub fn parse(s: &str) -> Result<Self> {
                match s {
                    $( $lit => Ok($name::$variant), )+
                    other => Err(HavenError::Invalid(
                        format!("invalid {} value: {:?}", stringify!($name), other))),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }

        impl FromSql for $name {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                let s = value.as_str()?;
                $name::parse(s).map_err(|e| FromSqlError::Other(Box::new(
                    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))))
            }
        }
    };
}

sql_enum! {
    /// Node type. `release`/`phase`/`gate` are container nodes (group targets).
    NodeType {
        Task => "task", Code => "code", Research => "research", Data => "data",
        Design => "design", Admin => "admin", Release => "release",
        Phase => "phase", Gate => "gate",
    }
}

sql_enum! {
    /// Maturity axis — how well-defined a node is.
    Status {
        Discovery => "discovery", Definition => "definition", Ready => "ready",
        InProgress => "in_progress", Blocked => "blocked", Done => "done",
        Superseded => "superseded", Archived => "archived",
    }
}

sql_enum! {
    /// Who executes a node.
    OwnerKind { Human => "human", Ai => "ai" }
}

impl OwnerKind {
    /// The other owner — used to infer a handoff's `from` when the item has no
    /// current owner (a human↔ai baton-pass has exactly two sides).
    pub fn opposite(self) -> Self {
        match self {
            OwnerKind::Human => OwnerKind::Ai,
            OwnerKind::Ai => OwnerKind::Human,
        }
    }
}

sql_enum! {
    /// Why a node is parked (orthogonal to status).
    WaitState {
        OnHuman => "on_human", OnDependency => "on_dependency", OnExternal => "on_external",
    }
}

sql_enum! {
    /// Lineage event kind (append-only core).
    EventType {
        Split => "split", Merge => "merge", Supersede => "supersede",
        Update => "update", Archive => "archive", Reopen => "reopen",
    }
}

sql_enum! {
    /// Artifact role — what kind of content a node points at.
    ArtifactRole {
        Spec => "spec", Research => "research", Design => "design",
        Handoff => "handoff", Decision => "decision", Scratch => "scratch",
        Source => "source", Delivery => "delivery", Vision => "vision",
    }
}

sql_enum! {
    /// Artifact storage kind.
    ArtifactKind { File => "file", External => "external", Delivery => "delivery" }
}

sql_enum! {
    /// Per-row sync status (servo pattern).
    SyncState { Local => "local", Synced => "synced", Failed => "failed" }
}

/// A project — namespace for a backlog, one per product/repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    #[serde(skip_serializing, default)]
    pub id: i64,
    pub public_id: String,
    pub key: String,
    pub ref_prefix: String,
    pub ref_counter: i64,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
    pub sync_state: SyncState,
}

/// The four edge layers, resolved to human `ref`s, attached to an item on
/// `--include edges`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Edges {
    /// Decomposition: nodes this is a part of.
    pub parents: Vec<String>,
    /// Decomposition: nodes that are parts of this.
    pub children: Vec<String>,
    /// Dependency: prerequisites that block this.
    pub depends_on: Vec<String>,
    /// Dependency: nodes blocked by this.
    pub blocks: Vec<String>,
    /// Grouping: release/phase/gate nodes this belongs to.
    pub groups: Vec<String>,
    /// Grouping: members, when this *is* a release/phase/gate.
    pub members: Vec<String>,
}

/// A typed reference from a node to content (file or external URI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(skip_serializing, default)]
    pub id: i64,
    pub public_id: String,
    /// `ref` of the owning node (filled on read for convenience).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_ref: Option<String>,
    pub role: ArtifactRole,
    pub kind: ArtifactKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_owner: Option<OwnerKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_owner: Option<OwnerKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    pub revision: i64,
    pub sync_state: SyncState,
}

/// A lineage event with its from→to edges, resolved to `ref`s. Returned by
/// `lineage`/`evolve graph`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageEvent {
    pub public_id: String,
    pub event_type: EventType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
    pub created_at: String,
    pub from: Vec<String>,
    pub to: Vec<String>,
}

/// The serialized view of a node row — the unit the CLI and MCP return.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    /// Local integer id — never serialized, never synced (SPEC §1).
    #[serde(skip_serializing, default)]
    pub id: i64,
    pub public_id: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub project: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Acceptance statement — what success is. The anchor an output is verified
    /// against on the ready→done transition. Short and structured, not content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_looks_like: Option<String>,
    /// One-line provenance / vision trace — why this item exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_kind: Option<OwnerKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_state: Option<WaitState>,
    pub committed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_key: Option<String>,
    pub metadata: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    pub revision: i64,
    pub sync_state: SyncState,

    // Optional includes (SPEC §2 `item get --include`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edges: Option<Edges>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<Artifact>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineage: Option<Vec<LineageEvent>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_sql_and_json_agree() {
        // The SQL literal and the JSON literal are the same string.
        assert_eq!(Status::InProgress.as_str(), "in_progress");
        assert_eq!(
            serde_json::to_string(&Status::InProgress).unwrap(),
            "\"in_progress\""
        );
        assert_eq!(Status::parse("in_progress").unwrap(), Status::InProgress);
        assert!(Status::parse("bogus").is_err());
    }

    #[test]
    fn wait_state_round_trips() {
        for s in ["on_human", "on_dependency", "on_external"] {
            assert_eq!(WaitState::parse(s).unwrap().as_str(), s);
        }
    }
}
