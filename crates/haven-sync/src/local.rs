//! Local read/write side of sync (SPEC §5).
//!
//! - **Push (read):** [`collect_push_batch`] gathers rows not yet `synced`,
//!   translating local integer foreign keys into the stable `public_id` UUIDs
//!   the remote uses.
//! - **Pull (write):** [`apply_snapshot`] does the inverse — reverse-translating
//!   each `public_id` FK back to a local id (parents first) and LWW-merging by
//!   `revision` — to reconcile a fetched remote snapshot into the local store.
//!
//! This FK translation is the trickiest part of sync, so both directions are
//! unit-tested against a real local database even though the HTTP transport is
//! exercised only against a live Supabase.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};

use crate::engine::RemoteSnapshot;
use crate::SyncError;

/// The full set of unsynced rows for one push pass, in dependency order:
/// projects → nodes → lineage (append-only core) → mutable edges → artifacts.
#[derive(Debug, Default)]
pub struct PushBatch {
    pub projects: Vec<Value>,
    pub nodes: Vec<Value>,
    pub lineage_events: Vec<Value>,
    pub lineage_edges: Vec<Value>,
    pub decomposition_edges: Vec<Value>,
    pub dependency_edges: Vec<Value>,
    pub grouping_edges: Vec<Value>,
    pub artifacts: Vec<Value>,
}

/// Parse a TEXT JSON column into a `Value`, falling back to a string if it isn't
/// valid JSON (so a corrupt cell never aborts a whole pass).
fn json_col(s: String) -> Value {
    serde_json::from_str(&s).unwrap_or(Value::String(s))
}

fn query_rows<F>(conn: &Connection, sql: &str, f: F) -> Result<Vec<Value>, SyncError>
where
    F: Fn(&Row<'_>) -> rusqlite::Result<Value>,
{
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| f(r))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Collect every unsynced row, with FKs already translated to `public_id`s.
/// Runs all reads inside one deferred transaction so the per-table SELECTs see a
/// single consistent snapshot — otherwise a concurrent writer (e.g. the MCP
/// server in the same process) could insert a node between the `nodes` and edge
/// reads, yielding an edge whose node isn't in this batch and would FK-fail
/// remotely.
pub fn collect_push_batch(conn: &Connection) -> Result<PushBatch, SyncError> {
    let tx = conn.unchecked_transaction()?;
    let batch = collect_push_batch_inner(&tx)?;
    tx.commit()?; // read-only; just ends the snapshot
    Ok(batch)
}

fn collect_push_batch_inner(conn: &Connection) -> Result<PushBatch, SyncError> {
    let projects = query_rows(
        conn,
        "SELECT public_id, key, ref_prefix, ref_counter, title, description, client_id, revision
         FROM projects WHERE sync_state <> 'synced'",
        |r| {
            Ok(json!({
                "public_id": r.get::<_, String>(0)?,
                "key": r.get::<_, String>(1)?,
                "ref_prefix": r.get::<_, String>(2)?,
                "ref_counter": r.get::<_, i64>(3)?,
                "title": r.get::<_, String>(4)?,
                "description": r.get::<_, Option<String>>(5)?,
                "client_id": r.get::<_, String>(6)?,
                "revision": r.get::<_, i64>(7)?,
            }))
        },
    )?;

    let nodes = query_rows(
        conn,
        "SELECT n.public_id, p.public_id, n.ref, n.title, n.body, n.type, n.status,
                n.owner_kind, n.assignee, n.wait_state, n.committed, n.priority, n.sort_key,
                n.metadata, n.created_at, n.updated_at, n.archived_at, n.client_id, n.revision,
                n.done_looks_like, n.why
         FROM nodes n JOIN projects p ON p.id = n.project_id
         WHERE n.sync_state <> 'synced'",
        |r| {
            Ok(json!({
                "public_id": r.get::<_, String>(0)?,
                "project_id": r.get::<_, String>(1)?,
                "ref": r.get::<_, String>(2)?,
                "title": r.get::<_, String>(3)?,
                "body": r.get::<_, Option<String>>(4)?,
                "type": r.get::<_, String>(5)?,
                "status": r.get::<_, String>(6)?,
                "owner_kind": r.get::<_, Option<String>>(7)?,
                "assignee": r.get::<_, Option<String>>(8)?,
                "wait_state": r.get::<_, Option<String>>(9)?,
                "committed": r.get::<_, bool>(10)?,
                "priority": r.get::<_, Option<i64>>(11)?,
                "sort_key": r.get::<_, Option<String>>(12)?,
                "metadata": json_col(r.get::<_, String>(13)?),
                "created_at": r.get::<_, String>(14)?,
                "updated_at": r.get::<_, String>(15)?,
                "archived_at": r.get::<_, Option<String>>(16)?,
                "client_id": r.get::<_, String>(17)?,
                "revision": r.get::<_, i64>(18)?,
                "done_looks_like": r.get::<_, Option<String>>(19)?,
                "why": r.get::<_, Option<String>>(20)?,
            }))
        },
    )?;

    let lineage_events = query_rows(
        conn,
        "SELECT e.public_id, p.public_id, e.event_type, e.rationale, e.triggered_by,
                e.context, e.created_at, e.client_id
         FROM lineage_events e JOIN projects p ON p.id = e.project_id
         WHERE e.sync_state <> 'synced'",
        |r| {
            Ok(json!({
                "public_id": r.get::<_, String>(0)?,
                "project_id": r.get::<_, String>(1)?,
                "event_type": r.get::<_, String>(2)?,
                "rationale": r.get::<_, Option<String>>(3)?,
                "triggered_by": r.get::<_, Option<String>>(4)?,
                "context": json_col(r.get::<_, String>(5)?),
                "created_at": r.get::<_, String>(6)?,
                "client_id": r.get::<_, String>(7)?,
            }))
        },
    )?;

    // Lineage edges have no sync_state of their own — they ride along with their
    // (unsynced) event.
    let lineage_edges = query_rows(
        conn,
        "SELECT ev.public_id, fromn.public_id, ton.public_id
         FROM lineage_edges le
         JOIN lineage_events ev ON ev.id = le.event_id
         JOIN nodes fromn ON fromn.id = le.from_node_id
         JOIN nodes ton ON ton.id = le.to_node_id
         WHERE ev.sync_state <> 'synced'",
        |r| {
            Ok(json!({
                "event_id": r.get::<_, String>(0)?,
                "from_node_id": r.get::<_, String>(1)?,
                "to_node_id": r.get::<_, String>(2)?,
            }))
        },
    )?;

    let edge = |node_a: &str, node_b: &str, table: &str| -> Result<Vec<Value>, SyncError> {
        let sql = format!(
            "SELECT a.public_id, b.public_id, e.client_id, e.created_at
             FROM {table} e
             JOIN nodes a ON a.id = e.{node_a}
             JOIN nodes b ON b.id = e.{node_b}
             WHERE e.sync_state <> 'synced'"
        );
        query_rows(conn, &sql, move |r| {
            Ok(json!({
                node_a: r.get::<_, String>(0)?,
                node_b: r.get::<_, String>(1)?,
                "client_id": r.get::<_, String>(2)?,
                "created_at": r.get::<_, String>(3)?,
            }))
        })
    };
    let decomposition_edges = edge("parent_id", "child_id", "decomposition_edges")?;
    let dependency_edges = edge("node_id", "depends_on_id", "dependency_edges")?;
    let grouping_edges = edge("group_id", "member_id", "grouping_edges")?;

    let artifacts = query_rows(
        conn,
        "SELECT a.public_id, n.public_id, a.role, a.kind, a.path, a.uri, a.title, a.excerpt,
                a.from_owner, a.to_owner, a.content_hash, a.remote_path, a.metadata,
                a.created_at, a.created_by, a.client_id, a.revision
         FROM artifacts a JOIN nodes n ON n.id = a.node_id
         WHERE a.sync_state <> 'synced'",
        |r| {
            Ok(json!({
                "public_id": r.get::<_, String>(0)?,
                "node_id": r.get::<_, String>(1)?,
                "role": r.get::<_, String>(2)?,
                "kind": r.get::<_, String>(3)?,
                "path": r.get::<_, Option<String>>(4)?,
                "uri": r.get::<_, Option<String>>(5)?,
                "title": r.get::<_, Option<String>>(6)?,
                "excerpt": r.get::<_, Option<String>>(7)?,
                "from_owner": r.get::<_, Option<String>>(8)?,
                "to_owner": r.get::<_, Option<String>>(9)?,
                "content_hash": r.get::<_, Option<String>>(10)?,
                "remote_path": r.get::<_, Option<String>>(11)?,
                "metadata": json_col(r.get::<_, String>(12)?),
                "created_at": r.get::<_, String>(13)?,
                "created_by": r.get::<_, Option<String>>(14)?,
                "client_id": r.get::<_, String>(15)?,
                "revision": r.get::<_, i64>(16)?,
            }))
        },
    )?;

    Ok(PushBatch {
        projects,
        nodes,
        lineage_events,
        lineage_edges,
        decomposition_edges,
        dependency_edges,
        grouping_edges,
        artifacts,
    })
}

/// Mark rows synced by `client_id` after a successful push of `table`. Tables
/// with a `last_synced_at` column get it stamped; others just flip sync_state.
pub fn mark_synced(conn: &Connection, table: &str, client_ids: &[String]) -> Result<(), SyncError> {
    let has_last_synced = matches!(table, "projects" | "nodes" | "artifacts");
    let set = if has_last_synced {
        "sync_state = 'synced', last_synced_at = datetime('now')"
    } else {
        "sync_state = 'synced'"
    };
    let sql = format!("UPDATE {table} SET {set} WHERE client_id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    for cid in client_ids {
        stmt.execute([cid])?;
    }
    Ok(())
}

// ============================================================================
// Content blobs — the file half of the artifact channel (SPEC §5).
//
// Artifact *rows* are typed pointers; the bytes live as files under
// `<content_root>/<project-key>/<rel_path>`. On push, changed `kind='file'`
// blobs upload to Storage (object key `<sub>/<project-key>/<rel_path>`) before
// their rows, so a pushed row's `remote_path`/`content_hash` always name a blob
// that exists. On read, a missing local file with a `remote_path` downloads
// lazily and is cached ([`write_hydrated`]).
//
// Selection is by **hash comparison**, not `sync_state`: editing a content file
// directly (the normal local workflow) never touches the DB row, so the only
// reliable change signal is `sha256(file) != content_hash`.
// ============================================================================

/// One `kind='file'` artifact row considered for a content upload.
#[derive(Debug, Clone)]
pub struct ContentCandidate {
    pub client_id: String,
    pub project_key: String,
    /// `artifacts.path` — relative to `<content_root>/<project_key>/`.
    pub rel_path: String,
    /// Hash recorded at creation / last sync; `None` never happens for
    /// kind=file in practice but is tolerated.
    pub content_hash: Option<String>,
    /// Storage object key once uploaded; `None` = never uploaded.
    pub remote_path: Option<String>,
}

/// All `kind='file'` artifacts with a local-relative path — every potential
/// blob upload. The hash check in [`needs_upload`] decides which actually go.
pub fn collect_content_candidates(conn: &Connection) -> Result<Vec<ContentCandidate>, SyncError> {
    let mut stmt = conn.prepare(
        "SELECT a.client_id, p.key, a.path, a.content_hash, a.remote_path
         FROM artifacts a
         JOIN nodes n ON n.id = a.node_id
         JOIN projects p ON p.id = n.project_id
         WHERE a.kind = 'file' AND a.path IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ContentCandidate {
            client_id: r.get(0)?,
            project_key: r.get(1)?,
            rel_path: r.get(2)?,
            content_hash: r.get(3)?,
            remote_path: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Decide whether a candidate's blob needs uploading: read the local file and
/// hash it; skip when the file is absent (e.g. a pulled row not yet hydrated —
/// nothing to upload) or when the bytes are unchanged *and* already uploaded.
/// Returns the bytes + their hash so the caller uploads exactly what was hashed.
pub fn needs_upload(c: &ContentCandidate, content_root: &Path) -> Option<(Vec<u8>, String)> {
    let full = content_root.join(&c.project_key).join(&c.rel_path);
    let bytes = std::fs::read(full).ok()?;
    let hash = sha256_hex(&bytes);
    if c.remote_path.is_some() && c.content_hash.as_deref() == Some(hash.as_str()) {
        return None;
    }
    Some((bytes, hash))
}

/// Record a successful blob upload on its artifact row. Bumps `revision` and
/// re-flags `sync_state='local'` so the row push (which follows uploads in the
/// pass) carries the new `remote_path`/`content_hash` — and other devices accept
/// the change via revision-LWW.
pub fn record_uploaded(
    conn: &Connection,
    client_id: &str,
    remote_path: &str,
    content_hash: &str,
) -> Result<(), SyncError> {
    conn.execute(
        "UPDATE artifacts SET remote_path = ?1, content_hash = ?2,
            revision = revision + 1, sync_state = 'local'
         WHERE client_id = ?3",
        params![remote_path, content_hash, client_id],
    )?;
    Ok(())
}

/// Cache downloaded blob bytes at `<content_root>/<project_key>/<rel_path>`,
/// verifying them against `expected_hash` when one is known (a mismatch means
/// the blob and the row disagree — fail rather than cache wrong content) and
/// guarding against path traversal in a remote-supplied `rel_path`.
pub fn write_hydrated(
    content_root: &Path,
    project_key: &str,
    rel_path: &str,
    bytes: &[u8],
    expected_hash: Option<&str>,
) -> Result<PathBuf, SyncError> {
    if rel_path.split(['/', '\\']).any(|seg| seg == "..") {
        return Err(SyncError::Permanent(format!(
            "refusing to hydrate path-traversing artifact path {rel_path:?}"
        )));
    }
    if let Some(expected) = expected_hash {
        let actual = sha256_hex(bytes);
        if actual != expected {
            return Err(SyncError::Permanent(format!(
                "downloaded content hash mismatch for {rel_path}: expected {expected}, got {actual}"
            )));
        }
    }
    let full = content_root.join(project_key).join(rel_path);
    if let Some(dir) = full.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| SyncError::Permanent(format!("creating {}: {e}", dir.display())))?;
    }
    std::fs::write(&full, bytes)
        .map_err(|e| SyncError::Permanent(format!("writing {}: {e}", full.display())))?;
    Ok(full)
}

/// sha256 → lowercase hex, matching how `haven-core` records `content_hash`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ============================================================================
// Pull reconcile — the inverse of `collect_push_batch`.
//
// Take remote rows (keyed by `public_id`, with `public_id`-valued foreign keys)
// and apply them to the local store, translating each FK back to its local
// integer id. Parents are applied before children (projects → nodes → lineage →
// edges → artifacts) so every FK resolves against rows already present.
//
// Conflict resolution is last-write-wins by `revision`. Every local mutation
// bumps `revision` and re-flags `sync_state = 'local'`, so `revision` is a
// per-row monotonic edit count. A remote row is applied only when its revision
// is *strictly greater* than the local one: a remote that has seen more edits
// than we have wins, while unpushed local edits (which carry a higher local
// revision) are preserved until they push. Under a single writer, divergent
// content never shares a revision, so the design's `updated_at` tiebreak is moot
// (and a naive cross-format string compare would only mis-clobber unpushed local
// edits) — we deliberately keep local on an exact-revision tie. Append-only rows
// (lineage events/edges and the structural edges) are insert-if-absent.
// ============================================================================

/// What a pull reconcile changed locally — rows inserted or LWW-updated.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileStats {
    pub projects: usize,
    pub nodes: usize,
    pub lineage_events: usize,
    pub lineage_edges: usize,
    pub edges: usize,
    pub artifacts: usize,
}

impl ReconcileStats {
    /// Total rows applied across all tables.
    pub fn total(&self) -> usize {
        self.projects
            + self.nodes
            + self.lineage_events
            + self.lineage_edges
            + self.edges
            + self.artifacts
    }
}

/// Apply a pulled [`RemoteSnapshot`] to the local store in one transaction:
/// reverse-translate every `public_id` FK to its local id (parents first),
/// LWW-merge the mutable tables by `revision`, and insert-if-absent the
/// append-only ones. All-or-nothing — a failure rolls back the whole pull.
/// `content_root` is used for content-cache invalidation (see
/// [`apply_artifacts`]'s stale-cache rule).
pub fn apply_snapshot(
    conn: &Connection,
    snap: &RemoteSnapshot,
    content_root: &Path,
) -> Result<ReconcileStats, SyncError> {
    let tx = conn.unchecked_transaction()?;
    let stats = ReconcileStats {
        projects: apply_projects(&tx, &snap.projects)?,
        nodes: apply_nodes(&tx, &snap.nodes)?,
        lineage_events: apply_lineage_events(&tx, &snap.lineage_events)?,
        lineage_edges: apply_lineage_edges(&tx, &snap.lineage_edges)?,
        edges: apply_edges(
            &tx,
            &snap.decomposition_edges,
            "decomposition_edges",
            "parent_id",
            "child_id",
        )? + apply_edges(
            &tx,
            &snap.dependency_edges,
            "dependency_edges",
            "node_id",
            "depends_on_id",
        )? + apply_edges(
            &tx,
            &snap.grouping_edges,
            "grouping_edges",
            "group_id",
            "member_id",
        )?,
        artifacts: apply_artifacts(&tx, &snap.artifacts, content_root)?,
    };
    tx.commit()?;
    Ok(stats)
}

// --- field extractors (remote JSON → SQLite-bindable values) ---------------

fn vstr(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}

fn vstr_req(v: &Value, k: &str) -> Result<String, SyncError> {
    vstr(v, k).ok_or_else(|| SyncError::Permanent(format!("pull: row missing string field `{k}`")))
}

fn vint(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(Value::as_i64)
}

fn vbool(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// Re-serialize a JSON object/array column (`metadata`, `context`) back to the
/// TEXT form SQLite stores. The remote returns it as real JSON; a corrupt or
/// missing cell falls back to `{}`.
fn vjson(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(x) if x.is_object() || x.is_array() => x.to_string(),
        Some(Value::String(s)) => s.clone(),
        _ => "{}".to_string(),
    }
}

/// Resolve a `public_id` to its local integer id in `table`, erroring if the
/// referenced row isn't present (the snapshot applied its parents out of order
/// or is incomplete).
fn resolve_id(conn: &Connection, table: &str, public_id: &str) -> Result<i64, SyncError> {
    let sql = format!("SELECT id FROM {table} WHERE public_id = ?1");
    conn.query_row(&sql, [public_id], |r| r.get::<_, i64>(0))
        .optional()?
        .ok_or_else(|| SyncError::Permanent(format!("pull: unknown {table} public_id {public_id}")))
}

/// The local `revision` for a row identified by `public_id`, if it exists.
fn local_revision(
    conn: &Connection,
    table: &str,
    public_id: &str,
) -> Result<Option<i64>, SyncError> {
    let sql = format!("SELECT revision FROM {table} WHERE public_id = ?1");
    Ok(conn
        .query_row(&sql, [public_id], |r| r.get::<_, i64>(0))
        .optional()?)
}

// --- per-table apply --------------------------------------------------------

fn apply_projects(conn: &Connection, rows: &[Value]) -> Result<usize, SyncError> {
    let mut n = 0;
    for r in rows {
        let pid = vstr_req(r, "public_id")?;
        let rev = vint(r, "revision").unwrap_or(1);
        let key = vstr_req(r, "key")?;
        let prefix = vstr_req(r, "ref_prefix")?;
        let counter = vint(r, "ref_counter").unwrap_or(0);
        let title = vstr_req(r, "title")?;
        let desc = vstr(r, "description");
        let cid = vstr_req(r, "client_id")?;
        let created = vstr(r, "created_at");
        let updated = vstr(r, "updated_at");
        match local_revision(conn, "projects", &pid)? {
            Some(local_rev) if rev <= local_rev => {}
            Some(_) => {
                conn.execute(
                    "UPDATE projects SET key=?2, ref_prefix=?3, ref_counter=?4, title=?5,
                        description=?6, client_id=?7, revision=?8,
                        updated_at=COALESCE(?9, datetime('now')),
                        sync_state='synced', last_synced_at=datetime('now')
                     WHERE public_id=?1",
                    params![pid, key, prefix, counter, title, desc, cid, rev, updated],
                )?;
                n += 1;
            }
            None => {
                conn.execute(
                    "INSERT INTO projects
                        (public_id, key, ref_prefix, ref_counter, title, description,
                         created_at, updated_at, client_id, revision, sync_state, last_synced_at)
                     VALUES (?1,?2,?3,?4,?5,?6,
                             COALESCE(?7, datetime('now')), COALESCE(?8, datetime('now')),
                             ?9, ?10, 'synced', datetime('now'))",
                    params![pid, key, prefix, counter, title, desc, created, updated, cid, rev],
                )?;
                n += 1;
            }
        }
    }
    Ok(n)
}

fn apply_nodes(conn: &Connection, rows: &[Value]) -> Result<usize, SyncError> {
    let mut n = 0;
    for r in rows {
        let pid = vstr_req(r, "public_id")?;
        let project_id = resolve_id(conn, "projects", &vstr_req(r, "project_id")?)?;
        let rev = vint(r, "revision").unwrap_or(1);
        let reference = vstr_req(r, "ref")?;
        let title = vstr_req(r, "title")?;
        let body = vstr(r, "body");
        let dll = vstr(r, "done_looks_like");
        let why = vstr(r, "why");
        let typ = vstr_req(r, "type")?;
        let status = vstr_req(r, "status")?;
        let owner = vstr(r, "owner_kind");
        let assignee = vstr(r, "assignee");
        let wait = vstr(r, "wait_state");
        let committed = vbool(r, "committed") as i64;
        let priority = vint(r, "priority");
        let sort_key = vstr(r, "sort_key");
        let metadata = vjson(r, "metadata");
        let created = vstr(r, "created_at");
        let updated = vstr(r, "updated_at");
        let archived = vstr(r, "archived_at");
        let cid = vstr_req(r, "client_id")?;
        match local_revision(conn, "nodes", &pid)? {
            Some(local_rev) if rev <= local_rev => {}
            Some(_) => {
                conn.execute(
                    "UPDATE nodes SET project_id=?2, ref=?3, title=?4, body=?5, type=?6,
                        status=?7, owner_kind=?8, assignee=?9, wait_state=?10, committed=?11,
                        priority=?12, sort_key=?13, metadata=?14,
                        updated_at=COALESCE(?15, datetime('now')), archived_at=?16,
                        client_id=?17, revision=?18, done_looks_like=?19, why=?20,
                        sync_state='synced', last_synced_at=datetime('now')
                     WHERE public_id=?1",
                    params![
                        pid, project_id, reference, title, body, typ, status, owner, assignee,
                        wait, committed, priority, sort_key, metadata, updated, archived, cid, rev,
                        dll, why
                    ],
                )?;
                n += 1;
            }
            None => {
                conn.execute(
                    "INSERT INTO nodes
                        (public_id, project_id, ref, title, body, type, status, owner_kind,
                         assignee, wait_state, committed, priority, sort_key, metadata,
                         created_at, updated_at, archived_at, client_id, revision, sync_state,
                         last_synced_at, done_looks_like, why)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
                             COALESCE(?15, datetime('now')), COALESCE(?16, datetime('now')), ?17,
                             ?18, ?19, 'synced', datetime('now'), ?20, ?21)",
                    params![
                        pid, project_id, reference, title, body, typ, status, owner, assignee,
                        wait, committed, priority, sort_key, metadata, created, updated, archived,
                        cid, rev, dll, why
                    ],
                )?;
                n += 1;
            }
        }
    }
    Ok(n)
}

fn apply_lineage_events(conn: &Connection, rows: &[Value]) -> Result<usize, SyncError> {
    let mut n = 0;
    for r in rows {
        let pid = vstr_req(r, "public_id")?;
        let project_id = resolve_id(conn, "projects", &vstr_req(r, "project_id")?)?;
        let etype = vstr_req(r, "event_type")?;
        let rationale = vstr(r, "rationale");
        let triggered = vstr(r, "triggered_by");
        let context = vjson(r, "context");
        let created = vstr(r, "created_at");
        let cid = vstr_req(r, "client_id")?;
        // Append-only: insert if absent, never update.
        n += conn.execute(
            "INSERT OR IGNORE INTO lineage_events
                (public_id, project_id, event_type, rationale, triggered_by, context,
                 created_at, client_id, sync_state)
             VALUES (?1,?2,?3,?4,?5,?6, COALESCE(?7, datetime('now')), ?8, 'synced')",
            params![pid, project_id, etype, rationale, triggered, context, created, cid],
        )?;
    }
    Ok(n)
}

fn apply_lineage_edges(conn: &Connection, rows: &[Value]) -> Result<usize, SyncError> {
    let mut n = 0;
    for r in rows {
        let event_id = resolve_id(conn, "lineage_events", &vstr_req(r, "event_id")?)?;
        let from = resolve_id(conn, "nodes", &vstr_req(r, "from_node_id")?)?;
        let to = resolve_id(conn, "nodes", &vstr_req(r, "to_node_id")?)?;
        n += conn.execute(
            "INSERT OR IGNORE INTO lineage_edges (event_id, from_node_id, to_node_id)
             VALUES (?1, ?2, ?3)",
            params![event_id, from, to],
        )?;
    }
    Ok(n)
}

/// Structural edges (decomposition/dependency/grouping) — insert-if-absent. The
/// JSON keys equal the column names and hold node `public_id`s.
fn apply_edges(
    conn: &Connection,
    rows: &[Value],
    table: &str,
    col_a: &str,
    col_b: &str,
) -> Result<usize, SyncError> {
    let mut n = 0;
    for r in rows {
        let a = resolve_id(conn, "nodes", &vstr_req(r, col_a)?)?;
        let b = resolve_id(conn, "nodes", &vstr_req(r, col_b)?)?;
        let cid = vstr_req(r, "client_id")?;
        let created = vstr(r, "created_at");
        let sql = format!(
            "INSERT OR IGNORE INTO {table} ({col_a}, {col_b}, created_at, client_id, sync_state)
             VALUES (?1, ?2, COALESCE(?3, datetime('now')), ?4, 'synced')"
        );
        n += conn.execute(&sql, params![a, b, created, cid])?;
    }
    Ok(n)
}

/// The cache-invalidation half of artifact LWW (see [`apply_artifacts`]): when
/// the incoming row's `content_hash` differs from the local row's, remove the
/// cached file **iff** its bytes still hash to the old row hash (an unedited
/// stale cache). Locally-edited files (bytes ≠ old hash) are kept.
fn invalidate_stale_cache(
    conn: &Connection,
    public_id: &str,
    new_hash: Option<&str>,
    content_root: &Path,
) -> Result<(), SyncError> {
    let row: Option<(Option<String>, Option<String>, String)> = conn
        .query_row(
            "SELECT a.content_hash, a.path, p.key
             FROM artifacts a
             JOIN nodes n ON n.id = a.node_id
             JOIN projects p ON p.id = n.project_id
             WHERE a.public_id = ?1 AND a.kind = 'file'",
            [public_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((old_hash, Some(rel_path), project_key)) = row else {
        return Ok(()); // not a file artifact / no path — nothing cached
    };
    if old_hash.as_deref() == new_hash {
        return Ok(()); // content unchanged — cache stays valid
    }
    let full = content_root.join(&project_key).join(&rel_path);
    if let Ok(bytes) = std::fs::read(&full) {
        if Some(sha256_hex(&bytes).as_str()) == old_hash.as_deref() {
            // Unedited stale cache: drop it; the next read lazy-downloads the
            // new content. (A failure to remove is non-fatal — the stale file
            // would just be served until removed.)
            let _ = std::fs::remove_file(&full);
        }
    }
    Ok(())
}

/// Apply remote artifact rows (revision-LWW), with **content-cache
/// invalidation**: when the remote row carries a different `content_hash` than
/// the local row, the locally cached file — if its bytes still hash to the *old*
/// row hash, i.e. an unedited stale cache — is removed so the next read
/// lazy-downloads the new content. A file whose bytes differ from the old row
/// hash was edited on this machine; it is kept, and becomes the next version on
/// this device's next push (file-level last-write-wins).
fn apply_artifacts(
    conn: &Connection,
    rows: &[Value],
    content_root: &Path,
) -> Result<usize, SyncError> {
    let mut n = 0;
    for r in rows {
        let pid = vstr_req(r, "public_id")?;
        let node_id = resolve_id(conn, "nodes", &vstr_req(r, "node_id")?)?;
        let rev = vint(r, "revision").unwrap_or(1);
        let role = vstr_req(r, "role")?;
        let kind = vstr(r, "kind").unwrap_or_else(|| "file".into());
        let path = vstr(r, "path");
        let uri = vstr(r, "uri");
        let title = vstr(r, "title");
        let excerpt = vstr(r, "excerpt");
        let from_owner = vstr(r, "from_owner");
        let to_owner = vstr(r, "to_owner");
        let content_hash = vstr(r, "content_hash");
        let remote_path = vstr(r, "remote_path");
        let metadata = vjson(r, "metadata");
        let created = vstr(r, "created_at");
        let created_by = vstr(r, "created_by");
        let cid = vstr_req(r, "client_id")?;
        match local_revision(conn, "artifacts", &pid)? {
            Some(local_rev) if rev <= local_rev => {}
            Some(_) => {
                invalidate_stale_cache(conn, &pid, content_hash.as_deref(), content_root)?;
                conn.execute(
                    "UPDATE artifacts SET node_id=?2, role=?3, kind=?4, path=?5, uri=?6,
                        title=?7, excerpt=?8, from_owner=?9, to_owner=?10, content_hash=?11,
                        remote_path=?12, metadata=?13, created_by=?14, client_id=?15,
                        revision=?16, sync_state='synced', last_synced_at=datetime('now')
                     WHERE public_id=?1",
                    params![
                        pid,
                        node_id,
                        role,
                        kind,
                        path,
                        uri,
                        title,
                        excerpt,
                        from_owner,
                        to_owner,
                        content_hash,
                        remote_path,
                        metadata,
                        created_by,
                        cid,
                        rev
                    ],
                )?;
                n += 1;
            }
            None => {
                conn.execute(
                    "INSERT INTO artifacts
                        (public_id, node_id, role, kind, path, uri, title, excerpt, from_owner,
                         to_owner, content_hash, remote_path, metadata, created_at, created_by,
                         client_id, revision, sync_state, last_synced_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,
                             COALESCE(?14, datetime('now')), ?15, ?16, ?17, 'synced',
                             datetime('now'))",
                    params![
                        pid,
                        node_id,
                        role,
                        kind,
                        path,
                        uri,
                        title,
                        excerpt,
                        from_owner,
                        to_owner,
                        content_hash,
                        remote_path,
                        metadata,
                        created,
                        created_by,
                        cid,
                        rev
                    ],
                )?;
                n += 1;
            }
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_core::{NewItem, Store};

    fn collect_value(values: &[Value], public_id: &str) -> Value {
        values
            .iter()
            .find(|v| v["public_id"] == public_id || v.get("public_id").is_none())
            .cloned()
            .unwrap_or(Value::Null)
    }

    #[test]
    fn fk_translation_uses_public_ids() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("haven.db");
        let store = Store::open(&db, dir.path()).unwrap();
        let project = store
            .add_project("haven", Some("HV"), "Haven", None)
            .unwrap();
        store.use_project("haven").unwrap();

        let parent = store
            .add_item(
                None,
                NewItem {
                    title: "Parent".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let child = store
            .add_item(
                None,
                NewItem {
                    title: "Child".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .decompose(None, &parent.reference, &child.reference, false)
            .unwrap();
        let split = store
            .evolve_split(None, &child.reference, &["A".into()], Some("why"), None)
            .unwrap();

        // Read via a separate connection to the same file.
        let conn = haven_core::db::open(&db).unwrap();
        let batch = collect_push_batch(&conn).unwrap();

        // The node's project_id is the project's public_id (a UUID), not an int.
        let parent_row = batch
            .nodes
            .iter()
            .find(|v| v["public_id"] == Value::String(parent.public_id.clone()))
            .unwrap();
        assert_eq!(
            parent_row["project_id"],
            Value::String(project.public_id.clone())
        );

        // The decomposition edge references node public_ids.
        assert_eq!(batch.decomposition_edges.len(), 1);
        let de = &batch.decomposition_edges[0];
        assert_eq!(de["parent_id"], Value::String(parent.public_id.clone()));
        assert_eq!(de["child_id"], Value::String(child.public_id.clone()));

        // The split produced a lineage event + edge, FK-translated to public_ids.
        assert_eq!(batch.lineage_events.len(), 1);
        assert_eq!(
            batch.lineage_events[0]["public_id"],
            Value::String(split.event_id.clone())
        );
        assert!(batch
            .lineage_edges
            .iter()
            .any(|e| e["from_node_id"] == Value::String(child.public_id.clone())));

        // metadata came back as a JSON object, not a quoted string.
        assert!(parent_row["metadata"].is_object());

        let _ = collect_value; // silence unused in some configs
    }

    #[test]
    fn mark_synced_clears_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("haven.db");
        let store = Store::open(&db, dir.path()).unwrap();
        store
            .add_project("haven", Some("HV"), "Haven", None)
            .unwrap();
        store.use_project("haven").unwrap();
        store
            .add_item(
                None,
                NewItem {
                    title: "X".into(),
                    ..Default::default()
                },
            )
            .unwrap();

        let conn = haven_core::db::open(&db).unwrap();
        let batch = collect_push_batch(&conn).unwrap();
        assert!(!batch.nodes.is_empty());

        let cids: Vec<String> = batch
            .nodes
            .iter()
            .map(|v| v["client_id"].as_str().unwrap().to_string())
            .collect();
        mark_synced(&conn, "nodes", &cids).unwrap();

        // Re-collecting now finds no unsynced nodes.
        let batch2 = collect_push_batch(&conn).unwrap();
        assert!(batch2.nodes.is_empty());
    }

    /// A push batch is, field-for-field, the shape PostgREST returns on pull
    /// (`select=*`), minus `user_id` (which the reconcile ignores). So we can
    /// drive `apply_snapshot` from a real `collect_push_batch` — no network — and
    /// exercise the full reverse FK translation + LWW apply against live data.
    fn snapshot_from_batch(b: PushBatch) -> RemoteSnapshot {
        RemoteSnapshot {
            projects: b.projects,
            nodes: b.nodes,
            lineage_events: b.lineage_events,
            lineage_edges: b.lineage_edges,
            decomposition_edges: b.decomposition_edges,
            dependency_edges: b.dependency_edges,
            grouping_edges: b.grouping_edges,
            artifacts: b.artifacts,
        }
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn pull_reconcile_round_trips_a_full_graph() {
        use haven_core::{ArtifactRole, NewArtifact};

        // --- source store A: a small but complete graph ---
        let dir_a = tempfile::tempdir().unwrap();
        let db_a = dir_a.path().join("haven.db");
        let a = Store::open(&db_a, dir_a.path()).unwrap();
        a.add_project("haven", Some("HV"), "Haven", None).unwrap();
        a.use_project("haven").unwrap();

        let parent = a
            .add_item(
                None,
                NewItem {
                    title: "Parent".into(),
                    done_looks_like: Some("p95 < 200ms".into()),
                    why: Some("perf goal".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let child = a
            .add_item(
                None,
                NewItem {
                    title: "Child".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        a.decompose(None, &parent.reference, &child.reference, false)
            .unwrap();
        a.evolve_split(
            None,
            &child.reference,
            &["Split-of".into()],
            Some("why"),
            None,
        )
        .unwrap();
        a.add_artifact(
            None,
            &parent.reference,
            NewArtifact {
                role: ArtifactRole::Spec,
                content: Some("the spec".into()),
                name: Some("spec.md".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let conn_a = haven_core::db::open(&db_a).unwrap();
        let snap = snapshot_from_batch(collect_push_batch(&conn_a).unwrap());

        // --- target store B: empty; reconcile the snapshot into it ---
        let dir_b = tempfile::tempdir().unwrap();
        let db_b = dir_b.path().join("haven.db");
        let _b = Store::open(&db_b, dir_b.path()).unwrap(); // creates the schema
        let conn_b = haven_core::db::open(&db_b).unwrap();

        let stats = apply_snapshot(&conn_b, &snap, dir_b.path()).unwrap();
        assert_eq!(stats.projects, 1);
        assert_eq!(stats.nodes, 3); // parent + child + split product
        assert_eq!(stats.lineage_events, 1);
        assert!(stats.lineage_edges >= 1);
        assert_eq!(stats.edges, 1); // the decomposition edge
        assert_eq!(stats.artifacts, 1);

        // The graph landed, FK-translated to B's own local ids.
        assert_eq!(count(&conn_b, "projects"), 1);
        assert_eq!(count(&conn_b, "nodes"), 3);
        assert_eq!(count(&conn_b, "decomposition_edges"), 1);
        assert_eq!(count(&conn_b, "lineage_events"), 1);
        assert_eq!(count(&conn_b, "artifacts"), 1);

        // Stable identity + the acceptance fields survived the round-trip.
        let (title, dll, why): (String, String, String) = conn_b
            .query_row(
                "SELECT title, done_looks_like, why FROM nodes WHERE public_id = ?1",
                [&parent.public_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "Parent");
        assert_eq!(dll, "p95 < 200ms");
        assert_eq!(why, "perf goal");

        // The decomposition edge resolves to B's local node ids (reverse FK).
        let (de_parent, de_child): (i64, i64) = conn_b
            .query_row(
                "SELECT parent_id, child_id FROM decomposition_edges",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let local_id = |pid: &str| -> i64 {
            conn_b
                .query_row("SELECT id FROM nodes WHERE public_id = ?1", [pid], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        assert_eq!(de_parent, local_id(&parent.public_id));
        assert_eq!(de_child, local_id(&child.public_id));

        // Pulled rows are marked synced, so they don't bounce back on the next push.
        assert_eq!(
            count(&conn_b, "nodes"),
            conn_b
                .query_row(
                    "SELECT count(*) FROM nodes WHERE sync_state='synced'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap()
        );

        // --- idempotency: re-applying the same snapshot changes nothing ---
        let again = apply_snapshot(&conn_b, &snap, dir_b.path()).unwrap();
        assert_eq!(again.total(), 0);
        assert_eq!(count(&conn_b, "nodes"), 3);
        assert_eq!(
            count(&conn_b, "lineage_edges"),
            count(&conn_a, "lineage_edges")
        );
    }

    #[test]
    fn pull_reconcile_is_last_write_wins_by_revision() {
        let dir_a = tempfile::tempdir().unwrap();
        let db_a = dir_a.path().join("haven.db");
        let a = Store::open(&db_a, dir_a.path()).unwrap();
        a.add_project("haven", Some("HV"), "Haven", None).unwrap();
        a.use_project("haven").unwrap();
        let item = a
            .add_item(
                None,
                NewItem {
                    title: "Original".into(),
                    ..Default::default()
                },
            )
            .unwrap();

        let conn_a = haven_core::db::open(&db_a).unwrap();
        let mut snap = snapshot_from_batch(collect_push_batch(&conn_a).unwrap());

        let dir_b = tempfile::tempdir().unwrap();
        let db_b = dir_b.path().join("haven.db");
        let _b = Store::open(&db_b, dir_b.path()).unwrap();
        let conn_b = haven_core::db::open(&db_b).unwrap();
        apply_snapshot(&conn_b, &snap, dir_b.path()).unwrap();

        let node = snap
            .nodes
            .iter_mut()
            .find(|v| v["public_id"] == json!(item.public_id))
            .unwrap();
        let base_rev = node["revision"].as_i64().unwrap();

        // A higher remote revision wins (LWW applies).
        node["title"] = json!("Remote wins");
        node["revision"] = json!(base_rev + 1);
        let s = apply_snapshot(&conn_b, &snap, dir_b.path()).unwrap();
        assert_eq!(s.nodes, 1);
        let title: String = conn_b
            .query_row(
                "SELECT title FROM nodes WHERE public_id=?1",
                [&item.public_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title, "Remote wins");

        // A stale (lower-or-equal revision) remote row is ignored — local stands.
        let node = snap
            .nodes
            .iter_mut()
            .find(|v| v["public_id"] == json!(item.public_id))
            .unwrap();
        node["title"] = json!("Stale loser");
        node["revision"] = json!(base_rev); // < the now-applied base_rev + 1
        let s = apply_snapshot(&conn_b, &snap, dir_b.path()).unwrap();
        assert_eq!(s.nodes, 0);
        let title: String = conn_b
            .query_row(
                "SELECT title FROM nodes WHERE public_id=?1",
                [&item.public_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title, "Remote wins");
    }

    /// A store with one project + one item carrying a `kind='file'` spec
    /// artifact, for content-upload tests. Returns (tempdir, store, conn, ref).
    fn store_with_file_artifact() -> (tempfile::TempDir, Store, Connection, String) {
        use haven_core::{ArtifactRole, NewArtifact};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("haven.db");
        let store = Store::open(&db, dir.path()).unwrap();
        store
            .add_project("haven", Some("HV"), "Haven", None)
            .unwrap();
        store.use_project("haven").unwrap();
        let item = store
            .add_item(
                None,
                NewItem {
                    title: "X".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .add_artifact(
                None,
                &item.reference,
                NewArtifact {
                    role: ArtifactRole::Spec,
                    content: Some("the spec".into()),
                    name: Some("spec.md".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let conn = haven_core::db::open(&db).unwrap();
        (dir, store, conn, item.reference)
    }

    #[test]
    fn upload_selection_is_by_hash_comparison() {
        let (dir, _store, conn, item_ref) = store_with_file_artifact();
        let root = dir.path();

        // Fresh artifact: never uploaded → needs upload, bytes match the file.
        let cands = collect_content_candidates(&conn).unwrap();
        assert_eq!(cands.len(), 1);
        let c = &cands[0];
        assert_eq!(c.project_key, "haven");
        assert_eq!(c.rel_path, format!("items/{item_ref}/spec.md"));
        let (bytes, hash) = needs_upload(c, root).expect("fresh artifact needs upload");
        assert_eq!(bytes, b"the spec");
        // The hash matches what haven-core recorded at add time.
        assert_eq!(c.content_hash.as_deref(), Some(hash.as_str()));

        // Simulate a successful upload.
        record_uploaded(&conn, &c.client_id, "test-user/haven/spec-key", &hash).unwrap();

        // Unchanged bytes + remote_path set → skip.
        let c = &collect_content_candidates(&conn).unwrap()[0];
        assert_eq!(c.remote_path.as_deref(), Some("test-user/haven/spec-key"));
        assert!(needs_upload(c, root).is_none());

        // Editing the file directly (no DB touch — the normal local workflow)
        // is detected by the hash compare → re-upload with the new hash.
        let file = root.join("haven").join(&c.rel_path);
        std::fs::write(&file, "edited directly").unwrap();
        let (bytes, new_hash) = needs_upload(c, root).expect("edited file needs re-upload");
        assert_eq!(bytes, b"edited directly");
        assert_ne!(Some(new_hash.as_str()), c.content_hash.as_deref());

        // Missing local file (e.g. a pulled, unhydrated row) → nothing to upload.
        std::fs::remove_file(&file).unwrap();
        assert!(needs_upload(c, root).is_none());
    }

    #[test]
    fn record_uploaded_bumps_revision_and_requeues_the_row() {
        let (_dir, _store, conn, _ref) = store_with_file_artifact();
        let c = &collect_content_candidates(&conn).unwrap()[0];

        // Pretend the row already pushed (synced), then a blob upload lands.
        mark_synced(&conn, "artifacts", std::slice::from_ref(&c.client_id)).unwrap();
        record_uploaded(&conn, &c.client_id, "u/haven/k", "newhash").unwrap();

        let (remote, hash, rev, state): (String, String, i64, String) = conn
            .query_row(
                "SELECT remote_path, content_hash, revision, sync_state
                 FROM artifacts WHERE client_id = ?1",
                [&c.client_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(remote, "u/haven/k");
        assert_eq!(hash, "newhash");
        assert_eq!(rev, 2); // bumped so other devices accept it via LWW
        assert_eq!(state, "local"); // re-queued: the row push carries the new fields
    }

    #[test]
    fn write_hydrated_caches_verifies_and_guards() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let bytes = b"downloaded content";
        let good_hash = sha256_hex(bytes);

        // Writes the file, creating intermediate dirs.
        let path =
            write_hydrated(root, "haven", "items/HV-1/spec.md", bytes, Some(&good_hash)).unwrap();
        assert_eq!(path, root.join("haven/items/HV-1/spec.md"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        // A hash mismatch refuses to cache wrong content.
        let err = write_hydrated(
            root,
            "haven",
            "items/HV-1/other.md",
            bytes,
            Some("deadbeef"),
        )
        .unwrap_err();
        assert!(matches!(err, SyncError::Permanent(_)));
        assert!(!root.join("haven/items/HV-1/other.md").exists());

        // Path traversal in a remote-supplied rel_path is rejected.
        let err = write_hydrated(root, "haven", "items/../../etc/pwned", bytes, None).unwrap_err();
        assert!(matches!(err, SyncError::Permanent(_)));
    }

    #[test]
    fn pull_invalidates_stale_cache_but_keeps_local_edits() {
        let (dir, _store, conn, item_ref) = store_with_file_artifact();
        let root = dir.path();
        let file = root.join("haven").join(format!("items/{item_ref}/spec.md"));

        // Build a remote artifact row that's one revision ahead with NEW content
        // (as another device's upload + push would produce).
        let batch = collect_push_batch(&conn).unwrap();
        let mut snap = snapshot_from_batch(batch);
        let art = &mut snap.artifacts[0];
        let rev = art["revision"].as_i64().unwrap();
        art["revision"] = json!(rev + 1);
        art["content_hash"] = json!(sha256_hex(b"new content from device A"));
        art["remote_path"] = json!("test-user/haven/spec-key");

        // Case 1: the local file is the unedited original ("the spec") — a clean
        // stale cache. Applying the newer row removes it so the next read
        // re-hydrates.
        assert!(file.exists());
        let stats = apply_snapshot(&conn, &snap, root).unwrap();
        assert_eq!(stats.artifacts, 1);
        assert!(!file.exists(), "stale cache should be invalidated");

        // Case 2: the file was edited locally — it must be preserved. Bump the
        // remote row again with yet another hash.
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "local edits in progress").unwrap();
        let art = &mut snap.artifacts[0];
        art["revision"] = json!(rev + 2);
        art["content_hash"] = json!(sha256_hex(b"even newer remote content"));
        let stats = apply_snapshot(&conn, &snap, root).unwrap();
        assert_eq!(stats.artifacts, 1);
        assert!(file.exists(), "locally-edited file must be kept");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "local edits in progress"
        );
    }
}
