//! Read queries: the `next` dispatch query, FTS search, the lineage resolver
//! (old ref → live descendant), per-node lineage hydration, and graph
//! traversal. Also the aggregate counts behind `haven status`.

use rusqlite::{params, Row};
use serde::Serialize;

use crate::error::Result;
use crate::model::*;

use super::{item_from_row, Store, ITEM_FROM, ITEM_SELECT};

/// Direction for `evolve graph`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageDirection {
    Ancestors,
    Descendants,
    Both,
}

impl LineageDirection {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "ancestors" => Ok(LineageDirection::Ancestors),
            "descendants" => Ok(LineageDirection::Descendants),
            "both" => Ok(LineageDirection::Both),
            other => Err(crate::error::HavenError::Invalid(format!(
                "unknown direction {other:?} (ancestors|descendants|both)"
            ))),
        }
    }
}

/// A lineage subgraph rooted at one node (returned by `evolve graph`).
#[derive(Debug, Clone, Serialize)]
pub struct LineageGraph {
    pub root: String,
    pub events: Vec<LineageEvent>,
}

const ORDER: &str =
    " ORDER BY n.priority IS NULL, n.priority, n.sort_key IS NULL, n.sort_key, n.created_at, n.id";

impl Store {
    /// `haven next`: committed, ready, not waiting, with no open dependency.
    /// Highest priority band first, then `sort_key` (SPEC §1).
    pub fn next(
        &self,
        project: Option<&str>,
        owner: Option<OwnerKind>,
        limit: Option<i64>,
    ) -> Result<Vec<Item>> {
        let (project_id, _) = self.require_project(project)?;
        let mut sql = format!(
            "SELECT {ITEM_SELECT} FROM {ITEM_FROM}
             WHERE n.project_id = ?1 AND n.committed = 1 AND n.status = 'ready'
               AND n.wait_state IS NULL
               AND NOT EXISTS (
                 SELECT 1 FROM dependency_edges d
                 JOIN nodes p ON p.id = d.depends_on_id
                 WHERE d.node_id = n.id AND p.status NOT IN ('done','superseded','archived')
               )"
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project_id)];
        if let Some(owner) = owner {
            sql.push_str(&format!(" AND n.owner_kind = ?{}", args.len() + 1));
            args.push(Box::new(owner.as_str()));
        }
        sql.push_str(ORDER);
        if let Some(limit) = limit {
            sql.push_str(&format!(" LIMIT ?{}", args.len() + 1));
            args.push(Box::new(limit));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
            item_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// FTS5 search over node title/body, ranked by relevance.
    pub fn search(
        &self,
        project: Option<&str>,
        query: &str,
        limit: Option<i64>,
    ) -> Result<Vec<Item>> {
        let (project_id, _) = self.require_project(project)?;
        let limit = limit.unwrap_or(20);
        // FTS5's MATCH operator must reference the virtual table by its real
        // name, not an alias — so `node_fts` is left unaliased here.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ITEM_SELECT} FROM node_fts
             JOIN nodes n ON n.id = node_fts.rowid
             JOIN projects p ON p.id = n.project_id
             WHERE node_fts MATCH ?1 AND n.project_id = ?2
             ORDER BY rank LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![query, project_id, limit], item_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Resolve a (possibly superseded/archived) ref forward through lineage to
    /// its live descendant(s). A live node resolves to itself (SPEC §1).
    pub fn resolve_live(&self, project: Option<&str>, selector: &str) -> Result<Vec<Item>> {
        let (project_id, _) = self.require_project(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        let mut stmt = self.conn.prepare(&format!(
            "WITH RECURSIVE descendants(id) AS (
                 SELECT ?1
                 UNION
                 SELECT le.to_node_id FROM lineage_edges le
                 JOIN descendants d ON le.from_node_id = d.id
             )
             SELECT {ITEM_SELECT} FROM {ITEM_FROM}
             WHERE n.id IN (SELECT id FROM descendants)
               AND n.status NOT IN ('superseded','archived'){ORDER}"
        ))?;
        let rows = stmt.query_map([node_id], item_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Lineage events touching a node (either endpoint), oldest first.
    pub(crate) fn lineage_events_for_node(&self, node_id: i64) -> Result<Vec<LineageEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT e.id, e.public_id, e.event_type, e.rationale, e.triggered_by, e.created_at
             FROM lineage_events e
             JOIN lineage_edges le ON le.event_id = e.id
             WHERE le.from_node_id = ?1 OR le.to_node_id = ?1
             ORDER BY e.id",
        )?;
        let rows = stmt
            .query_map([node_id], |r| Ok((r.get::<_, i64>(0)?, event_header(r)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, hdr)| self.hydrate_event(id, hdr))
            .collect()
    }

    fn hydrate_event(&self, event_id: i64, mut ev: LineageEvent) -> Result<LineageEvent> {
        ev.from = self.event_endpoint_refs(event_id, "from_node_id")?;
        ev.to = self.event_endpoint_refs(event_id, "to_node_id")?;
        Ok(ev)
    }

    fn event_endpoint_refs(&self, event_id: i64, col: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT DISTINCT n.ref FROM lineage_edges le
             JOIN nodes n ON n.id = le.{col}
             WHERE le.event_id = ?1 ORDER BY n.ref"
        ))?;
        let rows = stmt.query_map([event_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// `evolve graph`: lineage events within `depth` hops of `selector` in the
    /// given direction.
    pub fn evolve_graph(
        &self,
        project: Option<&str>,
        selector: &str,
        direction: LineageDirection,
        depth: Option<i64>,
    ) -> Result<LineageGraph> {
        let (project_id, _) = self.require_project(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        let root = self.node_ref(node_id)?;
        let depth = depth.unwrap_or(50);

        // Reachable node set in the requested direction(s).
        let mut ids: Vec<i64> = vec![node_id];
        if matches!(
            direction,
            LineageDirection::Ancestors | LineageDirection::Both
        ) {
            ids.extend(self.reachable(node_id, "to_node_id", "from_node_id", depth)?);
        }
        if matches!(
            direction,
            LineageDirection::Descendants | LineageDirection::Both
        ) {
            ids.extend(self.reachable(node_id, "from_node_id", "to_node_id", depth)?);
        }
        ids.sort_unstable();
        ids.dedup();

        // Events with at least one endpoint in the reachable set.
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT DISTINCT e.id, e.public_id, e.event_type, e.rationale, e.triggered_by, e.created_at
             FROM lineage_events e
             JOIN lineage_edges le ON le.event_id = e.id
             WHERE le.from_node_id IN ({placeholders}) OR le.to_node_id IN ({placeholders})
             ORDER BY e.id"
        ))?;
        let dup: Vec<i64> = ids.iter().chain(ids.iter()).copied().collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(dup), |r| {
                Ok((r.get::<_, i64>(0)?, event_header(r)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let events = rows
            .into_iter()
            .map(|(id, hdr)| self.hydrate_event(id, hdr))
            .collect::<Result<Vec<_>>>()?;
        Ok(LineageGraph { root, events })
    }

    fn reachable(&self, start: i64, from_col: &str, to_col: &str, depth: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(&format!(
            "WITH RECURSIVE walk(id, d) AS (
                 SELECT ?1, 0
                 UNION
                 SELECT le.{to_col}, walk.d + 1 FROM lineage_edges le
                 JOIN walk ON le.{from_col} = walk.id
                 WHERE walk.d < ?2
             )
             SELECT DISTINCT id FROM walk WHERE id <> ?1"
        ))?;
        let rows = stmt.query_map(params![start, depth], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Aggregate counts for `haven status`.
    pub fn store_status(&self, project: Option<&str>) -> Result<serde_json::Value> {
        let (project_id, key) = self.require_project(project)?;
        let total: i64 = self.conn.query_row(
            "SELECT count(*) FROM nodes WHERE project_id = ?1",
            [project_id],
            |r| r.get(0),
        )?;
        let committed: i64 = self.conn.query_row(
            "SELECT count(*) FROM nodes WHERE project_id = ?1 AND committed = 1",
            [project_id],
            |r| r.get(0),
        )?;
        let icebox: i64 = self.conn.query_row(
            "SELECT count(*) FROM nodes WHERE project_id = ?1 AND committed = 0
                 AND status NOT IN ('archived','superseded')",
            [project_id],
            |r| r.get(0),
        )?;

        let mut by_status = serde_json::Map::new();
        let mut stmt = self.conn.prepare(
            "SELECT status, count(*) FROM nodes WHERE project_id = ?1 GROUP BY status ORDER BY status",
        )?;
        let rows = stmt.query_map([project_id], |r: &Row<'_>| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (status, count) = row?;
            by_status.insert(status, serde_json::json!(count));
        }

        // Sync queue depth: any row across mutable tables not yet 'synced'.
        let sync_pending = self.sync_pending_count()?;

        Ok(serde_json::json!({
            "project": key,
            "total": total,
            "committed": committed,
            "icebox": icebox,
            "by_status": by_status,
            "sync_pending": sync_pending,
        }))
    }

    fn sync_pending_count(&self) -> Result<i64> {
        let tables = [
            "projects",
            "nodes",
            "decomposition_edges",
            "dependency_edges",
            "grouping_edges",
            "lineage_events",
            "artifacts",
        ];
        let mut total = 0i64;
        for t in tables {
            let n: i64 = self.conn.query_row(
                &format!("SELECT count(*) FROM {t} WHERE sync_state <> 'synced'"),
                [],
                |r| r.get(0),
            )?;
            total += n;
        }
        Ok(total)
    }
}

/// The non-edge columns of a lineage event (edges filled in by `hydrate_event`).
fn event_header(r: &Row<'_>) -> rusqlite::Result<LineageEvent> {
    Ok(LineageEvent {
        public_id: r.get(1)?,
        event_type: r.get(2)?,
        rationale: r.get(3)?,
        triggered_by: r.get(4)?,
        created_at: r.get(5)?,
        from: Vec::new(),
        to: Vec::new(),
    })
}
