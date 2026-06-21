//! Read queries: the `next` dispatch query, FTS search, the lineage resolver
//! (old ref → live descendant), per-node lineage hydration, and graph
//! traversal. Also the aggregate counts behind `haven status`.

use rusqlite::{params, Row};
use serde::Serialize;

use crate::error::Result;
use crate::model::*;

use super::{fts_user_query, item_from_row, EdgeKind, ItemFilter, Store, ITEM_FROM, ITEM_SELECT};

/// One structural edge in a [`ProjectGraph`], as a `{kind, from, to}` ref triple
/// — the same shape `add_edge` accepts, so a graph export round-trips.
#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    pub kind: EdgeKind,
    pub from: String,
    pub to: String,
}

/// A lineage link in a [`ProjectGraph`]: `{event, from, to}` per lineage edge.
#[derive(Debug, Clone, Serialize)]
pub struct LineageLink {
    pub event: String,
    pub from: String,
    pub to: String,
}

/// The whole project work-graph in one payload: every node plus a flat edge list
/// (and optionally lineage links). Returned by [`Store::project_graph`].
#[derive(Debug, Clone, Serialize)]
pub struct ProjectGraph {
    pub project: String,
    pub nodes: Vec<Item>,
    pub edges: Vec<GraphEdge>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<LineageLink>,
    /// A grooming nudge (HV-82) when untriaged/stale work has piled up — present
    /// only above threshold so lean reads stay lean. A planner reorienting via
    /// `haven graph` is then prompted to groom before planning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grooming_nudge: Option<String>,
}

impl ProjectGraph {
    /// Drop archived/superseded (dead) nodes and any structural edge or lineage
    /// link that would then dangle onto a removed node — the live-only view used
    /// by `haven graph` (default) to mirror `haven_graph`'s `all:false` (HV-53).
    /// Idempotent; cheap (one pass to build the live-ref set, one to filter).
    pub fn live_only(mut self) -> Self {
        use std::collections::HashSet;
        let keep: HashSet<String> = self
            .nodes
            .iter()
            .filter(|n| !matches!(n.status, Status::Superseded | Status::Archived))
            .map(|n| n.reference.clone())
            .collect();
        self.nodes
            .retain(|n| !matches!(n.status, Status::Superseded | Status::Archived));
        self.edges
            .retain(|e| keep.contains(&e.from) && keep.contains(&e.to));
        self.lineage
            .retain(|l| keep.contains(&l.from) && keep.contains(&l.to));
        self
    }
}

/// Grooming pressure for a project (HV-82): counts of untriaged + stale work and
/// a ready-made `nudge` emitted once either crosses [`GROOMING_NUDGE_THRESHOLD`].
#[derive(Debug, Clone, Serialize)]
pub struct GroomingPressure {
    pub untriaged: usize,
    pub stale: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nudge: Option<String>,
}

/// At or above this many untriaged floaters OR stale items, grooming is nudged.
pub const GROOMING_NUDGE_THRESHOLD: usize = 5;
/// Items untouched for this many days count as stale for grooming pressure.
const GROOMING_STALE_DAYS: i64 = 14;

/// Classify a container from the statuses of its LIVE committed descendants.
/// Total over the input: empty → Dormant; any `in_progress` → Active; all `done`
/// → Done; otherwise Queued. The caller supplies only committed, non-dead statuses.
pub(crate) fn rollup_from_statuses(statuses: &[Status]) -> RollupState {
    if statuses.is_empty() {
        return RollupState::Dormant;
    }
    if statuses.contains(&Status::InProgress) {
        return RollupState::Active;
    }
    if statuses.iter().all(|s| *s == Status::Done) {
        return RollupState::Done;
    }
    RollupState::Queued
}

/// One living-doc anchor and the artifacts attached to it.
#[derive(Debug, Clone, Serialize)]
pub struct DocAnchor {
    #[serde(flatten)]
    pub item: Item,
    pub artifacts: Vec<Artifact>,
}

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

/// The `haven next` dispatch filter: committed, ready, not waiting, with no open
/// dependency (SPEC §1). Shared verbatim by `next` (which returns the items) and
/// `next_explain`'s `count_dispatchable` (which counts them) so the diagnostic
/// can never disagree with the real queue. References only the `nodes` alias `n`.
const DISPATCHABLE_PREDICATE: &str = "n.committed = 1 AND n.status = 'ready'
             AND n.type <> 'anchor'
             AND n.wait_state IS NULL
             AND NOT EXISTS (
               SELECT 1 FROM dependency_edges d
               JOIN nodes p ON p.id = d.depends_on_id
               WHERE d.node_id = n.id AND p.status NOT IN ('done','superseded','archived')
             )";

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
             WHERE n.project_id = ?1 AND {DISPATCHABLE_PREDICATE}"
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project_id)];
        if let Some(owner) = owner {
            // HV-125: `next --owner` filters the *assignment* axis (`owner_kind`).
            // An unassigned (NULL `owner_kind`) row yields NULL under three-valued
            // logic ⇒ excluded, so it is never auto-pulled by a `--owner` query.
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

    /// Explain the local dispatch queue: useful when `next` is empty.
    pub fn next_explain(
        &self,
        project: Option<&str>,
        owner: Option<OwnerKind>,
    ) -> Result<serde_json::Value> {
        let (project_id, key) = self.require_project(project)?;
        let dispatchable = self.count_dispatchable(project_id, owner)?;
        let dispatchable_any_owner = self.count_dispatchable(project_id, None)?;
        let owner_mismatch = owner
            .map(|_| dispatchable_any_owner.saturating_sub(dispatchable))
            .unwrap_or(0);
        let waiting = self.count_next_where(
            project_id,
            owner,
            "n.type <> 'anchor' AND n.committed = 1 AND n.status = 'ready' AND n.wait_state IS NOT NULL",
        )?;
        let blocked_by_dependency = self.count_next_where(
            project_id,
            owner,
            "n.type <> 'anchor' AND n.committed = 1 AND n.status = 'ready' AND n.wait_state IS NULL
             AND EXISTS (
               SELECT 1 FROM dependency_edges d
               JOIN nodes p ON p.id = d.depends_on_id
               WHERE d.node_id = n.id AND p.status NOT IN ('done','superseded','archived')
             )",
        )?;
        let committed_not_ready = self.count_next_where(
            project_id,
            owner,
            "n.type <> 'anchor' AND n.committed = 1 AND n.status NOT IN ('ready','done','superseded','archived')",
        )?;
        let ready_but_uncommitted = self.count_next_where(
            project_id,
            owner,
            "n.type <> 'anchor' AND n.committed = 0 AND n.status = 'ready'",
        )?;

        let hint = if dispatchable > 0 {
            "queue has dispatchable items"
        } else if owner_mismatch > 0 {
            "ready items exist, but not for the requested owner"
        } else if blocked_by_dependency > 0 {
            "ready items are blocked by open dependencies"
        } else if waiting > 0 {
            "ready items are waiting on a human, dependency, or external event"
        } else if committed_not_ready > 0 {
            "committed items exist but are not ready yet"
        } else if ready_but_uncommitted > 0 {
            "ready items exist but are still uncommitted"
        } else {
            "no ready committed work found"
        };

        Ok(serde_json::json!({
            "project": key,
            "owner": owner.map(|o| o.as_str()),
            "dispatchable": dispatchable,
            "hint": hint,
            "counts": {
                "owner_mismatch": owner_mismatch,
                "blocked_by_dependency": blocked_by_dependency,
                "waiting": waiting,
                "committed_not_ready": committed_not_ready,
                "ready_but_uncommitted": ready_but_uncommitted,
            }
        }))
    }

    fn count_dispatchable(&self, project_id: i64, owner: Option<OwnerKind>) -> Result<i64> {
        self.count_next_where(project_id, owner, DISPATCHABLE_PREDICATE)
    }

    fn count_next_where(
        &self,
        project_id: i64,
        owner: Option<OwnerKind>,
        predicate: &str,
    ) -> Result<i64> {
        let mut sql =
            format!("SELECT count(*) FROM nodes n WHERE n.project_id = ?1 AND {predicate}");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project_id)];
        if let Some(owner) = owner {
            // HV-125: keep lockstep with `next()` — count the same `owner_kind`
            // (assignment) frontier. `count_dispatchable` (and every diagnostic
            // counter) routes through here, so `next --explain` agrees with the
            // real queue per owner, unassigned rows excluded by three-valued logic.
            sql.push_str(&format!(" AND n.owner_kind = ?{}", args.len() + 1));
            args.push(Box::new(owner.as_str()));
        }
        Ok(self.conn.query_row(
            &sql,
            rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
            |r| r.get(0),
        )?)
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
        // HV-30: sanitize the raw user query before it reaches FTS5's MATCH —
        // otherwise `-`, `:`, `"`, parens, and bareword AND/OR/NOT/NEAR are read
        // as FTS5 syntax (e.g. `HV-22` hits column-filter syntax → `no such
        // column: 22`). `fts_user_query` quotes each alphanumeric token; `None`
        // means an all-punctuation query with nothing to match, so return empty
        // rather than running MATCH on it.
        let Some(match_query) = fts_user_query(query) else {
            return Ok(vec![]);
        };
        // FTS5's MATCH operator must reference the virtual table by its real
        // name, not an alias — so `node_fts` is left unaliased here.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ITEM_SELECT} FROM node_fts
             JOIN nodes n ON n.id = node_fts.rowid
             JOIN projects p ON p.id = n.project_id
             WHERE node_fts MATCH ?1 AND n.project_id = ?2
             ORDER BY rank LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![match_query, project_id, limit], item_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The live descendant(s) of `node_id`, walking *lineage* forward (the
    /// supersede/split/merge graph — distinct from the structural
    /// [`Self::live_descendants`] that walks decomposition/grouping). A live node
    /// resolves to itself (SPEC §1); a superseded/archived node resolves to its
    /// live successor(s), or to nothing when there is no live successor.
    ///
    /// HV-154: this is the lineage walk that used to be the body of the public
    /// `resolve_live`. It now lives in the read path and runs automatically on
    /// every item resolution (see [`Store::resolve_node_id_hinted`]); the public
    /// `resolve_live` is a one-release deprecated alias over it.
    pub(crate) fn live_lineage_descendants(&self, node_id: i64) -> Result<Vec<Item>> {
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

    /// Resolve a (possibly superseded/archived) ref forward through lineage to
    /// its live descendant(s). A live node resolves to itself (SPEC §1).
    ///
    /// **Deprecated (HV-154):** the lineage walk now runs automatically in the
    /// read path — `get_item`/`update_item`/`add_edge` ride a `stale_ref` hint
    /// when the ref is dead. This public entry point is kept for one release as a
    /// thin alias over [`Store::live_descendants`]; prefer the automatic hint.
    #[deprecated(
        since = "0.2.0",
        note = "the lineage walk now runs automatically in the read path; \
                callers see a stale_ref hint on get_item/update_item/add_edge"
    )]
    pub fn resolve_live(&self, project: Option<&str>, selector: &str) -> Result<Vec<Item>> {
        let (project_id, _) = self.require_project(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        self.live_lineage_descendants(node_id)
    }

    /// The whole project work-graph in one read: every node, plus the structural
    /// edges as a flat `{kind, from, to}` list (the same shape `add_edge` takes,
    /// so it round-trips), and optionally the lineage links. This is the call an
    /// app uses to render the graph, or an agent uses to reason over the entire
    /// dependency structure at once (vs. N+1 per-node fetches). `nodes` is the
    /// faithful set — including `superseded`/`archived`; filter client-side.
    /// Grooming pressure (HV-82): untriaged inbox floaters + stale items, with a
    /// nudge string once either crosses [`GROOMING_NUDGE_THRESHOLD`]. Triggers
    /// grooming rather than waiting to be remembered — surfaced on
    /// [`Store::project_graph`] (where a planner reorients).
    pub fn grooming_pressure(&self, project: Option<&str>) -> Result<GroomingPressure> {
        let untriaged = self
            .list_items(
                project,
                &ItemFilter {
                    inbox: true,
                    ..Default::default()
                },
            )?
            .len();
        let stale = self
            .list_items(
                project,
                &ItemFilter {
                    stale_days: Some(GROOMING_STALE_DAYS),
                    // Preserve the pre-HV-53 grooming count (dead items included);
                    // the live-only default is a list/graph view concern, not this
                    // internal pressure signal.
                    include_dead: true,
                    ..Default::default()
                },
            )?
            .len();
        let nudge = (untriaged >= GROOMING_NUDGE_THRESHOLD || stale >= GROOMING_NUDGE_THRESHOLD)
            .then(|| {
                format!(
                    "{untriaged} untriaged floater(s), {stale} stale item(s) — groom (workflow 3) before planning"
                )
            });
        Ok(GroomingPressure {
            untriaged,
            stale,
            nudge,
        })
    }

    pub fn project_graph(&self, project: Option<&str>, lineage: bool) -> Result<ProjectGraph> {
        let (project_id, key) = self.require_project(project)?;
        // The whole-graph read always returns *all* nodes; the live-only view is a
        // surface concern (CLI `graph` / MCP `haven_graph` both filter to live and
        // drop dangling edges via `all`), so `project_graph` must not pre-filter
        // dead nodes here (HV-53).
        let mut nodes = self.list_items(
            project,
            &ItemFilter {
                include_dead: true,
                ..Default::default()
            },
        )?;
        let mut edges = Vec::new();
        edges.extend(self.edges_of_kind(
            project_id,
            EdgeKind::Decomposition,
            "SELECT pn.ref, cn.ref FROM decomposition_edges e
               JOIN nodes pn ON pn.id = e.parent_id
               JOIN nodes cn ON cn.id = e.child_id
             WHERE pn.project_id = ?1 ORDER BY pn.ref, cn.ref",
        )?);
        edges.extend(self.edges_of_kind(
            project_id,
            EdgeKind::Dependency,
            "SELECT nn.ref, dn.ref FROM dependency_edges e
               JOIN nodes nn ON nn.id = e.node_id
               JOIN nodes dn ON dn.id = e.depends_on_id
             WHERE nn.project_id = ?1 ORDER BY nn.ref, dn.ref",
        )?);
        edges.extend(self.edges_of_kind(
            project_id,
            EdgeKind::Grouping,
            "SELECT gn.ref, mn.ref FROM grouping_edges e
               JOIN nodes gn ON gn.id = e.group_id
               JOIN nodes mn ON mn.id = e.member_id
             WHERE gn.project_id = ?1 ORDER BY gn.ref, mn.ref",
        )?);
        let lineage = if lineage {
            self.project_lineage_links(project_id)?
        } else {
            Vec::new()
        };
        // Hydrate the derived rollup for every container, and the context-pack
        // pointer for every leaf — read-only projections so a whole-graph read
        // can triage which ready leaves carry a pack (HV-75).
        for node in &mut nodes {
            if node.node_type.is_container() {
                let (rollup, has_uncommitted) = self.container_rollup(node.id)?;
                node.rollup_state = Some(rollup);
                node.has_uncommitted_descendants = Some(has_uncommitted);
            } else {
                let (pack, clash) = self.context_pack_for_node(node.id)?;
                node.context_pack = pack;
                node.context_pack_clash = clash;
            }
        }
        let grooming_nudge = self.grooming_pressure(project)?.nudge;
        Ok(ProjectGraph {
            project: key,
            nodes,
            edges,
            lineage,
            grooming_nudge,
        })
    }

    /// A container's LIVE descendants as `(status, committed)` pairs, walking the
    /// union of decomposition (parent→child) and grouping (group→member) edges.
    /// The recursive set dedups ids, so a node reachable via both edge kinds is
    /// counted once; dead nodes (superseded/archived) are excluded. One walk feeds
    /// both derived signals (see [`Self::container_rollup`]) so they can never
    /// disagree about what "live descendant" means.
    pub(crate) fn live_descendants(&self, node_id: i64) -> Result<Vec<(Status, bool)>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE sub(id) AS (
                 SELECT ?1
                 UNION
                 SELECT e.child_id FROM decomposition_edges e JOIN sub ON e.parent_id = sub.id
                 UNION
                 SELECT e.member_id FROM grouping_edges e JOIN sub ON e.group_id = sub.id
             )
             SELECT n.status, n.committed FROM nodes n
             WHERE n.id IN (SELECT id FROM sub) AND n.id <> ?1
               AND n.status NOT IN ('superseded','archived')",
        )?;
        let rows = stmt.query_map([node_id], |r| {
            Ok((r.get::<_, Status>(0)?, r.get::<_, bool>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The derived container signals for one node: its [`RollupState`] (classified
    /// from the *committed* subtree only) plus `has_uncommitted_descendants` —
    /// whether any live descendant is uncommitted. Because the rollup ignores
    /// uncommitted floaters, a container can read `Done` while real work still sits
    /// beneath it; the second signal keeps that honest (HV-104). Both come from a
    /// single [`Self::live_descendants`] walk.
    pub(crate) fn container_rollup(&self, node_id: i64) -> Result<(RollupState, bool)> {
        let live = self.live_descendants(node_id)?;
        let committed: Vec<Status> = live.iter().filter(|(_, c)| *c).map(|(s, _)| *s).collect();
        let has_uncommitted = live.iter().any(|(_, c)| !*c);
        Ok((rollup_from_statuses(&committed), has_uncommitted))
    }

    /// Project-level living docs: all live anchor nodes plus their artifacts.
    pub fn docs(&self, project: Option<&str>) -> Result<Vec<DocAnchor>> {
        let (project_id, _) = self.require_project(project)?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ITEM_SELECT} FROM {ITEM_FROM}
             WHERE n.project_id = ?1
               AND n.type = 'anchor'
               AND n.status NOT IN ('archived','superseded')
             ORDER BY n.created_at, n.id"
        ))?;
        let rows = stmt.query_map([project_id], item_from_row)?;
        let mut anchors = Vec::new();
        for row in rows {
            let item = row?;
            let artifacts = self.load_artifacts(item.id)?;
            anchors.push(DocAnchor { item, artifacts });
        }
        Ok(anchors)
    }

    /// One structural edge layer for a project, as `{kind, from, to}` ref pairs.
    fn edges_of_kind(&self, project_id: i64, kind: EdgeKind, sql: &str) -> Result<Vec<GraphEdge>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([project_id], |r| {
            Ok(GraphEdge {
                kind,
                from: r.get(0)?,
                to: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Lineage links for the project: `{event, from, to}` per lineage edge.
    fn project_lineage_links(&self, project_id: i64) -> Result<Vec<LineageLink>> {
        let mut stmt = self.conn.prepare(
            "SELECT ev.event_type, fn.ref, tn.ref FROM lineage_edges le
               JOIN lineage_events ev ON ev.id = le.event_id
               JOIN nodes fn ON fn.id = le.from_node_id
               JOIN nodes tn ON tn.id = le.to_node_id
             WHERE fn.project_id = ?1 ORDER BY ev.id, fn.ref, tn.ref",
        )?;
        let rows = stmt.query_map([project_id], |r| {
            Ok(LineageLink {
                event: r.get(0)?,
                from: r.get(1)?,
                to: r.get(2)?,
            })
        })?;
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

#[cfg(test)]
mod tests {
    use super::ORDER;

    /// HV-67 freeze guard: `due_at` deliberately does NOT enter the dispatch
    /// ordering (the static-vs-computed ranking fork is deferred). This pins the
    /// `ORDER` const byte-for-byte so any edit — adding `due_at`, a derived
    /// urgency term, anything — fails the suite and forces a conscious decision.
    /// `next()` and `next_explain` both reference this single const, so freezing
    /// it freezes both in lockstep.
    #[test]
    fn order_const_is_byte_frozen() {
        assert_eq!(
            ORDER,
            " ORDER BY n.priority IS NULL, n.priority, n.sort_key IS NULL, n.sort_key, n.created_at, n.id"
        );
    }
}
