//! `haven prime` (HV-23): one-shot session-context injection. Assembles a single
//! compact, token-budgeted block — project state, the committed queue with the
//! next-eligible items flagged, in-progress/waiting work with owners, the
//! load-bearing conventions, and a compact untriaged-inbox view — so a fresh
//! agent reads ONE block instead of making N discovery calls (`status` + `next` +
//! `list` + `inbox` + the conventions every session).
//!
//! Everything here is a *projection*: `prime` reuses the existing query surfaces
//! (`store_status`, `next`, `list_items`, and the HV-82 `grooming_pressure`
//! machinery) rather than re-deriving any of them, so it can never disagree with
//! the canonical views. The CLI prints [`Prime::render`] raw; the MCP tool returns
//! the same rendered block.

use std::collections::HashSet;

use serde::Serialize;

use crate::error::Result;
use crate::model::*;

use super::{ItemFilter, Store};

/// How many committed-ready queue items to show before truncating (token budget).
const PRIME_QUEUE_CAP: usize = 8;
/// How many in-progress / waiting items to show before truncating.
const PRIME_ACTIVE_CAP: usize = 8;
/// How many untriaged inbox floaters to surface for triage.
const PRIME_INBOX_CAP: usize = 5;

/// One queue line in [`Prime`]: a committed-ready item, flagged when it is one of
/// the dispatch-eligible items `next` would actually return.
#[derive(Debug, Clone, Serialize)]
pub struct PrimeQueueItem {
    #[serde(rename = "ref")]
    pub reference: String,
    pub title: String,
    /// True for the items `next` returns (committed, ready, unblocked, not
    /// waiting) — the ones an agent can dispatch right now.
    pub next_eligible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<OwnerKind>,
}

/// One active/parked line in [`Prime`]: an `in_progress` item or one parked on a
/// `wait_state`, carrying its owner and what it's waiting on.
#[derive(Debug, Clone, Serialize)]
pub struct PrimeActiveItem {
    #[serde(rename = "ref")]
    pub reference: String,
    pub title: String,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<OwnerKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<WaitState>,
}

/// A compact inbox floater for the triage view.
#[derive(Debug, Clone, Serialize)]
pub struct PrimeInboxItem {
    #[serde(rename = "ref")]
    pub reference: String,
    pub title: String,
}

/// The assembled session-context block (HV-23). Serializable for a structured
/// client, with [`Prime::render`] producing the compact text a fresh agent reads.
#[derive(Debug, Clone, Serialize)]
pub struct Prime {
    /// Project key, e.g. `haven`.
    pub project: String,
    /// Project ref prefix, e.g. `HV`.
    pub prefix: String,
    pub total: i64,
    pub committed: i64,
    pub icebox: i64,
    pub sync_pending: i64,
    /// Committed-ready queue (next-eligible flagged), capped to [`PRIME_QUEUE_CAP`].
    pub queue: Vec<PrimeQueueItem>,
    /// Total committed-ready items before truncation (so the cap is never silent).
    pub queue_total: usize,
    /// How many of `queue_total` are dispatch-eligible right now (`next`).
    pub next_eligible_total: usize,
    /// In-progress + waiting items, capped to [`PRIME_ACTIVE_CAP`].
    pub active: Vec<PrimeActiveItem>,
    pub active_total: usize,
    /// Untriaged inbox count (the HV-82 grooming-pressure `untriaged` signal).
    pub inbox_untriaged: usize,
    /// Top untriaged floaters to triage, capped to [`PRIME_INBOX_CAP`].
    pub inbox: Vec<PrimeInboxItem>,
    /// A grooming nudge string when untriaged/stale work has piled up (HV-82).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grooming_nudge: Option<String>,
}

/// The load-bearing conventions a fresh agent needs at session start. Kept tight
/// — a handful of lines, not a manual. These mirror the project conventions an
/// agent would otherwise have to be told (or rediscover) every session.
const CONVENTIONS: &[&str] = &[
    "Mutate the graph ONLY via haven ops; never hand-edit the DB.",
    "body is a one-line summary, NOT content — real content lives in files/artifacts under the item.",
    "Close the loop: finish work with `item complete <ref> --evidence \"…\"` (it reports what it unblocked).",
    "Capture findings/gaps as floating items as they surface — don't just mention them in chat.",
    "CLI has a sticky current-project: pass -p <project> explicitly; MCP is per-call (project arg).",
];

impl Store {
    /// Assemble the one-shot session-context block (HV-23). Pure projection over
    /// the existing read surfaces — never a new source of truth.
    pub fn prime(&self, project: Option<&str>) -> Result<Prime> {
        // §1 Project + one-line state. Reuse store_status (and resolve the prefix
        // from the project record) rather than re-counting here.
        let (_project_id, key) = self.require_project(project)?;
        let status = self.store_status(project)?;
        let prefix = self.get_project(&key)?.ref_prefix;
        let get_i64 = |k: &str| status.get(k).and_then(|v| v.as_i64()).unwrap_or(0);

        // §2 Committed-ready queue with next-eligible flagged. `next` is the
        // canonical dispatch set (committed + ready + unblocked + not waiting);
        // the broader committed-ready list is the same band minus those gates, so
        // flagging by membership keeps prime in lockstep with the real queue.
        let next_eligible: HashSet<String> = self
            .next(project, None, None)?
            .into_iter()
            .map(|i| i.reference)
            .collect();
        let committed_ready = self.list_items(
            project,
            &ItemFilter {
                status: Some(Status::Ready),
                committed: Some(true),
                ..Default::default()
            },
        )?;
        let queue_total = committed_ready.len();
        let next_eligible_total = next_eligible.len();
        let queue: Vec<PrimeQueueItem> = committed_ready
            .into_iter()
            .take(PRIME_QUEUE_CAP)
            .map(|i| PrimeQueueItem {
                next_eligible: next_eligible.contains(&i.reference),
                reference: i.reference,
                title: i.title,
                owner: i.owner_kind,
            })
            .collect();

        // §3 In-progress + waiting, with owner. Union of the in_progress items and
        // any item carrying a wait_state (parked, regardless of status), deduped.
        let in_progress = self.list_items(
            project,
            &ItemFilter {
                status: Some(Status::InProgress),
                ..Default::default()
            },
        )?;
        let mut active: Vec<PrimeActiveItem> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut push = |i: Item, active: &mut Vec<PrimeActiveItem>| {
            if seen.insert(i.reference.clone()) {
                active.push(PrimeActiveItem {
                    reference: i.reference,
                    title: i.title,
                    status: i.status,
                    owner: i.owner_kind,
                    wait: i.wait_state,
                });
            }
        };
        for i in in_progress {
            push(i, &mut active);
        }
        for wait in [
            WaitState::OnHuman,
            WaitState::OnDependency,
            WaitState::OnExternal,
        ] {
            let waiting = self.list_items(
                project,
                &ItemFilter {
                    wait: Some(wait),
                    ..Default::default()
                },
            )?;
            for i in waiting {
                push(i, &mut active);
            }
        }
        let active_total = active.len();
        active.truncate(PRIME_ACTIVE_CAP);

        // §5 Untriaged-inbox view. Reuse the HV-82 grooming-pressure query for the
        // count + nudge, and the same inbox-floater filter it counts over for the
        // top few to triage — so handoff-swept captures resurface on resume.
        let pressure = self.grooming_pressure(project)?;
        let inbox: Vec<PrimeInboxItem> = self
            .list_items(
                project,
                &ItemFilter {
                    inbox: true,
                    ..Default::default()
                },
            )?
            .into_iter()
            .take(PRIME_INBOX_CAP)
            .map(|i| PrimeInboxItem {
                reference: i.reference,
                title: i.title,
            })
            .collect();

        Ok(Prime {
            project: key,
            prefix,
            total: get_i64("total"),
            committed: get_i64("committed"),
            icebox: get_i64("icebox"),
            sync_pending: get_i64("sync_pending"),
            queue,
            queue_total,
            next_eligible_total,
            active,
            active_total,
            inbox_untriaged: pressure.untriaged,
            inbox,
            grooming_nudge: pressure.nudge,
        })
    }
}

impl Prime {
    /// Render the compact, token-budgeted session-context block (HV-23). One block
    /// a fresh agent reads instead of N discovery calls — terse, one line per item,
    /// every list capped with an explicit truncation note.
    pub fn render(&self) -> String {
        self.render_with_sync(false)
    }

    /// Render with the internal sync queue exposed. This is deliberately opt-in:
    /// public local installs should not see "sync pending" for ordinary local rows
    /// while Cloud Sync remains preview-only.
    pub fn render_with_sync(&self, include_sync: bool) -> String {
        let mut out = String::new();

        // §1 Project + one-line state.
        let sync = if include_sync {
            let state = if self.sync_pending == 0 {
                "clean".to_string()
            } else {
                format!("{} pending", self.sync_pending)
            };
            format!(", sync {state}")
        } else {
            String::new()
        };
        out.push_str(&format!(
            "PROJECT {} ({}) — {} items, {} committed, {} icebox{}\n",
            self.project, self.prefix, self.total, self.committed, self.icebox, sync,
        ));

        // §2 Committed queue with next-eligible flagged ('>' = dispatchable now).
        out.push_str(&format!(
            "\nQUEUE (committed-ready: {}, dispatchable now: {}) — '>' = next-eligible\n",
            self.queue_total, self.next_eligible_total,
        ));
        if self.queue.is_empty() {
            out.push_str("  (none)\n");
        } else {
            for q in &self.queue {
                out.push_str(&format!(
                    "  {} {}  {}{}\n",
                    if q.next_eligible { ">" } else { " " },
                    q.reference,
                    q.title,
                    q.owner
                        .map(|o| format!(" [{}]", o.as_str()))
                        .unwrap_or_default(),
                ));
            }
            if self.queue_total > self.queue.len() {
                out.push_str(&format!(
                    "  … +{} more (haven next / item list)\n",
                    self.queue_total - self.queue.len()
                ));
            }
        }

        // §3 In-progress / waiting, with owner + what it's waiting on.
        out.push_str(&format!(
            "\nIN-PROGRESS / WAITING ({})\n",
            self.active_total
        ));
        if self.active.is_empty() {
            out.push_str("  (none)\n");
        } else {
            for a in &self.active {
                let owner = a
                    .owner
                    .map(|o| o.as_str())
                    .unwrap_or("unassigned")
                    .to_string();
                let wait = a
                    .wait
                    .map(|w| format!(" waiting:{}", w.as_str()))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "  {} {}  [{}{}] {}\n",
                    a.reference,
                    a.title,
                    owner,
                    wait,
                    a.status.as_str(),
                ));
            }
            if self.active_total > self.active.len() {
                out.push_str(&format!(
                    "  … +{} more\n",
                    self.active_total - self.active.len()
                ));
            }
        }

        // §4 Core conventions (a short fixed block).
        out.push_str("\nCONVENTIONS\n");
        for c in CONVENTIONS {
            out.push_str(&format!("  - {c}\n"));
        }

        // §5 Compact untriaged-inbox view (HV-82 reuse).
        out.push_str(&format!("\nINBOX (untriaged: {})\n", self.inbox_untriaged));
        if self.inbox.is_empty() {
            out.push_str("  (empty)\n");
        } else {
            for f in &self.inbox {
                out.push_str(&format!("  {} {}\n", f.reference, f.title));
            }
            if self.inbox_untriaged > self.inbox.len() {
                out.push_str(&format!(
                    "  … +{} more to triage (haven inbox)\n",
                    self.inbox_untriaged - self.inbox.len()
                ));
            }
        }
        if let Some(nudge) = &self.grooming_nudge {
            out.push_str(&format!("  ! {nudge}\n"));
        }

        out
    }
}
