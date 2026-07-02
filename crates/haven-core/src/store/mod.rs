//! The `Store` service — the single shared entry point the CLI and MCP both
//! call (the Muxra pattern, SPEC §7). Owns the SQLite connection and the
//! `~/.haven` content root. Domain operations are split across submodules by
//! area but are all methods on `Store`, so the two clients cannot drift.

mod backup;
mod content;
mod edge;
mod evolve;
mod import;
mod item;
mod prime;
mod query;
#[cfg(test)]
mod service_tests;

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension, Row};
use uuid::Uuid;

use crate::db;
use crate::error::{HavenError, Result};
use crate::model::*;

pub use backup::{BackupEntry, BackupReport, Integrity, ProjectArchive, RestoreReport};
pub use content::{ArtifactContent, NewArtifact};
pub use edge::EdgeKind;
pub use evolve::EvolveResult;
pub use import::{ImportItem, ImportOutcome};
pub use item::{
    AddOutcome, CompleteInput, CompleteResult, DueUpdate, HandoffInput, HandoffResult, Include,
    ItemFilter, ItemUpdate, NewItem, SimilarItem, WaitUpdate,
};
// HV-30: the search path sanitizes raw user input through this helper before MATCH.
pub(crate) use item::fts_user_query;
pub use prime::{Prime, PrimeActiveItem, PrimeInboxItem, PrimeQueueItem};
pub use query::{
    DispatchArtifact, DispatchCandidate, DispatchContextItem, DispatchRecommendation,
    DispatchSummary, DocAnchor, GraphEdge, GroomingPressure, LineageDirection, LineageGraph,
    LineageLink, ProjectGraph, ProjectGraphPage, DEFAULT_DISPATCH_LIMIT, DEFAULT_NEXT_LIMIT,
    GROOMING_NUDGE_THRESHOLD, ORCHESTRATE_ADVISORY, ORCHESTRATE_ADVISORY_THRESHOLD,
};

/// Columns selected for an `Item`, in the order `item_from_row` expects. Joined
/// against `projects` to resolve the human project key.
pub(crate) const ITEM_SELECT: &str = "\
    n.id, n.public_id, n.ref, p.key, n.title, n.body, n.type, n.status, \
    n.owner_kind, n.assignee, n.wait_state, n.committed, n.priority, n.sort_key, \
    n.metadata, n.created_at, n.updated_at, n.archived_at, n.revision, n.sync_state, \
    n.done_looks_like, n.why, n.due_at";

pub(crate) const ITEM_FROM: &str = "nodes n JOIN projects p ON p.id = n.project_id";

/// A "the ref you used is dead" advisory raised on the success response of a read
/// (`get_item`) or a write that takes a ref (`add_edge`/`update_item`) when the
/// resolved node is `superseded`/`archived` (HV-154). The op still applies to the
/// dead node — the hint just tells the caller where the live work moved, running
/// the lineage walk (formerly the public `resolve_live`) automatically.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StaleRef {
    /// The (canonical) ref the caller asked for — the dead one.
    #[serde(rename = "ref")]
    pub requested: String,
    /// The live descendant ref(s) the dead ref forwards to, walking lineage
    /// forward. Empty when there is no live successor (e.g. a plain archive).
    pub resolved_to: Vec<String>,
}

/// Split a ref like `HV-42` into its `(prefix, counter)`. `None` when it doesn't
/// have the `PREFIX-<digits>` shape (e.g. a `public_id` UUID, or junk). Used for
/// the not_found nearest-live + wrong-project-prefix hints (HV-154).
pub(crate) fn parse_ref(reference: &str) -> Option<(String, i64)> {
    let (prefix, num) = reference.rsplit_once('-')?;
    if prefix.is_empty() {
        return None;
    }
    let counter: i64 = num.parse().ok()?;
    Some((prefix.to_string(), counter))
}

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
        due_at: row.get(22)?,
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
        rollup_state: None,
        owner_rollup: None,
        has_uncommitted_descendants: None,
        edges: None,
        artifacts: None,
        lineage: None,
        context_pack: None,
        context_pack_clash: None,
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

    /// The store's applied schema version (SQLite `PRAGMA user_version`), set by
    /// the migration runner. After a successful open it equals
    /// `db::latest_schema_migration()`; exposed so `doctor` can report it.
    pub fn user_version(&self) -> Result<i64> {
        let v = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        Ok(v)
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

    /// Resolve one project by key. Resolves an **archived** project (so reopen and
    /// historical reads work) but treats a **tombstoned** row (`deleted_at IS NOT
    /// NULL`, HV-123) as `NotFound` — the namespace stays burned, but the project
    /// is effectively absent to every read/guard path (SPEC §3.5).
    pub fn get_project(&self, key: &str) -> Result<Project> {
        self.conn
            .query_row(
                "SELECT id, public_id, key, ref_prefix, ref_counter, title, description,
                        created_at, updated_at, revision, sync_state,
                        status, archived_at, archived_reason, deleted_at
                 FROM projects WHERE key = ?1 AND deleted_at IS NULL",
                [key],
                project_from_row,
            )
            .optional()?
            .ok_or_else(|| HavenError::NotFound(format!("project {key:?}")))
    }

    /// List projects. `include_archived=false` (the default-listing semantics)
    /// returns only `active` projects; `true` returns active **and** archived.
    /// Both modes always exclude tombstoned rows (`deleted_at IS NOT NULL`) —
    /// a deleted project is never listed (SPEC §3.5).
    pub fn list_projects(&self, include_archived: bool) -> Result<Vec<Project>> {
        // Column order is fixed in lockstep with `project_from_row` (trailing
        // lifecycle indices 11..14). Only the status predicate varies by mode.
        let sql = if include_archived {
            "SELECT id, public_id, key, ref_prefix, ref_counter, title, description,
                    created_at, updated_at, revision, sync_state,
                    status, archived_at, archived_reason, deleted_at
             FROM projects WHERE deleted_at IS NULL ORDER BY key"
        } else {
            "SELECT id, public_id, key, ref_prefix, ref_counter, title, description,
                    created_at, updated_at, revision, sync_state,
                    status, archived_at, archived_reason, deleted_at
             FROM projects WHERE deleted_at IS NULL AND status = 'active' ORDER BY key"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], project_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Set the current project (stored in `meta`). Refuses an **archived** project
    /// (reopen it first); a tombstoned key is `NotFound` (via `get_project`) — HV-123.
    pub fn use_project(&self, key: &str) -> Result<()> {
        let proj = self.get_project(key)?; // validates existence + rejects tombstones
        if proj.status == ProjectStatus::Archived {
            return Err(HavenError::Invalid(format!(
                "project {key:?} is archived; reopen it first"
            )));
        }
        self.meta_set("current_project", key)
    }

    /// Soft-archive a project (HV-123). The namespace is **RESERVED**:
    /// `key`/`ref_prefix`/`ref_counter` are explicitly left untouched, so refs are
    /// never reused and `reopen_project` restores the project completely. One
    /// transaction; bumps `revision` and sets `sync_state='local'`. Idempotent:
    /// archiving an already-archived project is a clean no-op re-read (NOT a second
    /// bump) — the deliberate divergence from `archive_item`, which re-bumps (SPEC §3.1).
    ///
    /// Resolves the project by `key` directly (NOT via `require_project`), so the
    /// archived-write guard `require_project` carries never blocks the lifecycle op
    /// itself (SPEC §3 preamble).
    pub fn archive_project(
        &self,
        key: &str,
        reason: Option<&str>,
        _by: Option<&str>,
    ) -> Result<Project> {
        let tx = self.conn.unchecked_transaction()?;
        // Resolve by key directly; NotFound if absent or tombstoned.
        let current = self.project_lifecycle_state(&tx, key)?;
        if current == ProjectStatus::Archived {
            // No-op: return the current row without a second revision bump.
            tx.commit()?;
            return self.get_project(key);
        }
        // `key`, `ref_prefix`, `ref_counter` are explicitly NOT in the SET list —
        // this is the load-bearing line for namespace reservation.
        tx.execute(
            "UPDATE projects
                SET status='archived', archived_at=datetime('now'), archived_reason=?2,
                    revision=revision+1, updated_at=datetime('now'), sync_state='local'
              WHERE key=?1 AND deleted_at IS NULL",
            params![key, reason],
        )?;
        tx.commit()?;
        // Clear the sticky selection if it named this key (SPEC §3.6).
        if self.current_project()?.as_deref() == Some(key) {
            self.conn
                .execute("DELETE FROM meta WHERE key = 'current_project'", [])?;
        }
        self.get_project(key)
    }

    /// Reopen an archived project (HV-123): the inverse of archive — a total
    /// restore (nothing was destroyed). Errors `Invalid` if the project is not
    /// archived; a tombstoned row is treated as absent (`NotFound`), so reopen can
    /// never resurrect a deleted project's empty shell (SPEC §3.2). Resolves by key
    /// directly, bypassing the `require_project` archived-write guard.
    pub fn reopen_project(&self, key: &str, _by: Option<&str>) -> Result<Project> {
        let tx = self.conn.unchecked_transaction()?;
        let current = self.project_lifecycle_state(&tx, key)?;
        if current == ProjectStatus::Active {
            return Err(HavenError::Invalid(
                "cannot reopen a project that is not archived".into(),
            ));
        }
        tx.execute(
            "UPDATE projects
                SET status='active', archived_at=NULL, archived_reason=NULL,
                    revision=revision+1, updated_at=datetime('now'), sync_state='local'
              WHERE key=?1 AND deleted_at IS NULL",
            params![key],
        )?;
        tx.commit()?;
        self.get_project(key)
    }

    /// Read a project's lifecycle status by key inside a transaction, for the
    /// lifecycle ops. `NotFound` if the row is absent **or tombstoned**
    /// (`deleted_at IS NOT NULL` → treated as absent), enriched with the
    /// available-projects hint (SPEC §3.1/§3.2).
    fn project_lifecycle_state(
        &self,
        tx: &rusqlite::Transaction<'_>,
        key: &str,
    ) -> Result<ProjectStatus> {
        tx.query_row(
            "SELECT status FROM projects WHERE key=?1 AND deleted_at IS NULL",
            [key],
            |r| r.get::<_, ProjectStatus>(0),
        )
        .optional()?
        .ok_or_else(|| {
            HavenError::NotFound(format!("project {key:?}{}", self.available_projects_hint()))
        })
    }

    pub fn current_project(&self) -> Result<Option<String>> {
        self.meta_get("current_project")
    }

    /// Project keys that currently exist, for enriching "which project?" errors so
    /// a caller — especially a headless MCP agent that just forgot `project` — can
    /// self-correct in one step. Best-effort: a query error yields an empty list
    /// rather than masking the original error.
    fn project_keys(&self) -> Vec<String> {
        // Active, non-tombstoned only (HV-123): an archived project stays
        // resolvable by exact key but is not *suggested* for selection, and a
        // tombstoned project is gone.
        self.conn
            .prepare(
                "SELECT key FROM projects
                  WHERE deleted_at IS NULL AND status = 'active' ORDER BY key",
            )
            .and_then(|mut stmt| {
                stmt.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap_or_default()
    }

    /// Suffix listing the available projects (or how to make one if there are
    /// none), appended to the project-selection errors below.
    fn available_projects_hint(&self) -> String {
        let keys = self.project_keys();
        if keys.is_empty() {
            " — no projects exist yet; create one with `haven_add_project` (MCP) \
             or `haven project add` (CLI)"
                .into()
        } else {
            format!(" — available projects: {}", keys.join(", "))
        }
    }

    /// The resolved project KEY for a selector — the same resolution every
    /// project-scoped op uses (explicit `selector`, else the sticky
    /// `current_project`). Exposed so the MCP layer can echo the key it actually
    /// resolved on a success response, making a ref-omitting call's project
    /// observable rather than silent (HV-153).
    pub fn resolve_project_key(&self, selector: Option<&str>) -> Result<String> {
        let (_id, key) = self.require_project(selector)?;
        Ok(key)
    }

    /// Resolve a project selector to `(id, key)`. Falls back to the current
    /// project when `selector` is `None`. Reads route through here: an **archived**
    /// project still resolves (so `get_item`/`list_items`/`lineage`/`xref` work
    /// under archive), but a **tombstoned** row (`deleted_at IS NOT NULL`) is
    /// `NotFound` — the deleted project is effectively absent (HV-123, SPEC §3.5).
    /// The write path uses [`Store::require_project_mut`], which adds the
    /// archived-write guard.
    pub(crate) fn require_project(&self, selector: Option<&str>) -> Result<(i64, String)> {
        let key = match selector {
            Some(k) => k.to_string(),
            None => self.current_project()?.ok_or_else(|| {
                HavenError::Invalid(format!(
                    "no project selected; pass `project` (MCP) or `--project` / \
                     `haven project use` (CLI){}",
                    self.available_projects_hint()
                ))
            })?,
        };
        let id = self
            .conn
            .query_row(
                "SELECT id FROM projects WHERE key = ?1 AND deleted_at IS NULL",
                [&key],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                HavenError::NotFound(format!("project {key:?}{}", self.available_projects_hint()))
            })?;
        Ok((id, key))
    }

    /// Resolve a project for a **mutating** caller (HV-123). Identical to
    /// [`Store::require_project`] but additionally refuses an **archived** project
    /// — writes into an archived project are blocked with an actionable "reopen it
    /// first" error, while reads (which use the plain resolver) keep working. Every
    /// item/edge/evolve/import/artifact write routes through here; the project
    /// lifecycle ops themselves resolve by key directly and deliberately bypass
    /// this guard (SPEC §3 preamble, §3.5).
    pub(crate) fn require_project_mut(&self, selector: Option<&str>) -> Result<(i64, String)> {
        let (id, key) = self.require_project(selector)?;
        let archived: bool = self
            .conn
            .query_row(
                "SELECT status = 'archived' FROM projects WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        if archived {
            return Err(HavenError::Invalid(format!(
                "project {key:?} is archived; reopen it to add or change items"
            )));
        }
        Ok((id, key))
    }

    // ---- ref resolution ---------------------------------------------------

    /// Resolve an item selector (`ref` like `HV-42`, or a `public_id` UUID) to
    /// its local node id, scoped to `project_id`. `public_id` is globally unique
    /// so it resolves regardless of project.
    ///
    /// On a miss the `NotFound` is enriched (HV-154): nearest live refs by
    /// numeric proximity + the project's ref prefix, and — when the requested
    /// ref's prefix belongs to a *different* project — that project is named.
    /// This is the single chokepoint every item op shares, so the CLI and MCP
    /// both get the better message for free.
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
            .ok_or_else(|| {
                HavenError::NotFound(format!(
                    "item {selector:?}{}",
                    self.not_found_hint(project_id, selector)
                ))
            })?;
        Ok(id)
    }

    /// Resolve a selector AND surface a [`StaleRef`] hint when the resolved node
    /// is `superseded`/`archived` — so the read path can ride a "this ref is
    /// dead; here's its live descendant(s)" warning on the success response
    /// instead of silently returning the dead item (HV-154). The lineage walk
    /// (formerly the public `resolve_live`) now runs here automatically. One
    /// place — `get_item`/`update_item`/`add_edge` all route through it.
    pub(crate) fn resolve_node_id_hinted(
        &self,
        project_id: i64,
        selector: &str,
    ) -> Result<(i64, Option<StaleRef>)> {
        let node_id = self.resolve_node_id(project_id, selector)?;
        let hint = self.stale_ref_for(node_id, selector)?;
        Ok((node_id, hint))
    }

    /// `Some(StaleRef)` when `node_id` is `superseded`/`archived`; `None` when it
    /// is live. `resolved_to` is the live descendant(s) reached by walking
    /// lineage forward (empty when the dead node has no live successor, e.g. a
    /// plain archive). The `requested` field echoes the ref the caller passed,
    /// for a clear "you asked for X; it's dead" message.
    fn stale_ref_for(&self, node_id: i64, requested: &str) -> Result<Option<StaleRef>> {
        let status: Status =
            self.conn
                .query_row("SELECT status FROM nodes WHERE id = ?1", [node_id], |r| {
                    r.get(0)
                })?;
        if !matches!(status, Status::Superseded | Status::Archived) {
            return Ok(None);
        }
        let resolved_to = self
            .live_lineage_descendants(node_id)?
            .into_iter()
            .map(|i| i.reference)
            .collect();
        // Echo the requested ref by its canonical form (the row's `ref`), so a
        // hint raised on a `public_id` selector still names the human ref.
        let canonical = self.node_ref(node_id).unwrap_or_else(|_| requested.into());
        Ok(Some(StaleRef {
            requested: canonical,
            resolved_to,
        }))
    }

    /// The not_found message tail (HV-154, shared with HV-153): nearest live
    /// same-prefix refs + the project's prefix, and a wrong-project-prefix note
    /// when the requested ref's prefix names a *different* project. Best-effort —
    /// any query error degrades to an empty tail rather than masking the original
    /// not_found. Returns a leading-space-prefixed fragment, or `""`.
    fn not_found_hint(&self, project_id: i64, selector: &str) -> String {
        // The project we searched: its key + prefix.
        let Ok((prefix, project_key)): rusqlite::Result<(String, String)> = self.conn.query_row(
            "SELECT ref_prefix, key FROM projects WHERE id = ?1",
            [project_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) else {
            return String::new();
        };

        // Wrong-project-prefix: if the selector's prefix belongs to ANOTHER
        // project, name it — "you're in project X; PREFIX is project Y's".
        let mut wrong = String::new();
        if let Some((sel_prefix, _)) = parse_ref(selector) {
            if !sel_prefix.eq_ignore_ascii_case(&prefix) {
                if let Ok(other_key) = self.conn.query_row(
                    "SELECT key FROM projects WHERE ref_prefix = ?1 COLLATE NOCASE",
                    [&sel_prefix],
                    |r| r.get::<_, String>(0),
                ) {
                    wrong = format!(
                        " — prefix {sel_prefix} is project {other_key:?}, not {project_key:?}"
                    );
                }
            }
        }

        let nearest = self
            .nearest_live_refs(project_id, selector, 3)
            .unwrap_or_default();
        let closest = if nearest.is_empty() {
            String::new()
        } else {
            format!("; closest live: {}", nearest.join(", "))
        };
        format!(" — no {selector} in {project_key}{closest}{wrong}; refs here use prefix {prefix}")
    }

    /// Up to `cap` LIVE same-prefix refs in `project_id`, nearest the requested
    /// counter by numeric proximity (HV-154). Live-only (excludes
    /// `archived`/`superseded`). When the selector has no parseable counter, or
    /// no live refs exist, returns the lowest live refs as a fallback so the
    /// message still orients. Reuses the `find_live_by_norm_title`/`similar_live`
    /// live-set precedent at `store/item.rs`.
    fn nearest_live_refs(
        &self,
        project_id: i64,
        selector: &str,
        cap: usize,
    ) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT ref FROM nodes
             WHERE project_id = ?1 AND status NOT IN ('archived','superseded')",
        )?;
        let rows = stmt.query_map([project_id], |r| r.get::<_, String>(0))?;
        let mut refs: Vec<(i64, String)> = rows
            .filter_map(|r| r.ok())
            .filter_map(|reference| parse_ref(&reference).map(|(_, n)| (n, reference)))
            .collect();
        // Numeric proximity to the requested counter; ties break low-counter
        // first (deterministic). With no counter to anchor on, fall back to the
        // lowest live refs.
        let target = parse_ref(selector).map(|(_, n)| n);
        match target {
            Some(t) => refs.sort_by_key(|(n, _)| ((n - t).abs(), *n)),
            None => refs.sort_by_key(|(n, _)| *n),
        }
        Ok(refs.into_iter().take(cap).map(|(_, r)| r).collect())
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
        status: row.get(11)?,
        archived_at: row.get(12)?,
        archived_reason: row.get(13)?,
        deleted_at: row.get(14)?,
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

        assert_eq!(s.list_projects(false).unwrap().len(), 2);
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

    // ---- HV-123: project archive / reopen lifecycle ----------------------

    /// GATE: namespace reservation — archive leaves key/ref_prefix/ref_counter
    /// byte-identical, bumps revision, flips sync_state=local, stamps reason.
    #[test]
    fn archive_reserves_namespace_and_bumps_revision() {
        let s = store();
        let p0 = s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        // Mint a ref so ref_counter is non-zero (proves it's preserved).
        s.use_project("haven").unwrap();
        s.add_item(
            None,
            NewItem {
                title: "X".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let before = s.get_project("haven").unwrap();
        assert_eq!(before.ref_counter, 1);

        let arch = s
            .archive_project("haven", Some("won't pursue"), Some("alice"))
            .unwrap();
        assert_eq!(arch.status, ProjectStatus::Archived);
        assert!(arch.archived_at.is_some());
        assert_eq!(arch.archived_reason.as_deref(), Some("won't pursue"));
        // Namespace BYTE-IDENTICAL across archive.
        assert_eq!(arch.key, before.key);
        assert_eq!(arch.ref_prefix, before.ref_prefix);
        assert_eq!(arch.ref_counter, before.ref_counter);
        assert_eq!(arch.public_id, p0.public_id);
        // Revision bumped + dirtied for sync.
        assert_eq!(arch.revision, before.revision + 1);
        assert_eq!(arch.sync_state, SyncState::Local);
    }

    /// GATE: idempotent re-archive — no SECOND revision bump / sync churn.
    #[test]
    fn re_archive_is_idempotent_noop() {
        let s = store();
        s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        let first = s.archive_project("haven", None, None).unwrap();
        let second = s.archive_project("haven", Some("ignored"), None).unwrap();
        assert_eq!(second.revision, first.revision, "no second bump");
        assert_eq!(second.status, ProjectStatus::Archived);
        // The re-archive did not overwrite the original reason with the new one.
        assert_eq!(second.archived_reason, first.archived_reason);
    }

    /// GATE: reopen restores fully; the next minted ref continues from the
    /// preserved counter (no reuse, no reset).
    #[test]
    fn reopen_restores_and_refs_continue_from_high_water_mark() {
        let s = store();
        s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        s.use_project("haven").unwrap();
        let i1 = s
            .add_item(
                None,
                NewItem {
                    title: "one".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(i1.reference, "HV-1");

        let arch = s.archive_project("haven", None, None).unwrap();
        let reopened = s.reopen_project("haven", None).unwrap();
        assert_eq!(reopened.status, ProjectStatus::Active);
        assert!(reopened.archived_at.is_none());
        assert!(reopened.archived_reason.is_none());
        assert_eq!(reopened.revision, arch.revision + 1);
        assert_eq!(reopened.ref_counter, 1, "counter preserved across cycle");

        // After reopen the next ref is HV-2, not a reused HV-1.
        s.use_project("haven").unwrap();
        let i2 = s
            .add_item(
                None,
                NewItem {
                    title: "two".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(i2.reference, "HV-2");
    }

    /// GATE: reopen of a never-archived project is a clean Invalid.
    #[test]
    fn reopen_of_active_project_errors_invalid() {
        let s = store();
        s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        let err = s.reopen_project("haven", None).unwrap_err();
        assert_eq!(err.code(), "invalid");
    }

    /// GATE: archive/reopen of a missing key → not_found.
    #[test]
    fn lifecycle_of_missing_key_is_not_found() {
        let s = store();
        assert_eq!(
            s.archive_project("nope", None, None).unwrap_err().code(),
            "not_found"
        );
        assert_eq!(
            s.reopen_project("nope", None).unwrap_err().code(),
            "not_found"
        );
    }

    /// GATE: the archived-write guard refuses MUTATING callers while reads still
    /// resolve; and lifecycle ops (reopen) BYPASS that guard.
    #[test]
    fn archived_write_guard_refuses_writes_but_reads_resolve() {
        let s = store();
        s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        s.use_project("haven").unwrap();
        let it = s
            .add_item(
                None,
                NewItem {
                    title: "live".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        s.archive_project("haven", None, None).unwrap();

        // READ still resolves under archive.
        let got = s.get_item(Some("haven"), &it.reference, &[]).unwrap();
        assert_eq!(got.reference, it.reference);
        assert_eq!(
            s.list_items(Some("haven"), &ItemFilter::default())
                .unwrap()
                .len(),
            1
        );

        // WRITE is refused with an actionable error.
        let err = s
            .add_item(
                Some("haven"),
                NewItem {
                    title: "blocked".into(),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert_eq!(err.code(), "invalid");
        assert!(err.to_string().contains("archived"));
        let err2 = s
            .update_item(
                Some("haven"),
                &it.reference,
                ItemUpdate {
                    title: Some("nope".into()),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert_eq!(err2.code(), "invalid");

        // LIFECYCLE op bypasses the guard: reopen succeeds on the archived project.
        let reopened = s.reopen_project("haven", None).unwrap();
        assert_eq!(reopened.status, ProjectStatus::Active);
        // And writes work again after reopen.
        s.add_item(
            Some("haven"),
            NewItem {
                title: "ok-now".into(),
                ..Default::default()
            },
        )
        .unwrap();
    }

    /// GATE: use_project refuses an archived project (reopen first).
    #[test]
    fn use_project_refuses_archived() {
        let s = store();
        s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        s.archive_project("haven", None, None).unwrap();
        let err = s.use_project("haven").unwrap_err();
        assert_eq!(err.code(), "invalid");
        assert!(err.to_string().contains("reopen"));
        // After reopen, selection works.
        s.reopen_project("haven", None).unwrap();
        s.use_project("haven").unwrap();
        assert_eq!(s.current_project().unwrap().as_deref(), Some("haven"));
    }

    /// archive clears current_project when it names the archived key.
    #[test]
    fn archive_clears_current_project_selection() {
        let s = store();
        s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        s.add_project("other", Some("OT"), "Other", None).unwrap();
        s.use_project("haven").unwrap();
        s.archive_project("haven", None, None).unwrap();
        assert_eq!(s.current_project().unwrap(), None, "selection cleared");
        // Archiving a non-selected project leaves the selection intact.
        s.use_project("other").unwrap();
        s.add_project("third", Some("TH"), "Third", None).unwrap();
        s.archive_project("third", None, None).unwrap();
        assert_eq!(s.current_project().unwrap().as_deref(), Some("other"));
    }

    /// list_projects(include_archived) + project_keys honour the filters.
    #[test]
    fn listing_filters_archived() {
        let s = store();
        s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        s.add_project("old", Some("OL"), "Old", None).unwrap();
        s.archive_project("old", None, None).unwrap();

        let default = s.list_projects(false).unwrap();
        assert_eq!(default.len(), 1);
        assert_eq!(default[0].key, "haven");

        let all = s.list_projects(true).unwrap();
        assert_eq!(all.len(), 2);

        // available_projects_hint / project_keys list active only → an archived
        // key is not suggested.
        assert_eq!(s.project_keys(), vec!["haven".to_string()]);

        // But the archived project still resolves by EXACT key.
        assert_eq!(
            s.get_project("old").unwrap().status,
            ProjectStatus::Archived
        );
    }

    /// READ-PATH GUARD (the only delete-touch in this leaf): a tombstoned row
    /// (`deleted_at` stamped) resolves NotFound everywhere; the namespace stays
    /// burned (add_project still Conflicts). No delete command exists yet — we
    /// stamp the column directly to prove the read path.
    #[test]
    fn tombstoned_row_resolves_not_found_but_key_stays_burned() {
        let s = store();
        s.add_project("gone", Some("GO"), "Gone", None).unwrap();
        // Simulate a future delete by stamping the tombstone column directly.
        s.conn
            .execute(
                "UPDATE projects SET deleted_at = datetime('now') WHERE key = 'gone'",
                [],
            )
            .unwrap();

        // get_project / require_project / lifecycle ops all treat it as absent.
        assert_eq!(s.get_project("gone").unwrap_err().code(), "not_found");
        assert_eq!(
            s.require_project(Some("gone")).unwrap_err().code(),
            "not_found"
        );
        assert_eq!(
            s.archive_project("gone", None, None).unwrap_err().code(),
            "not_found"
        );
        assert_eq!(
            s.reopen_project("gone", None).unwrap_err().code(),
            "not_found"
        );
        // Neither listing shows a tombstoned project.
        assert!(s.list_projects(false).unwrap().is_empty());
        assert!(s.list_projects(true).unwrap().is_empty());
        assert!(s.project_keys().is_empty());

        // The retained row keeps UNIQUE(key) occupied → re-adding Conflicts.
        assert_eq!(
            s.add_project("gone", None, "Revived", None)
                .unwrap_err()
                .code(),
            "conflict"
        );
    }

    /// xref works inside an ARCHIVED project (Store::xref passes
    /// include_archived=true). Needs a real content root for the artifact file.
    #[test]
    fn xref_resolves_inside_archived_project() {
        use crate::model::{ArtifactRole, Xref, XrefRelation};
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_in_memory_at(dir.path()).unwrap();
        s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        s.use_project("haven").unwrap();
        let target = s
            .add_item(
                None,
                NewItem {
                    title: "target".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let source = s
            .add_item(
                None,
                NewItem {
                    title: "source".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        // An outbound Haven xref on `source` pointing at `target`.
        s.add_artifact(
            None,
            &source.reference,
            NewArtifact {
                role: ArtifactRole::Spec,
                content: Some("body".into()),
                name: Some("spec.md".into()),
                metadata: Some(serde_json::json!({
                    "xref": [Xref {
                        relation: XrefRelation::DerivedFrom,
                        store: "haven".into(),
                        target: target.reference.clone(),
                        canonical: false,
                    }]
                })),
                ..Default::default()
            },
        )
        .unwrap();

        // Now archive the project; the xref read must still resolve.
        s.archive_project("haven", None, None).unwrap();
        let report = s.xref(Some("haven"), &target.reference).unwrap();
        assert_eq!(report.node, target.reference);
        assert_eq!(
            report.inbound.len(),
            1,
            "inbound backlink found under archive"
        );
        assert_eq!(report.inbound[0].source, source.reference);
    }
}
