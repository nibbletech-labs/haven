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
        let (project_id, _) = self.require_project(project)?;
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
        let (project_id, _) = self.require_project(project)?;
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
        let (project_id, _) = self.require_project(project)?;
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
