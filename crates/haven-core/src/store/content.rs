//! Content layer (SPEC §4): the file tree under `~/.haven/<project>/`, artifact
//! registration (typed pointer + file copy + sha256), `note` (free scratch, no
//! DB row), and the deterministic `backlog.md` projection.
//!
//! `Store::content_root()` is the `~/.haven` root; per-project content lives at
//! `<root>/<project-key>/`, and artifact `path`s are stored relative to that.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::PathBuf;

use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::error::{HavenError, Result};
use crate::model::*;
use crate::util::hex;

use super::{new_uuid, ItemFilter, Store};

/// The canonical filename of a context-pack artifact. `create-context-pack` writes
/// it as a `spec` artifact on a group's container; the pack-pointer derivation
/// (HV-75) reads it. This string is the contract between the skill and the store
/// — keep them in sync.
pub const CONTEXT_PACK_ARTIFACT: &str = "context-pack.md";

/// The recognized `xref.store` allowlist (HV-69). `store` is a free string —
/// stores legitimately proliferate — so an unrecognized value is only a doctor
/// *lint*, never a write-time rejection. Seeded with the known stores; easy to
/// extend.
pub const RECOGNIZED_STORES: &[&str] = &["servo", "vault", "github", "haven"];

/// Validate a `metadata.xref` payload on the write path (HV-69). Rejects an
/// unknown `relation` (the closed [`XrefRelation`] enum) or a missing/empty
/// `target`. `store` is free (never rejected) and `canonical` is optional. A
/// metadata object with no `xref` key, or whose `xref` is an empty array, is
/// vacuously valid. The whole `metadata` object must be a JSON object (or null).
pub(crate) fn validate_xref_metadata(meta: &serde_json::Value) -> Result<()> {
    if meta.is_null() {
        return Ok(());
    }
    let obj = meta
        .as_object()
        .ok_or_else(|| HavenError::Invalid("artifact metadata must be a JSON object".into()))?;
    let Some(xref) = obj.get("xref") else {
        return Ok(());
    };
    let arr = xref.as_array().ok_or_else(|| {
        HavenError::Invalid("metadata.xref must be an array of xref objects".into())
    })?;
    for (i, entry) in arr.iter().enumerate() {
        let e = entry
            .as_object()
            .ok_or_else(|| HavenError::Invalid(format!("metadata.xref[{i}] must be an object")))?;
        // `relation` — required, must be a valid closed-enum value.
        let relation = e.get("relation").ok_or_else(|| {
            HavenError::Invalid(format!("metadata.xref[{i}] missing required `relation`"))
        })?;
        if serde_json::from_value::<XrefRelation>(relation.clone()).is_err() {
            return Err(HavenError::Invalid(format!(
                "metadata.xref[{i}] has unknown `relation` {relation} — \
                 must be one of canonical-source|mirror|derived-from|discussed-in"
            )));
        }
        // `target` — required non-empty string.
        match e.get("target").and_then(|t| t.as_str()) {
            Some(t) if !t.trim().is_empty() => {}
            _ => {
                return Err(HavenError::Invalid(format!(
                    "metadata.xref[{i}] missing required non-empty `target`"
                )))
            }
        }
        // `store` — when present, must be a string (free value; not allowlisted here).
        if let Some(store) = e.get("store") {
            if !store.is_string() {
                return Err(HavenError::Invalid(format!(
                    "metadata.xref[{i}] `store` must be a string"
                )));
            }
        }
        // `canonical` — when present, must be a bool.
        if let Some(c) = e.get("canonical") {
            if !c.is_boolean() {
                return Err(HavenError::Invalid(format!(
                    "metadata.xref[{i}] `canonical` must be a boolean"
                )));
            }
        }
    }
    Ok(())
}

/// Whether `target` is shaped like a Haven ref for `prefix` — i.e. `<prefix>-<N>`
/// with a positive integer (HV-69). Used to decide whether a target is *intended*
/// as a Haven ref (existence-checked) vs an opaque cross-store locator
/// (shape-checked only), so a cross-store locator like `Entity:meal/abc` is never
/// false-flagged as dangling.
fn is_haven_ref(prefix: &str, target: &str) -> bool {
    target
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// Normalize a `(store, target)` key component for the canonical-conflict group:
/// trim surrounding whitespace and a single trailing slash so `servo:X` and
/// `servo:X/` collide, but distinct stores/targets never do (HV-69).
fn normalize_key(s: &str) -> String {
    s.trim().trim_end_matches('/').to_string()
}

/// One xref object folded from an artifact's `metadata.xref[]`, parsed
/// **leniently** from raw JSON so a malformed payload (bad `relation`, missing
/// `target`) is carried through and reported by the doctor scan rather than
/// failing the load (HV-69).
struct RawXref {
    node_ref: String,
    artifact: String,
    role: ArtifactRole,
    path: Option<String>,
    /// The raw `relation` string as stored (for diagnostics), `None` if absent.
    relation_raw: Option<String>,
    /// True when `relation` is present but not a valid [`XrefRelation`].
    relation_invalid: bool,
    /// The parsed relation, when valid.
    relation: Option<XrefRelation>,
    store: Option<String>,
    target: Option<String>,
    canonical: bool,
}

impl RawXref {
    /// Lenient parse of one `xref` array entry. Never errors — a non-object entry
    /// yields an all-`None` shell that the doctor reports as structurally invalid.
    fn parse(
        node_ref: String,
        artifact: String,
        role: ArtifactRole,
        path: Option<String>,
        entry: &serde_json::Value,
    ) -> Self {
        let obj = entry.as_object();
        let get_str = |k: &str| -> Option<String> {
            obj.and_then(|o| o.get(k))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let relation_value = obj.and_then(|o| o.get("relation"));
        let relation_raw = relation_value.and_then(|v| v.as_str()).map(str::to_string);
        let relation =
            relation_value.and_then(|v| serde_json::from_value::<XrefRelation>(v.clone()).ok());
        // Invalid only when a relation IS present but doesn't parse. An absent
        // relation is reported via the missing-target/`relation_raw` path; we
        // treat a present-but-bad relation as the dangling trigger.
        let relation_invalid = relation_value.is_some() && relation.is_none();
        let target = get_str("target").filter(|t| !t.trim().is_empty());
        let canonical = obj
            .and_then(|o| o.get("canonical"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        RawXref {
            node_ref,
            artifact,
            role,
            path,
            relation_raw,
            relation_invalid,
            relation,
            store: get_str("store"),
            target,
            canonical,
        }
    }

    /// Project a well-formed raw xref into the typed [`Xref`] for the read verb.
    /// Returns `None` for a structurally-invalid entry (no valid relation or no
    /// target) — the verb reports only well-formed links; the doctor reports the
    /// malformed ones.
    fn to_xref(&self) -> Option<Xref> {
        Some(Xref {
            relation: self.relation?,
            store: self.store.clone().unwrap_or_default(),
            target: self.target.clone()?,
            canonical: self.canonical,
        })
    }
}

/// Parameters for `artifact add`.
#[derive(Debug, Default, Clone)]
pub struct NewArtifact {
    pub role: ArtifactRole,
    pub kind: ArtifactKind,
    /// Source file to copy into the content tree (kind = file).
    pub file: Option<PathBuf>,
    /// Inline content to materialize into a file (kind = file). The content
    /// channel for filesystem-less clients: the bytes are written to a file in
    /// the content tree and only the typed pointer is stored in the DB — the
    /// content itself is never a DB column.
    pub content: Option<String>,
    /// Target filename: for inline `content` (defaults to `<role>.md`) and, when
    /// set, the destination name for a source `file` (else the source basename).
    pub name: Option<String>,
    pub uri: Option<String>,
    pub title: Option<String>,
    pub excerpt: Option<String>,
    pub from_owner: Option<OwnerKind>,
    pub to_owner: Option<OwnerKind>,
    pub created_by: Option<String>,
    /// Free-form JSON sidecar persisted to `artifacts.metadata`. Carries the
    /// typed `xref[]` vocabulary (HV-69). `None` writes the DDL default `'{}'`;
    /// a present `xref` array is validated on write (unknown `relation` /
    /// missing `target` rejected — see [`validate_xref_metadata`]).
    pub metadata: Option<serde_json::Value>,
    /// When the destination `(node, path)` already exists: `false` rejects the
    /// add (the safe default — a duplicate would shadow on read); `true` updates
    /// the existing row in place (rewrite file, recompute hash, bump revision).
    pub replace: bool,
}

/// The bytes (as text) behind an artifact, returned by `artifact get`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArtifactContent {
    pub node_ref: String,
    pub role: ArtifactRole,
    pub path: Option<String>,
    pub uri: Option<String>,
    pub content: Option<String>,
}

// These enums are generated by the `sql_enum!` macro (model.rs), which doesn't
// emit `#[derive(Default)]`; the manual impls give `NewArtifact: Default` a
// sensible role/kind. `derivable_impls` is allowed because the macro can't carry
// a `#[default]` variant marker.
#[allow(clippy::derivable_impls)]
impl Default for ArtifactRole {
    fn default() -> Self {
        ArtifactRole::Scratch
    }
}
#[allow(clippy::derivable_impls)]
impl Default for ArtifactKind {
    fn default() -> Self {
        ArtifactKind::File
    }
}

impl Store {
    /// Absolute path to a project's content directory (`<root>/<key>/`).
    fn project_dir(&self, project_key: &str) -> PathBuf {
        self.content_root().join(project_key)
    }

    /// Derive the context pack governing a leaf (HV-75): the LIVE grouping
    /// containers it belongs to that carry a `context-pack` artifact (HV-124),
    /// deduped by container. Returns `(pack, clash)` of which at most one is
    /// `Some` — zero containers → `(None, None)`; exactly one → the pack; more
    /// than one → a clash (the conflicting container refs). Pure read: the
    /// pointer is never stored, so the graph stays the single source of truth.
    /// Dedup by container means a re-prepped group (which appends a second
    /// `context-pack.md` row on the *same* container) is not read as a clash.
    pub(crate) fn context_pack_for_node(
        &self,
        node_id: i64,
    ) -> Result<(Option<ContextPack>, Option<Vec<String>>)> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT c.ref
               FROM grouping_edges ge
               JOIN nodes c ON c.id = ge.group_id
               JOIN artifacts a ON a.node_id = c.id
              WHERE ge.member_id = ?1
                AND a.role = 'context-pack'
                AND c.status NOT IN ('archived', 'superseded')
              ORDER BY c.ref",
        )?;
        let containers: Vec<String> = stmt
            .query_map([node_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(match containers.len() {
            0 => (None, None),
            1 => (
                Some(ContextPack {
                    container: containers.into_iter().next().expect("len == 1"),
                    artifact: CONTEXT_PACK_ARTIFACT.to_string(),
                }),
                None,
            ),
            _ => (None, Some(containers)),
        })
    }

    /// Public entry to [`Self::context_pack_for_node`] by selector — used by the
    /// CLI and by `create-context-pack`'s clash precondition.
    pub fn context_pack_for(
        &self,
        project: Option<&str>,
        selector: &str,
    ) -> Result<(Option<ContextPack>, Option<Vec<String>>)> {
        let (project_id, _key) = self.require_project(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        self.context_pack_for_node(node_id)
    }

    /// Read-only graph-integrity scan over context packs and artifact rows (HV-105),
    /// surfaced by `haven doctor`. Across every project it flags: a `context-pack`
    /// artifact whose content is a relocation tombstone (left behind when a
    /// subset build-batch is carved out — see `create-context-pack`); a node whose
    /// derived `context_pack` (HV-75) resolves to such a tombstone; and duplicate
    /// `(node, path)` artifact rows. Pure diagnostic — never mutates.
    pub fn context_pack_integrity(&self) -> Result<Vec<IntegrityIssue>> {
        // A context-pack.md whose content opens with one of these (leading
        // whitespace trimmed, case-insensitive) is a relocation tombstone.
        const RELOCATION_MARKERS: &[&str] = &["MOVED", "RELOCATED"];

        let mut issues = Vec::new();
        for proj in self.list_projects()? {
            let base = self.project_dir(&proj.key);

            // (1) Tombstone packs: live containers holding a `context-pack` artifact
            // whose file content opens with a relocation marker.
            let mut stmt = self.conn.prepare(
                "SELECT a.node_id, n.ref, a.path
                   FROM artifacts a
                   JOIN nodes n ON n.id = a.node_id
                  WHERE n.project_id = ?1
                    AND a.role = 'context-pack'
                    AND n.status NOT IN ('archived', 'superseded')",
            )?;
            let packs: Vec<(i64, String, Option<String>)> = stmt
                .query_map([proj.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            let mut tombstone_nodes: Vec<(i64, String)> = Vec::new();
            let mut tombstone_refs: HashSet<String> = HashSet::new();
            for (node_id, node_ref, rel) in &packs {
                let Some(rel) = rel else { continue };
                let full = base.join(rel);
                if !full.starts_with(&base) {
                    continue; // tampered path escaping the project tree — skip
                }
                if let Ok(text) = std::fs::read_to_string(&full) {
                    let head = text.trim_start().to_uppercase();
                    // Dedup by node: a container with two context-pack.md rows (a
                    // duplicate-row case, reported separately) is one tombstone.
                    if RELOCATION_MARKERS.iter().any(|m| head.starts_with(m))
                        && tombstone_refs.insert(node_ref.clone())
                    {
                        tombstone_nodes.push((*node_id, node_ref.clone()));
                        issues.push(IntegrityIssue {
                            kind: IntegrityKind::TombstonePack,
                            node: node_ref.clone(),
                            detail: format!(
                                "{node_ref} carries a context-pack.md whose content is a relocation tombstone, not a real pack — remove it via `haven artifact rm`"
                            ),
                        });
                    }
                }
            }

            // (2) Members whose derived context_pack resolves to a tombstone
            // container — they follow the HV-75 pointer into a dead redirect.
            let mut flagged: HashSet<i64> = HashSet::new();
            for (tomb_id, _) in &tombstone_nodes {
                let mut mstmt = self.conn.prepare(
                    "SELECT ge.member_id, m.ref
                       FROM grouping_edges ge
                       JOIN nodes m ON m.id = ge.member_id
                      WHERE ge.group_id = ?1
                        AND m.status NOT IN ('archived', 'superseded')",
                )?;
                let members: Vec<(i64, String)> = mstmt
                    .query_map([tomb_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (member_id, member_ref) in members {
                    if !flagged.insert(member_id) {
                        continue; // already reported (member under >1 tombstone)
                    }
                    let (pack, clash) = self.context_pack_for_node(member_id)?;
                    let hits = match (&pack, &clash) {
                        (Some(p), _) => tombstone_refs.contains(&p.container),
                        (_, Some(list)) => list.iter().any(|c| tombstone_refs.contains(c)),
                        _ => false,
                    };
                    if hits {
                        issues.push(IntegrityIssue {
                            kind: IntegrityKind::PointerToTombstone,
                            node: member_ref.clone(),
                            detail: format!(
                                "{member_ref} advertises a context_pack that resolves to a relocation tombstone"
                            ),
                        });
                    }
                }
            }

            // (3) Duplicate (node, path) artifact rows.
            let mut dstmt = self.conn.prepare(
                "SELECT n.ref, a.path, COUNT(*) AS c
                   FROM artifacts a
                   JOIN nodes n ON n.id = a.node_id
                  WHERE n.project_id = ?1 AND a.path IS NOT NULL AND a.path <> ''
                  GROUP BY a.node_id, a.path
                 HAVING c > 1
                  ORDER BY n.ref",
            )?;
            let dups: Vec<(String, String, i64)> = dstmt
                .query_map([proj.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (node_ref, path, count) in dups {
                issues.push(IntegrityIssue {
                    kind: IntegrityKind::DuplicateArtifactRow,
                    node: node_ref.clone(),
                    detail: format!(
                        "{node_ref} has {count} artifact rows for the same path {path}"
                    ),
                });
            }
        }
        Ok(issues)
    }

    // ---- xref (HV-69): typed cross-store links in `artifacts.metadata.xref[]` --

    /// Every artifact in a project that carries a `metadata.xref` array, folded
    /// into a flat list of [`RawXref`]s — one per xref object, parsed **leniently**
    /// from raw `serde_json::Value` (not the typed [`Xref`] struct) so a malformed
    /// xref arriving via raw DB / sync is *carried through* (and reported by the
    /// doctor) rather than failing the load. This is the single fold both
    /// [`Store::xref_integrity`] and [`Store::xref`]'s inbound scan share, so the
    /// checker and the verb can never drift.
    fn collect_project_xrefs(&self, project_id: i64) -> Result<Vec<RawXref>> {
        let mut stmt = self.conn.prepare(
            "SELECT n.ref, a.public_id, a.role, a.path, a.metadata
               FROM artifacts a
               JOIN nodes n ON n.id = a.node_id
              WHERE n.project_id = ?1
                AND n.status NOT IN ('archived', 'superseded')
              ORDER BY n.ref, a.public_id",
        )?;
        let rows: Vec<(String, String, ArtifactRole, Option<String>, String)> = stmt
            .query_map([project_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut out = Vec::new();
        for (node_ref, artifact, role, path, metadata) in rows {
            // Lenient: a metadata cell that isn't valid JSON, or whose `xref` is
            // not an array, simply yields no xrefs (it is not a load failure).
            let Ok(value) = serde_json::from_str::<serde_json::Value>(metadata.trim()) else {
                continue;
            };
            let Some(arr) = value.get("xref").and_then(|x| x.as_array()) else {
                continue;
            };
            for entry in arr {
                out.push(RawXref::parse(
                    node_ref.clone(),
                    artifact.clone(),
                    role,
                    path.clone(),
                    entry,
                ));
            }
        }
        Ok(out)
    }

    /// CLI-only `xref_integrity` doctor check (HV-69): a within-Haven, shape-only
    /// scan over every artifact's `metadata.xref[]`. It flags (1) a canonical
    /// conflict — more than one `canonical:true` xref for the same logical
    /// `(store, target)`; (2) a dangling xref — a Haven-ref `target` resolving to
    /// no live node, OR a structurally-invalid xref (missing `target`, or a
    /// `relation` that fails the closed enum); (3) an unknown-`store` lint (warn
    /// only). Cross-store targets are shape-checked only — Haven ships no client
    /// to other stores, so their existence is structurally unverifiable. A clean
    /// store returns an empty vec.
    pub fn xref_integrity(&self) -> Result<Vec<IntegrityIssue>> {
        let mut issues = Vec::new();
        for proj in self.list_projects()? {
            let raws = self.collect_project_xrefs(proj.id)?;

            // (1) Canonical conflict: group canonical:true xrefs by (store, target)
            // and flag any logical key claimed canonical by more than one.
            let mut canonical_by_key: std::collections::BTreeMap<(String, String), Vec<String>> =
                std::collections::BTreeMap::new();
            for raw in &raws {
                if raw.canonical {
                    if let (Some(store), Some(target)) = (&raw.store, &raw.target) {
                        canonical_by_key
                            .entry((normalize_key(store), normalize_key(target)))
                            .or_default()
                            .push(raw.node_ref.clone());
                    }
                }
            }
            for ((store, target), nodes) in &canonical_by_key {
                // Dedup node refs so the same node carrying two canonical xrefs to
                // the same key still reads as one conflicting party per node.
                let mut uniq: Vec<&String> = nodes.iter().collect();
                uniq.sort();
                uniq.dedup();
                if uniq.len() > 1 {
                    let names: Vec<String> = uniq.iter().map(|n| (*n).clone()).collect();
                    for node in &names {
                        issues.push(IntegrityIssue {
                            kind: IntegrityKind::CanonicalConflict,
                            node: node.clone(),
                            detail: format!(
                                "{} artifacts claim canonical:true for the same ({store}, {target}): {}",
                                names.len(),
                                names.join(", ")
                            ),
                        });
                    }
                }
            }

            // (2) Dangling + (3) unknown-store lint, per xref.
            for raw in &raws {
                // Structurally invalid: missing `target`, or a `relation` that is
                // absent or fails the closed enum. `relation.is_none()` covers BOTH
                // an absent relation and an unparseable one (both are required-field
                // violations only reachable via raw DB / sync, since the write path
                // rejects them).
                if raw.target.is_none() || raw.relation.is_none() {
                    let why = if raw.target.is_none() {
                        "missing required `target`".to_string()
                    } else if raw.relation_invalid {
                        format!(
                            "unknown `relation` {:?} (not one of canonical-source|mirror|derived-from|discussed-in)",
                            raw.relation_raw.as_deref().unwrap_or("")
                        )
                    } else {
                        "missing required `relation`".to_string()
                    };
                    issues.push(IntegrityIssue {
                        kind: IntegrityKind::DanglingXref,
                        node: raw.node_ref.clone(),
                        detail: format!("{}: structurally-invalid xref — {why}", raw.node_ref),
                    });
                } else if let Some(target) = &raw.target {
                    // A target shaped like this project's `<prefix>-N` is intended
                    // as a Haven ref → existence-check it against a live node.
                    // Anything else is an opaque cross-store locator (shape only).
                    if is_haven_ref(&proj.ref_prefix, target)
                        && self.resolve_live_node(proj.id, target)?.is_none()
                    {
                        issues.push(IntegrityIssue {
                            kind: IntegrityKind::DanglingXref,
                            node: raw.node_ref.clone(),
                            detail: format!(
                                "{}: xref target {target} is a Haven ref resolving to no live node",
                                raw.node_ref
                            ),
                        });
                    }
                }

                // (3) Unknown-store lint (warn only, never the sole rejecter).
                if let Some(store) = &raw.store {
                    if !RECOGNIZED_STORES.contains(&store.as_str()) {
                        issues.push(IntegrityIssue {
                            kind: IntegrityKind::UnknownStore,
                            node: raw.node_ref.clone(),
                            detail: format!(
                                "{}: xref store {store:?} is not a recognized store ({})",
                                raw.node_ref,
                                RECOGNIZED_STORES.join(", ")
                            ),
                        });
                    }
                }
            }
        }
        Ok(issues)
    }

    /// The dual-surface `haven xref <ref>` / `haven_xref` read verb (HV-69):
    /// a deterministic, sorted report of every xref on the node's artifacts
    /// (outbound) plus every other Haven artifact whose xref `target` resolves to
    /// this node (inbound backlinks). Read-only; the inbound scan reuses the same
    /// flat fold as [`Store::xref_integrity`].
    pub fn xref(&self, project: Option<&str>, selector: &str) -> Result<XrefReport> {
        let (project_id, _key) = self.require_project(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        let node_ref = self.node_ref(node_id)?;
        let proj = self
            .list_projects()?
            .into_iter()
            .find(|p| p.id == project_id)
            .ok_or_else(|| HavenError::NotFound("project".into()))?;

        let raws = self.collect_project_xrefs(project_id)?;

        // Outbound: every (well-formed) xref on this node's own artifacts.
        let mut outbound: Vec<XrefOut> = raws
            .iter()
            .filter(|r| r.node_ref == node_ref)
            .filter_map(|r| {
                r.to_xref().map(|xref| XrefOut {
                    artifact: r.artifact.clone(),
                    role: r.role,
                    path: r.path.clone(),
                    xref,
                })
            })
            .collect();
        outbound.sort_by(|a, b| {
            (&a.path, &a.xref.store, &a.xref.target, &a.artifact).cmp(&(
                &b.path,
                &b.xref.store,
                &b.xref.target,
                &b.artifact,
            ))
        });

        // Inbound: every OTHER node's artifact whose Haven-ref target resolves to
        // this node. Cross-store locators never produce a backlink (they don't
        // resolve to a Haven node).
        let mut inbound: Vec<XrefIn> = Vec::new();
        for r in &raws {
            if r.node_ref == node_ref {
                continue;
            }
            let Some(target) = &r.target else { continue };
            if !is_haven_ref(&proj.ref_prefix, target) {
                continue;
            }
            if self.resolve_live_node(project_id, target)? == Some(node_id) {
                if let Some(xref) = r.to_xref() {
                    inbound.push(XrefIn {
                        source: r.node_ref.clone(),
                        artifact: r.artifact.clone(),
                        role: r.role,
                        path: r.path.clone(),
                        xref,
                    });
                }
            }
        }
        inbound.sort_by(|a, b| {
            (&a.source, &a.xref.store, &a.artifact).cmp(&(&b.source, &b.xref.store, &b.artifact))
        });

        Ok(XrefReport {
            node: node_ref,
            outbound,
            inbound,
        })
    }

    /// Resolve a selector to a **live** (not archived/superseded) node id within a
    /// project, or `None`. Unlike [`Store::resolve_node_id`] this filters dead
    /// nodes (so a dangling-xref scan treats an archived target as dangling) and
    /// never errors on a non-match.
    fn resolve_live_node(&self, project_id: i64, selector: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM nodes
                  WHERE (public_id = ?1 OR (project_id = ?2 AND ref = ?1))
                    AND status NOT IN ('archived', 'superseded')",
                params![selector, project_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Register an artifact on a node. For `kind = file`, the content is written
    /// into `items/<ref>/` (or `items/<ref>/notes/` for handoffs) and its sha256
    /// recorded — from either a source `file` (copied) or inline `content` (the
    /// content channel for filesystem-less clients). The bytes live as a file;
    /// the DB stores only the typed pointer.
    pub fn add_artifact(
        &self,
        project: Option<&str>,
        selector: &str,
        new: NewArtifact,
    ) -> Result<Artifact> {
        let (project_id, project_key) = self.require_project(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        let node_ref = self.node_ref(node_id)?;

        if new.role == ArtifactRole::Handoff && (new.from_owner.is_none() || new.to_owner.is_none())
        {
            return Err(HavenError::Invalid(
                "a handoff artifact must carry --from and --to".into(),
            ));
        }

        // Validate any `metadata.xref` on the write path (reject an unknown
        // `relation` or a missing `target`) and serialize for binding. An empty /
        // absent metadata serializes to the DDL default `'{}'` so it reads back as
        // `None` — byte-stable.
        if let Some(meta) = &new.metadata {
            validate_xref_metadata(meta)?;
        }
        let metadata_json = match &new.metadata {
            Some(m) if !m.is_null() => m.to_string(),
            _ => "{}".to_string(),
        };

        // Resolve the row fields plus, for file kinds, the pending on-disk write —
        // deferred so the (node, path) collision check below can reject *before*
        // clobbering an existing file.
        let (rel_path, content_hash, pending_write) = match new.kind {
            ArtifactKind::File => {
                if new.file.is_some() && new.content.is_some() {
                    return Err(HavenError::Invalid(
                        "provide either a source --file or inline --content, not both".into(),
                    ));
                }
                // Resolve (filename, bytes) from whichever source was given.
                let (filename, bytes) = if let Some(content) = &new.content {
                    let name = new
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("{}.md", new.role.as_str()));
                    (name, content.clone().into_bytes())
                } else if let Some(src) = &new.file {
                    let bytes = std::fs::read(src).map_err(|e| {
                        HavenError::Invalid(format!("cannot read {}: {e}", src.display()))
                    })?;
                    // Honor an explicit --name as the destination filename (run
                    // through the same plain-name validation below); else fall
                    // back to the source basename.
                    let filename = match &new.name {
                        Some(name) => name.clone(),
                        None => src
                            .file_name()
                            .ok_or_else(|| HavenError::Invalid("source has no file name".into()))?
                            .to_string_lossy()
                            .to_string(),
                    };
                    (filename, bytes)
                } else {
                    return Err(HavenError::Invalid(
                        "a file artifact requires --file <path> or inline --content".into(),
                    ));
                };
                // Filename must be a single plain component (no separators / `..`),
                // so a client-supplied name can't escape the item directory.
                validate_plain_name(&filename)?;
                // Handoffs live under notes/; everything else directly in the item dir.
                let subdir = if new.role == ArtifactRole::Handoff {
                    format!("items/{node_ref}/notes")
                } else {
                    format!("items/{node_ref}")
                };
                let dest_dir = self.project_dir(&project_key).join(&subdir);
                let dest = dest_dir.join(&filename);
                let hash = hex(&Sha256::digest(&bytes));
                (
                    Some(format!("{subdir}/{filename}")),
                    Some(hash),
                    Some((dest_dir, dest, bytes)),
                )
            }
            ArtifactKind::External | ArtifactKind::Delivery => {
                if new.uri.is_none() {
                    return Err(HavenError::Invalid(
                        "an external artifact requires --uri".into(),
                    ));
                }
                (None, None, None)
            }
        };

        // Collision key is (node, path), NOT (node, role): a node may legitimately
        // hold several same-role artifacts (e.g. a leaf `spec` co-residing with a
        // group's `context-pack.md` spec). A duplicate path would shadow on read,
        // so reject by default; `--replace`/`replace:true` updates in place.
        let existing = if let Some(path) = &rel_path {
            self.load_artifacts(node_id)?
                .into_iter()
                .find(|a| a.path.as_deref() == Some(path.as_str()))
        } else {
            None
        };
        if let Some(existing) = &existing {
            if !new.replace {
                return Err(HavenError::Invalid(format!(
                    "an artifact already exists at {path:?} on {node_ref:?} \
                     (role {role}, id {id}); pass --replace to overwrite it, \
                     or remove it first with `haven artifact rm`",
                    path = existing.path.as_deref().unwrap_or_default(),
                    role = existing.role,
                    id = existing.public_id,
                )));
            }
        }

        // Past the collision gate: commit the deferred file write (once).
        if let Some((dest_dir, dest, bytes)) = &pending_write {
            std::fs::create_dir_all(dest_dir)?;
            std::fs::write(dest, bytes)?;
        }

        // Replace-in-place: rewrite the existing row (keep public_id / created_at /
        // history), refresh content_hash, bump revision; else insert a fresh row.
        let id = if let Some(existing) = existing {
            self.conn.execute(
                "UPDATE artifacts
                    SET role = ?1, kind = ?2, uri = ?3, title = ?4, excerpt = ?5,
                        from_owner = ?6, to_owner = ?7, content_hash = ?8,
                        metadata = ?9,
                        revision = revision + 1, sync_state = 'local'
                  WHERE id = ?10",
                params![
                    new.role,
                    new.kind,
                    new.uri,
                    new.title,
                    new.excerpt,
                    new.from_owner,
                    new.to_owner,
                    content_hash,
                    metadata_json,
                    existing.id,
                ],
            )?;
            existing.id
        } else {
            let public_id = new_uuid();
            self.conn.execute(
                "INSERT INTO artifacts
                   (public_id, node_id, role, kind, path, uri, title, excerpt,
                    from_owner, to_owner, content_hash, metadata, created_by, client_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    public_id,
                    node_id,
                    new.role,
                    new.kind,
                    rel_path,
                    new.uri,
                    new.title,
                    new.excerpt,
                    new.from_owner,
                    new.to_owner,
                    content_hash,
                    metadata_json,
                    new.created_by,
                    new_uuid(),
                ],
            )?;
            self.conn.last_insert_rowid()
        };
        let artifacts = self.load_artifacts(node_id)?;
        artifacts
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| HavenError::NotFound("artifact just inserted".into()))
    }

    pub fn list_artifacts(
        &self,
        project: Option<&str>,
        selector: &str,
        role: Option<ArtifactRole>,
    ) -> Result<Vec<Artifact>> {
        let (project_id, _) = self.require_project(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        let mut artifacts = self.load_artifacts(node_id)?;
        if let Some(role) = role {
            artifacts.retain(|a| a.role == role);
        }
        Ok(artifacts)
    }

    /// Read an artifact's content. Resolves by `path` if given, else the latest
    /// artifact of `role`. For `kind = file`, reads the local file; when the
    /// file is absent but the row carries a `remote_path` (a synced copy in
    /// cloud Storage), this returns [`HavenError::ContentNotLocal`] so a
    /// sync-aware front-end can lazy-download and retry (SPEC §5).
    pub fn get_artifact(
        &self,
        project: Option<&str>,
        selector: &str,
        role: Option<ArtifactRole>,
        path: Option<&str>,
    ) -> Result<ArtifactContent> {
        let (project_id, project_key) = self.require_project(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        let node_ref = self.node_ref(node_id)?;
        let artifacts = self.load_artifacts(node_id)?;

        let chosen = if let Some(path) = path {
            artifacts
                .into_iter()
                .find(|a| a.path.as_deref() == Some(path))
        } else if let Some(role) = role {
            // Latest of that role (load_artifacts is id-ascending).
            artifacts.into_iter().rfind(|a| a.role == role)
        } else {
            artifacts.into_iter().next_back()
        }
        .ok_or_else(|| HavenError::NotFound(format!("artifact on {node_ref:?}")))?;

        let content = match &chosen.path {
            Some(rel) => {
                let base = self.project_dir(&project_key);
                let full = base.join(rel);
                // Guard against a tampered `path` row escaping the project tree.
                if !full.starts_with(&base) {
                    return Err(HavenError::Invalid(format!(
                        "artifact path {rel:?} escapes the project directory"
                    )));
                }
                match std::fs::read_to_string(&full) {
                    Ok(text) => Some(text),
                    // Missing locally but synced to Storage → a typed signal the
                    // front-ends catch to lazy-download, then retry the read.
                    Err(_) if chosen.remote_path.is_some() && !full.exists() => {
                        return Err(HavenError::ContentNotLocal {
                            project: project_key,
                            rel_path: rel.clone(),
                            remote_path: chosen.remote_path.expect("checked is_some"),
                            content_hash: chosen.content_hash,
                        });
                    }
                    Err(e) => {
                        return Err(HavenError::NotFound(format!(
                            "content file {} not present locally: {e}",
                            full.display()
                        )));
                    }
                }
            }
            None => None,
        };

        Ok(ArtifactContent {
            node_ref,
            role: chosen.role,
            path: chosen.path,
            uri: chosen.uri,
            content,
        })
    }

    /// Resolve an [`ArtifactSelector`] to the single artifact it names on a node.
    /// `Id`/`Name` are unique keys; a `Role` selector that matches more than one
    /// row is refused — the caller must disambiguate by `--name`/`--id`.
    fn select_artifact(&self, node_id: i64, selector: &ArtifactSelector) -> Result<Artifact> {
        let artifacts = self.load_artifacts(node_id)?;
        let matches: Vec<Artifact> = match selector {
            ArtifactSelector::Role(role) => {
                artifacts.into_iter().filter(|a| a.role == *role).collect()
            }
            ArtifactSelector::Name(name) => artifacts
                .into_iter()
                .filter(|a| {
                    a.path
                        .as_deref()
                        .and_then(|p| p.rsplit('/').next())
                        .map(|base| base == name)
                        .unwrap_or(false)
                })
                .collect(),
            ArtifactSelector::Id(id) => artifacts
                .into_iter()
                .filter(|a| &a.public_id == id)
                .collect(),
        };
        match matches.len() {
            0 => Err(HavenError::NotFound(format!(
                "no artifact matched {selector:?}"
            ))),
            1 => Ok(matches.into_iter().next().expect("len == 1")),
            _ => {
                let ids: Vec<String> = matches
                    .iter()
                    .map(|a| {
                        let name = a
                            .path
                            .as_deref()
                            .and_then(|p| p.rsplit('/').next())
                            .unwrap_or("?");
                        format!("{name} ({})", a.public_id)
                    })
                    .collect();
                Err(HavenError::Invalid(format!(
                    "selector {selector:?} is ambiguous — matched {} artifacts; \
                     disambiguate by --name or --id ({})",
                    matches.len(),
                    ids.join(", "),
                )))
            }
        }
    }

    /// Remove one artifact: delete the DB row and, for `kind = file`, the backing
    /// file. The selector must resolve to exactly one row (an ambiguous `Role` is
    /// refused). Returns the removed artifact.
    pub fn remove_artifact(
        &self,
        project: Option<&str>,
        selector_ref: &str,
        selector: ArtifactSelector,
    ) -> Result<Artifact> {
        let (project_id, project_key) = self.require_project(project)?;
        let node_id = self.resolve_node_id(project_id, selector_ref)?;
        let target = self.select_artifact(node_id, &selector)?;

        // Drop the backing file first (guarded inside the project tree), then the
        // row — a leftover file is recoverable; a leftover row points at nothing.
        if target.kind == ArtifactKind::File {
            if let Some(rel) = &target.path {
                let base = self.project_dir(&project_key);
                let full = base.join(rel);
                if !full.starts_with(&base) {
                    return Err(HavenError::Invalid(format!(
                        "artifact path {rel:?} escapes the project directory"
                    )));
                }
                if full.exists() {
                    std::fs::remove_file(&full)?;
                }
            }
        }
        self.conn
            .execute("DELETE FROM artifacts WHERE id = ?1", params![target.id])?;
        Ok(target)
    }

    /// Rename one artifact's backing file and update its `path`, keeping the row
    /// (role / history / created_at) intact. `new_name` is validated as a plain
    /// name and rejected if it would collide with an existing path on the node.
    pub fn rename_artifact(
        &self,
        project: Option<&str>,
        selector_ref: &str,
        selector: ArtifactSelector,
        new_name: &str,
    ) -> Result<Artifact> {
        let (project_id, project_key) = self.require_project(project)?;
        let node_id = self.resolve_node_id(project_id, selector_ref)?;
        let node_ref = self.node_ref(node_id)?;
        let target = self.select_artifact(node_id, &selector)?;

        validate_plain_name(new_name)?;

        let rel = target.path.clone().ok_or_else(|| {
            HavenError::Invalid("only a file artifact (with a path) can be renamed".into())
        })?;
        // Preserve the subdir; swap the basename. `rsplit_once` splits off the
        // last segment, so handoffs under notes/ stay under notes/.
        let subdir = rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let new_rel = if subdir.is_empty() {
            new_name.to_string()
        } else {
            format!("{subdir}/{new_name}")
        };
        if new_rel == rel {
            return Ok(target); // no-op rename to the same name
        }

        // Reject a collision with another path on this node (per the add rule).
        if self
            .load_artifacts(node_id)?
            .iter()
            .any(|a| a.path.as_deref() == Some(new_rel.as_str()))
        {
            return Err(HavenError::Invalid(format!(
                "an artifact already exists at {new_rel:?} on {node_ref:?}; \
                 rename or remove it first"
            )));
        }

        let base = self.project_dir(&project_key);
        let from = base.join(&rel);
        let to = base.join(&new_rel);
        // Both endpoints must stay inside the project tree before any fs op.
        if !from.starts_with(&base) || !to.starts_with(&base) {
            return Err(HavenError::Invalid(
                "artifact rename would escape the project directory".into(),
            ));
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&from, &to)?;

        self.conn.execute(
            "UPDATE artifacts
                SET path = ?1, revision = revision + 1, sync_state = 'local'
              WHERE id = ?2",
            params![new_rel, target.id],
        )?;
        let artifacts = self.load_artifacts(node_id)?;
        artifacts
            .into_iter()
            .find(|a| a.id == target.id)
            .ok_or_else(|| HavenError::NotFound("artifact just renamed".into()))
    }

    /// Append a free-text scratch line to the node's dated notes file. No DB row
    /// (the `notes/` folder is a free filesystem, SPEC §4).
    pub fn note(&self, project: Option<&str>, selector: &str, text: &str) -> Result<PathBuf> {
        let (project_id, project_key) = self.require_project(project)?;
        let node_id = self.resolve_node_id(project_id, selector)?;
        let node_ref = self.node_ref(node_id)?;

        let (y, m, d) = crate::time::today_ymd();
        let dir = self
            .project_dir(&project_key)
            .join(format!("items/{node_ref}/notes"));
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!("{y:04}-{m:02}-{d:02}.md"));

        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)?;
        writeln!(f, "- {text}")?;
        Ok(file)
    }

    /// Regenerate the project's deterministic `backlog.md` projection.
    /// Read-only output, diffable in a vault — never hand-edited (SPEC §4).
    pub fn render(&self, project: Option<&str>) -> Result<PathBuf> {
        let (_project_id, project_key) = self.require_project(project)?;
        let proj = self.get_project(&project_key)?;
        let md = self.backlog_markdown(&project_key, &proj.title)?;

        let dir = self.project_dir(&project_key);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("backlog.md");
        std::fs::write(&path, md)?;
        Ok(path)
    }

    /// Build the backlog markdown (split out for testing without touching disk).
    pub fn backlog_markdown(&self, project_key: &str, title: &str) -> Result<String> {
        let committed = self.list_items(
            Some(project_key),
            &ItemFilter {
                committed: Some(true),
                ..Default::default()
            },
        )?;
        let icebox = self.list_items(
            Some(project_key),
            &ItemFilter {
                icebox: true,
                ..Default::default()
            },
        )?;

        let mut out = String::new();
        let _ = writeln!(out, "# {title} ({project_key})\n");
        let _ = writeln!(out, "_Generated by `haven render`. Do not hand-edit._\n");

        let _ = writeln!(out, "## Committed\n");
        if committed.is_empty() {
            let _ = writeln!(out, "_(none)_\n");
        } else {
            for item in &committed {
                out.push_str(&self.backlog_line(item)?);
            }
            out.push('\n');
        }

        let _ = writeln!(out, "## Icebox\n");
        if icebox.is_empty() {
            let _ = writeln!(out, "_(none)_\n");
        } else {
            for item in &icebox {
                out.push_str(&self.backlog_line(item)?);
            }
        }
        Ok(out)
    }

    fn backlog_line(&self, item: &Item) -> Result<String> {
        let owner = item
            .owner_kind
            .map(|o| format!(", {o}"))
            .unwrap_or_default();
        let pri = item.priority.map(|p| format!(" P{p}")).unwrap_or_default();
        let edges = self.load_edges(item.id)?;
        let blocked = if edges.depends_on.is_empty() {
            String::new()
        } else {
            format!(" — blocked by: {}", edges.depends_on.join(", "))
        };
        // Containers carry a derived rollup; surface it plus a `+uncommitted`
        // marker so a `done` rollup is never shown bare while live floaters remain
        // beneath the container (HV-104). Terse + deterministic — backlog.md is a
        // diffable projection (SPEC §4).
        let rollup = if item.node_type.is_container() {
            let (state, has_uncommitted) = self.container_rollup(item.id)?;
            let mark = if has_uncommitted { " +uncommitted" } else { "" };
            format!(" [rollup: {}{}]", state.as_str(), mark)
        } else {
            String::new()
        };
        Ok(format!(
            "- `{r}`{pri} {title} ({status}{owner}){blocked}{rollup}\n",
            r = item.reference,
            title = item.title,
            status = item.status,
        ))
    }
}

/// A client-supplied artifact filename must be a single plain component (no
/// path separators or `..`), so it can never escape the item directory.
fn validate_plain_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.split(['/', '\\']).any(|seg| seg == "..")
        || name == ".."
    {
        return Err(HavenError::Invalid(format!(
            "artifact file name {name:?} must be a plain file name"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewItem;

    fn store_with_root(root: &std::path::Path) -> Store {
        let s = Store::open_in_memory_at(root).unwrap();
        s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        s.use_project("haven").unwrap();
        s
    }

    #[test]
    fn artifact_file_roundtrip_and_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let item = s
            .add_item(
                None,
                NewItem {
                    title: "Spec work".into(),
                    ..Default::default()
                },
            )
            .unwrap();

        // A source file to register.
        let src = tmp.path().join("spec.md");
        std::fs::write(&src, b"# Spec\nhello\n").unwrap();

        let art = s
            .add_artifact(
                None,
                &item.reference,
                NewArtifact {
                    role: ArtifactRole::Spec,
                    kind: ArtifactKind::File,
                    file: Some(src.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(art.role, ArtifactRole::Spec);
        assert_eq!(art.path.as_deref(), Some("items/HV-1/spec.md"));
        assert!(art.content_hash.is_some());

        // The file was copied into the content tree.
        let copied = tmp.path().join("haven/items/HV-1/spec.md");
        assert!(copied.exists());

        // get_artifact reads it back.
        let got = s
            .get_artifact(None, &item.reference, Some(ArtifactRole::Spec), None)
            .unwrap();
        assert_eq!(got.content.as_deref(), Some("# Spec\nhello\n"));
    }

    #[test]
    fn backlog_line_annotates_container_rollup() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let phase = s
            .add_item(
                None,
                NewItem {
                    title: "Track".into(),
                    node_type: Some(NodeType::Phase),
                    commit: true,
                    ..Default::default()
                },
            )
            .unwrap();
        // Committed + done member → rollup Done; uncommitted floater → flag set.
        s.add_item(
            None,
            NewItem {
                title: "shipped".into(),
                commit: true,
                status: Some(crate::model::Status::Done),
                parent: Some(phase.reference.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        s.add_item(
            None,
            NewItem {
                title: "not yet committed".into(),
                parent: Some(phase.reference.clone()),
                ..Default::default()
            },
        )
        .unwrap();

        // The container line carries the rollup + uncommitted marker (the bare
        // `done` never appears alone).
        let line = s.backlog_line(&phase).unwrap();
        assert!(
            line.contains("[rollup: done +uncommitted]"),
            "container line missing annotation: {line}"
        );

        // A leaf line carries no rollup annotation.
        let leaf = s
            .add_item(
                None,
                NewItem {
                    title: "solo leaf".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let leaf_line = s.backlog_line(&leaf).unwrap();
        assert!(
            !leaf_line.contains("[rollup:"),
            "leaf line should not be annotated: {leaf_line}"
        );
    }

    #[test]
    fn context_pack_for_derives_found_clash_and_dedups() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let add_phase = |title: &str| {
            s.add_item(
                None,
                NewItem {
                    title: title.into(),
                    node_type: Some(NodeType::Phase),
                    ..Default::default()
                },
            )
            .unwrap()
            .reference
        };
        // `replace: true` so a re-prep overwrites the container's pack in place
        // (collision-safe add, HV-95) rather than erroring on the same path.
        let pack_on = |container: &str, body: &str| {
            s.add_artifact(
                None,
                container,
                NewArtifact {
                    role: ArtifactRole::ContextPack,
                    content: Some(body.into()),
                    name: Some(CONTEXT_PACK_ARTIFACT.into()),
                    replace: true,
                    ..Default::default()
                },
            )
            .unwrap();
        };

        let batch_a = add_phase("Batch A");
        let leaf = s
            .add_item(
                None,
                NewItem {
                    title: "leaf".into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .reference;
        s.group(None, &batch_a, &leaf, false).unwrap();

        // Grouped, but no pack on the container yet → nothing to point at.
        let (pack, clash) = s.context_pack_for(None, &leaf).unwrap();
        assert!(pack.is_none() && clash.is_none());

        // A pack on the one container → Found, pointing at it.
        pack_on(&batch_a, "# pack");
        let (pack, clash) = s.context_pack_for(None, &leaf).unwrap();
        assert_eq!(pack.unwrap().container, batch_a);
        assert!(clash.is_none());

        // Re-prepping the SAME container overwrites its context-pack.md in place
        // (collision-safe add) — still exactly one pack, and dedup-by-container
        // keeps even a hypothetical duplicate row from reading as a clash.
        pack_on(&batch_a, "# pack v2");
        let (pack, clash) = s.context_pack_for(None, &leaf).unwrap();
        assert!(
            pack.is_some() && clash.is_none(),
            "a re-prepped pack on one container must not read as a clash"
        );

        // A SECOND container also claiming the leaf, with its own pack → clash,
        // and no silent pick of either.
        let batch_b = add_phase("Batch B");
        s.group(None, &batch_b, &leaf, false).unwrap();
        pack_on(&batch_b, "# pack b");
        let (pack, clash) = s.context_pack_for(None, &leaf).unwrap();
        assert!(pack.is_none(), "a clash must not silently pick a pack");
        let mut refs = clash.unwrap();
        refs.sort();
        let mut want = vec![batch_a, batch_b];
        want.sort();
        assert_eq!(refs, want);
    }

    #[test]
    fn missing_file_signals_content_not_local_only_with_a_remote_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let item = s
            .add_item(
                None,
                NewItem {
                    title: "Spec work".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let art = s
            .add_artifact(
                None,
                &item.reference,
                NewArtifact {
                    role: ArtifactRole::Spec,
                    kind: ArtifactKind::File,
                    content: Some("the spec".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        // Delete the local file (simulating a row that arrived via pull).
        std::fs::remove_file(tmp.path().join("haven/items/HV-1/spec.md")).unwrap();

        // No remote copy → plain NotFound, as before.
        let err = s
            .get_artifact(None, &item.reference, Some(ArtifactRole::Spec), None)
            .unwrap_err();
        assert!(matches!(err, HavenError::NotFound(_)));

        // With a remote_path on the row (as a pulled row would carry) → the
        // typed signal a sync-aware front-end catches to lazy-download.
        s.conn
            .execute(
                "UPDATE artifacts SET remote_path = 'u/haven/items/HV-1/spec.md'
                 WHERE public_id = ?1",
                [&art.public_id],
            )
            .unwrap();
        let err = s
            .get_artifact(None, &item.reference, Some(ArtifactRole::Spec), None)
            .unwrap_err();
        match err {
            HavenError::ContentNotLocal {
                project,
                rel_path,
                remote_path,
                content_hash,
            } => {
                assert_eq!(project, "haven");
                assert_eq!(rel_path, "items/HV-1/spec.md");
                assert_eq!(remote_path, "u/haven/items/HV-1/spec.md");
                assert_eq!(content_hash, art.content_hash);
            }
            other => panic!("expected ContentNotLocal, got {other:?}"),
        }
        assert_eq!(err_code_is_stable(), "content_not_local");
    }

    fn err_code_is_stable() -> &'static str {
        HavenError::ContentNotLocal {
            project: String::new(),
            rel_path: String::new(),
            remote_path: String::new(),
            content_hash: None,
        }
        .code()
    }

    #[test]
    fn inline_content_materializes_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let item = s
            .add_item(
                None,
                NewItem {
                    title: "Spec work".into(),
                    ..Default::default()
                },
            )
            .unwrap();

        // Write via the content channel — no source file on disk.
        let art = s
            .add_artifact(
                None,
                &item.reference,
                NewArtifact {
                    role: ArtifactRole::Spec,
                    kind: ArtifactKind::File,
                    content: Some("# Spec\nwritten inline\n".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        // Default filename is <role>.md; the server created the file.
        assert_eq!(art.path.as_deref(), Some("items/HV-1/spec.md"));
        assert!(art.content_hash.is_some());
        assert!(tmp.path().join("haven/items/HV-1/spec.md").exists());

        // Read it back through the same content field.
        let got = s
            .get_artifact(None, &item.reference, Some(ArtifactRole::Spec), None)
            .unwrap();
        assert_eq!(got.content.as_deref(), Some("# Spec\nwritten inline\n"));

        // A custom name is honoured.
        let named = s
            .add_artifact(
                None,
                &item.reference,
                NewArtifact {
                    role: ArtifactRole::Research,
                    kind: ArtifactKind::File,
                    content: Some("notes".into()),
                    name: Some("findings.md".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(named.path.as_deref(), Some("items/HV-1/findings.md"));

        // file + content together is rejected; a traversal name is rejected.
        let both = s.add_artifact(
            None,
            &item.reference,
            NewArtifact {
                role: ArtifactRole::Scratch,
                kind: ArtifactKind::File,
                content: Some("x".into()),
                file: Some(tmp.path().join("anything.md")),
                ..Default::default()
            },
        );
        assert!(both.is_err());
        let evil = s.add_artifact(
            None,
            &item.reference,
            NewArtifact {
                role: ArtifactRole::Scratch,
                kind: ArtifactKind::File,
                content: Some("x".into()),
                name: Some("../escape.md".into()),
                ..Default::default()
            },
        );
        assert_eq!(evil.unwrap_err().code(), "invalid");
    }

    #[test]
    fn handoff_requires_from_and_to() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let item = s
            .add_item(
                None,
                NewItem {
                    title: "Work".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let src = tmp.path().join("h.md");
        std::fs::write(&src, b"handoff").unwrap();

        let err = s
            .add_artifact(
                None,
                &item.reference,
                NewArtifact {
                    role: ArtifactRole::Handoff,
                    kind: ArtifactKind::File,
                    file: Some(src.clone()),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert_eq!(err.code(), "invalid");

        // With from/to it succeeds and lands under notes/.
        let art = s
            .add_artifact(
                None,
                &item.reference,
                NewArtifact {
                    role: ArtifactRole::Handoff,
                    kind: ArtifactKind::File,
                    file: Some(src),
                    from_owner: Some(OwnerKind::Ai),
                    to_owner: Some(OwnerKind::Human),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(art.path.as_deref(), Some("items/HV-1/notes/h.md"));
        assert_eq!(art.from_owner, Some(OwnerKind::Ai));
    }

    #[test]
    fn external_artifact_needs_uri_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let item = s
            .add_item(
                None,
                NewItem {
                    title: "Research".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let art = s
            .add_artifact(
                None,
                &item.reference,
                NewArtifact {
                    role: ArtifactRole::Source,
                    kind: ArtifactKind::External,
                    uri: Some("obsidian://vault/note".into()),
                    title: Some("Field notes".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(art.kind, ArtifactKind::External);
        assert!(art.path.is_none());
        assert_eq!(art.uri.as_deref(), Some("obsidian://vault/note"));
    }

    #[test]
    fn note_appends_without_db_row() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let item = s
            .add_item(
                None,
                NewItem {
                    title: "Scratch".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let f = s.note(None, &item.reference, "first thought").unwrap();
        s.note(None, &item.reference, "second thought").unwrap();
        let body = std::fs::read_to_string(&f).unwrap();
        assert!(body.contains("first thought") && body.contains("second thought"));
        // No artifact row was created.
        assert!(s
            .list_artifacts(None, &item.reference, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn backlog_markdown_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        s.add_item(
            None,
            NewItem {
                title: "Ship it".into(),
                commit: true,
                priority: Some(0),
                status: Some(Status::Ready),
                done_looks_like: Some("shipped".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let dep = s
            .add_item(
                None,
                NewItem {
                    title: "Prereq".into(),
                    commit: true,
                    ..Default::default()
                },
            )
            .unwrap();
        s.add_item(
            None,
            NewItem {
                title: "Blocked".into(),
                commit: true,
                depends_on: Some(dep.reference.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        s.add_item(
            None,
            NewItem {
                title: "Someday".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let a = s.backlog_markdown("haven", "Haven").unwrap();
        let b = s.backlog_markdown("haven", "Haven").unwrap();
        assert_eq!(a, b, "render must be deterministic");
        assert!(a.contains("## Committed"));
        assert!(a.contains("## Icebox"));
        assert!(a.contains("Someday"));
        assert!(a.contains("blocked by: HV-2"));

        // render() writes the file.
        let path = s.render(None).unwrap();
        assert!(path.ends_with("haven/backlog.md"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), a);
    }

    // ---- HV-95: write/mutate surface (Parts A–D) ----

    /// A leaf to hang artifacts on, returning the store + its ref.
    fn store_with_item(tmp: &std::path::Path) -> (Store, String) {
        let s = store_with_root(tmp);
        let item = s
            .add_item(
                None,
                NewItem {
                    title: "Work".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        (s, item.reference)
    }

    /// Part A: `--file` + `--name` stores the file under the *given* name, not the
    /// source basename (the previously-untested regression — `--name` was dropped).
    #[test]
    fn file_add_honors_name_over_source_basename() {
        let tmp = tempfile::tempdir().unwrap();
        let (s, item) = store_with_item(tmp.path());

        let src = tmp.path().join("draft-x.md");
        std::fs::write(&src, b"# pack\n").unwrap();

        let art = s
            .add_artifact(
                None,
                &item,
                NewArtifact {
                    role: ArtifactRole::Spec,
                    kind: ArtifactKind::File,
                    file: Some(src),
                    name: Some(CONTEXT_PACK_ARTIFACT.into()),
                    ..Default::default()
                },
            )
            .unwrap();
        // Stored under the requested name, NOT draft-x.md.
        assert_eq!(art.path.as_deref(), Some("items/HV-1/context-pack.md"));
        assert!(tmp.path().join("haven/items/HV-1/context-pack.md").exists());
        assert!(!tmp.path().join("haven/items/HV-1/draft-x.md").exists());

        // A traversal name on the file branch is rejected too (same validation).
        let evil = tmp.path().join("evil.md");
        std::fs::write(&evil, b"x").unwrap();
        let err = s.add_artifact(
            None,
            &item,
            NewArtifact {
                role: ArtifactRole::Scratch,
                kind: ArtifactKind::File,
                file: Some(evil),
                name: Some("../escape.md".into()),
                ..Default::default()
            },
        );
        assert_eq!(err.unwrap_err().code(), "invalid");
    }

    /// Part B: collision key is (node, path). A second add at the same path errors
    /// by default; with `replace` it updates the one row in place (no duplicate).
    #[test]
    fn collision_safe_add_rejects_then_replaces_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let (s, item) = store_with_item(tmp.path());

        let first = s
            .add_artifact(
                None,
                &item,
                NewArtifact {
                    role: ArtifactRole::Spec,
                    kind: ArtifactKind::File,
                    content: Some("v1".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let path = first.path.clone().unwrap();

        // Second add at the same (node, path) WITHOUT replace → rejected, and the
        // original file is left untouched (no clobber, no duplicate row).
        let err = s.add_artifact(
            None,
            &item,
            NewArtifact {
                role: ArtifactRole::Spec,
                kind: ArtifactKind::File,
                content: Some("v2".into()),
                ..Default::default()
            },
        );
        assert_eq!(err.unwrap_err().code(), "invalid");
        assert_eq!(s.list_artifacts(None, &item, None).unwrap().len(), 1);
        assert_eq!(
            std::fs::read_to_string(s.project_dir("haven").join(&path)).unwrap(),
            "v1"
        );

        // With replace → updates the existing row in place: same public_id, bumped
        // revision, rewritten file, and still exactly ONE row.
        let replaced = s
            .add_artifact(
                None,
                &item,
                NewArtifact {
                    role: ArtifactRole::Spec,
                    kind: ArtifactKind::File,
                    content: Some("v2".into()),
                    replace: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(replaced.public_id, first.public_id);
        assert_eq!(replaced.revision, first.revision + 1);
        assert_eq!(replaced.path, first.path);
        assert_eq!(s.list_artifacts(None, &item, None).unwrap().len(), 1);
        assert_eq!(
            std::fs::read_to_string(s.project_dir("haven").join(&path)).unwrap(),
            "v2"
        );
        assert_ne!(replaced.content_hash, first.content_hash);
    }

    /// Part C: remove deletes the row + backing file; removes one of two same-role
    /// artifacts by name; an ambiguous role-only remove is refused.
    #[test]
    fn remove_artifact_row_file_selectors_and_ambiguity() {
        let tmp = tempfile::tempdir().unwrap();
        let (s, item) = store_with_item(tmp.path());

        let add = |role: ArtifactRole, name: &str, body: &str| {
            s.add_artifact(
                None,
                &item,
                NewArtifact {
                    role,
                    kind: ArtifactKind::File,
                    content: Some(body.into()),
                    name: Some(name.into()),
                    ..Default::default()
                },
            )
            .unwrap()
        };

        // Two same-role (spec) artifacts on the node — legitimate (leaf spec +
        // co-resident pack). Role-only remove can't disambiguate.
        add(ArtifactRole::Spec, "spec.md", "the spec");
        add(ArtifactRole::Spec, "context-pack.md", "the pack");
        let err = s.remove_artifact(None, &item, ArtifactSelector::Role(ArtifactRole::Spec));
        assert_eq!(err.unwrap_err().code(), "invalid");

        // By name → removes exactly that one; its file is gone, the other remains.
        let pack_path = tmp.path().join("haven/items/HV-1/context-pack.md");
        let spec_path = tmp.path().join("haven/items/HV-1/spec.md");
        assert!(pack_path.exists() && spec_path.exists());
        let removed = s
            .remove_artifact(
                None,
                &item,
                ArtifactSelector::Name("context-pack.md".into()),
            )
            .unwrap();
        assert_eq!(removed.path.as_deref(), Some("items/HV-1/context-pack.md"));
        assert!(!pack_path.exists(), "backing file must be deleted");
        assert!(spec_path.exists(), "the sibling spec must survive");
        let left = s.list_artifacts(None, &item, None).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].path.as_deref(), Some("items/HV-1/spec.md"));

        // Role-only now unambiguous (one spec left) → removes it.
        let removed = s
            .remove_artifact(None, &item, ArtifactSelector::Role(ArtifactRole::Spec))
            .unwrap();
        assert_eq!(removed.path.as_deref(), Some("items/HV-1/spec.md"));
        assert!(!spec_path.exists());
        assert!(s.list_artifacts(None, &item, None).unwrap().is_empty());
    }

    /// Part D: rename moves the backing file + updates `path`, preserving the row
    /// (public_id / role / created_at); a colliding new_name is rejected.
    #[test]
    fn rename_artifact_moves_file_preserves_row_and_rejects_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let (s, item) = store_with_item(tmp.path());

        let orig = s
            .add_artifact(
                None,
                &item,
                NewArtifact {
                    role: ArtifactRole::Spec,
                    kind: ArtifactKind::File,
                    content: Some("body".into()),
                    name: Some("draft.md".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let renamed = s
            .rename_artifact(
                None,
                &item,
                ArtifactSelector::Name("draft.md".into()),
                "spec.md",
            )
            .unwrap();
        // Path updated; file moved; row identity + role + hash preserved.
        assert_eq!(renamed.path.as_deref(), Some("items/HV-1/spec.md"));
        assert_eq!(renamed.public_id, orig.public_id);
        assert_eq!(renamed.role, ArtifactRole::Spec);
        assert_eq!(renamed.created_at, orig.created_at);
        assert_eq!(renamed.content_hash, orig.content_hash);
        assert!(!tmp.path().join("haven/items/HV-1/draft.md").exists());
        let moved = tmp.path().join("haven/items/HV-1/spec.md");
        assert_eq!(std::fs::read_to_string(&moved).unwrap(), "body");
        // Content reads back through the new path.
        let got = s
            .get_artifact(None, &item, None, Some("items/HV-1/spec.md"))
            .unwrap();
        assert_eq!(got.content.as_deref(), Some("body"));

        // Add a second artifact, then renaming onto its name collides → rejected.
        s.add_artifact(
            None,
            &item,
            NewArtifact {
                role: ArtifactRole::Research,
                kind: ArtifactKind::File,
                content: Some("notes".into()),
                name: Some("notes.md".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let err = s.rename_artifact(
            None,
            &item,
            ArtifactSelector::Name("spec.md".into()),
            "notes.md",
        );
        assert_eq!(err.unwrap_err().code(), "invalid");
        // The collision left both files intact.
        assert!(moved.exists());
        assert!(tmp.path().join("haven/items/HV-1/notes.md").exists());
    }

    #[test]
    fn context_pack_integrity_flags_tombstones_pointers_and_dupes() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());

        // Healthy baseline: a build-batch container with a REAL pack + a grouped
        // member. context_pack_integrity must NOT flag a healthy pack.
        let batch = s
            .add_item(
                None,
                NewItem {
                    title: "batch".into(),
                    node_type: Some(NodeType::Phase),
                    ..Default::default()
                },
            )
            .unwrap();
        let healthy_member = s
            .add_item(
                None,
                NewItem {
                    title: "leaf".into(),
                    group: Some(batch.reference.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        s.add_artifact(
            None,
            &batch.reference,
            NewArtifact {
                role: ArtifactRole::Spec,
                kind: ArtifactKind::File,
                content: Some("# Real pack\nFoundation, contracts, acceptance.\n".into()),
                name: Some(CONTEXT_PACK_ARTIFACT.into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            s.context_pack_integrity().unwrap().is_empty(),
            "a healthy pack must not be flagged"
        );

        // The HV-59 shape: a broad phase whose context-pack.md is a MOVED tombstone,
        // a still-grouped member, plus a legacy duplicate (node,path) row.
        let broad = s
            .add_item(
                None,
                NewItem {
                    title: "broad phase".into(),
                    node_type: Some(NodeType::Phase),
                    ..Default::default()
                },
            )
            .unwrap();
        let orphan = s
            .add_item(
                None,
                NewItem {
                    title: "still-grouped member".into(),
                    group: Some(broad.reference.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        s.add_artifact(
            None,
            &broad.reference,
            NewArtifact {
                role: ArtifactRole::ContextPack,
                kind: ArtifactKind::File,
                content: Some("MOVED: the pack now lives on the build batch. See HV-73.".into()),
                name: Some(CONTEXT_PACK_ARTIFACT.into()),
                ..Default::default()
            },
        )
        .unwrap();
        // Collision-safe add_artifact can't create a duplicate (node,path) row, so
        // insert one directly to simulate legacy data. Role `context-pack` (matching
        // the tombstone) keeps this test a tight guard for the role-keyed tombstone
        // query (HV-124): a regression to the old `role='spec'` predicate would find
        // neither row and the TombstonePack assertion below would fail.
        s.conn
            .execute(
                "INSERT INTO artifacts (public_id, node_id, role, kind, path, client_id)
                 VALUES ('dup-legacy-row', ?1, 'context-pack', 'file', ?2, 'test')",
                params![
                    broad.id,
                    format!("items/{}/context-pack.md", broad.reference)
                ],
            )
            .unwrap();

        let issues = s.context_pack_integrity().unwrap();
        let kinds: Vec<(IntegrityKind, &str)> =
            issues.iter().map(|i| (i.kind, i.node.as_str())).collect();
        assert!(
            kinds.contains(&(IntegrityKind::TombstonePack, broad.reference.as_str())),
            "expected tombstone pack on broad phase: {issues:?}"
        );
        assert!(
            kinds.contains(&(IntegrityKind::PointerToTombstone, orphan.reference.as_str())),
            "expected member pointing at tombstone: {issues:?}"
        );
        assert!(
            kinds.contains(&(
                IntegrityKind::DuplicateArtifactRow,
                broad.reference.as_str()
            )),
            "expected duplicate-row flag: {issues:?}"
        );
        // Exactly one tombstone-pack issue despite two context-pack.md rows (deduped).
        assert_eq!(
            issues
                .iter()
                .filter(|i| i.kind == IntegrityKind::TombstonePack)
                .count(),
            1,
            "tombstone pack must be reported once per container: {issues:?}"
        );
        // The healthy batch + its member are never flagged.
        assert!(
            !issues
                .iter()
                .any(|i| i.node == batch.reference || i.node == healthy_member.reference),
            "healthy pack/member must stay clean: {issues:?}"
        );
    }

    // ---- HV-69: artifact metadata + typed xref vocabulary ------------------

    /// Add an external artifact carrying the given metadata JSON on `item`.
    fn add_xref_artifact(
        s: &Store,
        item: &str,
        role: ArtifactRole,
        metadata: serde_json::Value,
    ) -> Artifact {
        s.add_artifact(
            None,
            item,
            NewArtifact {
                role,
                kind: ArtifactKind::External,
                uri: Some("https://example.test/x".into()),
                metadata: Some(metadata),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn new_node(s: &Store, title: &str) -> Item {
        s.add_item(
            None,
            NewItem {
                title: title.into(),
                ..Default::default()
            },
        )
        .unwrap()
    }

    /// (1) Round-trip: an artifact with an xref array reads back identically.
    #[test]
    fn artifact_metadata_xref_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let node = new_node(&s, "node with xref");

        let meta = serde_json::json!({
            "xref": [
                { "relation": "canonical-source", "store": "servo",
                  "target": "Entity:meal/abc123", "canonical": true }
            ]
        });
        let added = add_xref_artifact(&s, &node.reference, ArtifactRole::Source, meta.clone());
        assert_eq!(
            added.metadata.as_ref(),
            Some(&meta),
            "metadata round-trips on the returned row"
        );

        // And on a fresh load.
        let reloaded = s
            .list_artifacts(None, &node.reference, None)
            .unwrap()
            .into_iter()
            .find(|a| a.public_id == added.public_id)
            .unwrap();
        assert_eq!(reloaded.metadata.as_ref(), Some(&meta));

        // The typed view parses the xref back into the closed vocabulary.
        let report = s.xref(None, &node.reference).unwrap();
        assert_eq!(report.outbound.len(), 1);
        assert_eq!(
            report.outbound[0].xref.relation,
            XrefRelation::CanonicalSource
        );
        assert_eq!(report.outbound[0].xref.store, "servo");
        assert_eq!(report.outbound[0].xref.target, "Entity:meal/abc123");
        assert!(report.outbound[0].xref.canonical);
    }

    /// (4) An artifact with no metadata serializes byte-identically to before the
    /// `metadata` field existed: no `metadata` key in the JSON, and the stored
    /// cell is the DDL default `{}`.
    #[test]
    fn empty_metadata_artifact_is_byte_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let node = new_node(&s, "plain node");

        let art = s
            .add_artifact(
                None,
                &node.reference,
                NewArtifact {
                    role: ArtifactRole::Spec,
                    kind: ArtifactKind::File,
                    content: Some("body".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        // Read path: metadata is None.
        assert!(art.metadata.is_none(), "empty metadata reads as None");
        // Serialize path: no `metadata` key at all.
        let json = serde_json::to_value(&art).unwrap();
        assert!(
            json.get("metadata").is_none(),
            "an empty-metadata artifact must not serialize a `metadata` key: {json}"
        );
        // Storage path: the cell holds the DDL default, not a written-through value.
        let stored: String = s
            .conn
            .query_row(
                "SELECT metadata FROM artifacts WHERE public_id = ?1",
                params![art.public_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, "{}", "empty metadata is stored as the DDL default");
    }

    /// The write path REJECTS an unknown `relation` and a missing `target`.
    #[test]
    fn add_artifact_rejects_invalid_xref_on_write() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let node = new_node(&s, "node");

        let bad_relation = serde_json::json!({
            "xref": [ { "relation": "totally-made-up", "store": "servo", "target": "x" } ]
        });
        let err = s.add_artifact(
            None,
            &node.reference,
            NewArtifact {
                role: ArtifactRole::Source,
                kind: ArtifactKind::External,
                uri: Some("https://x".into()),
                metadata: Some(bad_relation),
                ..Default::default()
            },
        );
        assert_eq!(
            err.unwrap_err().code(),
            "invalid",
            "bad relation rejected on write"
        );

        let missing_target = serde_json::json!({
            "xref": [ { "relation": "mirror", "store": "servo" } ]
        });
        let err = s.add_artifact(
            None,
            &node.reference,
            NewArtifact {
                role: ArtifactRole::Source,
                kind: ArtifactKind::External,
                uri: Some("https://x".into()),
                metadata: Some(missing_target),
                ..Default::default()
            },
        );
        assert_eq!(
            err.unwrap_err().code(),
            "invalid",
            "missing target rejected on write"
        );
    }

    /// (3) The verb lists OUTBOUND xrefs and INBOUND backlinks correctly, and
    /// (5) a cross-store target produces no backlink (it doesn't resolve to a node).
    #[test]
    fn xref_verb_outbound_and_inbound_backlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let hub = new_node(&s, "hub");
        let linker = new_node(&s, "linker");

        // hub's own artifact has an outbound xref to a cross-store locator.
        add_xref_artifact(
            &s,
            &hub.reference,
            ArtifactRole::Source,
            serde_json::json!({
                "xref": [ { "relation": "mirror", "store": "vault", "target": "vault://note/42" } ]
            }),
        );
        // linker's artifact xrefs the hub by its Haven ref → an inbound backlink for hub.
        add_xref_artifact(
            &s,
            &linker.reference,
            ArtifactRole::Source,
            serde_json::json!({
                "xref": [ { "relation": "derived-from", "store": "haven", "target": hub.reference } ]
            }),
        );

        let report = s.xref(None, &hub.reference).unwrap();
        // Outbound: hub's one cross-store xref.
        assert_eq!(report.outbound.len(), 1, "{report:?}");
        assert_eq!(report.outbound[0].xref.target, "vault://note/42");
        // Inbound: the backlink from linker.
        assert_eq!(report.inbound.len(), 1, "{report:?}");
        assert_eq!(report.inbound[0].source, linker.reference);
        assert_eq!(report.inbound[0].xref.relation, XrefRelation::DerivedFrom);

        // The cross-store target on hub is shape-checked only — never existence-
        // flagged by doctor, and never produces a (false) backlink.
        let issues = s.xref_integrity().unwrap();
        assert!(
            !issues.iter().any(|i| i.kind == IntegrityKind::DanglingXref),
            "cross-store + live-ref targets must not dangle: {issues:?}"
        );
    }

    /// (2) The doctor flags a canonical conflict, a dangling Haven-ref target, a
    /// structurally-invalid xref, and an unknown-store lint; a CLEAN store passes.
    #[test]
    fn xref_integrity_flags_conflict_dangling_invalid_and_unknown_store() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());

        // Clean baseline: a single well-formed xref to a live Haven node passes.
        let target_node = new_node(&s, "target");
        let pointer = new_node(&s, "pointer");
        add_xref_artifact(
            &s,
            &pointer.reference,
            ArtifactRole::Source,
            serde_json::json!({
                "xref": [ { "relation": "discussed-in", "store": "github",
                            "target": target_node.reference } ]
            }),
        );
        assert!(
            s.xref_integrity().unwrap().is_empty(),
            "a well-formed xref to a live node + known store must pass"
        );

        // (a) Canonical conflict: two artifacts both claim canonical:true for the
        // same (store, target).
        let a1 = new_node(&s, "canon-1");
        let a2 = new_node(&s, "canon-2");
        add_xref_artifact(
            &s,
            &a1.reference,
            ArtifactRole::Source,
            serde_json::json!({
                "xref": [ { "relation": "canonical-source", "store": "servo",
                            "target": "Entity:meal/x", "canonical": true } ]
            }),
        );
        add_xref_artifact(
            &s,
            &a2.reference,
            ArtifactRole::Source,
            serde_json::json!({
                "xref": [ { "relation": "canonical-source", "store": "servo",
                            "target": "Entity:meal/x", "canonical": true } ]
            }),
        );

        // (b) Dangling Haven-ref target: a `HV-9999` that resolves to no live node.
        let dangler = new_node(&s, "dangler");
        add_xref_artifact(
            &s,
            &dangler.reference,
            ArtifactRole::Source,
            serde_json::json!({
                "xref": [ { "relation": "mirror", "store": "haven", "target": "HV-9999" } ]
            }),
        );

        // (c) Structurally-invalid xref (bad relation) injected via raw DB, since
        // the write path would reject it — simulates a malformed row from sync.
        let raw_bad = new_node(&s, "raw-bad");
        s.conn
            .execute(
                "INSERT INTO artifacts (public_id, node_id, role, kind, uri, metadata, client_id)
                 VALUES ('raw-bad-art', ?1, 'source', 'external', 'https://x', ?2, 'test')",
                params![
                    raw_bad.id,
                    r#"{"xref":[{"relation":"not-a-real-relation","store":"servo","target":"z"}]}"#
                ],
            )
            .unwrap();

        // (d) Unknown-store lint.
        let oddstore = new_node(&s, "oddstore");
        add_xref_artifact(
            &s,
            &oddstore.reference,
            ArtifactRole::Source,
            serde_json::json!({
                "xref": [ { "relation": "mirror", "store": "notion", "target": "page/7" } ]
            }),
        );

        let issues = s.xref_integrity().unwrap();
        let has =
            |k: IntegrityKind, node: &str| issues.iter().any(|i| i.kind == k && i.node == node);
        assert!(
            has(IntegrityKind::CanonicalConflict, &a1.reference)
                && has(IntegrityKind::CanonicalConflict, &a2.reference),
            "both canonical claimants flagged: {issues:?}"
        );
        assert!(
            has(IntegrityKind::DanglingXref, &dangler.reference),
            "dangling Haven-ref target flagged: {issues:?}"
        );
        assert!(
            has(IntegrityKind::DanglingXref, &raw_bad.reference),
            "structurally-invalid (bad relation) xref flagged: {issues:?}"
        );
        assert!(
            has(IntegrityKind::UnknownStore, &oddstore.reference),
            "unknown-store lint flagged: {issues:?}"
        );
        // The clean pointer→live-node xref is never flagged.
        assert!(
            !issues.iter().any(|i| i.node == pointer.reference),
            "the clean pointer must stay clean: {issues:?}"
        );
        // The malformed-relation load did not error the scan — it was REPORTED.
        // (Proven by reaching here with the assertion above passing.)
    }

    /// A raw-DB xref with a valid target but NO `relation` is structurally
    /// invalid → dangling (the write path rejects it, so it only arrives via sync).
    #[test]
    fn xref_missing_relation_is_dangling() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let live = new_node(&s, "live target");
        let bad = new_node(&s, "missing-relation");
        // Inject directly: a well-formed-looking xref to a LIVE node but with no
        // `relation`. Without the missing-relation check this would pass (target
        // resolves), silently accepting a required-field violation.
        s.conn
            .execute(
                "INSERT INTO artifacts (public_id, node_id, role, kind, uri, metadata, client_id)
                 VALUES ('no-rel-art', ?1, 'source', 'external', 'https://x', ?2, 'test')",
                params![
                    bad.id,
                    format!(
                        r#"{{"xref":[{{"store":"haven","target":"{}"}}]}}"#,
                        live.reference
                    )
                ],
            )
            .unwrap();
        let issues = s.xref_integrity().unwrap();
        assert!(
            issues.iter().any(|i| i.kind == IntegrityKind::DanglingXref
                && i.node == bad.reference
                && i.detail.contains("missing required `relation`")),
            "an xref with no `relation` must be flagged dangling: {issues:?}"
        );
    }

    /// A live-node target that is later ARCHIVED becomes dangling (resolve_live).
    #[test]
    fn xref_to_archived_target_is_dangling() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let target = new_node(&s, "soon-archived");
        let pointer = new_node(&s, "pointer");
        add_xref_artifact(
            &s,
            &pointer.reference,
            ArtifactRole::Source,
            serde_json::json!({
                "xref": [ { "relation": "mirror", "store": "haven", "target": target.reference } ]
            }),
        );
        assert!(s.xref_integrity().unwrap().is_empty(), "live target passes");

        s.archive_item(None, &target.reference, None, None).unwrap();
        let issues = s.xref_integrity().unwrap();
        assert!(
            issues
                .iter()
                .any(|i| i.kind == IntegrityKind::DanglingXref && i.node == pointer.reference),
            "an xref to an archived target is dangling: {issues:?}"
        );
        // And the backlink disappears from the verb (archived node not scanned;
        // the pointer's target no longer resolves live).
        // (The pointer's own outbound xref still shows, but no inbound on target.)
    }

    /// `--replace` re-write of an artifact updates its metadata (UPDATE path binds it).
    #[test]
    fn replace_artifact_updates_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store_with_root(tmp.path());
        let node = new_node(&s, "node");

        let first = s
            .add_artifact(
                None,
                &node.reference,
                NewArtifact {
                    role: ArtifactRole::Spec,
                    kind: ArtifactKind::File,
                    content: Some("v1".into()),
                    name: Some("doc.md".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(first.metadata.is_none());

        let updated = s
            .add_artifact(
                None,
                &node.reference,
                NewArtifact {
                    role: ArtifactRole::Spec,
                    kind: ArtifactKind::File,
                    content: Some("v2".into()),
                    name: Some("doc.md".into()),
                    metadata: Some(serde_json::json!({
                        "xref": [ { "relation": "mirror", "store": "vault", "target": "v://1" } ]
                    })),
                    replace: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.public_id, first.public_id, "replace keeps identity");
        assert_eq!(
            updated
                .metadata
                .as_ref()
                .and_then(|m| m.get("xref"))
                .map(|x| x.as_array().unwrap().len()),
            Some(1),
            "replace UPDATE binds the new metadata: {:?}",
            updated.metadata
        );
    }
}
