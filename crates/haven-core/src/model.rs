//! Domain types for the work-graph.
//!
//! The user-facing noun is **item** (SPEC §0); the table is `nodes` and the
//! model term is "node" — `Item` is the serialized view of a node row. Enum
//! string values are identical across JSON (serde) and SQL (rusqlite) by
//! construction (see the `sql_enum!` macro), so the CLI/MCP wire format and the
//! DB can never drift.

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

use crate::error::{HavenError, Result};

/// Generate an enum whose serde representation and SQL representation are the
/// same literal string for every variant — preventing wire/DB drift.
///
/// The generated `parse` error names the full legal value-set inline (HV-152),
/// so a closed-set rejection is self-correcting in one step rather than echoing
/// only the offending value. An optional `synonyms { "open" => "discovery", … }`
/// block additionally NAMES common wrong values as the correct one in the error
/// (the synonym is *named, never accepted* — it still errors).
macro_rules! sql_enum {
    // With an optional synonym map for the parse-error hint.
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $lit:literal),+ $(,)? }
     $(synonyms { $($syn:literal => $canon:literal),+ $(,)? })?) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $( #[serde(rename = $lit)] $variant ),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $( $name::$variant => $lit ),+ }
            }
            /// The comma-joined legal value-set, named in the `parse` error so a
            /// closed-set rejection is recoverable in one step (HV-152).
            pub fn legal_values() -> &'static str {
                concat!($($lit, ", "),+).trim_end_matches(", ")
            }
            /// `Some(canonical)` when `s` is a recognized synonym of a legal value
            /// — surfaced in the rejection ("use X"), never silently accepted.
            fn synonym_hint(_s: &str) -> Option<&'static str> {
                $( match _s { $( $syn => return Some($canon), )+ _ => {} } )?
                None
            }
            pub fn parse(s: &str) -> Result<Self> {
                match s {
                    $( $lit => Ok($name::$variant), )+
                    other => {
                        let did_you_mean = match Self::synonym_hint(other) {
                            Some(c) => format!(" — did you mean {c:?}?"),
                            None => String::new(),
                        };
                        Err(HavenError::Invalid(format!(
                            "invalid {} value: {:?}{} — valid: {}",
                            stringify!($name), other, did_you_mean, Self::legal_values())))
                    }
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }

        impl FromSql for $name {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                let s = value.as_str()?;
                $name::parse(s).map_err(|e| FromSqlError::Other(Box::new(
                    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))))
            }
        }
    };
}

sql_enum! {
    /// Node type. `release`/`phase`/`gate` are container nodes (group targets);
    /// `anchor` is a long-lived project-docs node.
    NodeType {
        Task => "task", Code => "code", Research => "research", Data => "data",
        Design => "design", Admin => "admin", Release => "release",
        Phase => "phase", Gate => "gate", Anchor => "anchor",
    }
    synonyms { "bug" => "code", "feature" => "code", "idea" => "task", "story" => "task", "chore" => "task" }
}

impl NodeType {
    /// Container nodes that own a subtree and carry a derived [`RollupState`].
    pub fn is_container(self) -> bool {
        matches!(
            self,
            NodeType::Anchor | NodeType::Release | NodeType::Phase | NodeType::Gate
        )
    }
}

sql_enum! {
    /// Maturity axis — how well-defined a node is.
    Status {
        Discovery => "discovery", Definition => "definition", Ready => "ready",
        InProgress => "in_progress", Blocked => "blocked", Done => "done",
        Superseded => "superseded", Archived => "archived",
    }
    synonyms {
        "open" => "discovery", "icebox" => "discovery", "new" => "discovery",
        "backlog" => "ready", "todo" => "ready",
        "doing" => "in_progress", "in-progress" => "in_progress", "wip" => "in_progress",
        "closed" => "done", "complete" => "done", "completed" => "done",
    }
}

sql_enum! {
    /// Who executes a node.
    OwnerKind { Human => "human", Ai => "ai" }
}

impl OwnerKind {
    /// The other owner — used to infer a handoff's `from` when the item has no
    /// current owner (a human↔ai baton-pass has exactly two sides).
    pub fn opposite(self) -> Self {
        match self {
            OwnerKind::Human => OwnerKind::Ai,
            OwnerKind::Ai => OwnerKind::Human,
        }
    }
}

sql_enum! {
    /// Why a node is parked (orthogonal to status).
    WaitState {
        OnHuman => "on_human", OnDependency => "on_dependency", OnExternal => "on_external",
    }
}

sql_enum! {
    /// Lineage event kind (append-only core).
    EventType {
        Split => "split", Merge => "merge", Supersede => "supersede",
        Update => "update", Archive => "archive", Reopen => "reopen",
    }
}

sql_enum! {
    /// Artifact role — what kind of content a node points at. `context-pack` (HV-124)
    /// is the build-ready brief on a grouping container; it is a first-class role
    /// (not a `spec` disambiguated by filename) so pack resolution keys on the role.
    ArtifactRole {
        Spec => "spec", Research => "research", Design => "design",
        Handoff => "handoff", Decision => "decision", Scratch => "scratch",
        Source => "source", Delivery => "delivery", Vision => "vision",
        ContextPack => "context-pack",
    }
    synonyms {
        "doc" => "spec", "docs" => "spec", "document" => "spec",
        "context_pack" => "context-pack", "contextpack" => "context-pack", "pack" => "context-pack",
        "result" => "delivery", "output" => "delivery", "deliverable" => "delivery",
    }
}

sql_enum! {
    /// Artifact storage kind.
    ArtifactKind { File => "file", External => "external", Delivery => "delivery" }
}

sql_enum! {
    /// Per-row sync status (servo pattern).
    SyncState { Local => "local", Synced => "synced", Failed => "failed" }
}

sql_enum! {
    /// Project lifecycle axis (HV-112) — a *separate, binary* axis from the node
    /// maturity [`Status`]: a project has no maturity, so reopen→`active` is
    /// unambiguous. Delete's terminal state is NOT a third value here — it is the
    /// orthogonal `deleted_at` column — so this live read-path enum stays binary.
    ProjectStatus { Active => "active", Archived => "archived" }
}

// Default = Active. Written as a manual impl rather than `#[derive(Default)]` +
// `#[default]` because the shared `sql_enum!` macro derives a fixed trait set for
// every enum it generates; teaching it `#[default]` would force a default variant
// on NodeType/Status/etc. too. The lint's derive suggestion targets the macro,
// not this type, so it is suppressed locally.
#[allow(clippy::derivable_impls)]
impl Default for ProjectStatus {
    fn default() -> Self {
        ProjectStatus::Active
    }
}

/// A container's effective state, derived ON READ from its committed subtree —
/// never stored, never parsed back (so no `sql_enum!`): a pure read projection.
/// Computed for container nodes only; leaves carry `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RollupState {
    /// No live committed descendants — nothing has been committed to yet.
    Dormant,
    /// Committed work exists, but none has started.
    Queued,
    /// At least one committed descendant is `in_progress`.
    Active,
    /// Every live committed descendant is `done`.
    Done,
}

impl RollupState {
    /// Lowercase wire name (matches the `serde(rename_all = "lowercase")` form).
    /// `RollupState` is intentionally not a `sql_enum!` (it's a read-only
    /// projection, never parsed back), so it has no `Display`/`FromSql`; this is
    /// just for rendering, e.g. the `backlog.md` annotation.
    pub fn as_str(&self) -> &'static str {
        match self {
            RollupState::Dormant => "dormant",
            RollupState::Queued => "queued",
            RollupState::Active => "active",
            RollupState::Done => "done",
        }
    }
}

/// A graph-integrity problem found by [`crate::Store::context_pack_integrity`]
/// (HV-105) — a read-only diagnostic surfaced by `haven doctor`, never stored.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IntegrityIssue {
    pub kind: IntegrityKind,
    /// The offending node's `ref` (the tombstone container, the member pointing
    /// at it, or the node carrying duplicate artifact rows).
    pub node: String,
    pub detail: String,
}

/// The kind of [`IntegrityIssue`].
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityKind {
    /// A `context-pack` artifact whose content is a relocation tombstone, not a
    /// real pack — so the pack-pointer derivation (HV-75) resolves it as live.
    TombstonePack,
    /// A node whose derived `context_pack` resolves to a tombstone container.
    PointerToTombstone,
    /// More than one artifact row for the same `(node, path)`.
    DuplicateArtifactRow,
    /// More than one `canonical:true` xref for the same logical `(store, target)`
    /// — two artifacts both claim to be the canonical copy (HV-69).
    CanonicalConflict,
    /// An xref whose `target` is a Haven ref resolving to no live node, OR a
    /// structurally-invalid xref (missing `target`, or a `relation` that fails to
    /// deserialize into the closed [`XrefRelation`] enum) — the latter can only
    /// arrive via raw DB / sync from another client (HV-69).
    DanglingXref,
    /// An xref whose `store` is not in the recognized allowlist — a warn-only
    /// lint, never the sole basis for rejection (HV-69).
    UnknownStore,
}

/// The closed relation vocabulary on a [`Xref`]. A plain serde enum (kebab-case
/// wire form), validated on the metadata write path — *not* a `sql_enum!` DB
/// column, since `relation` is a JSON sub-field of `artifacts.metadata`, not a
/// column. An unknown value fails to deserialize and is rejected on write (HV-69).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum XrefRelation {
    /// This artifact is the authoritative origin the cross-store copy derives from.
    CanonicalSource,
    /// A mirror of content that lives canonically elsewhere.
    Mirror,
    /// Content derived/transformed from the target.
    DerivedFrom,
    /// The target is a discussion / thread about this artifact.
    DiscussedIn,
}

/// A typed cross-store link living in `artifacts.metadata.xref[]` (HV-69). Turns
/// the loose provenance convention into a machine-checkable invariant: which
/// store holds the canonical copy, and what relation this artifact bears to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Xref {
    /// What relation this artifact bears to `target` — closed enum, rejected on
    /// write when the wire value is unknown.
    pub relation: XrefRelation,
    /// The store the target lives in (`servo` / `vault` / `github` / `haven` / …)
    /// — a free string; an unrecognized value is a doctor *lint* only, never
    /// rejected (stores legitimately proliferate).
    pub store: String,
    /// The target locator — required. When it parses as a Haven ref (this
    /// project's `<prefix>-N` shape) it is existence-checked against a live node;
    /// otherwise it is an opaque cross-store locator, shape-checked only.
    pub target: String,
    /// Whether this artifact points at the canonical copy. Defaults `false`;
    /// more than one `canonical:true` for the same `(store, target)` is a
    /// [`IntegrityKind::CanonicalConflict`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub canonical: bool,
}

/// `serde` skip predicate for a `bool` that defaults `false` — keeps a
/// non-canonical xref serializing without a `canonical` key (byte-stability).
fn is_false(b: &bool) -> bool {
    !*b
}

/// One outbound xref in a [`XrefReport`], carrying the owning artifact's identity
/// for traceability.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct XrefOut {
    /// The owning artifact's `public_id`.
    pub artifact: String,
    /// The owning artifact's role.
    pub role: ArtifactRole,
    /// The owning artifact's `path` (None for external artifacts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The xref itself.
    #[serde(flatten)]
    pub xref: Xref,
}

/// One inbound backlink in a [`XrefReport`]: another Haven artifact whose xref
/// `target` resolves to the queried node.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct XrefIn {
    /// The `ref` of the node owning the back-linking artifact.
    pub source: String,
    /// The back-linking artifact's `public_id`.
    pub artifact: String,
    /// The back-linking artifact's role.
    pub role: ArtifactRole,
    /// The back-linking artifact's `path` (None for external artifacts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The xref pointing at the queried node.
    #[serde(flatten)]
    pub xref: Xref,
}

/// The deterministic report returned by `haven xref <ref>` / `haven_xref`: every
/// xref on the node's artifacts (outbound) plus every other Haven artifact whose
/// xref `target` resolves to this node (inbound backlinks). Read-only, sorted, so
/// the JSON is vault-diffable (HV-69).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct XrefReport {
    /// The queried node's `ref`.
    pub node: String,
    /// Every xref on every artifact of the queried node, sorted.
    pub outbound: Vec<XrefOut>,
    /// Every other Haven artifact whose xref `target` resolves to this node, sorted.
    pub inbound: Vec<XrefIn>,
}

/// A project — namespace for a backlog, one per product/repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    #[serde(skip_serializing, default)]
    pub id: i64,
    pub public_id: String,
    pub key: String,
    pub ref_prefix: String,
    pub ref_counter: i64,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
    pub sync_state: SyncState,
    /// Lifecycle axis (HV-112) — `active` | `archived`. Always serialized.
    pub status: ProjectStatus,
    /// When the project was soft-archived; NULL while active. Mirrors `Item::archived_at`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// Optional archive rationale (no lineage edge target for projects).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_reason: Option<String>,
    /// Delete tombstone marker (HV-112) — only ever set on a tombstoned row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
}

/// The four edge layers, resolved to human `ref`s, attached to an item on
/// `--include edges`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Edges {
    /// Decomposition: nodes this is a part of.
    pub parents: Vec<String>,
    /// Decomposition: nodes that are parts of this.
    pub children: Vec<String>,
    /// Dependency: prerequisites that block this.
    pub depends_on: Vec<String>,
    /// Dependency: nodes blocked by this.
    pub blocks: Vec<String>,
    /// Grouping: release/phase/gate nodes this belongs to.
    pub groups: Vec<String>,
    /// Grouping: members, when this *is* a release/phase/gate.
    pub members: Vec<String>,
}

/// A typed reference from a node to content (file or external URI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(skip_serializing, default)]
    pub id: i64,
    pub public_id: String,
    /// `ref` of the owning node (filled on read for convenience).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_ref: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_owner: Option<OwnerKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_owner: Option<OwnerKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    /// Free-form JSON sidecar (the `artifacts.metadata` column). Carries the
    /// typed `xref[]` vocabulary (HV-69). `None` when the column is empty/`{}`,
    /// and skipped on serialize so an artifact with no metadata reads
    /// byte-identically to before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    pub revision: i64,
    pub sync_state: SyncState,
}

/// How `remove`/`rename` pick the one artifact to act on within a node. `Role`
/// is the convenient key but may match more than one row (a node can hold
/// several same-role artifacts) — callers refuse an ambiguous `Role` and ask
/// for `Name` (the `path` basename) or `Id` (the `public_id`), which are unique.
#[derive(Debug, Clone)]
pub enum ArtifactSelector {
    Role(ArtifactRole),
    Name(String),
    Id(String),
}

/// A lineage event with its from→to edges, resolved to `ref`s. Returned by
/// `lineage`/`evolve graph`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageEvent {
    pub public_id: String,
    pub event_type: EventType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
    pub created_at: String,
    pub from: Vec<String>,
    pub to: Vec<String>,
}

/// The context pack that governs building a leaf: the grouping container that
/// carries it, plus the pack artifact's name. Derived on read from
/// `member --grouping--> container` where the container holds a `context-pack`
/// artifact (HV-124) — never stored, so there is no second source of
/// truth. A leaf claimed by more than one packed container surfaces a clash
/// instead (see `Item::context_pack_clash`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextPack {
    /// The release/phase container whose `context-pack` artifact is the pack.
    pub container: String,
    /// The pack artifact's name (always `context-pack.md`).
    pub artifact: String,
}

/// The serialized view of a node row — the unit the CLI and MCP return.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    /// Local integer id — never serialized, never synced (SPEC §1).
    #[serde(skip_serializing, default)]
    pub id: i64,
    pub public_id: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub project: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Acceptance statement — what success is. The anchor an output is verified
    /// against on the ready→done transition. Short and structured, not content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_looks_like: Option<String>,
    /// One-line provenance / vision trace — why this item exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// Optional deadline as a calendar date `YYYY-MM-DD` (no time, no timezone),
    /// validated at the Store boundary. A stored attribute only — it does NOT
    /// influence `next()`'s ordering (the static-vs-computed ranking fork is
    /// deferred; see HV-67's spec). Surfaced on full reads only, omitted when null.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_kind: Option<OwnerKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_state: Option<WaitState>,
    pub committed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_key: Option<String>,
    pub metadata: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    pub revision: i64,
    pub sync_state: SyncState,

    /// Derived container rollup, computed on read (never stored, never parsed
    /// back). `Some` only for container nodes; `None` for leaves and wherever it
    /// hasn't been hydrated. Serializes out but is ignored on deserialize.
    #[serde(skip_serializing_if = "Option::is_none", skip_deserializing)]
    pub rollup_state: Option<RollupState>,

    /// Sibling of `rollup_state` (HV-104): `Some(true)` when this container has at
    /// least one LIVE uncommitted descendant — so a container reading `done` still
    /// signals real remaining work beneath it (the rollup itself counts only
    /// committed descendants). `Some` only for containers; `None` for leaves and
    /// wherever it hasn't been hydrated. Serializes out, ignored on deserialize.
    #[serde(skip_serializing_if = "Option::is_none", skip_deserializing)]
    pub has_uncommitted_descendants: Option<bool>,

    // Optional includes (SPEC §2 `item get --include`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edges: Option<Edges>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<Artifact>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineage: Option<Vec<LineageEvent>>,

    /// The context pack governing this leaf's build, derived on read (never
    /// stored). `Some` only when exactly one grouping container carries the
    /// pack; `None` when zero — or when there's a clash (see below).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub context_pack: Option<ContextPack>,
    /// Set *instead of* `context_pack` when the leaf is claimed by more than one
    /// packed container — the conflicting container refs. A dispatcher must not
    /// build against a guessed pack; the clash is resolved (re-prepped) first.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub context_pack_clash: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_sql_and_json_agree() {
        // The SQL literal and the JSON literal are the same string.
        assert_eq!(Status::InProgress.as_str(), "in_progress");
        assert_eq!(
            serde_json::to_string(&Status::InProgress).unwrap(),
            "\"in_progress\""
        );
        assert_eq!(Status::parse("in_progress").unwrap(), Status::InProgress);
        assert!(Status::parse("bogus").is_err());
    }

    #[test]
    fn wait_state_round_trips() {
        for s in ["on_human", "on_dependency", "on_external"] {
            assert_eq!(WaitState::parse(s).unwrap().as_str(), s);
        }
    }

    /// HV-152: every closed-set rejection names the legal value-set inline — not
    /// just the offending value — so a wrong value is self-correcting in one step.
    #[test]
    fn enum_parse_error_names_the_legal_set() {
        let err = Status::parse("doing").unwrap_err().to_string();
        // The bad value is still named …
        assert!(err.contains("doing"), "names the offending value: {err}");
        // … and so is the full legal set (every variant literal).
        for v in [
            "discovery",
            "definition",
            "ready",
            "in_progress",
            "blocked",
            "done",
            "superseded",
            "archived",
        ] {
            assert!(err.contains(v), "Status error must name {v:?}: {err}");
        }

        // NodeType too (same macro): the legal set rides the message.
        let err = NodeType::parse("bug").unwrap_err().to_string();
        for v in ["task", "code", "research", "design", "phase", "anchor"] {
            assert!(err.contains(v), "NodeType error must name {v:?}: {err}");
        }

        // ArtifactRole too.
        let err = ArtifactRole::parse("doc").unwrap_err().to_string();
        for v in ["spec", "design", "delivery", "context-pack"] {
            assert!(err.contains(v), "ArtifactRole error must name {v:?}: {err}");
        }
    }

    /// HV-152: common synonyms are NAMED in the rejection as the correct value
    /// (not silently accepted as aliases). bug/idea→type, open/icebox/doing/
    /// backlog→status, doc→artifact role.
    #[test]
    fn enum_parse_error_names_common_synonyms() {
        // type synonyms.
        let err = NodeType::parse("bug").unwrap_err().to_string();
        assert!(err.contains("code"), "bug should point to code: {err}");
        let err = NodeType::parse("idea").unwrap_err().to_string();
        assert!(err.contains("task"), "idea should point to task: {err}");
        // status synonyms.
        for (syn, want) in [
            ("open", "discovery"),
            ("icebox", "discovery"),
            ("doing", "in_progress"),
            ("backlog", "ready"),
        ] {
            let err = Status::parse(syn).unwrap_err().to_string();
            assert!(
                err.contains(want),
                "status {syn:?} should point to {want:?}: {err}"
            );
        }
        // artifact-role synonym.
        let err = ArtifactRole::parse("doc").unwrap_err().to_string();
        assert!(
            err.contains("spec") || err.contains("design"),
            "doc should point to a real role: {err}"
        );
        // Synonyms are NAMED, never accepted — they still error.
        assert!(NodeType::parse("bug").is_err());
        assert!(Status::parse("doing").is_err());
        assert!(ArtifactRole::parse("doc").is_err());
    }
}
