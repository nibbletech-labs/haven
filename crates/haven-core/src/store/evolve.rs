//! Evolution: split / merge / supersede. These create new nodes and append
//! immutable lineage events + edges (the append-only core, SPEC §3). The source
//! nodes are never deleted — they go to status `superseded` and lineage points
//! forward to their descendants, so an old `ref` always resolves.

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::error::{HavenError, Result};
use crate::model::*;

use super::{new_uuid, Store};

/// Result of an evolve op: the nodes created, the source `ref`s now superseded,
/// and the lineage event's `public_id`.
#[derive(Debug, Clone, Serialize)]
pub struct EvolveResult {
    pub new: Vec<Item>,
    pub superseded: Vec<String>,
    pub event_id: String,
}

impl Store {
    /// Append a lineage event with its from→to edges. Returns the event's
    /// `public_id`. Shared by the evolve ops and by archive/reopen.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_event(
        &self,
        conn: &Connection,
        project_id: i64,
        event_type: EventType,
        rationale: Option<&str>,
        triggered_by: Option<&str>,
        context: &serde_json::Value,
        edges: &[(i64, i64)],
    ) -> Result<String> {
        let public_id = new_uuid();
        conn.execute(
            "INSERT INTO lineage_events
               (public_id, project_id, event_type, rationale, triggered_by, context, client_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                public_id,
                project_id,
                event_type,
                rationale,
                triggered_by,
                context.to_string(),
                new_uuid(),
            ],
        )?;
        let event_id = conn.last_insert_rowid();
        for (from, to) in edges {
            conn.execute(
                "INSERT INTO lineage_edges (event_id, from_node_id, to_node_id) VALUES (?1, ?2, ?3)",
                params![event_id, from, to],
            )?;
        }
        Ok(public_id)
    }

    fn mark_superseded(&self, conn: &Connection, node_id: i64) -> Result<()> {
        conn.execute(
            "UPDATE nodes SET status = 'superseded', revision = revision + 1,
                 updated_at = datetime('now'), sync_state = 'local' WHERE id = ?1",
            [node_id],
        )?;
        Ok(())
    }

    /// The other endpoint of every `table` row whose `match_col` = `id`.
    fn edge_neighbors(
        &self,
        conn: &Connection,
        table: &str,
        match_col: &str,
        other_col: &str,
        id: i64,
    ) -> Result<Vec<i64>> {
        let sql = format!("SELECT {other_col} FROM {table} WHERE {match_col} = ?1");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([id], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// HV-126: re-point every structural edge (decomposition / dependency /
    /// grouping, both directions) from the superseded `sources` onto `survivor`,
    /// then delete the sources' now-dead edges. Without this, merging/superseding a
    /// node left the survivor orphaned (zero edges) and silently un-blocked the
    /// sources' dependents — the dispatch query treats a superseded prerequisite as
    /// satisfied (`status NOT IN done/superseded/archived`), so a dependent of a
    /// merged-away node loses its ordering the moment its prerequisite is retired.
    ///
    /// Reuses the cycle-guarded, idempotent `insert_*` helpers, so a re-pointed edge
    /// that already exists is a no-op and a re-pointed edge that would close a cycle
    /// is rejected (a genuinely cyclic merge fails loudly rather than corrupting the
    /// DAG). An edge between two sources — or between a source and the survivor —
    /// remaps to a self-loop and is skipped. Grouping members are re-homed only when
    /// the survivor is itself a container (a `Task` survivor — the merge case — can
    /// never be a grouping target).
    fn forward_structural_edges(
        &self,
        conn: &Connection,
        sources: &[i64],
        survivor: i64,
    ) -> Result<()> {
        let source_set: std::collections::HashSet<i64> = sources.iter().copied().collect();
        let remap = |id: i64| {
            if source_set.contains(&id) {
                survivor
            } else {
                id
            }
        };

        let survivor_type: NodeType =
            conn.query_row("SELECT type FROM nodes WHERE id = ?1", [survivor], |r| {
                r.get(0)
            })?;
        let survivor_is_container = matches!(
            survivor_type,
            NodeType::Release | NodeType::Phase | NodeType::Gate
        );

        // A non-container survivor cannot be a grouping target, so a source's
        // members have nowhere to be re-homed. The cleanup below deletes the
        // source's grouping edges either way — so rather than SILENTLY orphan those
        // members, reject the whole op up front (the transaction rolls back). The
        // merge survivor is always a Task, so this guards "merge a phase/release
        // that has members"; supersede --with a non-container hits it too. Members
        // that are themselves sources (they remap to the survivor) don't count.
        if !survivor_is_container {
            for &src in sources {
                for member in
                    self.edge_neighbors(conn, "grouping_edges", "group_id", "member_id", src)?
                {
                    if remap(member) != survivor {
                        return Err(HavenError::GraphRule(
                            "cannot merge/supersede a node that groups members into a \
                             non-container survivor — its members would be orphaned; regroup \
                             the members first, or supersede with a container survivor"
                                .into(),
                        ));
                    }
                }
            }
        }

        for &src in sources {
            // decomposition: inbound parents (src is a child) → parent of survivor.
            for parent in
                self.edge_neighbors(conn, "decomposition_edges", "child_id", "parent_id", src)?
            {
                let p = remap(parent);
                if p != survivor {
                    self.insert_decomposition(conn, p, survivor)?;
                }
            }
            // decomposition: outbound children (src is a parent) → children of survivor.
            for child in
                self.edge_neighbors(conn, "decomposition_edges", "parent_id", "child_id", src)?
            {
                let c = remap(child);
                if c != survivor {
                    self.insert_decomposition(conn, survivor, c)?;
                }
            }
            // dependency: inbound dependents (X depends on src) → X depends on survivor.
            for dependent in
                self.edge_neighbors(conn, "dependency_edges", "depends_on_id", "node_id", src)?
            {
                let d = remap(dependent);
                if d != survivor {
                    self.insert_dependency(conn, d, survivor)?;
                }
            }
            // dependency: outbound prereqs (src depends on Y) → survivor depends on Y.
            for prereq in
                self.edge_neighbors(conn, "dependency_edges", "node_id", "depends_on_id", src)?
            {
                let p = remap(prereq);
                if p != survivor {
                    self.insert_dependency(conn, survivor, p)?;
                }
            }
            // grouping: inbound groups (G groups src) → G groups survivor.
            for group in
                self.edge_neighbors(conn, "grouping_edges", "member_id", "group_id", src)?
            {
                let g = remap(group);
                if g != survivor {
                    self.insert_grouping(conn, g, survivor)?;
                }
            }
            // grouping: outbound members (src groups M) → survivor groups M, only if
            // the survivor is a container (else those members are left un-grouped —
            // their group was superseded and the survivor cannot be a group target).
            if survivor_is_container {
                for member in
                    self.edge_neighbors(conn, "grouping_edges", "group_id", "member_id", src)?
                {
                    let m = remap(member);
                    if m != survivor {
                        self.insert_grouping(conn, survivor, m)?;
                    }
                }
            }
        }

        // Drop the sources' now-dead structural edges — the survivor carries them.
        for &src in sources {
            conn.execute(
                "DELETE FROM decomposition_edges WHERE parent_id = ?1 OR child_id = ?1",
                [src],
            )?;
            conn.execute(
                "DELETE FROM dependency_edges WHERE node_id = ?1 OR depends_on_id = ?1",
                [src],
            )?;
            conn.execute(
                "DELETE FROM grouping_edges WHERE group_id = ?1 OR member_id = ?1",
                [src],
            )?;
        }
        Ok(())
    }

    /// Split one node into N new nodes (titles given). The source is superseded;
    /// lineage edges point source → each new node.
    pub fn evolve_split(
        &self,
        project: Option<&str>,
        selector: &str,
        into: &[String],
        rationale: Option<&str>,
        by: Option<&str>,
    ) -> Result<EvolveResult> {
        if into.is_empty() {
            return Err(HavenError::Invalid(
                "split requires at least one --into title".into(),
            ));
        }
        let (project_id, _) = self.require_project_mut(project)?;
        let source_id = self.resolve_node_id(project_id, selector)?;
        let source_ref = self.node_ref(source_id)?;

        let tx = self.conn.unchecked_transaction()?;
        let mut new_ids = Vec::new();
        let mut new_refs = Vec::new();
        for title in into {
            let (id, r) = self.insert_node(
                &tx,
                project_id,
                title,
                NodeType::Task,
                Status::Discovery,
                None, // body
                None, // done_looks_like
                None, // why
                None, // owner
                false,
                None,
                None, // due_at
                &serde_json::json!({}),
            )?;
            new_ids.push(id);
            new_refs.push(r);
        }
        let edges: Vec<(i64, i64)> = new_ids.iter().map(|&to| (source_id, to)).collect();
        let event_id = self.record_event(
            &tx,
            project_id,
            EventType::Split,
            rationale,
            by,
            &serde_json::json!({ "from": source_ref }),
            &edges,
        )?;
        // HV-129: the designated-primary child (first --into) inherits the source's
        // structural edges, so it is not orphaned and the source's dependents stay
        // blocked on it — same as merge/supersede (HV-126).
        self.forward_structural_edges(&tx, &[source_id], new_ids[0])?;
        self.mark_superseded(&tx, source_id)?;
        tx.commit()?;

        let new = new_refs
            .iter()
            .map(|r| self.get_item(project, r, &[]))
            .collect::<Result<Vec<_>>>()?;
        Ok(EvolveResult {
            new,
            superseded: vec![source_ref],
            event_id,
        })
    }

    /// Merge N nodes into one new node. Each source is superseded; lineage edges
    /// point each source → the new node.
    pub fn evolve_merge(
        &self,
        project: Option<&str>,
        selectors: &[String],
        title: &str,
        rationale: Option<&str>,
        by: Option<&str>,
    ) -> Result<EvolveResult> {
        if selectors.len() < 2 {
            return Err(HavenError::Invalid(
                "merge requires at least two source items".into(),
            ));
        }
        if title.trim().is_empty() {
            return Err(HavenError::Invalid("merge requires a --title".into()));
        }
        let (project_id, _) = self.require_project_mut(project)?;
        let mut source_ids = Vec::new();
        let mut source_refs = Vec::new();
        for sel in selectors {
            let id = self.resolve_node_id(project_id, sel)?;
            source_ids.push(id);
            source_refs.push(self.node_ref(id)?);
        }

        let tx = self.conn.unchecked_transaction()?;
        let (new_id, new_ref) = self.insert_node(
            &tx,
            project_id,
            title,
            NodeType::Task,
            Status::Discovery,
            None, // body
            None, // done_looks_like
            None, // why
            None, // owner
            false,
            None,
            None, // due_at
            &serde_json::json!({}),
        )?;
        let edges: Vec<(i64, i64)> = source_ids.iter().map(|&from| (from, new_id)).collect();
        let event_id = self.record_event(
            &tx,
            project_id,
            EventType::Merge,
            rationale,
            by,
            &serde_json::json!({ "into": new_ref }),
            &edges,
        )?;
        // HV-126: the survivor inherits the sources' structural edges, so it is not
        // orphaned and the sources' dependents stay blocked on it.
        self.forward_structural_edges(&tx, &source_ids, new_id)?;
        for &id in &source_ids {
            self.mark_superseded(&tx, id)?;
        }
        tx.commit()?;

        let new = vec![self.get_item(project, &new_ref, &[])?];
        Ok(EvolveResult {
            new,
            superseded: source_refs,
            event_id,
        })
    }

    /// Supersede a node with an existing node. Source → status superseded;
    /// lineage edge source → replacement.
    pub fn evolve_supersede(
        &self,
        project: Option<&str>,
        selector: &str,
        with: &str,
        rationale: Option<&str>,
        by: Option<&str>,
    ) -> Result<EvolveResult> {
        let (project_id, _) = self.require_project_mut(project)?;
        let source_id = self.resolve_node_id(project_id, selector)?;
        let with_id = self.resolve_node_id(project_id, with)?;
        if source_id == with_id {
            return Err(HavenError::Invalid(
                "cannot supersede a node with itself".into(),
            ));
        }
        let source_ref = self.node_ref(source_id)?;
        let with_ref = self.node_ref(with_id)?;

        let tx = self.conn.unchecked_transaction()?;
        let event_id = self.record_event(
            &tx,
            project_id,
            EventType::Supersede,
            rationale,
            by,
            &serde_json::json!({ "from": source_ref, "with": with_ref }),
            &[(source_id, with_id)],
        )?;
        // HV-126: the replacement inherits the source's structural edges, so the
        // source's dependents stay blocked on the live replacement.
        self.forward_structural_edges(&tx, &[source_id], with_id)?;
        self.mark_superseded(&tx, source_id)?;
        tx.commit()?;

        let new = vec![self.get_item(project, &with_ref, &[])?];
        Ok(EvolveResult {
            new,
            superseded: vec![source_ref],
            event_id,
        })
    }
}
