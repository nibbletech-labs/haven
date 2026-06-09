//! The `Store` service — the single shared entry point the CLI and MCP both
//! call (the Muxra pattern, SPEC §7). Owns the SQLite connection and the
//! `~/.haven` content root. Domain operations are split across submodules by
//! area but are all methods on `Store`, so the two clients cannot drift.

mod content;
mod edge;
mod evolve;
mod item;
mod query;
#[cfg(test)]
mod service_tests;

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row};
use uuid::Uuid;

use crate::db;
use crate::error::{HavenError, Result};
use crate::model::*;

pub use content::{ArtifactContent, NewArtifact};
pub use edge::EdgeKind;
pub use evolve::EvolveResult;
pub use item::{Include, ItemFilter, ItemUpdate, NewItem, WaitUpdate};
pub use query::{LineageDirection, LineageGraph};

/// Columns selected for an `Item`, in the order `item_from_row` expects. Joined
/// against `projects` to resolve the human project key.
pub(crate) const ITEM_SELECT: &str = "\
    n.id, n.public_id, n.ref, p.key, n.title, n.body, n.type, n.status, \
    n.owner_kind, n.assignee, n.wait_state, n.committed, n.priority, n.sort_key, \
    n.metadata, n.created_at, n.updated_at, n.archived_at, n.revision, n.sync_state, \
    n.done_looks_like, n.why";

pub(crate) const ITEM_FROM: &str = "nodes n JOIN projects p ON p.id = n.project_id";

/// Map a row selected via [`ITEM_SELECT`] into an [`Item`] (no includes).
pub(crate) fn item_from_row(row: &Row<'_>) -> rusqlite::Result<Item> {
    let metadata_str: String = row.get(14)?;
    let metadata = serde_json::from_str(&metadata_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(14, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Item {
        id: row.get(0)?,
        public_id: row.get(1)?,
        reference: row.get(2)?,
        project: row.get(3)?,
        title: row.get(4)?,
        body: row.get(5)?,
        done_looks_like: row.get(20)?,
        why: row.get(21)?,
        node_type: row.get(6)?,
        status: row.get(7)?,
        owner_kind: row.get(8)?,
        assignee: row.get(9)?,
        wait_state: row.get(10)?,
        committed: row.get(11)?,
        priority: row.get(12)?,
        sort_key: row.get(13)?,
        metadata,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
        archived_at: row.get(17)?,
        revision: row.get(18)?,
        sync_state: row.get(19)?,
        edges: None,
        artifacts: None,
        lineage: None,
    })
}

/// Fresh idempotency / sync identity key.
pub(crate) fn new_uuid() -> String {
    Uuid::new_v4().to_string()
}

pub struct Store {
    pub(crate) conn: Connection,
    content_root: PathBuf,
}

impl Store {
    /// Open the store at `db_path`, with `content_root` as the `~/.haven` tree
    /// (used by the content layer in Layer 4). Runs migrations.
    pub fn open(db_path: impl AsRef<Path>, content_root: impl Into<PathBuf>) -> Result<Self> {
        let conn = db::open(db_path)?;
        Ok(Store {
            conn,
            content_root: content_root.into(),
        })
    }

    /// In-memory store for tests (content root is a throwaway temp-ish path).
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self> {
        let conn = db::open_in_memory()?;
        Ok(Store {
            conn,
            content_root: PathBuf::from("/tmp/haven-test"),
        })
    }

    /// In-memory store with a real (temp) content root, for content-layer tests.
    #[cfg(test)]
    pub(crate) fn open_in_memory_at(content_root: impl Into<PathBuf>) -> Result<Self> {
        let conn = db::open_in_memory()?;
        Ok(Store {
            conn,
            content_root: content_root.into(),
        })
    }

    pub fn content_root(&self) -> &Path {
        &self.content_root
    }

    // ---- meta key/value (local-only config + sync bookkeeping) ------------

    /// Read a `meta` key. Returns `None` if the key was never set — note the
    /// migration only seeds `schema_version`; `current_project`, `device_id`,
    /// `last_pull_at` are absent until their setup ops run.
    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let v = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?;
        Ok(v)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (key, value),
        )?;
        Ok(())
    }

    // ---- projects ---------------------------------------------------------

    /// Create a project. `prefix` defaults to the uppercased first 2 chars of
    /// `key` when not given.
    pub fn add_project(
        &self,
        key: &str,
        prefix: Option<&str>,
        title: &str,
        description: Option<&str>,
    ) -> Result<Project> {
        if key.trim().is_empty() {
            return Err(HavenError::Invalid("project key must not be empty".into()));
        }
        let prefix = match prefix {
            Some(p) => p.to_uppercase(),
            None => key.chars().take(2).collect::<String>().to_uppercase(),
        };
        let public_id = new_uuid();
        let client_id = new_uuid();
        self.conn
            .execute(
                "INSERT INTO projects (public_id, key, ref_prefix, title, description, client_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (&public_id, key, &prefix, title, description, &client_id),
            )
            .map_err(|e| dup_to_conflict(e, &format!("project key {key:?} already exists")))?;
        self.get_project(key)
    }

    pub fn get_project(&self, key: &str) -> Result<Project> {
        self.conn
            .query_row(
                "SELECT id, public_id, key, ref_prefix, ref_counter, title, description,
                        created_at, updated_at, revision, sync_state
                 FROM projects WHERE key = ?1",
                [key],
                project_from_row,
            )
            .optional()?
            .ok_or_else(|| HavenError::NotFound(format!("project {key:?}")))
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, public_id, key, ref_prefix, ref_counter, title, description,
                    created_at, updated_at, revision, sync_state
             FROM projects ORDER BY key",
        )?;
        let rows = stmt.query_map([], project_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Set the current project (stored in `meta`).
    pub fn use_project(&self, key: &str) -> Result<()> {
        self.get_project(key)?; // validate it exists
        self.meta_set("current_project", key)
    }

    pub fn current_project(&self) -> Result<Option<String>> {
        self.meta_get("current_project")
    }

    /// Resolve a project selector to `(id, key)`. Falls back to the current
    /// project when `selector` is `None`.
    pub(crate) fn require_project(&self, selector: Option<&str>) -> Result<(i64, String)> {
        let key = match selector {
            Some(k) => k.to_string(),
            None => self.current_project()?.ok_or_else(|| {
                HavenError::Invalid(
                    "no project selected; pass --project or run `haven project use`".into(),
                )
            })?,
        };
        let id = self
            .conn
            .query_row("SELECT id FROM projects WHERE key = ?1", [&key], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| HavenError::NotFound(format!("project {key:?}")))?;
        Ok((id, key))
    }

    // ---- ref resolution ---------------------------------------------------

    /// Resolve an item selector (`ref` like `HV-42`, or a `public_id` UUID) to
    /// its local node id, scoped to `project_id`. `public_id` is globally unique
    /// so it resolves regardless of project.
    pub(crate) fn resolve_node_id(&self, project_id: i64, selector: &str) -> Result<i64> {
        let id = self
            .conn
            .query_row(
                "SELECT id FROM nodes
                 WHERE public_id = ?1 OR (project_id = ?2 AND ref = ?1)",
                rusqlite::params![selector, project_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| HavenError::NotFound(format!("item {selector:?}")))?;
        Ok(id)
    }

    /// The `ref` for a node id (for resolving edge endpoints back to handles).
    pub(crate) fn node_ref(&self, node_id: i64) -> Result<String> {
        Ok(self
            .conn
            .query_row("SELECT ref FROM nodes WHERE id = ?1", [node_id], |r| {
                r.get(0)
            })?)
    }
}

fn project_from_row(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get(0)?,
        public_id: row.get(1)?,
        key: row.get(2)?,
        ref_prefix: row.get(3)?,
        ref_counter: row.get(4)?,
        title: row.get(5)?,
        description: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        revision: row.get(9)?,
        sync_state: row.get(10)?,
    })
}

/// Map a UNIQUE-constraint failure to a friendly `Conflict`.
pub(crate) fn dup_to_conflict(e: rusqlite::Error, msg: &str) -> HavenError {
    if let rusqlite::Error::SqliteFailure(err, _) = &e {
        if err.code == rusqlite::ErrorCode::ConstraintViolation {
            return HavenError::Conflict(msg.to_string());
        }
    }
    HavenError::Db(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    #[test]
    fn project_crud_and_ref_prefix_default() {
        let s = store();
        let p = s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        assert_eq!(p.key, "haven");
        assert_eq!(p.ref_prefix, "HV");
        assert_eq!(p.ref_counter, 0);

        // default prefix = first two chars uppercased
        let p2 = s.add_project("billing", None, "Billing", None).unwrap();
        assert_eq!(p2.ref_prefix, "BI");

        assert_eq!(s.list_projects().unwrap().len(), 2);
        assert!(s.get_project("nope").is_err());

        // duplicate key -> conflict
        let err = s.add_project("haven", None, "Dup", None).unwrap_err();
        assert_eq!(err.code(), "conflict");
    }

    #[test]
    fn current_project_roundtrip() {
        let s = store();
        s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        assert_eq!(s.current_project().unwrap(), None);
        s.use_project("haven").unwrap();
        assert_eq!(s.current_project().unwrap().as_deref(), Some("haven"));
        assert!(s.use_project("missing").is_err());
    }

    #[test]
    fn meta_get_is_optional_on_fresh_db() {
        let s = store();
        assert_eq!(s.meta_get("device_id").unwrap(), None);
        assert_eq!(s.meta_get("schema_version").unwrap().as_deref(), Some("1"));
    }
}
