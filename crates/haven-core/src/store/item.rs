//! Item operations: CRUD, the maturity axis (status/wait), the commitment axis
//! (commit/uncommit/priority/rank), ownership, and lifecycle (archive/reopen,
//! which emit lineage events). All methods on [`Store`].

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::error::{HavenError, Result};
use crate::model::*;
use crate::sortkey;

use super::{item_from_row, new_uuid, NewArtifact, StaleRef, Store, ITEM_FROM, ITEM_SELECT};

/// Milliseconds since the Unix epoch — only used to disambiguate handoff
/// artifact filenames so successive handoffs on one item don't overwrite.
fn epoch_millis() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Parameters for `item add`. All fields beyond `title` are optional — the
/// default is a bare, floating, uncommitted, `discovery` node (SPEC §3).
#[derive(Debug, Default, Clone)]
pub struct NewItem {
    pub title: String,
    pub node_type: Option<NodeType>,
    pub body: Option<String>,
    pub done_looks_like: Option<String>,
    pub why: Option<String>,
    /// Optional deadline `YYYY-MM-DD`; validated at the Store boundary before any
    /// write (see [`crate::time::parse_due_date`]).
    pub due_at: Option<String>,
    pub status: Option<Status>,
    pub priority: Option<i64>,
    pub commit: bool,
    pub assign: Option<OwnerKind>,
    pub parent: Option<String>,
    pub depends_on: Option<String>,
    pub group: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// A live item whose title plausibly overlaps a just-created one — advisory
/// only, surfaced so an agent can merge instead of accumulating duplicates.
#[derive(Debug, Clone, Serialize)]
pub struct SimilarItem {
    #[serde(rename = "ref")]
    pub reference: String,
    pub title: String,
}

/// The result of a guarded add: the item (flattened, so consumers that read
/// `ref`/`title` off a bare item keep working), whether it was an existing item
/// returned by `--if-absent` instead of a create, and any similar-title
/// warnings (absent when empty).
#[derive(Debug, Serialize)]
pub struct AddOutcome {
    #[serde(flatten)]
    pub item: Item,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub existing: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub similar: Vec<SimilarItem>,
}

/// Title normalization for `--if-absent` dedupe: collapse whitespace, strip
/// trailing punctuation, case-fold. An empty result means "never matches".
pub(crate) fn normalize_title(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', ',', ';', ':', '!', '?'])
        .trim_end()
        .to_lowercase()
}

/// Build an FTS5 MATCH query over a title's words, scoped to the title column.
/// Tokens are pure-alphanumeric and double-quoted, which neutralizes FTS5
/// syntax (`"`, `-`, parens) and bareword operators (AND/OR/NOT/NEAR).
/// `None` when the title has no alphanumeric tokens — skip the query.
pub(crate) fn fts_title_query(title: &str) -> Option<String> {
    let tokens: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect();
    (!tokens.is_empty()).then(|| format!("title : ({})", tokens.join(" OR ")))
}

/// Build an FTS5 MATCH query from a raw user search string (HV-30). Like
/// [`fts_title_query`] it splits on non-alphanumerics and double-quotes each
/// token, which neutralizes FTS5 syntax (`-`, `:`, `"`, parens) and bareword
/// operators (AND/OR/NOT/NEAR) so a bare ref like `HV-22` can't be read as
/// column-filter syntax. Unlike that helper it is *not* column-scoped (searches
/// title+body) and AND-joins the tokens (implicit-AND: every token must appear)
/// for precise general search. `None` when there are no alphanumeric tokens —
/// the caller should skip the MATCH and return no results.
pub(crate) fn fts_user_query(query: &str) -> Option<String> {
    let tokens: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect();
    (!tokens.is_empty()).then(|| tokens.join(" "))
}

/// How to change a node's `wait_state`: set it, or clear it (`--wait none`).
#[derive(Debug, Clone, Copy)]
pub enum WaitUpdate {
    Set(WaitState),
    Clear,
}

/// How to change a node's `due_at`: set it to a date, or clear it
/// (`--due-at none`). Mirrors [`WaitUpdate`] so "clear" is explicit in the type
/// and never collides with a literal date string. The `Set` value is validated
/// at the Store boundary before any write.
#[derive(Debug, Clone)]
pub enum DueUpdate {
    Set(String),
    Clear,
}

/// Optional inputs for [`Store::handoff`] beyond the required target owner.
/// `None` fields take the documented defaults.
#[derive(Debug, Default)]
pub struct HandoffInput<'a> {
    /// Who is handing off. Defaults to the item's current owner, else the
    /// opposite of the target (a baton-pass has two sides).
    pub from: Option<OwnerKind>,
    /// The baton note, recorded as a `handoff` artifact under `notes/`.
    pub note: Option<&'a str>,
    /// Status on pickup. Defaults to `blocked` when handing to a human (the work
    /// is now waiting on them); unchanged otherwise.
    pub status: Option<Status>,
    /// Wait-state. Defaults to `on_human` when the target is human; unchanged
    /// otherwise.
    pub wait: Option<WaitState>,
    /// Actor handle recorded as the new assignee and the artifact author.
    pub actor: Option<&'a str>,
}

/// The result of a handoff: the updated item and the handoff artifact (when a
/// note was recorded).
#[derive(Debug, Serialize)]
pub struct HandoffResult {
    pub item: Item,
    pub artifact: Option<Artifact>,
}

/// Optional inputs for [`Store::complete_item`].
#[derive(Debug, Default)]
pub struct CompleteInput<'a> {
    /// Proof the work is done (test output, a summary, a link). Recorded as an
    /// artifact so the completion is auditable.
    pub evidence: Option<&'a str>,
    /// Role for the evidence artifact. Defaults to `delivery`.
    pub artifact_role: Option<ArtifactRole>,
    /// Creator handle recorded on the evidence artifact.
    pub by: Option<&'a str>,
}

/// The result of completing an item: the item (now `done`), the evidence
/// artifact (when given), the items/gates this completion unblocked, and any
/// advisory warnings (e.g. no acceptance was set).
#[derive(Debug, Serialize)]
pub struct CompleteResult {
    pub item: Item,
    pub artifact: Option<Artifact>,
    /// Items that depended on this one and now have no open dependency — the
    /// newly-actionable work (includes gates whose triggers are all complete).
    pub unblocked: Vec<Item>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Mutable fields for `item update`. `None` means "leave unchanged".
#[derive(Debug, Default, Clone)]
pub struct ItemUpdate {
    pub title: Option<String>,
    pub body: Option<String>,
    pub done_looks_like: Option<String>,
    pub why: Option<String>,
    pub status: Option<Status>,
    pub priority: Option<i64>,
    pub node_type: Option<NodeType>,
    pub wait: Option<WaitUpdate>,
    /// `None` leaves `due_at` unchanged; `Some(DueUpdate::Set)` sets it (after
    /// boundary validation); `Some(DueUpdate::Clear)` sets it NULL.
    pub due: Option<DueUpdate>,
}

/// Filters for `item list`. `committed`/`icebox` are mutually-exclusive views.
#[derive(Debug, Default, Clone)]
pub struct ItemFilter {
    pub status: Option<Status>,
    pub node_type: Option<NodeType>,
    pub owner: Option<OwnerKind>,
    pub committed: Option<bool>,
    /// `icebox` view: committed = 0 and not archived/superseded.
    pub icebox: bool,
    /// `inbox` view: the icebox AND `done_looks_like IS NULL` — untriaged
    /// floaters with no acceptance yet. A composable subset of `icebox`.
    pub inbox: bool,
    pub group: Option<String>,
    /// Items parked on a specific wait-state — answers "what's waiting on me?"
    /// (`on_human`) or "stuck on something external?" (`on_external`).
    pub wait: Option<WaitState>,
    /// Items untouched for at least this many days (by `updated_at`) — surfaces
    /// stale/forgotten work.
    pub stale_days: Option<i64>,
    /// Include archived/superseded (dead) items. Default (`false`) hides them,
    /// giving a live-only list (HV-53). Ignored when an explicit `status` filter
    /// is set (asking for `archived`/`superseded` by name is a deliberate ask),
    /// and when the `icebox`/`inbox` views — which carry their own dead exclusion
    /// — are active.
    pub include_dead: bool,
}

/// True when an acceptance statement is absent or only whitespace. The
/// `ready`-requires-`done_looks_like` guard (HV-80) treats both as "no
/// acceptance". `pub(super)` so the import path (HV-159) reuses the exact same
/// "blank acceptance" notion rather than re-deriving it.
pub(super) fn acceptance_blank(done: Option<&str>) -> bool {
    match done {
        None => true,
        Some(s) => s.trim().is_empty(),
    }
}

/// The Store-boundary gate for `due_at`: accept only a well-formed,
/// civil-round-trippable `YYYY-MM-DD`, else `HavenError::Invalid`. Reuses the
/// hand-rolled civil math in [`crate::time`] — no `chrono`, no per-write CHECK.
/// The DB column is plain `TEXT`, so this is the only place garbage is stopped.
fn validate_due_at(value: &str) -> Result<()> {
    if crate::time::parse_due_date(value).is_some() {
        Ok(())
    } else {
        Err(HavenError::Invalid(format!(
            "invalid due_at {value:?} — expected a calendar date YYYY-MM-DD"
        )))
    }
}

impl Store {
    /// Create a node. Mints a `ref` from the project counter, applies the
    /// requested axes, and wires any parent/dependency/group edges given.
    pub fn add_item(&self, project: Option<&str>, new: NewItem) -> Result<Item> {
        if new.title.trim().is_empty() {
            return Err(HavenError::Invalid("item title must not be empty".into()));
        }
        // HV-80: an item cannot be born `ready` without acceptance either —
        // keeps the backstop airtight so no `ready`-without-`done_looks_like`
        // node can ever be created.
        if matches!(new.status, Some(Status::Ready))
            && acceptance_blank(new.done_looks_like.as_deref())
        {
            return Err(HavenError::Invalid(
                "cannot create an item as ready without acceptance — set done_looks_like first"
                    .into(),
            ));
        }
        // Reject a malformed/impossible deadline before any write touches the DB.
        if let Some(due) = new.due_at.as_deref() {
            validate_due_at(due)?;
        }
        let (project_id, _key) = self.require_project_mut(project)?;
        let tx = self.conn.unchecked_transaction()?;

        let node_type = new.node_type.unwrap_or(NodeType::Task);
        let status = new.status.unwrap_or(Status::Discovery);
        let metadata = new
            .metadata
            .clone()
            .unwrap_or_else(|| serde_json::json!({}));

        let (node_id, reference) = self.insert_node(
            &tx,
            project_id,
            &new.title,
            node_type,
            status,
            new.body.as_deref(),
            new.done_looks_like.as_deref(),
            new.why.as_deref(),
            new.assign,
            new.commit,
            new.priority,
            new.due_at.as_deref(),
            &metadata,
        )?;

        // Optional edges. These reuse the edge helpers, which run inside `tx`.
        if let Some(parent) = &new.parent {
            let parent_id = self.resolve_node_id(project_id, parent)?;
            self.insert_decomposition(&tx, parent_id, node_id)?;
        }
        if let Some(dep) = &new.depends_on {
            let dep_id = self.resolve_node_id(project_id, dep)?;
            self.insert_dependency(&tx, node_id, dep_id)?;
        }
        if let Some(group) = &new.group {
            let group_id = self.resolve_node_id(project_id, group)?;
            self.insert_grouping(&tx, group_id, node_id)?;
        }

        tx.commit()?;
        self.get_item(project, &reference, &[])
    }

    /// [`Store::add_item`] with capture guards: when `if_absent` and a live
    /// item's normalized title matches, return it (`existing: true`) instead of
    /// creating a duplicate; otherwise create and attach up to 3 FTS-backed
    /// `similar` warnings. The dedupe check and the create are separate
    /// transactions — acceptable for a single-user local store.
    pub fn add_item_checked(
        &self,
        project: Option<&str>,
        new: NewItem,
        if_absent: bool,
    ) -> Result<AddOutcome> {
        if if_absent {
            let (project_id, _key) = self.require_project(project)?;
            let norm = normalize_title(&new.title);
            if !norm.is_empty() {
                if let Some(node_id) = self.find_live_by_norm_title(project_id, &norm)? {
                    let reference = self.node_ref(node_id)?;
                    return Ok(AddOutcome {
                        item: self.get_item(project, &reference, &[])?,
                        existing: true,
                        similar: vec![],
                    });
                }
            }
        }
        let item = self.add_item(project, new)?;
        let (project_id, _key) = self.require_project(project)?;
        let similar = self.similar_live(project_id, item.id, &item.title, 3)?;
        Ok(AddOutcome {
            item,
            existing: false,
            similar,
        })
    }

    /// Find a live (non-archived, non-superseded) node whose normalized title
    /// equals `norm`. Compares in Rust — fine at Haven scale; an indexed
    /// normalized column is the upgrade path. Sees in-transaction state when
    /// called under `unchecked_transaction` (same connection).
    pub(crate) fn find_live_by_norm_title(
        &self,
        project_id: i64,
        norm: &str,
    ) -> Result<Option<i64>> {
        if norm.is_empty() {
            return Ok(None);
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, title FROM nodes
             WHERE project_id = ?1 AND status NOT IN ('archived','superseded')
             ORDER BY id",
        )?;
        let rows = stmt.query_map([project_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, title) = row?;
            if normalize_title(&title) == norm {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Live items whose titles plausibly overlap `title`, best-match first.
    fn similar_live(
        &self,
        project_id: i64,
        exclude_id: i64,
        title: &str,
        cap: i64,
    ) -> Result<Vec<SimilarItem>> {
        let Some(query) = fts_title_query(title) else {
            return Ok(vec![]);
        };
        // FTS5's MATCH must reference the virtual table by its real name.
        let mut stmt = self.conn.prepare(
            "SELECT n.ref, n.title FROM node_fts
             JOIN nodes n ON n.id = node_fts.rowid
             WHERE node_fts MATCH ?1 AND n.project_id = ?2 AND n.id <> ?3
               AND n.status NOT IN ('archived','superseded')
             ORDER BY rank LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![query, project_id, exclude_id, cap], |r| {
            Ok(SimilarItem {
                reference: r.get(0)?,
                title: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Fetch one item, optionally hydrating edges/artifacts/lineage.
    /// [`Store::get_item`] plus a [`StaleRef`] hint when the ref is dead
    /// (superseded/archived) — the read still returns the item, but the caller
    /// learns where the live work moved (HV-154). The MCP read surface uses this;
    /// the CLI keeps `get_item` (the hint rides MCP responses).
    pub fn get_item_hinted(
        &self,
        project: Option<&str>,
        selector: &str,
        include: &[Include],
    ) -> Result<(Item, Option<StaleRef>)> {
        let (project_id, _key) = self.require_project(project)?;
        let (_node_id, hint) = self.resolve_node_id_hinted(project_id, selector)?;
        Ok((self.get_item(project, selector, include)?, hint))
    }

    pub fn get_item(
        &self,
        project: Option<&str>,
        selector: &str,
        include: &[Include],
    ) -> Result<Item> {
        let (project_id, _key) = self.require_project(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        let mut item = self
            .conn
            .query_row(
                &format!("SELECT {ITEM_SELECT} FROM {ITEM_FROM} WHERE n.id = ?1"),
                [node_id],
                item_from_row,
            )
            .optional()?
            .ok_or_else(|| HavenError::NotFound(format!("item {selector:?}")))?;

        for inc in include {
            match inc {
                Include::Edges => item.edges = Some(self.load_edges(node_id)?),
                Include::Artifacts => item.artifacts = Some(self.load_artifacts(node_id)?),
                Include::Lineage => item.lineage = Some(self.lineage_events_for_node(node_id)?),
            }
        }
        if item.node_type.is_container() {
            let (rollup, has_uncommitted) = self.container_rollup(node_id)?;
            item.rollup_state = Some(rollup);
            item.has_uncommitted_descendants = Some(has_uncommitted);
        } else {
            // A leaf advertises the context pack governing its build, so a
            // dispatcher can't build it naked or guess which group carries the
            // pack (HV-75). Derived on read; containers hold packs, not consume.
            let (pack, clash) = self.context_pack_for_node(node_id)?;
            item.context_pack = pack;
            item.context_pack_clash = clash;
        }
        Ok(item)
    }

    /// List items in a project under the given filters.
    pub fn list_items(&self, project: Option<&str>, filter: &ItemFilter) -> Result<Vec<Item>> {
        let (project_id, _key) = self.require_project(project)?;
        let mut sql = format!("SELECT {ITEM_SELECT} FROM {ITEM_FROM} WHERE n.project_id = ?1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project_id)];

        if let Some(status) = filter.status {
            sql.push_str(&format!(" AND n.status = ?{}", args.len() + 1));
            args.push(Box::new(status.as_str()));
        }
        // Live-only by default (HV-53): drop archived/superseded unless `include_dead`
        // is set. Skipped when an explicit `status` filter is present (asking for a
        // dead status by name is deliberate) and when `icebox`/`inbox` are active
        // (they carry their own dead exclusion below).
        if !filter.include_dead && filter.status.is_none() && !filter.icebox && !filter.inbox {
            sql.push_str(" AND n.status NOT IN ('archived','superseded')");
        }
        if let Some(t) = filter.node_type {
            sql.push_str(&format!(" AND n.type = ?{}", args.len() + 1));
            args.push(Box::new(t.as_str()));
        }
        if let Some(owner) = filter.owner {
            sql.push_str(&format!(" AND n.owner_kind = ?{}", args.len() + 1));
            args.push(Box::new(owner.as_str()));
        }
        if let Some(committed) = filter.committed {
            sql.push_str(&format!(" AND n.committed = ?{}", args.len() + 1));
            args.push(Box::new(committed as i64));
        }
        if filter.icebox {
            sql.push_str(" AND n.committed = 0 AND n.status NOT IN ('archived','superseded')");
        }
        if filter.inbox {
            sql.push_str(
                " AND n.committed = 0 AND n.status NOT IN ('archived','superseded') AND n.done_looks_like IS NULL",
            );
        }
        if let Some(wait) = filter.wait {
            sql.push_str(&format!(" AND n.wait_state = ?{}", args.len() + 1));
            args.push(Box::new(wait.as_str()));
        }
        if let Some(days) = filter.stale_days {
            // `updated_at` is SQLite `datetime('now')`; compare against the cutoff.
            sql.push_str(" AND n.type <> 'anchor'");
            sql.push_str(&format!(
                " AND n.updated_at < datetime('now', ?{})",
                args.len() + 1
            ));
            args.push(Box::new(format!("-{days} days")));
        }
        if let Some(group) = &filter.group {
            let group_id = self.resolve_node_id(project_id, group)?;
            sql.push_str(&format!(
                " AND n.id IN (SELECT member_id FROM grouping_edges WHERE group_id = ?{})",
                args.len() + 1
            ));
            args.push(Box::new(group_id));
        }
        sql.push_str(" ORDER BY n.priority IS NULL, n.priority, n.sort_key IS NULL, n.sort_key, n.created_at, n.id");

        let mut stmt = self.conn.prepare(&sql)?;
        let params = rusqlite::params_from_iter(args.iter().map(|b| b.as_ref()));
        let rows = stmt.query_map(params, item_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// [`Store::update_item`] plus a [`StaleRef`] hint when the target ref is dead
    /// (superseded/archived) — the update still applies, but the caller is told
    /// the work has moved on (HV-154). The MCP write surface uses this.
    pub fn update_item_hinted(
        &self,
        project: Option<&str>,
        selector: &str,
        upd: ItemUpdate,
    ) -> Result<(Item, Option<StaleRef>)> {
        let (project_id, _key) = self.require_project(project)?;
        let (_node_id, hint) = self.resolve_node_id_hinted(project_id, selector)?;
        Ok((self.update_item(project, selector, upd)?, hint))
    }

    /// Update mutable attributes (maturity axis + content). Bumps revision.
    pub fn update_item(
        &self,
        project: Option<&str>,
        selector: &str,
        upd: ItemUpdate,
    ) -> Result<Item> {
        let (project_id, _key) = self.require_project_mut(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;

        // HV-80: `ready` requires acceptance. This is the single chokepoint for
        // status→ready (CLI `item update` and MCP `haven_update_item` both route
        // here). Refuse the ready-transition without `done_looks_like`, and
        // refuse clearing acceptance on an already-`ready` item. The check fires
        // only on those two moves, so unrelated edits to a grandfathered
        // ready-without-acceptance item are left untouched.
        let setting_ready = matches!(upd.status, Some(Status::Ready));
        let clearing_done = upd
            .done_looks_like
            .as_deref()
            .is_some_and(|s| s.trim().is_empty());
        if setting_ready || clearing_done {
            let current = self.get_item(project, selector, &[])?;
            let effective_done = match upd.done_looks_like.as_deref() {
                Some(s) => Some(s),
                None => current.done_looks_like.as_deref(),
            };
            let effective_ready = setting_ready || matches!(current.status, Status::Ready);
            if effective_ready && acceptance_blank(effective_done) {
                return Err(HavenError::Invalid(format!(
                    "cannot mark {selector} ready without acceptance — set done_looks_like (what success looks like) first"
                )));
            }
        }

        let mut sets: Vec<String> = Vec::new();
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        macro_rules! set {
            ($col:literal, $val:expr) => {{
                sets.push(format!("{} = ?{}", $col, args.len() + 1));
                args.push(Box::new($val));
            }};
        }
        if let Some(title) = upd.title {
            set!("title", title);
        }
        if let Some(body) = upd.body {
            set!("body", body);
        }
        if let Some(done) = upd.done_looks_like {
            set!("done_looks_like", done);
        }
        if let Some(why) = upd.why {
            set!("why", why);
        }
        if let Some(status) = upd.status {
            set!("status", status.as_str());
        }
        if let Some(priority) = upd.priority {
            set!("priority", priority);
        }
        if let Some(t) = upd.node_type {
            set!("type", t.as_str());
        }
        match upd.wait {
            Some(WaitUpdate::Set(w)) => set!("wait_state", w.as_str()),
            Some(WaitUpdate::Clear) => sets.push("wait_state = NULL".into()),
            None => {}
        }
        match upd.due {
            Some(DueUpdate::Set(d)) => {
                // Validate at the boundary before binding — no garbage in the column.
                validate_due_at(&d)?;
                set!("due_at", d);
            }
            Some(DueUpdate::Clear) => sets.push("due_at = NULL".into()),
            None => {}
        }
        if sets.is_empty() {
            return Err(HavenError::Invalid("update: no fields to change".into()));
        }
        sets.push("revision = revision + 1".into());
        sets.push("updated_at = datetime('now')".into());
        sets.push("sync_state = 'local'".into());

        let sql = format!(
            "UPDATE nodes SET {} WHERE id = ?{}",
            sets.join(", "),
            args.len() + 1
        );
        args.push(Box::new(node_id));
        let params = rusqlite::params_from_iter(args.iter().map(|b| b.as_ref()));
        self.conn.execute(&sql, params)?;
        self.get_item(project, selector, &[])
    }

    /// Apply the same update to several items in one grooming step ("mark these
    /// ready"). Refs are validated up front, so a typo aborts the batch before
    /// any item is touched. Returns the updated items.
    pub fn update_items(
        &self,
        project: Option<&str>,
        refs: &[&str],
        upd: ItemUpdate,
    ) -> Result<Vec<Item>> {
        self.validate_refs(project, refs)?;
        refs.iter()
            .map(|r| self.update_item(project, r, upd.clone()))
            .collect()
    }

    /// Commitment axis: float → committed (optionally setting a priority band).
    pub fn commit_item(
        &self,
        project: Option<&str>,
        selector: &str,
        priority: Option<i64>,
    ) -> Result<Item> {
        let (project_id, _key) = self.require_project_mut(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        match priority {
            Some(p) => self.conn.execute(
                "UPDATE nodes SET committed = 1, priority = ?1, revision = revision + 1,
                     updated_at = datetime('now'), sync_state = 'local' WHERE id = ?2",
                params![p, node_id],
            )?,
            None => self.conn.execute(
                "UPDATE nodes SET committed = 1, revision = revision + 1,
                     updated_at = datetime('now'), sync_state = 'local' WHERE id = ?1",
                [node_id],
            )?,
        };
        self.get_item(project, selector, &[])
    }

    /// Commitment axis: back to the icebox (floating). Priority is retained.
    pub fn uncommit_item(&self, project: Option<&str>, selector: &str) -> Result<Item> {
        let (project_id, _key) = self.require_project_mut(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        self.conn.execute(
            "UPDATE nodes SET committed = 0, revision = revision + 1,
                 updated_at = datetime('now'), sync_state = 'local' WHERE id = ?1",
            [node_id],
        )?;
        self.get_item(project, selector, &[])
    }

    /// Commit several items in one grooming step ("commit these two"). All refs
    /// are validated up front, so a typo aborts the batch before any write rather
    /// than committing a prefix. Returns the updated items.
    pub fn commit_items(
        &self,
        project: Option<&str>,
        refs: &[&str],
        priority: Option<i64>,
    ) -> Result<Vec<Item>> {
        self.validate_refs(project, refs)?;
        refs.iter()
            .map(|r| self.commit_item(project, r, priority))
            .collect()
    }

    /// Uncommit several items at once (validated up front). Returns the updated
    /// items.
    pub fn uncommit_items(&self, project: Option<&str>, refs: &[&str]) -> Result<Vec<Item>> {
        self.validate_refs(project, refs)?;
        refs.iter()
            .map(|r| self.uncommit_item(project, r))
            .collect()
    }

    /// Resolve every ref (erroring on the first unknown) before a batch mutation,
    /// so an unknown ref aborts the whole batch instead of half-applying it.
    fn validate_refs(&self, project: Option<&str>, refs: &[&str]) -> Result<()> {
        let (project_id, _key) = self.require_project_mut(project)?;
        for r in refs {
            self.resolve_node_id(project_id, r)?;
        }
        Ok(())
    }

    /// Ownership: set who executes the node and an optional actor handle.
    pub fn assign_item(
        &self,
        project: Option<&str>,
        selector: &str,
        owner: OwnerKind,
        actor: Option<&str>,
    ) -> Result<Item> {
        let (project_id, _key) = self.require_project_mut(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        self.conn.execute(
            "UPDATE nodes SET owner_kind = ?1, assignee = ?2, revision = revision + 1,
                 updated_at = datetime('now'), sync_state = 'local' WHERE id = ?3",
            params![owner, actor, node_id],
        )?;
        self.get_item(project, selector, &[])
    }

    /// Atomic handoff — the baton-pass when a node changes hands (ai↔human).
    /// Does in one call the three steps agents otherwise do inconsistently:
    /// records a `handoff` artifact (the note, stamped with `from`/`to`), flips
    /// the owner, and sets the wait-state/status. Direction-aware defaults (an
    /// explicit `status`/`wait` in [`HandoffInput`] overrides them): handing to a
    /// human parks it `blocked` + `on_human` (now waiting on them); handing to ai
    /// clears the wait and, if it was `blocked`, makes it `ready` (actionable
    /// again). Returns the updated item + the handoff artifact (when a note given).
    ///
    /// Best-effort composition, not a single transaction: a handoff spans the
    /// filesystem (the artifact file) and several row writes, so it can't be one
    /// atomic unit. Ordered artifact → owner → state so a mid-way failure leaves
    /// the most recoverable state (the note is always recorded first).
    pub fn handoff(
        &self,
        project: Option<&str>,
        selector: &str,
        to: OwnerKind,
        opts: HandoffInput,
    ) -> Result<HandoffResult> {
        // Validate the item exists up front (clean error before any write), and
        // read the current owner/status to default `from` and the new state.
        let before = self.get_item(project, selector, &[])?;
        let from = opts
            .from
            .or(before.owner_kind)
            .unwrap_or_else(|| to.opposite());

        // 1. Record the baton note as a handoff artifact (preserves from/to+text).
        //    A unique filename so successive handoffs don't overwrite each other.
        let artifact = match opts.note {
            Some(note) => Some(self.add_artifact(
                project,
                selector,
                NewArtifact {
                    role: ArtifactRole::Handoff,
                    kind: ArtifactKind::File,
                    content: Some(note.to_string()),
                    name: Some(format!("handoff-{}.md", epoch_millis())),
                    from_owner: Some(from),
                    to_owner: Some(to),
                    created_by: opts.actor.map(String::from),
                    ..Default::default()
                },
            )?),
            None => None,
        };

        // 2. Flip ownership.
        self.assign_item(project, selector, to, opts.actor)?;

        // 3. Wait-state + status, direction-aware (see the doc comment). An
        //    explicit caller value always wins over the default.
        let wait = match opts.wait {
            Some(w) => WaitUpdate::Set(w),
            None if to == OwnerKind::Human => WaitUpdate::Set(WaitState::OnHuman),
            None => WaitUpdate::Clear,
        };
        let status = opts.status.or_else(|| match to {
            OwnerKind::Human => Some(Status::Blocked),
            OwnerKind::Ai => (before.status == Status::Blocked).then_some(Status::Ready),
        });
        let item = self.update_item(
            project,
            selector,
            ItemUpdate {
                status,
                wait: Some(wait),
                ..Default::default()
            },
        )?;

        Ok(HandoffResult { item, artifact })
    }

    /// Mark an item done — the reliable "I finished this" path. Records the
    /// `evidence` as an artifact (default role `delivery`) so completion is
    /// auditable, sets status `done`, and returns the items/gates this unblocks
    /// (their last open dependency just closed) so an agent loop knows what's now
    /// actionable. Warns — but does not refuse — when no acceptance
    /// (`done_looks_like`) was ever set. Refuses to complete a superseded or
    /// archived item (reopen it first).
    ///
    /// Like [`Store::handoff`], a best-effort composition (artifact write + row
    /// updates), not one transaction.
    pub fn complete_item(
        &self,
        project: Option<&str>,
        selector: &str,
        input: CompleteInput,
    ) -> Result<CompleteResult> {
        let (project_id, _key) = self.require_project_mut(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        let before = self.get_item(project, selector, &[])?;
        if matches!(before.status, Status::Superseded | Status::Archived) {
            return Err(HavenError::Invalid(format!(
                "cannot complete a {} item; reopen it first",
                before.status.as_str()
            )));
        }
        self.refuse_artifact_bearing_anchor(node_id, "complete")?;

        let mut warnings = Vec::new();
        if before.done_looks_like.is_none() {
            warnings.push(
                "no acceptance was set (done_looks_like) — completing without a verifiable anchor"
                    .to_string(),
            );
        }

        // 1. Record evidence as an artifact (default role: delivery).
        let artifact = match input.evidence {
            Some(evidence) => Some(self.add_artifact(
                project,
                selector,
                NewArtifact {
                    role: input.artifact_role.unwrap_or(ArtifactRole::Delivery),
                    kind: ArtifactKind::File,
                    content: Some(evidence.to_string()),
                    created_by: input.by.map(String::from),
                    ..Default::default()
                },
            )?),
            None => None,
        };

        // 2. Mark it done.
        let item = self.update_item(
            project,
            selector,
            ItemUpdate {
                status: Some(Status::Done),
                ..Default::default()
            },
        )?;

        // 3. What did finishing this unblock? (Query after the status flip so the
        //    NOT EXISTS sees this item as complete.)
        let unblocked = self.unblocked_dependents(node_id)?;

        Ok(CompleteResult {
            item,
            artifact,
            unblocked,
            warnings,
        })
    }

    fn refuse_artifact_bearing_anchor(&self, node_id: i64, op: &str) -> Result<()> {
        let (node_type, artifact_count): (NodeType, i64) = self.conn.query_row(
            "SELECT type, (SELECT count(*) FROM artifacts WHERE node_id = nodes.id)
             FROM nodes WHERE id = ?1",
            [node_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if matches!(node_type, NodeType::Anchor) && artifact_count > 0 {
            return Err(HavenError::Invalid(format!(
                "cannot {op} an artifact-bearing anchor; move or remove its artifacts first"
            )));
        }
        Ok(())
    }

    /// Items that depend on `completed_id` and now have no remaining open
    /// dependency — i.e. became dispatchable/reviewable when it completed. Run
    /// after `completed_id` is marked done. Includes gate nodes whose triggers
    /// are all complete.
    fn unblocked_dependents(&self, completed_id: i64) -> Result<Vec<Item>> {
        let sql = format!(
            "SELECT {ITEM_SELECT} FROM {ITEM_FROM}
             WHERE n.id IN (SELECT node_id FROM dependency_edges WHERE depends_on_id = ?1)
               AND n.type <> 'anchor'
               AND n.status NOT IN ('done','superseded','archived')
               AND NOT EXISTS (
                 SELECT 1 FROM dependency_edges d2
                 JOIN nodes p ON p.id = d2.depends_on_id
                 WHERE d2.node_id = n.id AND p.status NOT IN ('done','superseded','archived')
               )
             ORDER BY n.id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([completed_id], item_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Set `sort_key` so the item sorts immediately before/after `target`.
    pub fn rank_item(
        &self,
        project: Option<&str>,
        selector: &str,
        before: Option<&str>,
        after: Option<&str>,
    ) -> Result<Item> {
        let (project_id, _key) = self.require_project_mut(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        let (target_sel, is_before) = match (before, after) {
            (Some(t), None) => (t, true),
            (None, Some(t)) => (t, false),
            _ => {
                return Err(HavenError::Invalid(
                    "rank requires exactly one of --before/--after".into(),
                ))
            }
        };
        let target_id = self.resolve_node_id(project_id, target_sel)?;
        if target_id == node_id {
            return Err(HavenError::Invalid(
                "cannot rank an item relative to itself".into(),
            ));
        }

        let (node_type, target_type): (NodeType, NodeType) = self.conn.query_row(
            "SELECT n.type, t.type FROM nodes n JOIN nodes t ON t.id = ?2 WHERE n.id = ?1",
            params![node_id, target_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if matches!(node_type, NodeType::Anchor) || matches!(target_type, NodeType::Anchor) {
            return Err(HavenError::Invalid(
                "anchor nodes cannot be ranked or used as rank targets".into(),
            ));
        }

        // Ensure the target has a key to anchor against. Fetch its priority band
        // too: `sort_key` is fine ordering *within a band* (SPEC §0 Q2), so the
        // gap search is scoped to the target's band — a key from another band must
        // not narrow the gap (it would burn key space without affecting order).
        let (mut target_key, target_priority): (Option<String>, Option<i64>) =
            self.conn.query_row(
                "SELECT sort_key, priority FROM nodes WHERE id = ?1",
                [target_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
        if target_key.is_none() {
            let k = sortkey::between(None, None)?;
            self.conn.execute(
                "UPDATE nodes SET sort_key = ?1, revision = revision + 1,
                     updated_at = datetime('now'), sync_state = 'local' WHERE id = ?2",
                params![k, target_id],
            )?;
            target_key = Some(k);
        }
        let target_key = target_key.unwrap();

        // Find the neighbour on the far side of the target, in the same band,
        // excluding the moving node; mint a key in the gap. `priority IS ?4` is
        // SQLite NULL-safe equality, so the unprioritised band matches too.
        let new_key = if is_before {
            let lower: Option<String> = self.conn.query_row(
                "SELECT max(sort_key) FROM nodes
                 WHERE project_id = ?1 AND sort_key < ?2 AND id <> ?3 AND priority IS ?4",
                params![project_id, target_key, node_id, target_priority],
                |r| r.get(0),
            )?;
            sortkey::between(lower.as_deref(), Some(&target_key))?
        } else {
            let upper: Option<String> = self.conn.query_row(
                "SELECT min(sort_key) FROM nodes
                 WHERE project_id = ?1 AND sort_key > ?2 AND id <> ?3 AND priority IS ?4",
                params![project_id, target_key, node_id, target_priority],
                |r| r.get(0),
            )?;
            sortkey::between(Some(&target_key), upper.as_deref())?
        };

        self.conn.execute(
            "UPDATE nodes SET sort_key = ?1, revision = revision + 1,
                 updated_at = datetime('now'), sync_state = 'local' WHERE id = ?2",
            params![new_key, node_id],
        )?;
        self.get_item(project, selector, &[])
    }

    /// Archive: status → archived, stamp `archived_at`, emit a lineage `archive`
    /// event (never hard-delete, SPEC §3).
    pub fn archive_item(
        &self,
        project: Option<&str>,
        selector: &str,
        rationale: Option<&str>,
        by: Option<&str>,
    ) -> Result<Item> {
        let (project_id, _key) = self.require_project_mut(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        self.refuse_artifact_bearing_anchor(node_id, "archive")?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE nodes SET status = 'archived', archived_at = datetime('now'),
                 revision = revision + 1, updated_at = datetime('now'), sync_state = 'local'
             WHERE id = ?1",
            [node_id],
        )?;
        self.record_event(
            &tx,
            project_id,
            EventType::Archive,
            rationale,
            by,
            &serde_json::json!({}),
            &[(node_id, node_id)],
        )?;
        tx.commit()?;
        self.get_item(project, selector, &[])
    }

    /// Archive several items in one grooming step ("archive those"). Refs are
    /// validated up front, so a typo aborts the batch before any item is parked.
    /// Each archival is its own transaction (they don't share one); the shared
    /// `rationale`/`by` apply to all. Returns the archived items.
    pub fn archive_items(
        &self,
        project: Option<&str>,
        refs: &[&str],
        rationale: Option<&str>,
        by: Option<&str>,
    ) -> Result<Vec<Item>> {
        self.validate_refs(project, refs)?;
        refs.iter()
            .map(|r| self.archive_item(project, r, rationale, by))
            .collect()
    }

    /// Reopen an archived/superseded item back into the maturity flow.
    pub fn reopen_item(
        &self,
        project: Option<&str>,
        selector: &str,
        rationale: Option<&str>,
        by: Option<&str>,
    ) -> Result<Item> {
        let (project_id, _key) = self.require_project_mut(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE nodes SET status = 'discovery', archived_at = NULL,
                 revision = revision + 1, updated_at = datetime('now'), sync_state = 'local'
             WHERE id = ?1",
            [node_id],
        )?;
        self.record_event(
            &tx,
            project_id,
            EventType::Reopen,
            rationale,
            by,
            &serde_json::json!({}),
            &[(node_id, node_id)],
        )?;
        tx.commit()?;
        self.get_item(project, selector, &[])
    }

    /// Mint a `ref` and insert a node row on `conn`. Shared by `add_item` and
    /// the evolve ops so node creation has exactly one implementation.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_node(
        &self,
        conn: &Connection,
        project_id: i64,
        title: &str,
        node_type: NodeType,
        status: Status,
        body: Option<&str>,
        done_looks_like: Option<&str>,
        why: Option<&str>,
        owner: Option<OwnerKind>,
        committed: bool,
        priority: Option<i64>,
        due_at: Option<&str>,
        metadata: &serde_json::Value,
    ) -> Result<(i64, String)> {
        conn.execute(
            "UPDATE projects
             SET ref_counter = ref_counter + 1, revision = revision + 1,
                 updated_at = datetime('now'), sync_state = 'local'
             WHERE id = ?1",
            [project_id],
        )?;
        let (prefix, counter): (String, i64) = conn.query_row(
            "SELECT ref_prefix, ref_counter FROM projects WHERE id = ?1",
            [project_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let reference = format!("{prefix}-{counter}");
        conn.execute(
            "INSERT INTO nodes
               (public_id, project_id, ref, title, body, done_looks_like, why,
                type, status, owner_kind, committed, priority, metadata, due_at, client_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                new_uuid(),
                project_id,
                reference,
                title,
                body,
                done_looks_like,
                why,
                node_type,
                status,
                owner,
                committed as i64,
                priority,
                metadata.to_string(),
                due_at,
                new_uuid(),
            ],
        )?;
        Ok((conn.last_insert_rowid(), reference))
    }

    // ---- artifacts read (file add/get is Layer 4) -------------------------

    pub(crate) fn load_artifacts(&self, node_id: i64) -> Result<Vec<Artifact>> {
        let node_ref = self.node_ref(node_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT id, public_id, role, kind, path, uri, title, excerpt,
                    from_owner, to_owner, content_hash, remote_path,
                    created_at, created_by, revision, sync_state, metadata
             FROM artifacts WHERE node_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([node_id], |row: &Row<'_>| {
            let metadata_str: String = row.get(16)?;
            let metadata = parse_metadata(&metadata_str).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    16,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(Artifact {
                id: row.get(0)?,
                public_id: row.get(1)?,
                node_ref: Some(node_ref.clone()),
                role: row.get(2)?,
                kind: row.get(3)?,
                path: row.get(4)?,
                uri: row.get(5)?,
                title: row.get(6)?,
                excerpt: row.get(7)?,
                from_owner: row.get(8)?,
                to_owner: row.get(9)?,
                content_hash: row.get(10)?,
                remote_path: row.get(11)?,
                metadata,
                created_at: row.get(12)?,
                created_by: row.get(13)?,
                revision: row.get(14)?,
                sync_state: row.get(15)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// Parse the `artifacts.metadata` TEXT column into `Option<Value>`. The DDL
/// default is `'{}'`, and we normalize empty / `{}` / `null` to `None` so an
/// artifact carrying no metadata serializes byte-identically to before the
/// field existed. Parsed leniently as a raw [`serde_json::Value`] (not the typed
/// xref struct) so a malformed xref arriving via raw DB / sync still loads and is
/// reported by the doctor scan rather than failing the whole read.
pub(crate) fn parse_metadata(s: &str) -> serde_json::Result<Option<serde_json::Value>> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_str(trimmed)?;
    Ok(match &value {
        serde_json::Value::Null => None,
        serde_json::Value::Object(map) if map.is_empty() => None,
        _ => Some(value),
    })
}

/// What to hydrate on `item get` (SPEC §2 `--include`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Include {
    Edges,
    Artifacts,
    Lineage,
}

impl Include {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "edges" => Ok(Include::Edges),
            "artifacts" => Ok(Include::Artifacts),
            "lineage" => Ok(Include::Lineage),
            other => Err(HavenError::Invalid(format!(
                "unknown include {other:?} — valid: edges, artifacts, lineage"
            ))),
        }
    }
}

#[cfg(test)]
mod include_tests {
    use super::*;

    /// HV-152: an unknown include names the legal set inline.
    #[test]
    fn include_parse_error_names_the_legal_set() {
        let err = Include::parse("comments").unwrap_err().to_string();
        assert!(err.contains("comments"), "names the bad value: {err}");
        for v in ["edges", "artifacts", "lineage"] {
            assert!(err.contains(v), "include error must name {v:?}: {err}");
        }
    }
}
