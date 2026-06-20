//! The three structural edge layers (decomposition, dependency, grouping) —
//! add/remove with DAG cycle guards — plus `load_edges` for `--include edges`.
//! Lineage (the fourth, append-only layer) lives in `evolve.rs`.

use rusqlite::{params, Connection};

use crate::error::{HavenError, Result};
use crate::model::*;

use super::{new_uuid, Store};

/// Which structural edge layer an `add_edge`/`remove_edge` call targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Decomposition,
    Dependency,
    Grouping,
}

impl EdgeKind {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "decomposition" => Ok(EdgeKind::Decomposition),
            "dependency" => Ok(EdgeKind::Dependency),
            "grouping" => Ok(EdgeKind::Grouping),
            other => Err(HavenError::Invalid(format!("unknown edge kind {other:?}"))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Decomposition => "decomposition",
            EdgeKind::Dependency => "dependency",
            EdgeKind::Grouping => "grouping",
        }
    }
}

impl serde::Serialize for EdgeKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl Store {
    // ---- low-level inserts (run inside the caller's transaction) ----------

    pub(crate) fn insert_decomposition(
        &self,
        conn: &Connection,
        parent_id: i64,
        child_id: i64,
    ) -> Result<()> {
        if parent_id == child_id {
            return Err(HavenError::GraphRule(
                "a node cannot decompose into itself".into(),
            ));
        }
        if self.edge_exists(
            conn,
            "decomposition_edges",
            "parent_id",
            "child_id",
            parent_id,
            child_id,
        )? {
            return Ok(()); // idempotent
        }
        // Cycle guard: adding parent→child must not let child already reach
        // parent through decomposition.
        if self.reaches(
            conn,
            "decomposition_edges",
            "parent_id",
            "child_id",
            child_id,
            parent_id,
        )? {
            return Err(HavenError::GraphRule(
                "decomposition edge would create a cycle".into(),
            ));
        }
        conn.execute(
            "INSERT INTO decomposition_edges (parent_id, child_id, client_id) VALUES (?1, ?2, ?3)",
            params![parent_id, child_id, new_uuid()],
        )?;
        Ok(())
    }

    pub(crate) fn insert_dependency(
        &self,
        conn: &Connection,
        node_id: i64,
        depends_on_id: i64,
    ) -> Result<()> {
        if node_id == depends_on_id {
            return Err(HavenError::GraphRule(
                "a node cannot depend on itself".into(),
            ));
        }
        if self.edge_exists(
            conn,
            "dependency_edges",
            "node_id",
            "depends_on_id",
            node_id,
            depends_on_id,
        )? {
            return Ok(());
        }
        // Cycle guard: the prerequisite must not (transitively) depend on the
        // node being blocked.
        if self.reaches(
            conn,
            "dependency_edges",
            "node_id",
            "depends_on_id",
            depends_on_id,
            node_id,
        )? {
            return Err(HavenError::GraphRule(
                "dependency edge would create a cycle".into(),
            ));
        }
        conn.execute(
            "INSERT INTO dependency_edges (node_id, depends_on_id, client_id) VALUES (?1, ?2, ?3)",
            params![node_id, depends_on_id, new_uuid()],
        )?;
        Ok(())
    }

    pub(crate) fn insert_grouping(
        &self,
        conn: &Connection,
        group_id: i64,
        member_id: i64,
    ) -> Result<()> {
        if group_id == member_id {
            return Err(HavenError::GraphRule("a node cannot group itself".into()));
        }
        // The group target must be a container node (SPEC §1).
        let group_type: NodeType =
            conn.query_row("SELECT type FROM nodes WHERE id = ?1", [group_id], |r| {
                r.get(0)
            })?;
        if !matches!(
            group_type,
            NodeType::Release | NodeType::Phase | NodeType::Gate
        ) {
            return Err(HavenError::GraphRule(format!(
                "group target must be a release/phase/gate node, not {group_type} \
                 — the container is the group side (the `from` of a grouping edge), \
                 the member the other"
            )));
        }
        if self.edge_exists(
            conn,
            "grouping_edges",
            "group_id",
            "member_id",
            group_id,
            member_id,
        )? {
            return Ok(());
        }
        conn.execute(
            "INSERT INTO grouping_edges (group_id, member_id, client_id) VALUES (?1, ?2, ?3)",
            params![group_id, member_id, new_uuid()],
        )?;
        Ok(())
    }

    // ---- public edge ops --------------------------------------------------

    pub fn decompose(
        &self,
        project: Option<&str>,
        parent: &str,
        child: &str,
        remove: bool,
    ) -> Result<()> {
        let (project_id, _) = self.require_project(project)?;
        let parent_id = self.resolve_node_id(project_id, parent)?;
        let child_id = self.resolve_node_id(project_id, child)?;
        if remove {
            self.conn.execute(
                "DELETE FROM decomposition_edges WHERE parent_id = ?1 AND child_id = ?2",
                params![parent_id, child_id],
            )?;
            Ok(())
        } else {
            self.insert_decomposition(&self.conn, parent_id, child_id)
        }
    }

    pub fn depend(
        &self,
        project: Option<&str>,
        node: &str,
        depends_on: &str,
        remove: bool,
    ) -> Result<()> {
        let (project_id, _) = self.require_project(project)?;
        let node_id = self.resolve_node_id(project_id, node)?;
        let dep_id = self.resolve_node_id(project_id, depends_on)?;
        if remove {
            self.conn.execute(
                "DELETE FROM dependency_edges WHERE node_id = ?1 AND depends_on_id = ?2",
                params![node_id, dep_id],
            )?;
            Ok(())
        } else {
            self.insert_dependency(&self.conn, node_id, dep_id)
        }
    }

    pub fn group(
        &self,
        project: Option<&str>,
        group: &str,
        member: &str,
        remove: bool,
    ) -> Result<()> {
        let (project_id, _) = self.require_project(project)?;
        let group_id = self.resolve_node_id(project_id, group)?;
        let member_id = self.resolve_node_id(project_id, member)?;
        if remove {
            self.conn.execute(
                "DELETE FROM grouping_edges WHERE group_id = ?1 AND member_id = ?2",
                params![group_id, member_id],
            )?;
            Ok(())
        } else {
            self.insert_grouping(&self.conn, group_id, member_id)
        }
    }

    /// Generic dispatcher backing the MCP `haven_add_edge` tool. `from`/`to`
    /// map to (parent,child) / (node,depends_on) / (group,member).
    pub fn add_edge(
        &self,
        project: Option<&str>,
        kind: EdgeKind,
        from: &str,
        to: &str,
        remove: bool,
    ) -> Result<()> {
        match kind {
            EdgeKind::Decomposition => self.decompose(project, from, to, remove),
            EdgeKind::Dependency => self.depend(project, from, to, remove),
            EdgeKind::Grouping => self.group(project, from, to, remove),
        }
    }

    // ---- edge loading -----------------------------------------------------

    pub(crate) fn load_edges(&self, node_id: i64) -> Result<Edges> {
        Ok(Edges {
            parents: self.refs_for(
                "SELECT n.ref FROM decomposition_edges e JOIN nodes n ON n.id = e.parent_id WHERE e.child_id = ?1 ORDER BY n.ref",
                node_id,
            )?,
            children: self.refs_for(
                "SELECT n.ref FROM decomposition_edges e JOIN nodes n ON n.id = e.child_id WHERE e.parent_id = ?1 ORDER BY n.ref",
                node_id,
            )?,
            depends_on: self.refs_for(
                "SELECT n.ref FROM dependency_edges e JOIN nodes n ON n.id = e.depends_on_id WHERE e.node_id = ?1 ORDER BY n.ref",
                node_id,
            )?,
            blocks: self.refs_for(
                "SELECT n.ref FROM dependency_edges e JOIN nodes n ON n.id = e.node_id WHERE e.depends_on_id = ?1 ORDER BY n.ref",
                node_id,
            )?,
            groups: self.refs_for(
                "SELECT n.ref FROM grouping_edges e JOIN nodes n ON n.id = e.group_id WHERE e.member_id = ?1 ORDER BY n.ref",
                node_id,
            )?,
            members: self.refs_for(
                "SELECT n.ref FROM grouping_edges e JOIN nodes n ON n.id = e.member_id WHERE e.group_id = ?1 ORDER BY n.ref",
                node_id,
            )?,
        })
    }

    fn refs_for(&self, sql: &str, node_id: i64) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([node_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ---- graph helpers ----------------------------------------------------

    fn edge_exists(
        &self,
        conn: &Connection,
        table: &str,
        a: &str,
        b: &str,
        av: i64,
        bv: i64,
    ) -> Result<bool> {
        let sql = format!("SELECT 1 FROM {table} WHERE {a} = ?1 AND {b} = ?2 LIMIT 1");
        let found: Option<i64> = conn
            .query_row(&sql, params![av, bv], |r| r.get(0))
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(found.is_some())
    }

    /// Does `start` reach `goal` by following `from_col → to_col` edges?
    /// Used for cycle detection before inserting a new edge.
    fn reaches(
        &self,
        conn: &Connection,
        table: &str,
        from_col: &str,
        to_col: &str,
        start: i64,
        goal: i64,
    ) -> Result<bool> {
        let sql = format!(
            "WITH RECURSIVE reach(id) AS (
                 SELECT ?1
                 UNION
                 SELECT e.{to_col} FROM {table} e JOIN reach r ON e.{from_col} = r.id
             )
             SELECT 1 FROM reach WHERE id = ?2 LIMIT 1"
        );
        let found: Option<i64> = conn
            .query_row(&sql, params![start, goal], |r| r.get(0))
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(found.is_some())
    }
}
