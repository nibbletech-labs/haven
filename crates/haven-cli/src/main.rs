//! `haven` — the CLI binary. A thin clap front-end over the `haven-core`
//! `Store` service. JSON to stdout by default; `--pretty` for tables; errors as
//! `{"error": {...}}` on stderr with a non-zero exit (SPEC §2).

mod config;
mod output;

use std::path::PathBuf;
use std::time::Instant;

use clap::{Args, Parser, Subcommand, ValueEnum};
use haven_core::{
    telemetry::{self, TelemetryLine},
    ArtifactKind, ArtifactRole, ArtifactSelector, CompleteInput, DueUpdate, HandoffInput,
    HavenError, Include, IntegrityKind, ItemFilter, ItemUpdate, LineageDirection, NewArtifact,
    NewItem, NodeType, OwnerKind, Result, Status, Store, WaitState, WaitUpdate,
    DEFAULT_DISPATCH_LIMIT, DEFAULT_NEXT_LIMIT,
};

use output::Output;

const CLOUD_SYNC_PREVIEW_ENV: &str = "HAVEN_CLOUD_SYNC_PREVIEW";

fn cloud_sync_preview_enabled() -> bool {
    std::env::var(CLOUD_SYNC_PREVIEW_ENV)
        .ok()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn require_cloud_sync_preview() -> Result<()> {
    if cloud_sync_preview_enabled() {
        Ok(())
    } else {
        Err(HavenError::Invalid(format!(
            "Cloud Sync is in private preview. Set {CLOUD_SYNC_PREVIEW_ENV}=1 to enable the unfinished auth/sync commands."
        )))
    }
}

fn hide_cloud_sync_status(v: &mut serde_json::Value) {
    if !cloud_sync_preview_enabled() {
        if let Some(obj) = v.as_object_mut() {
            obj.remove("sync_pending");
        }
    }
}

#[derive(Parser)]
#[command(name = "haven", version, about = "Local-first work-graph store")]
struct Cli {
    /// Project key (defaults to the current project set by `haven project use`).
    #[arg(short, long, global = true)]
    project: Option<String>,

    /// Render human-readable tables instead of JSON.
    #[arg(long, global = true)]
    pretty: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create ~/.haven, init the DB + migrations, wire MCP, install the skill (idempotent).
    Setup {
        /// Agent wiring to install: all, claude, or codex.
        #[arg(long, value_enum, default_value_t = AgentTarget::All)]
        agent: AgentTarget,
        /// Skip installing the Claude skill (headless / non-Claude installs).
        #[arg(long)]
        no_skill: bool,
        /// Write or refresh the repo-local AGENTS.md Haven discovery stanza.
        #[arg(long = "agents-md")]
        agents_md: bool,
        /// Optional first project key to create/select during setup.
        #[arg(long = "project-key")]
        project_key: Option<String>,
        /// Title for --project-key. Defaults to the key when omitted.
        #[arg(long = "project-title")]
        project_title: Option<String>,
        /// Ref prefix for --project-key, e.g. HV. Defaults to the first two key letters.
        #[arg(long)]
        prefix: Option<String>,
    },
    /// Initialise/migrate the database only.
    Init,
    /// DB location, per-project counts, sync queue depth.
    Status {
        /// Optional project key — a bare positional that resolves exactly like
        /// `-p <key>` (so `haven status demo` == `haven status -p demo`).
        project_key: Option<String>,
    },
    /// Diagnose db integrity, migration version, auth, connectivity.
    Doctor,
    /// Get/set local config values.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Manage projects.
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Manage items (the work-graph nodes).
    Item {
        #[command(subcommand)]
        cmd: ItemCmd,
    },
    /// Bulk-import items from a JSON file: one validated, all-or-nothing
    /// transaction. Items take the `item add` fields plus a temp `id` and
    /// ref-or-temp-id edge fields (`parent`, `depends_on` array, `group`).
    Import {
        /// Path to a JSON array of item objects.
        file: std::path::PathBuf,
        /// Per item: skip creation when a live item's normalized title matches.
        #[arg(long = "if-absent")]
        if_absent: bool,
    },
    /// One-shot session-context block: project state, the committed queue (next
    /// flagged), in-progress/waiting, conventions, and the untriaged inbox — read
    /// at session start instead of several list/next/status calls (HV-23).
    Prime {
        /// Optional project key — a bare positional that resolves like `-p <key>`.
        project_key: Option<String>,
    },
    /// The ready-to-dispatch query.
    Next(NextArgs),
    /// Lean "what should I work on?" briefing: bounded next + targeted context.
    Dispatch(DispatchArgs),
    /// Untriaged floaters: uncommitted, live, no acceptance yet — the triage queue.
    Inbox(InboxArgs),
    /// Cross-store links on a node's artifacts: outbound xrefs + inbound backlinks.
    Xref(XrefArgs),
    /// Decomposition edges: HV-3 is composed of HV-7, HV-8.
    Decompose(DecomposeArgs),
    /// Dependency edges: HV-5 depends on (is blocked by) HV-4.
    Depend(DependArgs),
    /// Grouping edges: a release/phase/gate contains members.
    Group(GroupArgs),
    /// Lineage operations: split / merge / supersede / graph.
    Evolve {
        #[command(subcommand)]
        cmd: EvolveCmd,
    },
    /// Full-text search over item title/body.
    Search(SearchArgs),
    /// Export the whole project work-graph (all nodes + edges) in one read.
    Graph(GraphArgs),
    /// List project living-doc anchors and their artifacts.
    Docs,
    /// Manage content artifacts on an item.
    Artifact {
        #[command(subcommand)]
        cmd: ArtifactCmd,
    },
    /// Append a free scratch line to an item's dated notes file (no DB row).
    Note { reference: String, text: String },
    /// (Re)write the project's backlog.md projection.
    Render,
    /// Create a visible repo-local Haven/ workspace projection.
    Link {
        /// Visible workspace directory to create in the current repo.
        #[arg(long, default_value = "Haven")]
        name: PathBuf,
    },
    /// Install the embedded skill snapshot.
    Skill {
        #[command(subcommand)]
        cmd: SkillCmd,
    },
    /// Manage this `haven` binary: install/relink it onto PATH, or update.
    #[command(name = "self")]
    Slf {
        #[command(subcommand)]
        cmd: SelfCmd,
    },
    /// Run the MCP server over stdio (the surface builder/app consume).
    Mcp,
    /// Auth0 sign-in / sign-out / status.
    #[command(hide = true)]
    Auth {
        #[command(subcommand)]
        cmd: AuthCmd,
    },
    /// Sync with the cloud (push now, or report queue status).
    #[command(hide = true)]
    Sync {
        #[command(subcommand)]
        cmd: Option<SyncCmd>,
        /// Run as a background loop (reachability-driven). Designed, not v1.
        #[arg(long)]
        watch: bool,
    },
    /// Local self-backups: snapshot, list, verify integrity, restore.
    Backup {
        #[command(subcommand)]
        cmd: BackupCmd,
    },
    /// Catch-all for an unrecognized verb (hidden). Lets us intercept the common
    /// verb-divergence guesses — `list-items`, the MCP-flat `get`/`add`/`archive`/
    /// `handoff` that collide with item-nesting — and answer with an error naming
    /// the exact corrective `haven …` command instead of clap's bare
    /// "unrecognized subcommand" (HV-158). The first word is the typed verb; any
    /// remainder is the rest of the line (used to echo args into the tip).
    #[command(external_subcommand)]
    Unknown(Vec<String>),
}

#[derive(Subcommand)]
enum BackupCmd {
    /// List snapshots (id, size, integrity, format), newest first.
    List,
    /// Take a snapshot now: content-addressed objects + a per-snapshot manifest.
    Now,
    /// Verify a snapshot: re-hash every referenced object (or integrity-check a
    /// legacy snapshot's DB). Latest if no id.
    Verify(BackupVerifyArgs),
    /// Restore a snapshot. Safety-snapshots current state first, then swaps it in.
    Restore(BackupRestoreArgs),
    /// Clear a quarantined (`*-SUSPECT`) snapshot, un-freezing rotation + GC.
    Clear(BackupClearArgs),
}

#[derive(Args)]
struct BackupVerifyArgs {
    /// Snapshot id (the `<UTC-ts>` manifest/dir name). Defaults to the latest.
    id: Option<String>,
}

#[derive(Args)]
struct BackupRestoreArgs {
    /// Snapshot id (the `<UTC-ts>` manifest/dir name) to restore.
    id: String,
    /// Confirm the destructive DB + content-file swap (required).
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct BackupClearArgs {
    /// The quarantined snapshot id (a `<UTC-ts>-SUSPECT`) to remove.
    id: String,
}

#[derive(Subcommand)]
enum AuthCmd {
    /// Sign in via Auth0 Device Flow, or paste a token with --token.
    Login {
        #[arg(long)]
        token: Option<String>,
    },
    /// Clear stored tokens (local data is untouched).
    Logout,
    /// Show whether you're signed in and when the token expires.
    Status,
}

#[derive(Subcommand)]
enum SyncCmd {
    /// Queue depth, last error, pending counts.
    Status,
}

#[derive(Subcommand)]
enum SkillCmd {
    /// Write the embedded skill(s) to the selected agent skill path.
    Install {
        /// Agent skill target: all, claude, or codex.
        #[arg(long, value_enum, default_value_t = AgentTarget::Claude)]
        agent: AgentTarget,
        /// Install only this skill (default: all shipped skills).
        #[arg(long)]
        skill: Option<String>,
    },
}

#[derive(Subcommand)]
enum SelfCmd {
    /// Promote this build onto your PATH (copy), or --link it for development.
    Install(SelfInstallArgs),
    /// Report how to update (install-method aware) and the latest version.
    Update(SelfUpdateArgs),
}

#[derive(Args)]
struct SelfInstallArgs {
    /// Dev mode: symlink the install dir's `haven` at this build so rebuilds go
    /// live, instead of copying.
    #[arg(long)]
    link: bool,
    /// Install directory. Defaults to the first writable of $HAVEN_BIN_DIR,
    /// /usr/local/bin, ~/.local/bin (mirrors install.sh).
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Overwrite an existing binary at the destination (copy mode).
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct SelfUpdateArgs {
    /// Only check the latest version and report; change nothing.
    #[arg(long)]
    check: bool,
    /// For a Homebrew install, actually run `brew upgrade` (default: print only).
    #[arg(long)]
    run: bool,
    /// Download + verify + swap the prebuilt release binary in place. The default
    /// for copied (install.sh) installs; required to force it for an unrecognized
    /// install location. Has no effect on Homebrew (use `--run`) or dev symlinks.
    #[arg(long)]
    binary: bool,
    /// Install this exact release tag instead of the latest (e.g. `v0.1.1-rc.1`).
    /// Implies the binary path and bypasses the newer-than-current check — handy
    /// for testing a pre-release.
    #[arg(long)]
    tag: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum AgentTarget {
    All,
    Claude,
    Codex,
}

impl AgentTarget {
    fn includes_claude(self) -> bool {
        matches!(self, AgentTarget::All | AgentTarget::Claude)
    }

    fn includes_codex(self) -> bool {
        matches!(self, AgentTarget::All | AgentTarget::Codex)
    }
}

#[derive(Subcommand)]
// `Add` carries more fields than `List`/`Get`; this is a parsed-once CLI value,
// so boxing would only add indirection.
#[allow(clippy::large_enum_variant)]
enum ArtifactCmd {
    Add(ArtifactAddArgs),
    List(ArtifactListArgs),
    /// Get one artifact by ref and role.
    #[command(alias = "show")]
    Get(ArtifactGetArgs),
    /// Remove an artifact (row + backing file) by role/name/id.
    Rm(ArtifactRmArgs),
    /// Rename an artifact's backing file (role/history preserved).
    Mv(ArtifactMvArgs),
}

#[derive(Args)]
struct ArtifactAddArgs {
    reference: String,
    /// spec | research | design | handoff | decision | scratch | source | delivery | vision.
    #[arg(long)]
    role: String,
    /// file | external | delivery. Inferred from --file/--content/--uri when omitted.
    #[arg(long)]
    kind: Option<String>,
    /// Source file to copy into ~/.haven/<project>/items/<ref>/.
    #[arg(long)]
    file: Option<PathBuf>,
    /// Inline content written to a file by the server (alternative to --file).
    #[arg(long)]
    content: Option<String>,
    /// Destination filename. For --content it defaults to <role>.md; for --file
    /// it overrides the source basename.
    #[arg(long)]
    name: Option<String>,
    /// Overwrite in place if an artifact already exists at the same path on the
    /// item (default: reject the collision).
    #[arg(long)]
    replace: bool,
    /// External URL, obsidian:// link, or delivery link.
    #[arg(long)]
    uri: Option<String>,
    /// Optional display title for external/delivery artifacts.
    #[arg(long)]
    title: Option<String>,
    /// Optional short excerpt or note.
    #[arg(long)]
    excerpt: Option<String>,
    /// Handoff source owner: human | ai.
    #[arg(long)]
    from: Option<String>,
    /// Handoff target owner: human | ai.
    #[arg(long)]
    to: Option<String>,
    /// Optional creator handle.
    #[arg(long)]
    by: Option<String>,
}

#[derive(Args)]
struct ArtifactListArgs {
    reference: String,
    #[arg(long)]
    role: Option<String>,
}

#[derive(Args)]
struct ArtifactGetArgs {
    reference: String,
    #[arg(long)]
    role: Option<String>,
    #[arg(long)]
    path: Option<String>,
}

#[derive(Args)]
struct ArtifactRmArgs {
    reference: String,
    /// Select by role (refused if it matches more than one artifact).
    #[arg(long)]
    role: Option<String>,
    /// Select by file name (the path basename).
    #[arg(long)]
    name: Option<String>,
    /// Select by artifact public id.
    #[arg(long)]
    id: Option<String>,
}

#[derive(Args)]
struct ArtifactMvArgs {
    reference: String,
    /// New plain file name for the artifact.
    new_name: String,
    /// Select by role (refused if it matches more than one artifact).
    #[arg(long)]
    role: Option<String>,
    /// Select by file name (the path basename).
    #[arg(long)]
    name: Option<String>,
    /// Select by artifact public id.
    #[arg(long)]
    id: Option<String>,
}

#[derive(Subcommand)]
enum ConfigCmd {
    Get { key: String },
    Set { key: String, value: String },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Create a project namespace/backlog.
    Add(ProjectAddArgs),
    /// List project namespaces/backlogs. Hides archived projects unless
    /// `--include-archived`.
    List(ProjectListArgs),
    /// Get one project by key.
    #[command(alias = "show")]
    Get { key: String },
    /// Set the default project for later commands.
    Use { key: String },
    /// Soft-archive a project: retire it (hidden from default listings, writes
    /// refused) while keeping its namespace fully reserved. Reversible via
    /// `project reopen`. The everyday "retire this" — never a hard delete.
    Archive {
        key: String,
        /// Why it's being archived (recorded on the row). Alias: `--reason`.
        #[arg(long, alias = "reason")]
        rationale: Option<String>,
        /// Actor handle, for audit.
        #[arg(long)]
        by: Option<String>,
    },
    /// Reopen an archived project: restore it fully (refs continue from the
    /// preserved counter — nothing was destroyed).
    Reopen {
        key: String,
        /// Actor handle, for audit.
        #[arg(long)]
        by: Option<String>,
    },
}

#[derive(Args)]
struct ProjectAddArgs {
    /// Project slug used by --project and current-project selection.
    #[arg(long)]
    key: String,
    /// Human-readable project title.
    #[arg(long)]
    title: String,
    /// Item ref prefix, e.g. HV. Defaults to the first two key letters.
    #[arg(long)]
    prefix: Option<String>,
    /// Optional project description.
    #[arg(long)]
    description: Option<String>,
}

#[derive(Args)]
struct ProjectListArgs {
    /// Include archived projects in the listing (default: active only). Never
    /// shows a deleted/tombstoned project.
    #[arg(long = "include-archived")]
    include_archived: bool,
}

#[derive(Subcommand)]
enum ItemCmd {
    /// Create an item. Defaults to uncommitted discovery work in the icebox.
    Add(ItemAddArgs),
    /// List items with optional filters.
    List(ItemListArgs),
    /// Get one item by ref or public_id.
    #[command(alias = "show")]
    Get(ItemGetArgs),
    /// Update item fields such as status, type, acceptance, wait-state, and priority.
    ///
    /// Commitment is its own verb (`item commit`/`uncommit`), not a flag here.
    /// To pass the work baton between ai and human (flip owner + record a note in
    /// one atomic step), use `haven item handoff <ref> --to human|ai`.
    Update(ItemUpdateArgs),
    /// Commit one or more items so ready/unblocked work can appear in `haven next`.
    Commit {
        #[arg(required = true)]
        references: Vec<String>,
        /// Priority band 0-4. Lower numbers sort first.
        #[arg(long)]
        priority: Option<i64>,
    },
    /// Mark one or more items uncommitted. Priority is retained.
    Uncommit {
        #[arg(required = true)]
        references: Vec<String>,
    },
    /// Claim an item: set owner + in_progress atomically (errors if already claimed).
    Claim(ItemClaimArgs),
    /// Assign execution ownership to human or ai.
    Assign(ItemAssignArgs),
    /// Hand an item over (ai↔human): record a handoff note, flip owner, set wait/status.
    Handoff(ItemHandoffArgs),
    /// Mark an item done: record evidence, set status, report what it unblocked.
    Complete(ItemCompleteArgs),
    /// Fine-order an item before or after a sibling in the same priority band.
    Rank(ItemRankArgs),
    /// Park one or more items without deleting them, emitting lineage.
    Archive {
        #[arg(required = true)]
        references: Vec<String>,
        #[arg(long)]
        rationale: Option<String>,
    },
    /// Reopen an archived/superseded item into discovery.
    Reopen {
        reference: String,
        #[arg(long)]
        rationale: Option<String>,
    },
}

#[derive(Args)]
struct ItemAddArgs {
    title: String,
    /// task | code | research | data | design | admin | release | phase | gate | anchor.
    #[arg(long = "type")]
    node_type: Option<String>,
    /// Short summary. Rich content should be stored as artifacts.
    #[arg(long)]
    body: Option<String>,
    /// Acceptance statement — what success looks like (the verify anchor).
    #[arg(long = "done-looks-like")]
    done_looks_like: Option<String>,
    /// One-line provenance — why this item exists.
    #[arg(long)]
    why: Option<String>,
    /// discovery | definition | ready | in_progress | blocked | done | superseded | archived.
    #[arg(long)]
    status: Option<String>,
    /// Priority band 0-4. Lower numbers sort first.
    #[arg(long)]
    priority: Option<i64>,
    /// Commit immediately so ready/unblocked work can appear in `haven next`.
    #[arg(long)]
    commit: bool,
    /// Owner kind: human | ai.
    #[arg(long)]
    assign: Option<String>,
    /// Add a decomposition parent edge.
    #[arg(long)]
    parent: Option<String>,
    /// Add a dependency edge; this item depends on the referenced item.
    #[arg(long = "depends-on")]
    depends_on: Option<String>,
    /// Add this item to a release/phase/gate group.
    #[arg(long)]
    group: Option<String>,
    /// Deadline as a calendar date YYYY-MM-DD (no time, no timezone).
    #[arg(long = "due-at")]
    due_at: Option<String>,
    /// Return the existing live item (marked `existing: true`) instead of
    /// creating a duplicate, when a normalized-title match exists.
    #[arg(long = "if-absent")]
    if_absent: bool,
}

#[derive(Args)]
struct ItemListArgs {
    /// Filter by status.
    #[arg(long)]
    status: Option<String>,
    /// Filter by item type.
    #[arg(long = "type")]
    node_type: Option<String>,
    /// Filter by owner kind: human | ai.
    #[arg(long)]
    owner: Option<String>,
    /// Show committed items only.
    #[arg(long)]
    committed: bool,
    /// Show uncommitted, non-archived/superseded items.
    #[arg(long)]
    icebox: bool,
    /// Show members of a release/phase/gate group.
    #[arg(long)]
    group: Option<String>,
    /// Only items parked on this wait-state: on_human | on_dependency | on_external.
    #[arg(long)]
    wait: Option<String>,
    /// Only items untouched for at least N days (stale/forgotten work).
    #[arg(long)]
    stale: Option<i64>,
    /// Return at most N items (parity with `next`); applied after ordering.
    #[arg(long)]
    limit: Option<usize>,
    /// Skip the first N items (paginate with --limit).
    #[arg(long)]
    offset: Option<usize>,
    /// Include archived/superseded (dead) items. Default hides them; an explicit
    /// `--status archived|superseded` still reaches them regardless (HV-53).
    #[arg(long)]
    all: bool,
}

#[derive(Args)]
struct ItemGetArgs {
    reference: String,
    /// Comma-separated: edges,artifacts,lineage
    #[arg(long, value_delimiter = ',')]
    include: Vec<String>,
}

#[derive(Args)]
struct ItemUpdateArgs {
    /// One or more refs — the same update is applied to each (grooming).
    #[arg(required = true)]
    references: Vec<String>,
    /// Replace the title.
    #[arg(long)]
    title: Option<String>,
    /// Replace the short summary.
    #[arg(long)]
    body: Option<String>,
    #[arg(long = "done-looks-like")]
    done_looks_like: Option<String>,
    #[arg(long)]
    why: Option<String>,
    /// discovery | definition | ready | in_progress | blocked | done | superseded | archived.
    #[arg(long)]
    status: Option<String>,
    /// Priority band 0-4. Lower numbers sort first.
    #[arg(long)]
    priority: Option<i64>,
    /// task | code | research | data | design | admin | release | phase | gate | anchor.
    #[arg(long = "type")]
    node_type: Option<String>,
    /// on_human | on_dependency | on_external | none
    #[arg(long)]
    wait: Option<String>,
    /// Deadline as a calendar date YYYY-MM-DD; `none` clears it.
    #[arg(long = "due-at")]
    due_at: Option<String>,
    /// Hidden capture of the common `item update --commit` mistake: commitment is
    /// its own verb. Set → we error naming `haven item commit <ref…>` (HV-158).
    #[arg(long, hide = true)]
    commit: bool,
    /// Hidden capture of `item update --uncommit` → error naming
    /// `haven item uncommit <ref…>`.
    #[arg(long, hide = true)]
    uncommit: bool,
}

impl ItemUpdateArgs {
    /// If `--commit`/`--uncommit` were (mis)used on `item update`, return the
    /// exact corrective command — the dedicated verb, with these refs echoed —
    /// so the error can name it. `None` when neither flag is set.
    fn misused_commit_tip(&self) -> Option<String> {
        let refs = self.references.join(" ");
        match (self.commit, self.uncommit) {
            (true, _) => Some(format!(
                "commitment is its own verb, not an `item update` flag — use `haven item commit {refs}`"
            )),
            (_, true) => Some(format!(
                "use `haven item uncommit {refs}` (commitment is its own verb, not an `item update` flag)"
            )),
            _ => None,
        }
    }
}

#[derive(Args)]
struct ItemAssignArgs {
    reference: String,
    /// Owner kind: human | ai.
    #[arg(long = "to")]
    to: String,
    /// Optional actor handle, e.g. ai:claude or human:tom.
    #[arg(long)]
    actor: Option<String>,
}

#[derive(Args)]
struct ItemClaimArgs {
    reference: String,
    /// Who's taking it: ai (default) | human. Claim-on-pickup is the agent case.
    #[arg(long = "as", default_value = "ai")]
    owner: String,
    /// Optional actor handle recorded as the assignee, e.g. ai:claude or human:tom.
    #[arg(long)]
    actor: Option<String>,
}

#[derive(Args)]
struct ItemHandoffArgs {
    reference: String,
    /// Who picks it up next: human | ai.
    #[arg(long = "to")]
    to: String,
    /// Who's handing off (defaults to the item's current owner).
    #[arg(long)]
    from: Option<String>,
    /// The baton note, recorded as a handoff artifact under notes/.
    #[arg(long)]
    note: Option<String>,
    /// Override the status (default: blocked when handing to a human).
    #[arg(long)]
    status: Option<String>,
    /// Override the wait-state (default: on_human to a human, cleared to ai).
    #[arg(long)]
    wait: Option<String>,
    /// Actor handle recorded as the new assignee / note author.
    #[arg(long)]
    actor: Option<String>,
}

#[derive(Args)]
struct ItemCompleteArgs {
    reference: String,
    /// Proof the work is done (test output, summary, link) — saved as an artifact.
    #[arg(long)]
    evidence: Option<String>,
    /// Role for the evidence artifact (default: delivery).
    #[arg(long)]
    role: Option<String>,
    /// Creator handle recorded on the evidence artifact.
    #[arg(long)]
    by: Option<String>,
}

#[derive(Args)]
struct ItemRankArgs {
    reference: String,
    /// Place this item before the referenced item.
    #[arg(long)]
    before: Option<String>,
    /// Place this item after the referenced item.
    #[arg(long)]
    after: Option<String>,
}

#[derive(Args)]
struct NextArgs {
    /// Explain why the dispatch queue is empty instead of returning items.
    #[arg(long)]
    explain: bool,
    /// Filter by owner kind: human | ai.
    #[arg(long)]
    owner: Option<String>,
    /// Maximum number of dispatchable items to return (defaults to the top 50 of
    /// the ranked frontier; pass a larger value for more).
    #[arg(long)]
    limit: Option<i64>,
}

#[derive(Args)]
struct DispatchArgs {
    /// Filter by owner kind: human | ai.
    #[arg(long)]
    owner: Option<String>,
    /// Maximum number of candidates to return (defaults to 5).
    #[arg(long)]
    limit: Option<i64>,
    /// Restrict candidates to live descendants of this parent/release/phase ref.
    #[arg(long)]
    scope: Option<String>,
    /// Include diagnostic counts even when candidates exist.
    #[arg(long)]
    explain: bool,
}

#[derive(Args)]
struct InboxArgs {
    /// Filter by owner kind: human | ai.
    #[arg(long)]
    owner: Option<String>,
    /// Return at most N items.
    #[arg(long)]
    limit: Option<usize>,
    /// Skip the first N items.
    #[arg(long)]
    offset: Option<usize>,
}

#[derive(Args)]
struct DecomposeArgs {
    parent: String,
    #[arg(long = "into")]
    into: Vec<String>,
    #[arg(long = "remove")]
    remove: Vec<String>,
}

#[derive(Args)]
struct DependArgs {
    node: String,
    #[arg(long = "on")]
    on: Vec<String>,
    #[arg(long = "remove")]
    remove: Vec<String>,
}

#[derive(Args)]
struct GroupArgs {
    group: String,
    #[arg(long = "add")]
    add: Vec<String>,
    #[arg(long = "remove")]
    remove: Vec<String>,
}

#[derive(Subcommand)]
enum EvolveCmd {
    Split(EvolveSplitArgs),
    Merge(EvolveMergeArgs),
    Supersede(EvolveSupersedeArgs),
    Graph(EvolveGraphArgs),
    /// Follow a stale (superseded/archived) ref forward to its live descendant(s).
    Resolve {
        reference: String,
    },
}

#[derive(Args)]
struct EvolveSplitArgs {
    reference: String,
    #[arg(long = "into")]
    into: Vec<String>,
    #[arg(long)]
    rationale: Option<String>,
    #[arg(long)]
    by: Option<String>,
}

#[derive(Args)]
struct EvolveMergeArgs {
    references: Vec<String>,
    #[arg(long)]
    title: String,
    #[arg(long)]
    rationale: Option<String>,
    #[arg(long)]
    by: Option<String>,
}

#[derive(Args)]
struct EvolveSupersedeArgs {
    reference: String,
    #[arg(long)]
    with: String,
    #[arg(long)]
    rationale: Option<String>,
    #[arg(long)]
    by: Option<String>,
}

#[derive(Args)]
struct EvolveGraphArgs {
    reference: String,
    #[arg(long)]
    direction: Option<String>,
    #[arg(long)]
    depth: Option<i64>,
}

#[derive(Args)]
struct SearchArgs {
    query: String,
    #[arg(long)]
    limit: Option<i64>,
}

#[derive(Args)]
struct GraphArgs {
    /// Also include lineage links (split/merge/supersede/archive history).
    #[arg(long)]
    lineage: bool,
    /// Include archived/superseded (dead) nodes. Default is live-only, matching
    /// the `haven_graph` MCP tool (HV-53).
    #[arg(long)]
    all: bool,
}

#[derive(Args)]
struct XrefArgs {
    /// The item whose artifacts' cross-store links to report (a `ref` or public_id).
    #[arg(value_name = "REF")]
    reference: String,
}

fn main() {
    let cli = Cli::parse();
    // Loud, on every command: a quarantined snapshot freezes rotation until the
    // operator clears it. stderr only (stdout is structured Output / the MCP channel).
    warn_if_quarantined();
    match run(&cli) {
        Ok(out) => {
            out.render(cli.pretty);
            maybe_render(&cli);
            maybe_daily_backup();
        }
        Err(err) => std::process::exit(output::render_error(&err)),
    }
}

/// Map a typed-but-unrecognized top-level verb to the exact corrective `haven …`
/// command, when it's a known divergence guess. `words[0]` is the verb; the rest
/// is echoed into the tip so the user can copy-paste it. Returns `None` for a
/// genuinely unknown verb (the caller falls back to generic help). Covers the
/// MCP-flat names that collide with item-nesting (`get`/`add`/`archive`/
/// `handoff`) and the `list-items` contraction (HV-158).
fn corrective_for_unknown(words: &[String]) -> Option<String> {
    let (verb, rest) = words.split_first()?;
    let tail = rest.join(" ");
    let with_tail = |cmd: &str| {
        if tail.is_empty() {
            format!("did you mean `{cmd}`?")
        } else {
            format!("did you mean `{cmd} {tail}`?")
        }
    };
    let tip = match verb.as_str() {
        "list-items" | "items" => with_tail("haven item list"),
        // MCP tools are flat (`haven_get_item`, `haven_archive`, …); the CLI nests
        // these under `item`. A user reaching for the flat name lands here.
        "get" | "show" => with_tail("haven item get"),
        "add" | "new" => with_tail("haven item add"),
        "update" => with_tail("haven item update"),
        "archive" => with_tail("haven item archive"),
        "reopen" => with_tail("haven item reopen"),
        "complete" | "done" => with_tail("haven item complete"),
        "handoff" => with_tail("haven item handoff"),
        "assign" => with_tail("haven item assign"),
        "commit" => with_tail("haven item commit"),
        "uncommit" => with_tail("haven item uncommit"),
        "rank" => with_tail("haven item rank"),
        _ => return None,
    };
    Some(tip)
}

/// How the repo-binding guard treats a command: a project-scoped write that
/// could mis-file, a project-scoped read, or something the binding doesn't touch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GuardKind {
    Mutation,
    Read,
    Exempt,
}

/// Classify every command (exhaustive — no wildcard, so a new command/subcommand
/// fails to compile until it's deliberately classified).
fn guard_kind(cmd: &Command) -> GuardKind {
    match cmd {
        Command::Setup { .. }
        | Command::Init
        | Command::Doctor
        | Command::Config { .. }
        | Command::Project { .. }
        | Command::Link { .. }
        | Command::Skill { .. }
        | Command::Slf { .. }
        | Command::Mcp
        | Command::Auth { .. }
        | Command::Sync { .. }
        | Command::Backup { .. }
        // An unrecognized verb never touches the store — it short-circuits to a
        // corrective error before any project resolution (HV-158).
        | Command::Unknown(_) => GuardKind::Exempt,
        Command::Status { .. }
        | Command::Prime { .. }
        | Command::Next(_)
        | Command::Dispatch(_)
        | Command::Inbox(_)
        | Command::Xref(_)
        | Command::Search(_)
        | Command::Graph(_)
        | Command::Docs
        | Command::Render => GuardKind::Read,
        Command::Import { .. }
        | Command::Decompose(_)
        | Command::Depend(_)
        | Command::Group(_)
        | Command::Evolve { .. }
        | Command::Note { .. } => GuardKind::Mutation,
        Command::Item { cmd } => match cmd {
            ItemCmd::List(_) | ItemCmd::Get(_) => GuardKind::Read,
            ItemCmd::Add(_)
            | ItemCmd::Update(_)
            | ItemCmd::Commit { .. }
            | ItemCmd::Uncommit { .. }
            | ItemCmd::Claim(_)
            | ItemCmd::Assign(_)
            | ItemCmd::Handoff(_)
            | ItemCmd::Complete(_)
            | ItemCmd::Rank(_)
            | ItemCmd::Archive { .. }
            | ItemCmd::Reopen { .. } => GuardKind::Mutation,
        },
        Command::Artifact { cmd } => match cmd {
            ArtifactCmd::List(_) | ArtifactCmd::Get(_) => GuardKind::Read,
            ArtifactCmd::Add(_) | ArtifactCmd::Rm(_) | ArtifactCmd::Mv(_) => GuardKind::Mutation,
        },
    }
}

/// The guard's decision for a bound repo, given the op's resolved target project.
#[derive(Debug, PartialEq, Eq)]
enum GuardOutcome {
    Allow,
    Warn(String),
    Block(String),
}

/// Pure decision: `explicit` = an explicit `-p` was passed (the deliberate
/// cross-project override). A matching target — or an exempt op — is always fine.
fn guard_outcome(
    kind: GuardKind,
    bound: &str,
    target: Option<&str>,
    explicit: bool,
) -> GuardOutcome {
    if target == Some(bound) {
        return GuardOutcome::Allow;
    }
    let target_desc = target.unwrap_or("<none selected>");
    match kind {
        // Exempt is short-circuited before guard_outcome in practice; handled here
        // too so the match stays exhaustive without a wildcard (and unit-testable).
        GuardKind::Exempt => GuardOutcome::Allow,
        GuardKind::Read => GuardOutcome::Warn(format!(
            "reading project '{target_desc}', not this repo's linked project '{bound}'"
        )),
        // An explicit -p is the deliberate override → warn, don't block.
        GuardKind::Mutation if explicit => GuardOutcome::Warn(format!(
            "writing to project '{target_desc}', not this repo's linked project '{bound}' (-p override)"
        )),
        // The mis-file case: a write with no -p whose target differs from the binding.
        GuardKind::Mutation => GuardOutcome::Block(format!(
            "this repo is linked to project '{bound}', but the active project is '{target_desc}'. \
             Re-run with -p {bound} (or -p <other> for a deliberate cross-project write)."
        )),
    }
}

/// Gate a mis-filing write / warn a cross-project read when this repo carries a
/// Haven binding and the op's target project differs (HV-147). No binding → no-op
/// (project resolution is unchanged), so this is purely additive.
fn guard_repo_binding(cli: &Cli) -> Result<()> {
    let kind = guard_kind(&cli.command);
    if kind == GuardKind::Exempt {
        return Ok(());
    }
    let Some(bound) = config::repo_binding()? else {
        return Ok(());
    };
    // Resolved target = explicit -p, else the global current project (best-effort).
    let target = match cli.project.as_deref() {
        Some(p) => Some(p.to_string()),
        None => config::open_store()
            .ok()
            .and_then(|s| s.current_project().ok().flatten()),
    };
    match guard_outcome(kind, &bound, target.as_deref(), cli.project.is_some()) {
        GuardOutcome::Allow => Ok(()),
        GuardOutcome::Warn(msg) => {
            eprintln!("warn: {msg}");
            Ok(())
        }
        GuardOutcome::Block(msg) => Err(HavenError::Invalid(msg)),
    }
}

fn run(cli: &Cli) -> Result<Output> {
    let project = cli.project.as_deref();
    guard_repo_binding(cli)?;
    match &cli.command {
        Command::Setup {
            agent,
            no_skill,
            agents_md,
            project_key,
            project_title,
            prefix,
        } => cmd_setup(
            *agent,
            *no_skill,
            *agents_md,
            project_key.as_deref(),
            project_title.as_deref(),
            prefix.as_deref(),
        ),
        Command::Init => {
            config::open_store()?;
            Ok(Output::Message("database initialised".into()))
        }
        Command::Status { project_key } => {
            // The bare positional resolves exactly like `-p`: explicit `-p` still
            // wins, else the positional, else the current project (HV-158).
            cmd_status(project.or(project_key.as_deref()))
        }
        Command::Prime { project_key } => {
            // The bare positional resolves exactly like `-p` (mirrors `status`).
            let s = config::open_store()?;
            let block = s
                .prime(project.or(project_key.as_deref()))?
                .render_with_sync(cloud_sync_preview_enabled());
            Ok(Output::Text(block))
        }
        Command::Doctor => cmd_doctor(),
        Command::Config { cmd } => cmd_config(cmd),
        Command::Project { cmd } => cmd_project(cmd),
        Command::Item { cmd } => cmd_item_telemetered(project, cmd),
        Command::Import { file, if_absent } => cmd_import(project, file, *if_absent),
        Command::Next(a) => {
            let s = config::open_store()?;
            let owner = a.owner.as_deref().map(OwnerKind::parse).transpose()?;
            if a.explain {
                Ok(Output::Json(s.next_explain(project, owner)?))
            } else {
                // Bound the frontier by default (HV-194); an explicit --limit wins.
                Ok(Output::Items(s.next(
                    project,
                    owner,
                    a.limit.or(Some(DEFAULT_NEXT_LIMIT)),
                )?))
            }
        }
        Command::Dispatch(a) => {
            let s = config::open_store()?;
            let owner = a.owner.as_deref().map(OwnerKind::parse).transpose()?;
            Ok(Output::Json(serde_json::to_value(s.dispatch(
                project,
                owner,
                a.limit.or(Some(DEFAULT_DISPATCH_LIMIT)),
                a.scope.as_deref(),
                a.explain,
            )?)?))
        }
        Command::Inbox(a) => {
            let s = config::open_store()?;
            let owner = a.owner.as_deref().map(OwnerKind::parse).transpose()?;
            let filter = ItemFilter {
                inbox: true,
                owner,
                ..Default::default()
            };
            let mut items = s.list_items(project, &filter)?;
            if a.offset.is_some() || a.limit.is_some() {
                items = items
                    .into_iter()
                    .skip(a.offset.unwrap_or(0))
                    .take(a.limit.unwrap_or(usize::MAX))
                    .collect();
            }
            Ok(Output::Items(items))
        }
        Command::Decompose(a) => cmd_decompose(project, a),
        Command::Depend(a) => cmd_depend(project, a),
        Command::Group(a) => cmd_group(project, a),
        Command::Evolve { cmd } => cmd_evolve(project, cmd),
        Command::Search(a) => {
            let s = config::open_store()?;
            Ok(Output::Items(s.search(project, &a.query, a.limit)?))
        }
        Command::Graph(a) => {
            let s = config::open_store()?;
            let graph = s.project_graph(project, a.lineage)?;
            // Live-only by default (drop dead nodes + dangling edges), matching the
            // `haven_graph` MCP tool; `--all` includes archived/superseded (HV-53).
            let graph = if a.all { graph } else { graph.live_only() };
            Ok(Output::Json(serde_json::to_value(graph)?))
        }
        Command::Xref(a) => {
            let s = config::open_store()?;
            Ok(Output::Json(serde_json::to_value(
                s.xref(project, &a.reference)?,
            )?))
        }
        Command::Docs => {
            let s = config::open_store()?;
            Ok(Output::Json(serde_json::to_value(s.docs(project)?)?))
        }
        Command::Artifact { cmd } => cmd_artifact(project, cmd),
        Command::Note { reference, text } => {
            let s = config::open_store()?;
            let path = s.note(project, reference, text)?;
            Ok(Output::Json(
                serde_json::json!({ "note": path.display().to_string() }),
            ))
        }
        Command::Render => {
            let s = config::open_store()?;
            let path = s.render(project)?;
            Ok(Output::Json(
                serde_json::json!({ "rendered": path.display().to_string() }),
            ))
        }
        Command::Link { name } => cmd_link(project, name),
        Command::Skill { cmd } => cmd_skill(cmd),
        Command::Slf { cmd } => cmd_self(cmd),
        Command::Mcp => {
            // Self-heal installed skill snapshots before serving, so a binary
            // upgrade propagates the skill on the next agent session without a
            // manual `haven skill install`. stderr only: stdout is the MCP channel.
            for dir in config::refresh_stale_skill_snapshots() {
                eprintln!("haven: refreshed skill snapshot at {}", dir.display());
            }
            // Serve until stdin EOF; stdout is the MCP channel, so exit without
            // printing any Output afterwards.
            let s = config::open_store()?;
            haven_mcp::serve(&s).map_err(HavenError::Io)?;
            std::process::exit(0);
        }
        Command::Auth { cmd } => cmd_auth(cmd),
        Command::Sync { cmd, watch } => cmd_sync(project, cmd, *watch),
        Command::Backup { cmd } => cmd_backup(cmd),
        // An unrecognized verb: if it's a known divergence guess (e.g. the MCP-flat
        // `archive`/`handoff`/`get`/`add`, or `list-items`), name the exact
        // corrective command; otherwise point at `haven --help` (HV-158).
        Command::Unknown(words) => {
            let verb = words.first().map(String::as_str).unwrap_or("");
            let msg = match corrective_for_unknown(words) {
                Some(tip) => format!("unrecognized command `{verb}` — {tip}"),
                None => {
                    format!("unrecognized command `{verb}` — run `haven --help` to list commands")
                }
            };
            Err(HavenError::Invalid(msg))
        }
    }
}

/// Run an async future to completion on a current-thread runtime.
fn block_on<F: std::future::Future>(f: F) -> Result<F::Output> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(HavenError::Io)?;
    Ok(rt.block_on(f))
}

fn cmd_auth(cmd: &AuthCmd) -> Result<Output> {
    require_cloud_sync_preview()?;
    let store = config::open_store()?;
    let token_store = haven_auth::TokenStore::new();
    match cmd {
        AuthCmd::Login { token: Some(jwt) } => {
            // Headless path: store a pasted token (no refresh; far-future expiry).
            // It lands in `access_token`; `bearer_token()` falls back to it.
            let tokens = haven_auth::Tokens {
                access_token: jwt.clone(),
                id_token: None,
                refresh_token: None,
                expires_at: u64::MAX,
            };
            token_store.save(&tokens).map_err(auth_err)?;
            Ok(Output::Message("token stored".into()))
        }
        AuthCmd::Login { token: None } => {
            let cfg = config::auth_config(&store)?;
            let tokens = block_on(async move {
                let auth = haven_auth::device::start(&cfg).await?;
                let url = auth
                    .verification_uri_complete
                    .clone()
                    .unwrap_or_else(|| auth.verification_uri.clone());
                eprintln!(
                    "To sign in, open {url}\n  and enter the code: {}",
                    auth.user_code
                );
                haven_auth::device::poll(&cfg, &auth).await
            })?
            .map_err(auth_err)?;
            token_store.save(&tokens).map_err(auth_err)?;
            Ok(Output::Message("signed in".into()))
        }
        AuthCmd::Logout => {
            token_store.clear().map_err(auth_err)?;
            Ok(Output::Message("signed out (local data untouched)".into()))
        }
        AuthCmd::Status => {
            let tokens = token_store.load().map_err(auth_err)?;
            Ok(Output::Json(match tokens {
                Some(t) => serde_json::json!({
                    "signed_in": true,
                    "expires_at": t.expires_at,
                    "has_refresh_token": t.refresh_token.is_some(),
                    // Without an ID token, Supabase sees no `role` claim and
                    // RLS reads as anon — surfaced here so `doctor`-style
                    // triage can spot a stale pre-ID-token session.
                    "has_id_token": t.id_token.is_some(),
                }),
                None => serde_json::json!({ "signed_in": false }),
            }))
        }
    }
}

fn cmd_sync(project: Option<&str>, cmd: &Option<SyncCmd>, watch: bool) -> Result<Output> {
    require_cloud_sync_preview()?;
    let store = config::open_store()?;
    if let Some(SyncCmd::Status) = cmd {
        let status = store.store_status(project)?;
        return Ok(Output::Json(serde_json::json!({
            "sync_pending": status.get("sync_pending").cloned().unwrap_or(serde_json::json!(0)),
            "watch_supported": false,
        })));
    }
    if watch {
        return Err(HavenError::Invalid(
            "`sync --watch` (background daemon) is designed but not built in v1".into(),
        ));
    }

    // One foreground push (rows + content blobs) + pull pass.
    let sync_cfg = config::sync_config(&store)?;
    let paths = config::resolve()?;

    let pending_before = store
        .store_status(project)
        .ok()
        .and_then(|v| v.get("sync_pending").and_then(|n| n.as_i64()))
        .unwrap_or(0);

    let (uploaded, stats) = block_on(async move {
        let access = resolve_access_token(&store).await?;
        let engine = haven_sync::SyncEngine::new(sync_cfg, access);
        let conn = haven_core::db::open(&paths.db)?;
        // Push local changes first, then pull + reconcile remote state (so a
        // just-pushed row round-trips cleanly instead of being re-applied).
        let uploaded = engine
            .push_pass(&conn, &paths.root)
            .await
            .map_err(sync_err)?;
        let stats = engine
            .pull_pass(&conn, &paths.root)
            .await
            .map_err(sync_err)?;
        Ok::<_, HavenError>((uploaded, stats))
    })??;

    Ok(Output::Json(serde_json::json!({
        "pushed": true,
        "uploaded": uploaded,
        "pending_before": pending_before,
        "pulled": {
            "total": stats.total(),
            "projects": stats.projects,
            "nodes": stats.nodes,
            "lineage_events": stats.lineage_events,
            "lineage_edges": stats.lineage_edges,
            "edges": stats.edges,
            "artifacts": stats.artifacts,
        },
    })))
}

/// Resolve the bearer token for remote calls: `$HAVEN_ACCESS_TOKEN` (headless/CI,
/// SPEC §6 paste-a-token) wins; otherwise load from the keyring, auto-refreshing
/// via Auth0. Keyring sessions send the **ID token** (it carries the `role`
/// claim Supabase requires; Auth0 won't put it on an access token).
async fn resolve_access_token(store: &haven_core::Store) -> Result<String> {
    if let Some(t) = std::env::var("HAVEN_ACCESS_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
    {
        return Ok(t);
    }
    let cfg = config::auth_config(store)?;
    let token_store = haven_auth::TokenStore::new();
    haven_auth::current_bearer_token(&cfg, &token_store)
        .await
        .map_err(auth_err)
}

fn auth_err(e: haven_auth::AuthError) -> HavenError {
    HavenError::Invalid(format!("auth: {e}"))
}

fn sync_err(e: haven_sync::SyncError) -> HavenError {
    HavenError::Invalid(format!("sync: {e}"))
}

fn cmd_artifact(project: Option<&str>, cmd: &ArtifactCmd) -> Result<Output> {
    let s = config::open_store()?;
    match cmd {
        ArtifactCmd::Add(a) => {
            // Infer kind: explicit --kind wins, else file/content→file, uri→external.
            let kind = match a.kind.as_deref() {
                Some(k) => ArtifactKind::parse(k)?,
                None if a.file.is_some() || a.content.is_some() => ArtifactKind::File,
                None => ArtifactKind::External,
            };
            let new = NewArtifact {
                role: ArtifactRole::parse(&a.role)?,
                kind,
                file: a.file.clone(),
                content: a.content.clone(),
                name: a.name.clone(),
                uri: a.uri.clone(),
                title: a.title.clone(),
                excerpt: a.excerpt.clone(),
                from_owner: opt_parse(&a.from, OwnerKind::parse)?,
                to_owner: opt_parse(&a.to, OwnerKind::parse)?,
                created_by: a.by.clone(),
                // No xref write flag yet — xref metadata is authored via the core
                // NewArtifact path (the read verb + doctor only need read). HV-69.
                metadata: None,
                replace: a.replace,
            };
            Ok(Output::Json(serde_json::to_value(s.add_artifact(
                project,
                &a.reference,
                new,
            )?)?))
        }
        ArtifactCmd::List(a) => {
            let role = opt_parse(&a.role, ArtifactRole::parse)?;
            Ok(Output::Json(serde_json::to_value(s.list_artifacts(
                project,
                &a.reference,
                role,
            )?)?))
        }
        ArtifactCmd::Get(a) => {
            let role = opt_parse(&a.role, ArtifactRole::parse)?;
            let got = match s.get_artifact(project, &a.reference, role, a.path.as_deref()) {
                // Content synced to Storage but not on this machine: lazy-pull
                // it (SPEC §5), cache it in the content tree, and retry once.
                Err(HavenError::ContentNotLocal {
                    project: pkey,
                    rel_path,
                    remote_path,
                    content_hash,
                }) => {
                    hydrate_content(&s, &pkey, &rel_path, &remote_path, content_hash.as_deref())?;
                    s.get_artifact(project, &a.reference, role, a.path.as_deref())?
                }
                other => other?,
            };
            Ok(Output::Json(serde_json::to_value(got)?))
        }
        ArtifactCmd::Rm(a) => {
            let selector = artifact_selector(&a.role, &a.name, &a.id)?;
            Ok(Output::Json(serde_json::to_value(s.remove_artifact(
                project,
                &a.reference,
                selector,
            )?)?))
        }
        ArtifactCmd::Mv(a) => {
            let selector = artifact_selector(&a.role, &a.name, &a.id)?;
            Ok(Output::Json(serde_json::to_value(s.rename_artifact(
                project,
                &a.reference,
                selector,
                &a.new_name,
            )?)?))
        }
    }
}

/// Build an [`ArtifactSelector`] from the mutually-exclusive --role/--name/--id
/// flags shared by `artifact rm` and `artifact mv`. Exactly one must be given.
fn artifact_selector(
    role: &Option<String>,
    name: &Option<String>,
    id: &Option<String>,
) -> Result<ArtifactSelector> {
    match (role, name, id) {
        (Some(r), None, None) => Ok(ArtifactSelector::Role(ArtifactRole::parse(r)?)),
        (None, Some(n), None) => Ok(ArtifactSelector::Name(n.clone())),
        (None, None, Some(i)) => Ok(ArtifactSelector::Id(i.clone())),
        _ => Err(HavenError::Invalid(
            "provide exactly one of --role, --name, or --id".into(),
        )),
    }
}

/// Download one artifact's content from Storage into the local content tree —
/// the lazy-pull half of the content channel. Needs sync configured + a token;
/// without them, errors with the context to fix it.
fn hydrate_content(
    store: &haven_core::Store,
    project_key: &str,
    rel_path: &str,
    remote_path: &str,
    content_hash: Option<&str>,
) -> Result<()> {
    require_cloud_sync_preview()?;
    let sync_cfg = config::sync_config(store).map_err(|e| {
        HavenError::Invalid(format!(
            "content file {rel_path} is in cloud Storage but sync isn't configured here: {e}"
        ))
    })?;
    let paths = config::resolve()?;
    block_on(async move {
        let access = resolve_access_token(store).await?;
        let engine = haven_sync::SyncEngine::new(sync_cfg, access);
        engine
            .hydrate(
                &paths.root,
                project_key,
                rel_path,
                remote_path,
                content_hash,
            )
            .await
            .map_err(sync_err)?;
        Ok::<(), HavenError>(())
    })??;
    Ok(())
}

/// After a command that changes the work-graph, regenerate `backlog.md` so the
/// projection never drifts from the DB (SPEC §4). Best-effort: a render failure
/// (e.g. no project selected) must not fail the underlying command.
/// Commands that mutate the work-graph (drive both the backlog re-render and the
/// opportunistic daily backup). Read-only commands and the backup verbs are excluded.
fn mutates(command: &Command) -> bool {
    matches!(
        command,
        Command::Item { .. }
            | Command::Import { .. }
            | Command::Decompose(_)
            | Command::Depend(_)
            | Command::Group(_)
            | Command::Evolve { .. }
            | Command::Artifact { .. }
            | Command::Note { .. }
    )
}

fn maybe_render(cli: &Cli) {
    if !mutates(&cli.command) {
        return;
    }
    if let Ok(s) = config::open_store() {
        let _ = s.render(cli.project.as_deref());
    }
}

/// Opportunistic ≤1/day snapshot, fired after any successful command — including
/// read-only ones, since a day of direct content-file edits under
/// `~/.haven/<project>/items/` may touch no DB-mutating command yet still wants a
/// snapshot (HV-89). Best-effort: a backup failure must never fail the user's
/// actual command. The cheap `last_backup` marker check inside makes this a no-op
/// on all but the first command of the day (no cron/launchd).
fn maybe_daily_backup() {
    let Ok(paths) = config::resolve() else {
        return;
    };
    if let Ok(s) = config::open_store() {
        let _ = s.maybe_daily_backup(&paths.root.join("backups"));
    }
}

/// Print a loud warning to stderr while any snapshot is quarantined — rotation
/// is frozen until the operator removes the `*-SUSPECT` dir(s).
fn warn_if_quarantined() {
    let Ok(paths) = config::resolve() else {
        return;
    };
    let backups = paths.root.join("backups");
    if let Ok(frozen) = Store::backups_frozen(&backups) {
        if !frozen.is_empty() {
            eprintln!(
                "haven: WARNING — {} quarantined backup(s) ({}). Integrity check failed; \
                 backup rotation AND object GC are FROZEN to protect good snapshots. \
                 Run `haven backup clear <id>` (or remove the *-SUSPECT manifest/dir under {}) \
                 to clear.",
                frozen.len(),
                frozen.join(", "),
                backups.display(),
            );
        }
    }
}

fn cmd_setup(
    agent: AgentTarget,
    no_skill: bool,
    write_agents_md: bool,
    project_key: Option<&str>,
    project_title: Option<&str>,
    prefix: Option<&str>,
) -> Result<Output> {
    let s = config::open_store()?;
    if project_title.is_some() && project_key.is_none() {
        return Err(HavenError::Invalid(
            "--project-title requires --project-key".into(),
        ));
    }
    // Generate a stable device id once.
    if s.meta_get("device_id")?.is_none() {
        s.meta_set("device_id", &uuid_like())?;
    }
    let mut warnings = Vec::new();
    let claude_mcp_config = if agent.includes_claude() {
        match config::ensure_mcp_wiring() {
            Ok(p) => p.display().to_string(),
            Err(e) => {
                warnings.push(format!("Claude MCP wiring skipped: {e}"));
                format!("skipped: {e}")
            }
        }
    } else {
        "skipped (--agent codex)".to_string()
    };
    let codex_mcp_config = if agent.includes_codex() {
        match config::ensure_codex_mcp_wiring() {
            Ok(p) => p.display().to_string(),
            Err(e) => {
                warnings.push(format!("Codex MCP wiring skipped: {e}"));
                format!("skipped: {e}")
            }
        }
    } else {
        "skipped (--agent claude)".to_string()
    };

    // Install every shipped skill for each in-scope agent; collect per-skill
    // paths, and keep a `skill` headline (the Claude `haven` path) for back-compat.
    let mut claude_skills = serde_json::Map::new();
    let mut codex_skills = serde_json::Map::new();
    if !no_skill {
        for name in config::skill_names() {
            if agent.includes_claude() {
                let v = match config::ensure_skill_installed(name) {
                    Ok(p) => p.display().to_string(),
                    Err(e) => {
                        warnings.push(format!("Claude skill `{name}` install skipped: {e}"));
                        format!("skipped: {e}")
                    }
                };
                claude_skills.insert(name.to_string(), serde_json::json!(v));
            }
            if agent.includes_codex() {
                let v = match config::ensure_codex_skill_installed(name) {
                    Ok(p) => p.display().to_string(),
                    Err(e) => {
                        warnings.push(format!("Codex skill `{name}` install skipped: {e}"));
                        format!("skipped: {e}")
                    }
                };
                codex_skills.insert(name.to_string(), serde_json::json!(v));
            }
        }
    }
    let claude_skill = if no_skill {
        "skipped (--no-skill)".to_string()
    } else if agent.includes_claude() {
        claude_skills
            .get("haven")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    } else {
        "skipped (--agent codex)".to_string()
    };

    let agents_md = if write_agents_md {
        match config::ensure_agents_md() {
            Ok(p) => p.display().to_string(),
            Err(e) => {
                warnings.push(format!("AGENTS.md discovery skipped: {e}"));
                format!("skipped: {e}")
            }
        }
    } else {
        "skipped (--agents-md not requested)".to_string()
    };

    let mut project_created = false;
    let current_project = if let Some(key) = project_key {
        match s.get_project(key) {
            Ok(_) => {}
            Err(HavenError::NotFound(_)) => {
                let title = project_title.unwrap_or(key);
                s.add_project(key, prefix, title, None)?;
                project_created = true;
            }
            Err(e) => return Err(e),
        }
        s.use_project(key)?;
        Some(key.to_string())
    } else {
        s.current_project()?
    };
    let paths = config::resolve()?;
    let mut out = serde_json::json!({
        "message": "haven local setup complete",
        "root": paths.root.display().to_string(),
        "db": paths.db.display().to_string(),
        "mcp_config": claude_mcp_config.clone(),
        "skill": claude_skill,
        "claude_mcp_config": claude_mcp_config,
        "claude_skills": claude_skills,
        "codex_mcp_config": codex_mcp_config,
        "codex_skills": codex_skills,
        "agents_md": agents_md,
        "current_project": current_project,
        "project_created": project_created,
        "warnings": warnings,
        "next": if current_project.is_some() {
            "add items with `haven item add ...`"
        } else {
            "create a project with `haven project add --key <key> --title <title>` then `haven project use <key>`"
        },
    });
    if cloud_sync_preview_enabled() {
        out["note"] =
            serde_json::json!("Cloud Sync preview enabled; configure auth/sync separately");
    }
    Ok(Output::Json(out))
}

fn cmd_skill(cmd: &SkillCmd) -> Result<Output> {
    match cmd {
        SkillCmd::Install { agent, skill } => {
            let names: Vec<&str> = match skill {
                Some(s) => {
                    if !config::skill_names().any(|n| n == s.as_str()) {
                        return Err(HavenError::Invalid(format!("unknown skill: {s}")));
                    }
                    vec![s.as_str()]
                }
                None => config::skill_names().collect(),
            };
            let mut installed = serde_json::Map::new();
            let mut files = serde_json::Map::new();
            for name in names {
                if agent.includes_claude() {
                    installed.insert(
                        format!("claude_{name}"),
                        serde_json::json!(config::ensure_skill_installed(name)?
                            .display()
                            .to_string()),
                    );
                }
                if agent.includes_codex() {
                    installed.insert(
                        format!("codex_{name}"),
                        serde_json::json!(config::ensure_codex_skill_installed(name)?
                            .display()
                            .to_string()),
                    );
                }
                files.insert(
                    name.to_string(),
                    serde_json::json!(config::skill_file_list(name)),
                );
            }
            Ok(Output::Json(serde_json::json!({
                "installed": installed,
                "files": files,
            })))
        }
    }
}

fn cmd_self(cmd: &SelfCmd) -> Result<Output> {
    match cmd {
        SelfCmd::Install(a) => cmd_self_install(a),
        SelfCmd::Update(a) => cmd_self_update(a),
    }
}

fn cmd_self_install(a: &SelfInstallArgs) -> Result<Output> {
    // When invoked *through* the installed symlink, current_exe() can return the
    // symlink itself; canonicalize resolves it to the real build artifact so
    // --link never points a link at itself.
    let raw_exe = std::env::current_exe().map_err(HavenError::Io)?;
    let source = std::fs::canonicalize(&raw_exe).map_err(HavenError::Io)?;

    let dir = config::resolve_install_dir(a.dir.as_deref())?;
    let dest = dir.join("haven");

    // Already resolves to this exact build → nothing to do (idempotent re-run).
    if std::fs::canonicalize(&dest).ok().as_deref() == Some(source.as_path()) {
        return Ok(Output::Json(serde_json::json!({
            "installed": dest.display().to_string(),
            "source": source.display().to_string(),
            "mode": if a.link { "link" } else { "copy" },
            "noop": true,
            "on_path": config::dir_on_path(&dir),
            "next": "already current",
        })));
    }

    // A copy must not silently clobber a different existing binary.
    if !a.link && !a.force && std::fs::symlink_metadata(&dest).is_ok() {
        return Err(HavenError::Invalid(format!(
            "{} already exists — pass --force to overwrite (or --link)",
            dest.display()
        )));
    }

    let mode = if a.link {
        config::install_link(&source, &dest)?;
        "link"
    } else {
        config::install_copy(&source, &dest)?;
        "copy"
    };

    // Best-effort: make the on-disk skill match this freshly built binary.
    let refreshed: Vec<String> = config::refresh_stale_skill_snapshots()
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    Ok(Output::Json(serde_json::json!({
        "installed": dest.display().to_string(),
        "source": source.display().to_string(),
        "mode": mode,
        "skill_refreshed": refreshed,
        "on_path": config::dir_on_path(&dir),
        "next": "run `haven doctor` to verify wiring",
    })))
}

fn cmd_self_update(a: &SelfUpdateArgs) -> Result<Output> {
    let method = config::detect_install_method()?;
    let advice = config::update_advice(&method);
    let current = env!("CARGO_PKG_VERSION");

    // The only network path in the CLI; best-effort and offline-safe.
    let latest = config::latest_release_version();
    // semver-aware so "newer" is a real comparison, not string inequality.
    let newer = latest.as_deref().and_then(|l| config::is_newer(current, l));
    let up_to_date = newer.map(|n| !n);

    if a.check {
        return Ok(Output::Json(serde_json::json!({
            "method": method.tag(),
            "current": current,
            "latest": latest,
            "up_to_date": up_to_date,
            "advice": advice,
        })));
    }

    let mut actions: Vec<String> = Vec::new();
    match &method {
        config::InstallMethod::Homebrew => {
            if a.run {
                let status = std::process::Command::new("brew")
                    .args(["upgrade", "nibbletech-labs/tap/haven"])
                    .status();
                actions.push(match status {
                    Ok(s) if s.success() => {
                        "ran `brew upgrade` — run `haven doctor` with the new binary to finish reconcile"
                            .into()
                    }
                    Ok(s) => format!("`brew upgrade` exited with status {s}"),
                    Err(e) => format!("could not run brew ({e}); upgrade manually"),
                });
            } else if a.binary || a.tag.is_some() {
                // Don't fight the package manager — Homebrew owns this binary.
                actions.push(
                    "Homebrew install — `--binary` doesn't apply; update with \
                     `brew upgrade nibbletech-labs/tap/haven` (or pass --run)."
                        .into(),
                );
            }
        }
        config::InstallMethod::DevSymlink { .. } => {
            for d in config::refresh_stale_skill_snapshots() {
                actions.push(format!("refreshed skill snapshot at {}", d.display()));
            }
        }
        // Copied install (install.sh) — swap the prebuilt binary by default.
        config::InstallMethod::InstallSh => {
            actions.extend(binary_update(a, current, latest.as_deref(), newer)?);
        }
        // Unrecognized location — only swap when explicitly asked (safety).
        config::InstallMethod::Unknown { .. } => {
            if a.binary || a.tag.is_some() {
                actions.extend(binary_update(a, current, latest.as_deref(), newer)?);
            }
        }
    }

    Ok(Output::Json(serde_json::json!({
        "method": method.tag(),
        "current": current,
        "latest": latest,
        "up_to_date": up_to_date,
        "advice": advice,
        "actions": actions,
    })))
}

/// Download + verify + atomically swap the prebuilt release binary for a copied
/// install. Returns human-readable action strings (never partially swaps: the
/// rename only happens after a verified extract). `--tag` targets an exact
/// release and bypasses the newer-than-current gate; otherwise we only act when
/// the published release is semver-newer than this build.
fn binary_update(
    a: &SelfUpdateArgs,
    current: &str,
    latest: Option<&str>,
    newer: Option<bool>,
) -> Result<Vec<String>> {
    let mut actions = Vec::new();

    // The tag to install: an explicit --tag, else the discovered latest release.
    let tag = match a.tag.as_deref().or(latest) {
        Some(t) => t.to_string(),
        None => {
            actions.push("no release to install (offline, or none published yet)".into());
            return Ok(actions);
        }
    };

    // Without an explicit --tag, only swap when the release is actually newer.
    if a.tag.is_none() && newer != Some(true) {
        actions.push(format!(
            "already up to date (v{current}); nothing to install"
        ));
        return Ok(actions);
    }

    let target = match config::current_target_triple() {
        Some(t) => t,
        None => {
            actions.push(format!(
                "no prebuilt binary for this platform ({}/{}); reinstall from source",
                std::env::consts::OS,
                std::env::consts::ARCH
            ));
            return Ok(actions);
        }
    };
    let version = tag.trim_start_matches('v').to_string();

    // Swap the *real* running binary (resolve a symlink to its target, though a
    // copied install is already a regular file). Old inode stays live until exit.
    let raw_exe = std::env::current_exe().map_err(HavenError::Io)?;
    let dest = std::fs::canonicalize(&raw_exe).map_err(HavenError::Io)?;

    let bytes = config::fetch_release_binary(&tag, &version, target)?;
    config::atomic_write_executable(&dest, &bytes)?;

    actions.push(format!(
        "downloaded haven {version} ({target}), verified sha256, swapped {} — \
         re-run `haven doctor` to finish reconcile",
        dest.display()
    ));
    Ok(actions)
}

fn cmd_link(project: Option<&str>, name: &std::path::Path) -> Result<Output> {
    let s = config::open_store()?;
    let linked = config::link_workspace(&s, project, name)?;
    Ok(Output::Json(serde_json::json!({
        "workspace": linked.workspace.display().to_string(),
        "backlog": linked.backlog.display().to_string(),
        "canonical_backlog": linked.canonical_backlog.display().to_string(),
        "git_exclude": linked.git_exclude.map(|p| p.display().to_string()),
        "binding": linked.binding.display().to_string(),
        "note": "canonical graph/content remains under ~/.haven; this workspace is disposable",
    })))
}

fn cmd_status(project: Option<&str>) -> Result<Output> {
    let s = config::open_store()?;
    let paths = config::resolve()?;
    let mut status = match s.store_status(project) {
        Ok(v) => v,
        // No project selected yet — still report db location.
        Err(HavenError::Invalid(_)) => serde_json::json!({ "project": null }),
        Err(e) => return Err(e),
    };
    if let Some(obj) = status.as_object_mut() {
        obj.insert(
            "db".into(),
            serde_json::json!(paths.db.display().to_string()),
        );
        obj.insert(
            "projects".into(),
            serde_json::json!(s.list_projects(false)?.len()),
        );
        if cloud_sync_preview_enabled() {
            obj.insert(
                "auth".into(),
                serde_json::json!("not configured (Cloud Sync preview)"),
            );
        }
        let backups = paths.root.join("backups");
        let entries = Store::list_backups(&backups).unwrap_or_default();
        let frozen = Store::backups_frozen(&backups).unwrap_or_default();
        obj.insert(
            "backups".into(),
            serde_json::json!({
                "count": entries.len(),
                "latest": entries.first().map(|e| e.id.clone()),
                "frozen": !frozen.is_empty(),
            }),
        );
    }
    hide_cloud_sync_status(&mut status);
    Ok(Output::Json(status))
}

/// One diagnostic line: a named check, a status, and a human-readable detail.
/// `status` is "ok" | "warn" | "error"; an "error" makes the whole report not-ok.
fn check(name: &str, status: &str, detail: String) -> serde_json::Value {
    serde_json::json!({ "name": name, "status": status, "detail": detail })
}

fn cmd_doctor() -> Result<Output> {
    // Pass the store *open result* through, not a `?`-unwrapped store: a failed
    // open (e.g. StoreTooNew) must still produce a report (HV-39).
    Ok(Output::Json(doctor_report(
        config::open_store(),
        &config::resolve()?,
    )?))
}

/// Build the `doctor` diagnostic report from the store *open result*.
///
/// When the store opened, every check runs as before. When it failed (the case
/// you most need doctor in — e.g. a store migrated by a newer binary,
/// `StoreTooNew`), the open error becomes the `database` check's error detail,
/// the store-dependent checks (`schema`, `context_pack_integrity`,
/// `xref_integrity`) are skipped, and the non-store checks (install wiring,
/// backups) still run. `ok` is then false and the store-derived report fields
/// are null.
fn doctor_report(store: Result<Store>, paths: &config::Paths) -> Result<serde_json::Value> {
    let mut checks = Vec::new();

    // The store, if it opened — threaded through so the store-dependent checks
    // and the final report fields can use it, and skipped otherwise.
    let store = match store {
        Ok(s) => {
            // 1. Database — the store opened (migrations ran); confirm the seed stamp.
            match s.meta_get("schema_version")? {
                Some(_) => checks.push(check(
                    "database",
                    "ok",
                    format!("store at {}", paths.db.display()),
                )),
                None => checks.push(check(
                    "database",
                    "error",
                    "schema_version missing — run `haven setup`".into(),
                )),
            }

            // 1b. Schema version: the store's applied version vs what this binary
            // supports. (A newer store fails the open below, not here.)
            let store_v = s.user_version()?;
            let binary_v = haven_core::db::latest_schema_migration();
            checks.push(check(
                "schema",
                "ok",
                format!("store schema v{store_v}, binary supports v{binary_v}"),
            ));
            Some(s)
        }
        Err(e) => {
            // The store didn't open — report it as the `database` check's error
            // detail rather than aborting, so the non-store checks below still
            // run. StoreTooNew names both versions; other errors carry `e`.
            let detail = match &e {
                HavenError::StoreTooNew {
                    db_version,
                    supported_version,
                    ..
                } => format!(
                    "store schema v{db_version}, binary supports v{supported_version} — store too new; upgrade/reinstall `haven`"
                ),
                other => format!("could not open store: {other}"),
            };
            checks.push(check("database", "error", detail));
            // Skip `schema`, `context_pack_integrity`, `xref_integrity` — they
            // need an open store.
            None
        }
    };

    // 2–6. Install wiring: MCP stanzas, skill snapshots, AGENTS.md, binary on PATH.
    match config::install_check() {
        Ok(w) => {
            let skill_check =
                |agent: &str, name: &str, st: &config::SkillInstallStatus| -> serde_json::Value {
                    let check_name = format!("{agent}_skill_{name}");
                    let hint = if agent == "codex" {
                        "run `haven skill install --agent codex`"
                    } else {
                        "run `haven skill install`"
                    };
                    if st.present && st.current {
                        check(
                            &check_name,
                            "ok",
                            format!("up to date at {}", st.dir.display()),
                        )
                    } else if st.present {
                        check(
                            &check_name,
                            "warn",
                            format!("installed but stale vs this binary — {hint}"),
                        )
                    } else {
                        check(
                            &check_name,
                            "warn",
                            format!("missing {} — {hint}", st.missing_files.join(", ")),
                        )
                    }
                };

            checks.push(if w.claude_mcp_registered {
                check(
                    "claude_mcp",
                    "ok",
                    format!(
                        "`haven` server registered in {}",
                        w.claude_mcp_config_path.display()
                    ),
                )
            } else {
                check(
                    "claude_mcp",
                    "warn",
                    format!(
                        "not registered in {} — run `haven setup`",
                        w.claude_mcp_config_path.display()
                    ),
                )
            });

            for (name, st) in &w.claude_skills {
                checks.push(skill_check("claude", name, st));
            }

            checks.push(if w.codex_mcp_registered {
                check(
                    "codex_mcp",
                    "ok",
                    format!(
                        "`haven` server registered in {}",
                        w.codex_mcp_config_path.display()
                    ),
                )
            } else {
                check(
                    "codex_mcp",
                    "warn",
                    format!(
                        "not registered in {} — run `haven setup --agent codex`",
                        w.codex_mcp_config_path.display()
                    ),
                )
            });

            for (name, st) in &w.codex_skills {
                checks.push(skill_check("codex", name, st));
            }

            checks.push(match &w.agents_md_path {
                Some(path) if w.agents_md_current => check(
                    "agents_md",
                    "ok",
                    format!("Haven discovery stanza present in {}", path.display()),
                ),
                Some(path) if w.agents_md_present => check(
                    "agents_md",
                    "warn",
                    format!(
                        "Haven discovery stanza stale in {} — run `haven setup --agents-md`",
                        path.display()
                    ),
                ),
                Some(path) => check(
                    "agents_md",
                    "warn",
                    format!("missing {} — run `haven setup --agents-md`", path.display()),
                ),
                None => check(
                    "agents_md",
                    "skip",
                    "no repo-local AGENTS.md discovered; run `haven setup --agents-md` in a repo to add one"
                        .into(),
                ),
            });

            checks.push(match w.haven_on_path {
                Some(p) => check("path", "ok", format!("`haven` resolves to {}", p.display())),
                None => check(
                    "path",
                    "warn",
                    "`haven` not on $PATH — the MCP `command: \"haven\"` stanza can't launch it"
                        .into(),
                ),
            });
        }
        Err(e) => checks.push(check("install", "warn", format!("could not inspect: {e}"))),
    }

    // 7. Backups: report count + freeze state. A quarantined snapshot is a warn.
    let backups = paths.root.join("backups");
    let entries = Store::list_backups(&backups).unwrap_or_default();
    let frozen = Store::backups_frozen(&backups).unwrap_or_default();
    if !frozen.is_empty() {
        checks.push(check(
            "backups",
            "warn",
            format!(
                "{} quarantined snapshot(s) ({}); rotation + GC frozen — run `haven backup clear <id>` (or remove the *-SUSPECT manifest/dir under {})",
                frozen.len(),
                frozen.join(", "),
                backups.display(),
            ),
        ));
    } else {
        checks.push(check(
            "backups",
            "ok",
            format!(
                "{} snapshot(s){}",
                entries.len(),
                entries
                    .first()
                    .map(|e| format!(", latest {}", e.id))
                    .unwrap_or_default(),
            ),
        ));
    }

    if cloud_sync_preview_enabled() {
        checks.push(check(
            "auth",
            "skip",
            "Cloud Sync preview enabled; auth not configured".into(),
        ));
        checks.push(check(
            "sync",
            "skip",
            "Cloud Sync preview enabled; sync not configured".into(),
        ));
    }

    // Graph integrity (HV-105): context-pack tombstones, pointers into them, and
    // duplicate (node, path) artifact rows. A data-integrity warn, not store-fatal.
    // Both integrity scans need an open store; skipped entirely without one.
    if let Some(s) = &store {
        match s.context_pack_integrity() {
            Ok(issues) if issues.is_empty() => checks.push(check(
                "context_pack_integrity",
                "ok",
                "no context-pack tombstones, dangling pointers, or duplicate artifact rows".into(),
            )),
            Ok(issues) => {
                let mut tombstones = Vec::new();
                let mut pointers = Vec::new();
                let mut dupes = Vec::new();
                for i in &issues {
                    match i.kind {
                        IntegrityKind::TombstonePack => tombstones.push(i.node.as_str()),
                        IntegrityKind::PointerToTombstone => pointers.push(i.node.as_str()),
                        IntegrityKind::DuplicateArtifactRow => dupes.push(i.node.as_str()),
                        // xref kinds are produced only by `xref_integrity`, reported in
                        // its own block below — never by `context_pack_integrity`.
                        IntegrityKind::CanonicalConflict
                        | IntegrityKind::DanglingXref
                        | IntegrityKind::UnknownStore => {}
                    }
                }
                let mut parts = Vec::new();
                if !tombstones.is_empty() {
                    parts.push(format!(
                        "{} tombstone pack(s) [{}]",
                        tombstones.len(),
                        tombstones.join(", ")
                    ));
                }
                if !pointers.is_empty() {
                    parts.push(format!(
                        "{} pointer(s) to tombstone [{}]",
                        pointers.len(),
                        pointers.join(", ")
                    ));
                }
                if !dupes.is_empty() {
                    parts.push(format!(
                        "{} duplicate-row node(s) [{}]",
                        dupes.len(),
                        dupes.join(", ")
                    ));
                }
                checks.push(check("context_pack_integrity", "warn", parts.join("; ")));
            }
            Err(e) => checks.push(check(
                "context_pack_integrity",
                "warn",
                format!("integrity scan failed: {e}"),
            )),
        }

        // Xref integrity (HV-69): canonical conflicts, dangling/structurally-invalid
        // xrefs, and unrecognized-store lints across every artifact's metadata.xref[].
        // CLI-only (no haven_doctor), a warn — never store-fatal.
        match s.xref_integrity() {
            Ok(issues) if issues.is_empty() => checks.push(check(
                "xref_integrity",
                "ok",
                "no canonical conflicts, dangling xrefs, or unknown-store lints".into(),
            )),
            Ok(issues) => {
                let mut conflicts = Vec::new();
                let mut dangling = Vec::new();
                let mut unknown = Vec::new();
                for i in &issues {
                    match i.kind {
                        IntegrityKind::CanonicalConflict => conflicts.push(i.node.as_str()),
                        IntegrityKind::DanglingXref => dangling.push(i.node.as_str()),
                        IntegrityKind::UnknownStore => unknown.push(i.node.as_str()),
                        // Context-pack kinds come from a different scan, never here.
                        IntegrityKind::TombstonePack
                        | IntegrityKind::PointerToTombstone
                        | IntegrityKind::DuplicateArtifactRow => {}
                    }
                }
                let mut parts = Vec::new();
                if !conflicts.is_empty() {
                    parts.push(format!(
                        "{} canonical conflict(s) [{}]",
                        conflicts.len(),
                        conflicts.join(", ")
                    ));
                }
                if !dangling.is_empty() {
                    parts.push(format!(
                        "{} dangling xref(s) [{}]",
                        dangling.len(),
                        dangling.join(", ")
                    ));
                }
                if !unknown.is_empty() {
                    parts.push(format!(
                        "{} unknown-store lint(s) [{}]",
                        unknown.len(),
                        unknown.join(", ")
                    ));
                }
                checks.push(check("xref_integrity", "warn", parts.join("; ")));
            }
            Err(e) => checks.push(check(
                "xref_integrity",
                "warn",
                format!("xref scan failed: {e}"),
            )),
        }
    } // end `if let Some(s) = &store` — store-dependent integrity checks

    let ok = !checks
        .iter()
        .any(|c| c["status"] == "error" || c["status"] == "warn");
    // Store-derived report fields are null when the store didn't open.
    let (schema_version, device_id) = match &store {
        Some(s) => (s.meta_get("schema_version")?, s.meta_get("device_id")?),
        None => (None, None),
    };
    let mut report = serde_json::json!({
        "ok": ok,
        "schema_version": schema_version,
        "device_id": device_id,
        "checks": checks,
    });
    if !ok {
        report["hint"] =
            serde_json::json!("`haven setup` re-wires MCP + skill; warnings above are non-fatal");
    }
    Ok(report)
}

fn cmd_config(cmd: &ConfigCmd) -> Result<Output> {
    let s = config::open_store()?;
    let paths = config::resolve()?;
    match cmd {
        ConfigCmd::Get { key } => {
            let value = match key.as_str() {
                "db-path" => Some(paths.db.display().to_string()),
                "current-project" => s.meta_get("current_project")?,
                "device-id" => s.meta_get("device_id")?,
                "api-url" => s.meta_get("api_url")?,
                other => s.meta_get(other)?,
            };
            Ok(Output::Json(
                serde_json::json!({ "key": key, "value": value }),
            ))
        }
        ConfigCmd::Set { key, value } => {
            let meta_key = match key.as_str() {
                "db-path" => {
                    return Err(HavenError::Invalid(
                        "db-path is derived from HAVEN_HOME and not settable".into(),
                    ))
                }
                "current-project" => "current_project",
                "device-id" => "device_id",
                "api-url" => "api_url",
                other => other,
            };
            s.meta_set(meta_key, value)?;
            Ok(Output::Message(format!("{key} = {value}")))
        }
    }
}

fn cmd_project(cmd: &ProjectCmd) -> Result<Output> {
    let s = config::open_store()?;
    match cmd {
        ProjectCmd::Add(a) => Ok(Output::Project(s.add_project(
            &a.key,
            a.prefix.as_deref(),
            &a.title,
            a.description.as_deref(),
        )?)),
        ProjectCmd::List(a) => Ok(Output::Projects(s.list_projects(a.include_archived)?)),
        ProjectCmd::Get { key } => Ok(Output::Project(s.get_project(key)?)),
        ProjectCmd::Use { key } => {
            s.use_project(key)?;
            Ok(Output::Message(format!("current project: {key}")))
        }
        ProjectCmd::Archive { key, rationale, by } => Ok(Output::Project(s.archive_project(
            key,
            rationale.as_deref(),
            by.as_deref(),
        )?)),
        ProjectCmd::Reopen { key, by } => {
            Ok(Output::Project(s.reopen_project(key, by.as_deref())?))
        }
    }
}

/// `<HAVEN_HOME>/backups` — backups are store-wide, not per-project.
fn backups_dir() -> Result<PathBuf> {
    Ok(config::resolve()?.root.join("backups"))
}

fn cmd_backup(cmd: &BackupCmd) -> Result<Output> {
    let backups = backups_dir()?;
    match cmd {
        BackupCmd::List => {
            let frozen = Store::backups_frozen(&backups)?;
            Ok(Output::Json(serde_json::json!({
                "backups": Store::list_backups(&backups)?,
                "frozen": !frozen.is_empty(),
                "quarantined": frozen,
            })))
        }
        BackupCmd::Now => {
            let s = config::open_store()?;
            Ok(Output::Json(serde_json::to_value(s.backup_now(&backups)?)?))
        }
        BackupCmd::Verify(a) => {
            let id = match &a.id {
                Some(id) => id.clone(),
                None => latest_backup_id(&backups)?,
            };
            let integrity = Store::verify_backup(&backups, &id)?;
            Ok(Output::Json(serde_json::json!({
                "id": id,
                "integrity": integrity,
                "checked": "objects+integrity",
            })))
        }
        BackupCmd::Restore(a) => {
            if !a.yes {
                return Err(HavenError::Invalid(
                    "restore overwrites the live database and content files; pass --yes to confirm"
                        .into(),
                ));
            }
            let paths = config::resolve()?;
            let report = Store::restore_backup(&paths.db, &paths.root, &backups, &a.id)?;
            Ok(Output::Json(serde_json::to_value(report)?))
        }
        BackupCmd::Clear(a) => {
            Store::clear_quarantine(&backups, &a.id)?;
            Ok(Output::Json(serde_json::json!({
                "cleared": a.id,
                "frozen": !Store::backups_frozen(&backups)?.is_empty(),
            })))
        }
    }
}

fn latest_backup_id(backups: &std::path::Path) -> Result<String> {
    Store::list_backups(backups)?
        .into_iter()
        .next()
        .map(|e| e.id)
        .ok_or_else(|| HavenError::NotFound("no backups exist yet; run `haven backup now`".into()))
}

fn cmd_import(project: Option<&str>, file: &std::path::Path, if_absent: bool) -> Result<Output> {
    let raw = std::fs::read_to_string(file)
        .map_err(|e| HavenError::Invalid(format!("cannot read {}: {e}", file.display())))?;
    let items: Vec<haven_core::ImportItem> = serde_json::from_str(&raw).map_err(|e| {
        HavenError::Invalid(format!(
            "{} is not a valid import file: {e}",
            file.display()
        ))
    })?;
    let s = config::open_store()?;
    let outcomes = s.import_items(project, items, if_absent)?;
    Ok(Output::Json(serde_json::to_value(outcomes)?))
}

/// Stable op name for the per-call telemetry line (HV-166), one per `ItemCmd`
/// variant (`item.add`, `item.complete`, …).
fn item_op_name(cmd: &ItemCmd) -> &'static str {
    match cmd {
        ItemCmd::Add(_) => "item.add",
        ItemCmd::List(_) => "item.list",
        ItemCmd::Get(_) => "item.get",
        ItemCmd::Update(_) => "item.update",
        ItemCmd::Commit { .. } => "item.commit",
        ItemCmd::Uncommit { .. } => "item.uncommit",
        ItemCmd::Claim(_) => "item.claim",
        ItemCmd::Assign(_) => "item.assign",
        ItemCmd::Handoff(_) => "item.handoff",
        ItemCmd::Complete(_) => "item.complete",
        ItemCmd::Rank(_) => "item.rank",
        ItemCmd::Archive { .. } => "item.archive",
        ItemCmd::Reopen { .. } => "item.reopen",
    }
}

/// The CLI item-dispatch chokepoint (HV-166): run the op timed, then emit ONE
/// structured telemetry line to **stderr** for every item op — mirroring the MCP
/// `handle_tool_call` line so drift between CLI and MCP is observable the same
/// way. `project_passed` is the `-p`/`--project` selector as given; `project_resolved`
/// is the key the op would resolve to (sticky `current_project` when none passed —
/// the HV-153 drift), resolved best-effort with a read-only store open. stderr
/// only: stdout is the structured `Output` channel and must stay clean.
fn cmd_item_telemetered(project: Option<&str>, cmd: &ItemCmd) -> Result<Output> {
    let started = Instant::now();
    let result = cmd_item(project, cmd);
    let latency_ms = started.elapsed().as_millis();

    let project_resolved = config::open_store()
        .ok()
        .and_then(|s| s.resolve_project_key(project).ok());
    TelemetryLine::new(
        item_op_name(cmd),
        project.map(str::to_string),
        project_resolved,
        telemetry::error_class(&result),
        latency_ms,
    )
    .emit();
    result
}

fn cmd_item(project: Option<&str>, cmd: &ItemCmd) -> Result<Output> {
    let s = config::open_store()?;
    match cmd {
        ItemCmd::Add(a) => {
            let new = NewItem {
                title: a.title.clone(),
                node_type: opt_parse(&a.node_type, NodeType::parse)?,
                body: a.body.clone(),
                done_looks_like: a.done_looks_like.clone(),
                why: a.why.clone(),
                due_at: a.due_at.clone(),
                status: opt_parse(&a.status, Status::parse)?,
                priority: a.priority,
                commit: a.commit,
                assign: opt_parse(&a.assign, OwnerKind::parse)?,
                parent: a.parent.clone(),
                depends_on: a.depends_on.clone(),
                group: a.group.clone(),
                metadata: None,
            };
            Ok(Output::AddOutcome(s.add_item_checked(
                project,
                new,
                a.if_absent,
            )?))
        }
        ItemCmd::List(a) => {
            let filter = ItemFilter {
                status: opt_parse(&a.status, Status::parse)?,
                node_type: opt_parse(&a.node_type, NodeType::parse)?,
                owner: opt_parse(&a.owner, OwnerKind::parse)?,
                committed: if a.committed { Some(true) } else { None },
                icebox: a.icebox,
                inbox: false,
                group: a.group.clone(),
                wait: opt_parse(&a.wait, WaitState::parse)?,
                stale_days: a.stale,
                // Live-only by default; `--all` surfaces archived/superseded. An
                // explicit `--status archived|superseded` reaches them regardless
                // (the core filter honors a named status over this default) (HV-53).
                include_dead: a.all,
            };
            // `--limit`/`--offset` slice the ordered result (parity with `next`).
            // Default is unbounded so existing CLI/script output is unchanged.
            let mut items = s.list_items(project, &filter)?;
            if a.offset.is_some() || a.limit.is_some() {
                let offset = a.offset.unwrap_or(0);
                let limit = a.limit.unwrap_or(usize::MAX);
                items = items.into_iter().skip(offset).take(limit).collect();
            }
            Ok(Output::Items(items))
        }
        ItemCmd::Get(a) => {
            let includes = a
                .include
                .iter()
                .map(|i| Include::parse(i))
                .collect::<Result<Vec<_>>>()?;
            Ok(Output::Item(s.get_item(
                project,
                &a.reference,
                &includes,
            )?))
        }
        ItemCmd::Update(a) => {
            // `item update --commit/--uncommit` is a common reflex, but commitment
            // is its own verb. Intercept and name the corrective command (HV-158).
            if let Some(tip) = a.misused_commit_tip() {
                return Err(HavenError::Invalid(tip));
            }
            let wait = match a.wait.as_deref() {
                None => None,
                Some("none") => Some(WaitUpdate::Clear),
                Some(w) => Some(WaitUpdate::Set(WaitState::parse(w)?)),
            };
            let due = match a.due_at.as_deref() {
                None => None,
                Some("none") => Some(DueUpdate::Clear),
                Some(d) => Some(DueUpdate::Set(d.to_string())),
            };
            let upd = ItemUpdate {
                title: a.title.clone(),
                body: a.body.clone(),
                done_looks_like: a.done_looks_like.clone(),
                why: a.why.clone(),
                status: opt_parse(&a.status, Status::parse)?,
                priority: a.priority,
                node_type: opt_parse(&a.node_type, NodeType::parse)?,
                wait,
                due,
            };
            Ok(Output::Items(s.update_items(
                project,
                &refs(&a.references),
                upd,
            )?))
        }
        ItemCmd::Commit {
            references,
            priority,
        } => Ok(Output::Items(s.commit_items(
            project,
            &refs(references),
            *priority,
        )?)),
        ItemCmd::Uncommit { references } => {
            Ok(Output::Items(s.uncommit_items(project, &refs(references))?))
        }
        ItemCmd::Claim(a) => {
            let owner = OwnerKind::parse(&a.owner)?;
            Ok(Output::Item(s.claim(
                project,
                &a.reference,
                owner,
                a.actor.as_deref(),
            )?))
        }
        ItemCmd::Assign(a) => {
            let owner = OwnerKind::parse(&a.to)?;
            Ok(Output::Item(s.assign_item(
                project,
                &a.reference,
                owner,
                a.actor.as_deref(),
            )?))
        }
        ItemCmd::Handoff(a) => {
            let to = OwnerKind::parse(&a.to)?;
            let input = HandoffInput {
                from: opt_parse(&a.from, OwnerKind::parse)?,
                note: a.note.as_deref(),
                status: opt_parse(&a.status, Status::parse)?,
                wait: opt_parse(&a.wait, WaitState::parse)?,
                actor: a.actor.as_deref(),
            };
            Ok(Output::Json(serde_json::to_value(s.handoff(
                project,
                &a.reference,
                to,
                input,
            )?)?))
        }
        ItemCmd::Complete(a) => {
            let input = CompleteInput {
                evidence: a.evidence.as_deref(),
                artifact_role: opt_parse(&a.role, ArtifactRole::parse)?,
                by: a.by.as_deref(),
            };
            Ok(Output::Json(serde_json::to_value(s.complete_item(
                project,
                &a.reference,
                input,
            )?)?))
        }
        ItemCmd::Rank(a) => Ok(Output::Item(s.rank_item(
            project,
            &a.reference,
            a.before.as_deref(),
            a.after.as_deref(),
        )?)),
        ItemCmd::Archive {
            references,
            rationale,
        } => Ok(Output::Items(s.archive_items(
            project,
            &refs(references),
            rationale.as_deref(),
            None,
        )?)),
        ItemCmd::Reopen {
            reference,
            rationale,
        } => Ok(Output::Item(s.reopen_item(
            project,
            reference,
            rationale.as_deref(),
            None,
        )?)),
    }
}

fn cmd_decompose(project: Option<&str>, a: &DecomposeArgs) -> Result<Output> {
    let s = config::open_store()?;
    for child in &a.into {
        s.decompose(project, &a.parent, child, false)?;
    }
    for child in &a.remove {
        s.decompose(project, &a.parent, child, true)?;
    }
    Ok(Output::Unit)
}

fn cmd_depend(project: Option<&str>, a: &DependArgs) -> Result<Output> {
    let s = config::open_store()?;
    for on in &a.on {
        s.depend(project, &a.node, on, false)?;
    }
    for on in &a.remove {
        s.depend(project, &a.node, on, true)?;
    }
    Ok(Output::Unit)
}

fn cmd_group(project: Option<&str>, a: &GroupArgs) -> Result<Output> {
    let s = config::open_store()?;
    for member in &a.add {
        s.group(project, &a.group, member, false)?;
    }
    for member in &a.remove {
        s.group(project, &a.group, member, true)?;
    }
    Ok(Output::Unit)
}

fn cmd_evolve(project: Option<&str>, cmd: &EvolveCmd) -> Result<Output> {
    let s = config::open_store()?;
    match cmd {
        EvolveCmd::Split(a) => {
            let res = s.evolve_split(
                project,
                &a.reference,
                &a.into,
                a.rationale.as_deref(),
                a.by.as_deref(),
            )?;
            Ok(Output::Json(serde_json::to_value(res)?))
        }
        EvolveCmd::Merge(a) => {
            let res = s.evolve_merge(
                project,
                &a.references,
                &a.title,
                a.rationale.as_deref(),
                a.by.as_deref(),
            )?;
            Ok(Output::Json(serde_json::to_value(res)?))
        }
        EvolveCmd::Supersede(a) => {
            let res = s.evolve_supersede(
                project,
                &a.reference,
                &a.with,
                a.rationale.as_deref(),
                a.by.as_deref(),
            )?;
            Ok(Output::Json(serde_json::to_value(res)?))
        }
        EvolveCmd::Graph(a) => {
            let dir = a
                .direction
                .as_deref()
                .map(LineageDirection::parse)
                .transpose()?
                .unwrap_or(LineageDirection::Both);
            let g = s.evolve_graph(project, &a.reference, dir, a.depth)?;
            Ok(Output::Json(serde_json::to_value(g)?))
        }
        EvolveCmd::Resolve { reference } => {
            // `evolve resolve` is the CLI's one-release alias over the retired
            // public `resolve_live` (HV-154); MCP reads now ride a stale_ref hint
            // automatically. Kept working, deprecated at the Store boundary.
            #[allow(deprecated)]
            let live = s.resolve_live(project, reference)?;
            Ok(Output::Items(live))
        }
    }
}

/// Parse an optional string field with a core parser, propagating errors.
fn opt_parse<T>(s: &Option<String>, f: fn(&str) -> Result<T>) -> Result<Option<T>> {
    s.as_deref().map(f).transpose()
}

/// Borrow a `Vec<String>` of refs as `&[&str]` for the batch store methods.
fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// Cheap unique device id without pulling uuid into the CLI crate.
fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("dev-{nanos:x}")
}

#[cfg(test)]
mod guard_tests {
    use super::{guard_outcome, GuardKind, GuardOutcome};

    #[test]
    fn matching_target_or_exempt_always_allows() {
        // Target == binding → nothing to guard, for reads or writes.
        assert_eq!(
            guard_outcome(GuardKind::Mutation, "haven", Some("haven"), false),
            GuardOutcome::Allow
        );
        assert_eq!(
            guard_outcome(GuardKind::Read, "haven", Some("haven"), false),
            GuardOutcome::Allow
        );
        // Exempt ops never trip the guard even on a mismatch.
        assert_eq!(
            guard_outcome(GuardKind::Exempt, "haven", Some("other"), false),
            GuardOutcome::Allow
        );
    }

    #[test]
    fn mutation_mismatch_blocks_without_p_warns_with_it() {
        // The mis-file case: a write, no -p, current project differs from binding.
        assert!(matches!(
            guard_outcome(GuardKind::Mutation, "haven", Some("other"), false),
            GuardOutcome::Block(_)
        ));
        // A binding exists but nothing resolves → still blocked, with guidance.
        assert!(matches!(
            guard_outcome(GuardKind::Mutation, "haven", None, false),
            GuardOutcome::Block(_)
        ));
        // An explicit -p is the deliberate override → warn, not block.
        assert!(matches!(
            guard_outcome(GuardKind::Mutation, "haven", Some("other"), true),
            GuardOutcome::Warn(_)
        ));
    }

    #[test]
    fn read_mismatch_only_warns_never_blocks() {
        assert!(matches!(
            guard_outcome(GuardKind::Read, "haven", Some("other"), false),
            GuardOutcome::Warn(_)
        ));
        assert!(matches!(
            guard_outcome(GuardKind::Read, "haven", None, true),
            GuardOutcome::Warn(_)
        ));
    }
}

#[cfg(test)]
mod parse_tests {
    //! Parse-level coverage for the verb-divergence guesses (HV-158): every
    //! known wrong verb must either *resolve* to the right command or be
    //! intercepted with an error naming the exact corrective `haven …` command.
    //! These assert on parse structure + the pure corrective helpers, so they
    //! need no store.
    use super::*;
    use clap::Parser;

    /// Parse a `haven …` arg vector (clap auto-prepends the bin name).
    fn parse(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("haven").chain(args.iter().copied()))
    }

    // ---- top-level MCP-flat / divergent verbs route to a corrective error ----

    #[test]
    fn list_items_routes_to_item_list_corrective() {
        let cli = parse(&["list-items"]).expect("external verb should parse, not hard-error");
        let Command::Unknown(words) = &cli.command else {
            panic!("expected Command::Unknown, got something else");
        };
        let tip = corrective_for_unknown(words).expect("list-items must have a corrective");
        assert!(tip.contains("haven item list"), "tip: {tip}");
    }

    #[test]
    fn mcp_flat_names_tip_to_item_verb() {
        // get / add / archive / handoff collide with item-nesting: each tips to
        // the `item <verb>` form.
        for (verb, want) in [
            ("get", "haven item get"),
            ("add", "haven item add"),
            ("archive", "haven item archive"),
            ("handoff", "haven item handoff"),
        ] {
            let cli = parse(&[verb]).unwrap_or_else(|_| panic!("`{verb}` should parse as Unknown"));
            let Command::Unknown(words) = &cli.command else {
                panic!("`{verb}` should route to Command::Unknown");
            };
            let tip = corrective_for_unknown(words)
                .unwrap_or_else(|| panic!("`{verb}` must carry a corrective"));
            assert!(
                tip.contains(want),
                "`{verb}` tip should name `{want}`, got: {tip}"
            );
        }
    }

    #[test]
    fn unknown_verb_with_no_mapping_still_errors_gracefully() {
        let cli = parse(&["frobnicate"]).expect("any unknown verb parses as Unknown");
        let Command::Unknown(words) = &cli.command else {
            panic!("expected Command::Unknown");
        };
        // No specific mapping → generic guidance (the error path still names help).
        assert!(corrective_for_unknown(words).is_none());
    }

    // ---- item show is already aliased to item get ----

    #[test]
    fn item_show_resolves_to_item_get() {
        let cli = parse(&["item", "show", "HV-1"]).expect("`item show` should resolve");
        assert!(matches!(
            cli.command,
            Command::Item {
                cmd: ItemCmd::Get(_)
            }
        ));
    }

    // ---- item update --commit / --uncommit error with the commit/uncommit verb ----

    #[test]
    fn item_update_commit_flag_errors_with_commit_verb() {
        let cli = parse(&["item", "update", "HV-1", "--commit"]).expect("flag should parse");
        let Command::Item {
            cmd: ItemCmd::Update(a),
        } = &cli.command
        else {
            panic!("expected item update");
        };
        let tip = a
            .misused_commit_tip()
            .expect("--commit on update must be intercepted");
        assert!(tip.contains("haven item commit HV-1"), "tip: {tip}");
    }

    #[test]
    fn item_update_uncommit_flag_errors_with_uncommit_verb() {
        let cli = parse(&["item", "update", "HV-1", "HV-2", "--uncommit"]).expect("parses");
        let Command::Item {
            cmd: ItemCmd::Update(a),
        } = &cli.command
        else {
            panic!("expected item update");
        };
        let tip = a
            .misused_commit_tip()
            .expect("--uncommit on update must be intercepted");
        assert!(tip.contains("haven item uncommit HV-1 HV-2"), "tip: {tip}");
    }

    #[test]
    fn item_update_without_commit_flags_has_no_tip() {
        let cli = parse(&["item", "update", "HV-1", "--status", "ready"]).unwrap();
        let Command::Item {
            cmd: ItemCmd::Update(a),
        } = &cli.command
        else {
            panic!("expected item update");
        };
        assert!(a.misused_commit_tip().is_none());
    }

    // ---- status <key> positional resolves like -p <key> ----

    #[test]
    fn status_positional_key_resolves_like_p() {
        let cli = parse(&["status", "demo"]).expect("status takes an optional positional key");
        let Command::Status { project_key } = &cli.command else {
            panic!("expected Command::Status");
        };
        assert_eq!(project_key.as_deref(), Some("demo"));
        // No positional → None, identical to the old no-arg form.
        let bare = parse(&["status"]).unwrap();
        assert!(matches!(
            bare.command,
            Command::Status { project_key: None }
        ));
    }
}

#[cfg(test)]
mod doctor_tests {
    //! `haven doctor` must degrade rather than die when the store can't open —
    //! exactly the situation (e.g. a store migrated by a newer binary,
    //! `StoreTooNew`) where you most need the diagnostic (HV-39). The report is
    //! built by `doctor_report`, which takes the store *open result* so a failed
    //! open becomes one error check and the non-store checks still run.
    use super::*;
    use haven_core::Store;

    /// Build a real on-disk store, then bump its `user_version` past what this
    /// binary supports so a re-open yields `HavenError::StoreTooNew`. Mirrors the
    /// scenario in `haven_core::db`'s `newer_database_gets_actionable_error`.
    fn too_new_store(root: &std::path::Path) -> (config::Paths, HavenError) {
        let db = root.join("haven.db");
        // First open creates + migrates the store to the current schema.
        Store::open(&db, root).expect("initial open should succeed");
        // Stamp a future schema version directly via SQLite, then re-open.
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.pragma_update(
            None,
            "user_version",
            haven_core::db::latest_schema_migration() + 1,
        )
        .unwrap();
        drop(conn);
        // `Store` is not `Debug`, so destructure rather than `expect_err`.
        let err = match Store::open(&db, root) {
            Ok(_) => panic!("re-open should fail as StoreTooNew"),
            Err(e) => e,
        };
        (
            config::Paths {
                db,
                root: root.to_path_buf(),
            },
            err,
        )
    }

    #[test]
    fn doctor_degrades_when_store_too_new() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, err) = too_new_store(tmp.path());
        // Sanity: this is the variant we expect to degrade gracefully.
        let supported = haven_core::db::latest_schema_migration();
        assert!(matches!(err, HavenError::StoreTooNew { .. }));

        let report = doctor_report(Err(err), &paths).expect("report builds despite failed open");

        // The report is produced (not an error) and flags overall failure.
        assert_eq!(report["ok"], serde_json::Value::Bool(false));
        // Store-derived fields are null with no open store.
        assert_eq!(report["schema_version"], serde_json::Value::Null);
        assert_eq!(report["device_id"], serde_json::Value::Null);

        let checks = report["checks"].as_array().expect("checks is an array");

        // The database check carries the failure as its error detail, naming
        // BOTH the store's version and the version this binary supports.
        let db_check = checks
            .iter()
            .find(|c| c["name"] == "database")
            .expect("a database check is present");
        assert_eq!(db_check["status"], "error");
        let detail = db_check["detail"].as_str().unwrap();
        assert!(
            detail.contains(&(supported + 1).to_string())
                && detail.contains(&supported.to_string()),
            "database detail should name both store v{} and binary v{}, got: {detail}",
            supported + 1,
            supported,
        );

        // The store-dependent checks must be SKIPPED (no open store to run them).
        for skipped in ["schema", "context_pack_integrity", "xref_integrity"] {
            assert!(
                !checks.iter().any(|c| c["name"] == skipped),
                "store-dependent check `{skipped}` should be skipped on failed open"
            );
        }

        // ...but the non-store checks STILL run. `backups` reads the filesystem,
        // not the open store, so it must be present; `path` comes from the
        // install check and is likewise store-independent.
        assert!(
            checks.iter().any(|c| c["name"] == "backups"),
            "non-store `backups` check should still run after a failed open"
        );
        assert!(
            checks.iter().any(|c| c["name"] == "path"),
            "non-store `path` check should still run after a failed open"
        );
    }

    #[test]
    fn doctor_full_report_when_store_opens() {
        // The healthy path: a freshly-opened store runs every check, including
        // the store-dependent ones, and reports ok.
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("haven.db");
        let store = Store::open(&db, tmp.path()).unwrap();
        let paths = config::Paths {
            db,
            root: tmp.path().to_path_buf(),
        };

        let report = doctor_report(Ok(store), &paths).expect("report builds for an open store");

        let checks = report["checks"].as_array().unwrap();
        for present in [
            "database",
            "schema",
            "context_pack_integrity",
            "xref_integrity",
        ] {
            assert!(
                checks.iter().any(|c| c["name"] == present),
                "open store should run the `{present}` check"
            );
        }
        // schema_version is seeded by the migration, so it is non-null here.
        assert!(report["schema_version"].is_string());
    }
}
