//! Local read side of sync: collect rows not yet `synced`, translating local
//! integer foreign keys into the stable `public_id` UUIDs the remote uses
//! (SPEC §5). The reverse translation happens on pull (insert unknown nodes
//! first so FKs resolve) — sketched in [`crate::engine`].
//!
//! This translation is the trickiest part of push, so it is unit-tested against
//! a real local database even though the HTTP transport is not.

use rusqlite::{Connection, Row};
use serde_json::{json, Value};

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
}
