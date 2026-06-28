//! Read queries: the `next` dispatch query, FTS search, the lineage resolver
//! (old ref → live descendant), per-node lineage hydration, and graph
//! traversal. Also the aggregate counts behind `haven status`.

use rusqlite::{params, Row};
use serde::Serialize;
use serde_json::Value;

use crate::error::Result;
use crate::model::*;

use super::{
    fts_user_query, item_from_row, EdgeKind, Include, ItemFilter, Store, ITEM_FROM, ITEM_SELECT,
};

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

/// Bounded graph payload for context-constrained surfaces. Totals describe the
/// visible graph before node/edge/lineage caps; vectors contain only the payload
/// slice. The CLI `graph` command uses this too — bounded by default, `--full`
/// lifts the caps.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectGraphPage {
    pub project: String,
    pub nodes: Vec<Item>,
    pub edges: Vec<GraphEdge>,
    pub lineage: Vec<LineageLink>,
    pub node_total: usize,
    pub edge_total: usize,
    pub lineage_total: usize,
    pub grooming_nudge: Option<String>,
}

/// Compact context for a parent/group/blocker shown in a dispatch summary.
#[derive(Debug, Clone, Serialize)]
pub struct DispatchContextItem {
    #[serde(rename = "ref")]
    pub reference: String,
    pub title: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
}

/// Artifact metadata shown in a dispatch summary. This intentionally omits
/// internal ids, hashes, revisions, and metadata so dispatch stays lean.
#[derive(Debug, Clone, Serialize)]
pub struct DispatchArtifact {
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
}

/// One candidate in a [`DispatchSummary`]: the ranked `next` row plus the
/// targeted context a human/agent otherwise tends to fetch with several reads.
#[derive(Debug, Clone, Serialize)]
pub struct DispatchCandidate {
    pub rank: usize,
    #[serde(rename = "ref")]
    pub reference: String,
    pub title: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_kind: Option<OwnerKind>,
    pub committed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_looks_like: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub parents: Vec<DispatchContextItem>,
    pub groups: Vec<DispatchContextItem>,
    pub blocks: Vec<DispatchContextItem>,
    pub artifacts: Vec<DispatchArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_pack: Option<ContextPack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_pack_clash: Option<Vec<String>>,
    pub eligibility: String,
}

/// The top-ranked dispatch candidate and why it is the recommendation.
#[derive(Debug, Clone, Serialize)]
pub struct DispatchRecommendation {
    #[serde(rename = "ref")]
    pub reference: String,
    pub reason: String,
}

/// Purpose-built "what should I work on?" payload: bounded `next` plus targeted
/// details for just those candidates, optionally scoped to a subtree.
#[derive(Debug, Clone, Serialize)]
pub struct DispatchSummary {
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<OwnerKind>,
    pub limit: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<DispatchContextItem>,
    pub candidates: Vec<DispatchCandidate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<DispatchRecommendation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<serde_json::Value>,
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

/// Classify a container's owner from the `owner_kind`s of its LIVE, committed,
/// owner-bearing descendants (HV-128). Total over the input, parallel to
/// [`rollup_from_statuses`]: empty → `Unassigned`; all `Ai` → `Ai`; all `Human`
/// → `Human`; both present → `Mixed`. The caller supplies only the owner kinds
/// of live, committed descendants with a non-NULL `owner_kind`.
pub(crate) fn owner_rollup_from(owners: &[OwnerKind]) -> OwnerRollup {
    if owners.is_empty() {
        return OwnerRollup::Unassigned;
    }
    let any_ai = owners.contains(&OwnerKind::Ai);
    let any_human = owners.contains(&OwnerKind::Human);
    match (any_ai, any_human) {
        (true, true) => OwnerRollup::Mixed,
        (true, false) => OwnerRollup::Ai,
        (false, true) => OwnerRollup::Human,
        // Unreachable: the input is non-empty and `OwnerKind` is closed over
        // {Ai, Human}, so at least one branch above is set.
        (false, false) => OwnerRollup::Unassigned,
    }
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

/// Default dispatch-frontier size when a user-facing surface (`haven_next` over
/// MCP or CLI) passes no explicit `limit`. Core [`Store::next`] itself stays
/// unbounded — internal callers like `prime` need the full frontier to compute
/// their `+N more` overflow — so the cap is applied only at the surfaces, the
/// same split as [`crate::store`]'s list reads (`DEFAULT_LIST_LIMIT`). A wide
/// ready frontier on a mature graph would otherwise return an unbounded list and
/// blow an agent's context budget; the orchestrator re-polls between batches, so
/// the top of the ranked frontier is all it needs in one read (HV-194).
pub const DEFAULT_NEXT_LIMIT: i64 = 50;
/// Default candidate count for the richer dispatch summary. This is lower than
/// `next` because each candidate carries targeted detail.
pub const DEFAULT_DISPATCH_LIMIT: i64 = 5;

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

impl From<Item> for DispatchContextItem {
    fn from(item: Item) -> Self {
        DispatchContextItem {
            reference: item.reference,
            title: item.title,
            node_type: item.node_type,
        }
    }
}

impl From<Artifact> for DispatchArtifact {
    fn from(artifact: Artifact) -> Self {
        DispatchArtifact {
            role: artifact.role,
            kind: artifact.kind,
            path: artifact.path,
            uri: artifact.uri,
            title: artifact.title,
            excerpt: artifact.excerpt,
        }
    }
}

fn push_scope_filter(
    sql: &mut String,
    args: &mut Vec<Box<dyn rusqlite::ToSql>>,
    scope_id: Option<i64>,
) {
    if let Some(scope_id) = scope_id {
        let idx = args.len() + 1;
        sql.push_str(&format!(
            " AND n.id IN (
                WITH RECURSIVE sub(id) AS (
                    SELECT ?{idx}
                    UNION
                    SELECT e.child_id FROM decomposition_edges e JOIN sub ON e.parent_id = sub.id
                    UNION
                    SELECT e.member_id FROM grouping_edges e JOIN sub ON e.group_id = sub.id
                )
                SELECT id FROM sub WHERE id <> ?{idx}
            )"
        ));
        args.push(Box::new(scope_id));
    }
}

fn dispatch_eligibility(owner: Option<OwnerKind>, scope: Option<&DispatchContextItem>) -> String {
    let mut parts = vec!["committed", "ready", "unblocked", "not waiting"];
    if owner.is_some() {
        parts.push("owner matched");
    }
    if scope.is_some() {
        parts.push("inside scope");
    }
    parts.join(", ")
}

fn recommendation_reason(owner: Option<OwnerKind>, scope: Option<&DispatchContextItem>) -> String {
    match (owner, scope) {
        (Some(owner), Some(scope)) => format!(
            "highest-ranked dispatchable item for owner {} under {}",
            owner.as_str(),
            scope.reference
        ),
        (Some(owner), None) => format!(
            "highest-ranked dispatchable item for owner {}",
            owner.as_str()
        ),
        (None, Some(scope)) => {
            format!("highest-ranked dispatchable item under {}", scope.reference)
        }
        (None, None) => "highest-ranked dispatchable item".into(),
    }
}

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
        self.next_for_project(project_id, owner, limit, None)
    }

    fn next_for_project(
        &self,
        project_id: i64,
        owner: Option<OwnerKind>,
        limit: Option<i64>,
        scope_id: Option<i64>,
    ) -> Result<Vec<Item>> {
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
        push_scope_filter(&mut sql, &mut args, scope_id);
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

    /// A lean dispatch briefing: bounded `next` plus targeted context for only
    /// the visible candidates. This is the cheap path for "what should I work on?"
    /// when `next` alone is too sparse but `graph` would be excessive.
    pub fn dispatch(
        &self,
        project: Option<&str>,
        owner: Option<OwnerKind>,
        limit: Option<i64>,
        scope: Option<&str>,
        include_explain: bool,
    ) -> Result<DispatchSummary> {
        let (project_id, key) = self.require_project(project)?;
        let limit = limit.unwrap_or(DEFAULT_DISPATCH_LIMIT).max(0);
        let scope_id = scope
            .map(|s| self.resolve_node_id(project_id, s))
            .transpose()?;
        let scope_item = scope_id
            .map(|id| self.dispatch_context_for_id(id))
            .transpose()?;

        let items = self.next_for_project(project_id, owner, Some(limit), scope_id)?;
        let mut candidates = Vec::with_capacity(items.len());
        for (idx, item) in items.into_iter().enumerate() {
            let mut full = self.get_item(
                Some(&key),
                &item.reference,
                &[Include::Edges, Include::Artifacts],
            )?;
            let edges = full.edges.take().unwrap_or_default();
            let artifacts = full
                .artifacts
                .take()
                .unwrap_or_default()
                .into_iter()
                .map(DispatchArtifact::from)
                .collect();
            let eligibility = dispatch_eligibility(owner, scope_item.as_ref());
            candidates.push(DispatchCandidate {
                rank: idx + 1,
                reference: full.reference,
                title: full.title,
                node_type: full.node_type,
                status: full.status,
                owner_kind: full.owner_kind,
                committed: full.committed,
                priority: full.priority,
                done_looks_like: full.done_looks_like,
                why: full.why,
                body: full.body,
                parents: self.dispatch_contexts_for_refs(&key, &edges.parents)?,
                groups: self.dispatch_contexts_for_refs(&key, &edges.groups)?,
                blocks: self.dispatch_contexts_for_refs(&key, &edges.blocks)?,
                artifacts,
                context_pack: full.context_pack,
                context_pack_clash: full.context_pack_clash,
                eligibility,
            });
        }

        let recommendation = candidates.first().map(|c| DispatchRecommendation {
            reference: c.reference.clone(),
            reason: recommendation_reason(owner, scope_item.as_ref()),
        });
        let explain = (include_explain || candidates.is_empty())
            .then(|| {
                self.next_explain_for_project(
                    project_id,
                    key.clone(),
                    owner,
                    scope_id,
                    scope_item.as_ref(),
                )
            })
            .transpose()?;

        Ok(DispatchSummary {
            project: key,
            owner,
            limit,
            scope: scope_item,
            candidates,
            recommendation,
            explain,
        })
    }

    /// Explain the local dispatch queue: useful when `next` is empty.
    pub fn next_explain(
        &self,
        project: Option<&str>,
        owner: Option<OwnerKind>,
    ) -> Result<serde_json::Value> {
        let (project_id, key) = self.require_project(project)?;
        self.next_explain_for_project(project_id, key, owner, None, None)
    }

    fn next_explain_for_project(
        &self,
        project_id: i64,
        key: String,
        owner: Option<OwnerKind>,
        scope_id: Option<i64>,
        scope: Option<&DispatchContextItem>,
    ) -> Result<serde_json::Value> {
        let dispatchable = self.count_dispatchable(project_id, owner, scope_id)?;
        let dispatchable_any_owner = self.count_dispatchable(project_id, None, scope_id)?;
        let owner_mismatch = owner
            .map(|_| dispatchable_any_owner.saturating_sub(dispatchable))
            .unwrap_or(0);
        let waiting = self.count_next_where(
            project_id,
            owner,
            scope_id,
            "n.type <> 'anchor' AND n.committed = 1 AND n.status = 'ready' AND n.wait_state IS NOT NULL",
        )?;
        let blocked_by_dependency = self.count_next_where(
            project_id,
            owner,
            scope_id,
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
            scope_id,
            "n.type <> 'anchor' AND n.committed = 1 AND n.status NOT IN ('ready','done','superseded','archived')",
        )?;
        let ready_but_uncommitted = self.count_next_where(
            project_id,
            owner,
            scope_id,
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

        let mut out = serde_json::json!({
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
        });
        if let Some(scope) = scope {
            if let serde_json::Value::Object(map) = &mut out {
                map.insert("scope".into(), serde_json::to_value(scope)?);
            }
        }
        Ok(out)
    }

    fn count_dispatchable(
        &self,
        project_id: i64,
        owner: Option<OwnerKind>,
        scope_id: Option<i64>,
    ) -> Result<i64> {
        self.count_next_where(project_id, owner, scope_id, DISPATCHABLE_PREDICATE)
    }

    fn count_next_where(
        &self,
        project_id: i64,
        owner: Option<OwnerKind>,
        scope_id: Option<i64>,
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
        push_scope_filter(&mut sql, &mut args, scope_id);
        Ok(self.conn.query_row(
            &sql,
            rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
            |r| r.get(0),
        )?)
    }

    fn dispatch_context_for_id(&self, node_id: i64) -> Result<DispatchContextItem> {
        let item = self.conn.query_row(
            &format!("SELECT {ITEM_SELECT} FROM {ITEM_FROM} WHERE n.id = ?1"),
            [node_id],
            item_from_row,
        )?;
        Ok(DispatchContextItem::from(item))
    }

    fn dispatch_contexts_for_refs(
        &self,
        project: &str,
        refs: &[String],
    ) -> Result<Vec<DispatchContextItem>> {
        let ref_slices: Vec<&str> = refs.iter().map(String::as_str).collect();
        self.get_items_hinted(Some(project), &ref_slices, &[])
            .map(|items| {
                items
                    .into_iter()
                    .map(|(item, _stale)| DispatchContextItem::from(item))
                    .collect()
            })
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
        for node in &mut nodes {
            self.hydrate_graph_node(node)?;
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

    /// Bounded graph read for MCP: compute totals over the visible graph, but
    /// hydrate expensive derived node fields only for payload nodes.
    pub fn project_graph_page(
        &self,
        project: Option<&str>,
        lineage: bool,
        include_dead: bool,
        node_limit: usize,
        edge_limit: usize,
        lineage_limit: usize,
    ) -> Result<ProjectGraphPage> {
        use std::collections::HashSet;

        let (project_id, key) = self.require_project(project)?;
        let visible_nodes = self.list_items(
            project,
            &ItemFilter {
                include_dead,
                ..Default::default()
            },
        )?;
        let visible_refs: HashSet<String> =
            visible_nodes.iter().map(|n| n.reference.clone()).collect();
        let node_total = visible_nodes.len();
        let mut nodes: Vec<Item> = visible_nodes.into_iter().take(node_limit).collect();
        for node in &mut nodes {
            self.hydrate_graph_node(node)?;
        }
        let payload_refs: HashSet<String> = nodes.iter().map(|n| n.reference.clone()).collect();

        let mut all_edges = Vec::new();
        all_edges.extend(self.edges_of_kind(
            project_id,
            EdgeKind::Decomposition,
            "SELECT pn.ref, cn.ref FROM decomposition_edges e
               JOIN nodes pn ON pn.id = e.parent_id
               JOIN nodes cn ON cn.id = e.child_id
             WHERE pn.project_id = ?1 ORDER BY pn.ref, cn.ref",
        )?);
        all_edges.extend(self.edges_of_kind(
            project_id,
            EdgeKind::Dependency,
            "SELECT nn.ref, dn.ref FROM dependency_edges e
               JOIN nodes nn ON nn.id = e.node_id
               JOIN nodes dn ON dn.id = e.depends_on_id
             WHERE nn.project_id = ?1 ORDER BY nn.ref, dn.ref",
        )?);
        all_edges.extend(self.edges_of_kind(
            project_id,
            EdgeKind::Grouping,
            "SELECT gn.ref, mn.ref FROM grouping_edges e
               JOIN nodes gn ON gn.id = e.group_id
               JOIN nodes mn ON mn.id = e.member_id
             WHERE gn.project_id = ?1 ORDER BY gn.ref, mn.ref",
        )?);
        let visible_edges: Vec<GraphEdge> = all_edges
            .into_iter()
            .filter(|e| visible_refs.contains(&e.from) && visible_refs.contains(&e.to))
            .collect();
        let edge_total = visible_edges.len();
        let edges = visible_edges
            .into_iter()
            .filter(|e| payload_refs.contains(&e.from) && payload_refs.contains(&e.to))
            .take(edge_limit)
            .collect();

        let visible_lineage = if lineage {
            self.project_lineage_links(project_id)?
                .into_iter()
                .filter(|l| visible_refs.contains(&l.from) && visible_refs.contains(&l.to))
                .collect()
        } else {
            Vec::new()
        };
        let lineage_total = visible_lineage.len();
        let lineage = visible_lineage
            .into_iter()
            .filter(|l| payload_refs.contains(&l.from) && payload_refs.contains(&l.to))
            .take(lineage_limit)
            .collect();
        let grooming_nudge = self.grooming_pressure(project)?.nudge;
        Ok(ProjectGraphPage {
            project: key,
            nodes,
            edges,
            lineage,
            node_total,
            edge_total,
            lineage_total,
            grooming_nudge,
        })
    }

    fn hydrate_graph_node(&self, node: &mut Item) -> Result<()> {
        // Hydrate the derived rollup for containers, and the context-pack pointer
        // for leaves — read-only projections used by graph/detail views.
        if node.node_type.is_container() {
            let (rollup, owner_rollup, has_uncommitted) = self.container_rollup(node.id)?;
            node.rollup_state = Some(rollup);
            node.owner_rollup = Some(owner_rollup);
            node.has_uncommitted_descendants = Some(has_uncommitted);
        } else {
            let (pack, clash) = self.context_pack_for_node(node.id)?;
            node.context_pack = pack;
            node.context_pack_clash = clash;
        }
        Ok(())
    }

    /// A container's LIVE descendants as `(status, committed, owner_kind)` tuples,
    /// walking the union of decomposition (parent→child) and grouping
    /// (group→member) edges. The recursive set dedups ids, so a node reachable via
    /// both edge kinds is counted once; dead nodes (superseded/archived) are
    /// excluded. ONE walk feeds every derived container signal (see
    /// [`Self::container_rollup`]) — the status/owner rollups and the uncommitted
    /// flag — so they can never disagree about what "live descendant" means.
    pub(crate) fn live_descendants(
        &self,
        node_id: i64,
    ) -> Result<Vec<(Status, bool, Option<OwnerKind>)>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE sub(id) AS (
                 SELECT ?1
                 UNION
                 SELECT e.child_id FROM decomposition_edges e JOIN sub ON e.parent_id = sub.id
                 UNION
                 SELECT e.member_id FROM grouping_edges e JOIN sub ON e.group_id = sub.id
             )
             SELECT n.status, n.committed, n.owner_kind FROM nodes n
             WHERE n.id IN (SELECT id FROM sub) AND n.id <> ?1
               AND n.status NOT IN ('superseded','archived')",
        )?;
        let rows = stmt.query_map([node_id], |r| {
            Ok((
                r.get::<_, Status>(0)?,
                r.get::<_, bool>(1)?,
                r.get::<_, Option<OwnerKind>>(2)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The derived container signals for one node: its [`RollupState`] (classified
    /// from the *committed* subtree only), its [`OwnerRollup`] (the owner of that
    /// same committed subtree, HV-128), plus `has_uncommitted_descendants` —
    /// whether any live descendant is uncommitted. Because the rollups ignore
    /// uncommitted floaters, a container can read `Done` while real work still sits
    /// beneath it; the uncommitted flag keeps that honest (HV-104). All three come
    /// from a single [`Self::live_descendants`] walk, so they share one definition
    /// of the live-descendant set.
    pub(crate) fn container_rollup(
        &self,
        node_id: i64,
    ) -> Result<(RollupState, OwnerRollup, bool)> {
        let live = self.live_descendants(node_id)?;
        let committed: Vec<Status> = live
            .iter()
            .filter(|(_, c, _)| *c)
            .map(|(s, _, _)| *s)
            .collect();
        // Owner rollup considers only committed, owner-bearing descendants — the
        // same committed subtree the status rollup classifies.
        let owners: Vec<OwnerKind> = live
            .iter()
            .filter(|(_, c, _)| *c)
            .filter_map(|(_, _, o)| *o)
            .collect();
        let has_uncommitted = live.iter().any(|(_, c, _)| !*c);
        Ok((
            rollup_from_statuses(&committed),
            owner_rollup_from(&owners),
            has_uncommitted,
        ))
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
            "SELECT DISTINCT e.id, e.public_id, e.event_type, e.rationale, e.triggered_by, e.context, e.created_at
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
            "SELECT DISTINCT e.id, e.public_id, e.event_type, e.rationale, e.triggered_by, e.context, e.created_at
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
    let context: String = r.get(5)?;
    Ok(LineageEvent {
        public_id: r.get(1)?,
        event_type: r.get(2)?,
        rationale: r.get(3)?,
        triggered_by: r.get(4)?,
        context: serde_json::from_str(&context)
            .unwrap_or_else(|_| Value::Object(Default::default())),
        created_at: r.get(6)?,
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
