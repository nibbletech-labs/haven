//! Bulk item import: one validated batch → one all-or-nothing transaction.
//! Items may carry a file-local temp `id` so edges (`parent` / `depends_on` /
//! `group`) can reference batch siblings — including forward references —
//! alongside existing refs.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::error::{HavenError, Result};
use crate::model::*;

use super::item::{acceptance_blank, normalize_title};
use super::Store;

/// "Engaged" statuses an item must not be born in via import (HV-159): these
/// imply work already underway/finished, which a bulk capture has no business
/// minting at birth. Mirrors the spirit of [`Store::add_item`]'s born-state
/// rules (`ready` needs acceptance) — the same guard now applies to import so a
/// future `haven_import` (HV-155) inherits it from this shared core path.
fn engaged_status(status: Status) -> bool {
    matches!(status, Status::InProgress | Status::Blocked | Status::Done)
}

/// One item in an import file. Mirrors the `item add` surface, plus `id` (a
/// temp id local to the file) and ref-or-temp-id edge fields. Unknown keys are
/// rejected so a typo'd field fails the batch loudly instead of being dropped.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportItem {
    pub title: String,
    /// Temp id, local to this file — lets other items in the batch reference
    /// this one in edge fields before (or after) it appears.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, rename = "type")]
    pub node_type: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub done_looks_like: Option<String>,
    #[serde(default)]
    pub why: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub commit: bool,
    #[serde(default)]
    pub assign: Option<String>,
    /// Decomposition parent: an existing ref or a temp id from this file.
    #[serde(default)]
    pub parent: Option<String>,
    /// Dependencies: existing refs or temp ids from this file.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Grouping container: an existing ref or a temp id from this file.
    #[serde(default)]
    pub group: Option<String>,
}

/// One import result, in input order. No `similar` warnings here — in a batch
/// they would mostly echo batch siblings; `if_absent` is the bulk dedupe
/// mechanism.
#[derive(Debug, Serialize)]
pub struct ImportOutcome {
    /// The temp id from the input, echoed back for correlation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(flatten)]
    pub item: Item,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub existing: bool,
}

/// Enum fields parsed up front so the whole file validates before any write.
struct ParsedAxes {
    node_type: NodeType,
    status: Status,
    assign: Option<OwnerKind>,
}

impl Store {
    /// Import a batch of items in one transaction. Everything is validated
    /// first (titles, enums, temp-id uniqueness and shadowing, edge targets);
    /// any failure — including a cycle detected while wiring edges — rolls the
    /// whole batch back, ref counter included. With `if_absent`, items whose
    /// normalized title matches a live item (pre-existing or earlier in the
    /// batch) are not created; their temp ids resolve to the matched node.
    pub fn import_items(
        &self,
        project: Option<&str>,
        items: Vec<ImportItem>,
        if_absent: bool,
    ) -> Result<Vec<ImportOutcome>> {
        let (project_id, _key) = self.require_project_mut(project)?;
        if items.is_empty() {
            return Ok(vec![]);
        }

        // -- Validation pass: no writes. -----------------------------------
        let mut temp_ids: HashSet<&str> = HashSet::new();
        let mut axes: Vec<ParsedAxes> = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            if item.title.trim().is_empty() {
                return Err(HavenError::Invalid(format!(
                    "items[{i}]: title must not be empty"
                )));
            }
            if let Some(id) = item.id.as_deref() {
                if !temp_ids.insert(id) {
                    return Err(HavenError::Invalid(format!(
                        "items[{i}]: duplicate temp id {id:?}"
                    )));
                }
                // A temp id that already resolves as a real ref/public_id would
                // make edge targets ambiguous.
                if self.resolve_node_id(project_id, id).is_ok() {
                    return Err(HavenError::Invalid(format!(
                        "items[{i}]: temp id {id:?} shadows an existing item"
                    )));
                }
            }
            let node_type = item
                .node_type
                .as_deref()
                .map(NodeType::parse)
                .transpose()
                .map_err(|e| HavenError::Invalid(format!("items[{i}]: {e}")))?
                .unwrap_or(NodeType::Task);
            let status = item
                .status
                .as_deref()
                .map(Status::parse)
                .transpose()
                .map_err(|e| HavenError::Invalid(format!("items[{i}]: {e}")))?
                .unwrap_or(Status::Discovery);
            let assign = item
                .assign
                .as_deref()
                .map(OwnerKind::parse)
                .transpose()
                .map_err(|e| HavenError::Invalid(format!("items[{i}]: {e}")))?;

            // HV-159: a bulk import must not silently mint items in an "engaged"
            // born-state. Hard-reject (matching add_item's spirit — no opt-in
            // flag): an item born `in_progress`/`blocked`/`done` or `commit:true`
            // implies work already underway, which capture doesn't create; and an
            // item born `ready` still needs acceptance (HV-80, the same guard
            // add_item enforces — reusing `acceptance_blank`). The error names the
            // offending item by its temp id, else its batch index. Living in this
            // validation pass (before any write) keeps the whole batch atomic.
            let label = match item.id.as_deref() {
                Some(id) => format!("{id:?}"),
                None => format!("index {i}"),
            };
            if engaged_status(status) {
                return Err(HavenError::Invalid(format!(
                    "items[{i}]: cannot import item {label} born {} — \
                     bulk import must not mint items in an engaged state \
                     (in_progress/blocked/done); import at discovery/definition/ready \
                     and advance it afterwards",
                    status.as_str(),
                )));
            }
            if item.commit {
                return Err(HavenError::Invalid(format!(
                    "items[{i}]: cannot import item {label} with commit:true — \
                     bulk import must not mint committed items; import uncommitted \
                     and commit it afterwards",
                )));
            }
            if matches!(status, Status::Ready) && acceptance_blank(item.done_looks_like.as_deref())
            {
                return Err(HavenError::Invalid(format!(
                    "items[{i}]: cannot import item {label} as ready without acceptance \
                     — set done_looks_like first",
                )));
            }

            axes.push(ParsedAxes {
                node_type,
                status,
                assign,
            });
        }
        // Every edge target must be a temp id from this file or resolve in the
        // project. (Temp-id targets may appear later in the file than their
        // referrers — forward references are fine.)
        let mut existing_targets: HashMap<&str, i64> = HashMap::new();
        for (i, item) in items.iter().enumerate() {
            let targets = item
                .parent
                .iter()
                .chain(item.depends_on.iter())
                .chain(item.group.iter());
            for target in targets {
                if temp_ids.contains(target.as_str())
                    || existing_targets.contains_key(target.as_str())
                {
                    continue;
                }
                let node_id = self.resolve_node_id(project_id, target).map_err(|_| {
                    HavenError::Invalid(format!(
                        "items[{i}]: edge target {target:?} is neither a temp id in this file nor an existing item"
                    ))
                })?;
                existing_targets.insert(target.as_str(), node_id);
            }
        }

        // -- One transaction: insert all nodes, then wire all edges. -------
        // Dropping `tx` on any `?` rolls everything back, ref counter included.
        let tx = self.conn.unchecked_transaction()?;
        let mut temp_map: HashMap<&str, i64> = HashMap::new();
        let mut seen_titles: HashMap<String, i64> = HashMap::new();
        // (node_id, existing) per input item, in order.
        let mut inserted: Vec<(i64, bool)> = Vec::with_capacity(items.len());

        for (item, parsed) in items.iter().zip(&axes) {
            let norm = normalize_title(&item.title);
            let matched = if if_absent && !norm.is_empty() {
                match seen_titles.get(&norm) {
                    Some(&id) => Some(id),
                    None => self.find_live_by_norm_title(project_id, &norm)?,
                }
            } else {
                None
            };
            let (node_id, existing) = match matched {
                Some(id) => (id, true),
                None => {
                    let (id, _reference) = self.insert_node(
                        &tx,
                        project_id,
                        &item.title,
                        parsed.node_type,
                        parsed.status,
                        item.body.as_deref(),
                        item.done_looks_like.as_deref(),
                        item.why.as_deref(),
                        parsed.assign,
                        item.commit,
                        item.priority,
                        None, // due_at — not an import field
                        &serde_json::json!({}),
                    )?;
                    (id, false)
                }
            };
            if !norm.is_empty() {
                seen_titles.entry(norm).or_insert(node_id);
            }
            if let Some(id) = item.id.as_deref() {
                temp_map.insert(id, node_id);
            }
            inserted.push((node_id, existing));
        }

        let resolve_target = |target: &str| -> i64 {
            temp_map
                .get(target)
                .or_else(|| existing_targets.get(target))
                .copied()
                .expect("edge targets were validated before the insert pass")
        };
        for (item, &(node_id, _)) in items.iter().zip(&inserted) {
            if let Some(parent) = &item.parent {
                self.insert_decomposition(&tx, resolve_target(parent), node_id)?;
            }
            for dep in &item.depends_on {
                self.insert_dependency(&tx, node_id, resolve_target(dep))?;
            }
            if let Some(group) = &item.group {
                self.insert_grouping(&tx, resolve_target(group), node_id)?;
            }
        }
        tx.commit()?;

        items
            .iter()
            .zip(inserted)
            .map(|(item, (node_id, existing))| {
                let reference = self.node_ref(node_id)?;
                Ok(ImportOutcome {
                    id: item.id.clone(),
                    item: self.get_item(project, &reference, &[])?,
                    existing,
                })
            })
            .collect()
    }
}
