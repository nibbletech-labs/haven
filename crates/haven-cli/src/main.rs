//! `haven` — the CLI binary. A thin clap front-end over the `haven-core`
//! `Store` service. JSON to stdout by default; `--pretty` for tables; errors as
//! `{"error": {...}}` on stderr with a non-zero exit (SPEC §2).

mod config;
mod output;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use haven_core::{
    ArtifactKind, ArtifactRole, CompleteInput, HandoffInput, HavenError, Include, ItemFilter,
    ItemUpdate, LineageDirection, NewArtifact, NewItem, NodeType, OwnerKind, Result, Status,
    WaitState, WaitUpdate,
};

use output::Output;

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
        /// Skip installing the Claude skill (headless / non-Claude installs).
        #[arg(long)]
        no_skill: bool,
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
    Status,
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
    /// The ready-to-dispatch query.
    Next(NextArgs),
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
    /// Manage content artifacts on an item.
    Artifact {
        #[command(subcommand)]
        cmd: ArtifactCmd,
    },
    /// Append a free scratch line to an item's dated notes file (no DB row).
    Note { reference: String, text: String },
    /// (Re)write the project's backlog.md projection.
    Render,
    /// Install the embedded Claude skill snapshot.
    Skill {
        #[command(subcommand)]
        cmd: SkillCmd,
    },
    /// Run the MCP server over stdio (the surface builder/app consume).
    Mcp,
    /// Auth0 sign-in / sign-out / status.
    Auth {
        #[command(subcommand)]
        cmd: AuthCmd,
    },
    /// Sync with the cloud (push now, or report queue status).
    Sync {
        #[command(subcommand)]
        cmd: Option<SyncCmd>,
        /// Run as a background loop (reachability-driven). Designed, not v1.
        #[arg(long)]
        watch: bool,
    },
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
    /// Write the embedded skill to ~/.claude/skills/haven/ (overridable via $HAVEN_CLAUDE_DIR).
    Install,
}

#[derive(Subcommand)]
// `Add` carries more fields than `List`/`Get`; this is a parsed-once CLI value,
// so boxing would only add indirection.
#[allow(clippy::large_enum_variant)]
enum ArtifactCmd {
    Add(ArtifactAddArgs),
    List(ArtifactListArgs),
    Get(ArtifactGetArgs),
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
    /// Filename for --content (defaults to <role>.md).
    #[arg(long)]
    name: Option<String>,
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

#[derive(Subcommand)]
enum ConfigCmd {
    Get { key: String },
    Set { key: String, value: String },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Create a project namespace/backlog.
    Add(ProjectAddArgs),
    /// List project namespaces/backlogs.
    List,
    /// Show one project by key.
    Get { key: String },
    /// Set the default project for later commands.
    Use { key: String },
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

#[derive(Subcommand)]
enum ItemCmd {
    /// Create an item. Defaults to uncommitted discovery work in the icebox.
    Add(ItemAddArgs),
    /// List items with optional filters.
    List(ItemListArgs),
    /// Show one item by ref or public_id.
    Get(ItemGetArgs),
    /// Update item fields such as status, type, acceptance, wait-state, and priority.
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
    /// task | code | research | data | design | admin | release | phase | gate.
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
    /// task | code | research | data | design | admin | release | phase | gate.
    #[arg(long = "type")]
    node_type: Option<String>,
    /// on_human | on_dependency | on_external | none
    #[arg(long)]
    wait: Option<String>,
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
    /// Maximum number of dispatchable items to return.
    #[arg(long)]
    limit: Option<i64>,
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
}

fn main() {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(out) => {
            out.render(cli.pretty);
            maybe_render(&cli);
        }
        Err(err) => std::process::exit(output::render_error(&err)),
    }
}

fn run(cli: &Cli) -> Result<Output> {
    let project = cli.project.as_deref();
    match &cli.command {
        Command::Setup {
            no_skill,
            project_key,
            project_title,
            prefix,
        } => cmd_setup(
            *no_skill,
            project_key.as_deref(),
            project_title.as_deref(),
            prefix.as_deref(),
        ),
        Command::Init => {
            config::open_store()?;
            Ok(Output::Message("database initialised".into()))
        }
        Command::Status => cmd_status(project),
        Command::Doctor => cmd_doctor(),
        Command::Config { cmd } => cmd_config(cmd),
        Command::Project { cmd } => cmd_project(cmd),
        Command::Item { cmd } => cmd_item(project, cmd),
        Command::Next(a) => {
            let s = config::open_store()?;
            let owner = a.owner.as_deref().map(OwnerKind::parse).transpose()?;
            if a.explain {
                Ok(Output::Json(s.next_explain(project, owner)?))
            } else {
                Ok(Output::Items(s.next(project, owner, a.limit)?))
            }
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
            Ok(Output::Json(serde_json::to_value(
                s.project_graph(project, a.lineage)?,
            )?))
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
        Command::Skill { cmd } => cmd_skill(cmd),
        Command::Mcp => {
            // Serve until stdin EOF; stdout is the MCP channel, so exit without
            // printing any Output afterwards.
            let s = config::open_store()?;
            haven_mcp::serve(&s).map_err(HavenError::Io)?;
            std::process::exit(0);
        }
        Command::Auth { cmd } => cmd_auth(cmd),
        Command::Sync { cmd, watch } => cmd_sync(project, cmd, *watch),
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
    let store = config::open_store()?;
    let token_store = haven_auth::TokenStore::new();
    match cmd {
        AuthCmd::Login { token: Some(jwt) } => {
            // Headless path: store a pasted token (no refresh; far-future expiry).
            let tokens = haven_auth::Tokens {
                access_token: jwt.clone(),
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
                }),
                None => serde_json::json!({ "signed_in": false }),
            }))
        }
    }
}

fn cmd_sync(project: Option<&str>, cmd: &Option<SyncCmd>, watch: bool) -> Result<Output> {
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

    // One foreground push pass.
    let sync_cfg = config::sync_config(&store)?;
    let paths = config::resolve()?;

    // Token source: $HAVEN_ACCESS_TOKEN (headless/CI, SPEC §6 paste-a-token)
    // wins; otherwise load from the keyring, auto-refreshing via Auth0.
    let env_token = std::env::var("HAVEN_ACCESS_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    let auth_cfg = if env_token.is_some() {
        None
    } else {
        Some(config::auth_config(&store)?)
    };

    let pending_before = store
        .store_status(project)
        .ok()
        .and_then(|v| v.get("sync_pending").and_then(|n| n.as_i64()))
        .unwrap_or(0);

    let stats = block_on(async move {
        let access = match env_token {
            Some(t) => t,
            None => {
                let token_store = haven_auth::TokenStore::new();
                let cfg = auth_cfg
                    .as_ref()
                    .expect("auth_cfg is Some whenever env_token is None");
                haven_auth::current_access_token(cfg, &token_store)
                    .await
                    .map_err(auth_err)?
            }
        };
        let engine = haven_sync::SyncEngine::new(sync_cfg, access);
        let conn = haven_core::db::open(&paths.db)?;
        // Push local changes first, then pull + reconcile remote state (so a
        // just-pushed row round-trips cleanly instead of being re-applied).
        engine.push_pass(&conn).await.map_err(sync_err)?;
        let stats = engine.pull_pass(&conn).await.map_err(sync_err)?;
        Ok::<_, HavenError>(stats)
    })??;

    Ok(Output::Json(serde_json::json!({
        "pushed": true,
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
            Ok(Output::Json(serde_json::to_value(s.get_artifact(
                project,
                &a.reference,
                role,
                a.path.as_deref(),
            )?)?))
        }
    }
}

/// After a command that changes the work-graph, regenerate `backlog.md` so the
/// projection never drifts from the DB (SPEC §4). Best-effort: a render failure
/// (e.g. no project selected) must not fail the underlying command.
fn maybe_render(cli: &Cli) {
    let mutates = matches!(
        cli.command,
        Command::Item { .. }
            | Command::Decompose(_)
            | Command::Depend(_)
            | Command::Group(_)
            | Command::Evolve { .. }
            | Command::Artifact { .. }
            | Command::Note { .. }
    );
    if !mutates {
        return;
    }
    if let Ok(s) = config::open_store() {
        let _ = s.render(cli.project.as_deref());
    }
}

fn cmd_setup(
    no_skill: bool,
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
    // Register the MCP server with Claude (best-effort; never fail setup on it).
    let mcp_config = match config::ensure_mcp_wiring() {
        Ok(p) => p.display().to_string(),
        Err(e) => {
            warnings.push(format!("MCP wiring skipped: {e}"));
            format!("skipped: {e}")
        }
    };
    // Install the embedded skill alongside it (skippable; also best-effort).
    let skill = if no_skill {
        "skipped (--no-skill)".to_string()
    } else {
        match config::ensure_skill_installed() {
            Ok(p) => p.display().to_string(),
            Err(e) => {
                warnings.push(format!("skill install skipped: {e}"));
                format!("skipped: {e}")
            }
        }
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
    Ok(Output::Json(serde_json::json!({
        "message": "haven local setup complete",
        "root": paths.root.display().to_string(),
        "db": paths.db.display().to_string(),
        "mcp_config": mcp_config,
        "skill": skill,
        "current_project": current_project,
        "project_created": project_created,
        "warnings": warnings,
        "next": if current_project.is_some() {
            "add items with `haven item add ...`"
        } else {
            "create a project with `haven project add --key <key> --title <title>` then `haven project use <key>`"
        },
        "note": "cloud auth/sync is configured separately",
    })))
}

fn cmd_skill(cmd: &SkillCmd) -> Result<Output> {
    match cmd {
        SkillCmd::Install => {
            let dir = config::ensure_skill_installed()?;
            Ok(Output::Json(serde_json::json!({
                "installed": dir.display().to_string(),
                "files": ["SKILL.md", "references/workflows.md", "references/surface-map.md"],
            })))
        }
    }
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
            serde_json::json!(s.list_projects()?.len()),
        );
        obj.insert("auth".into(), serde_json::json!("not configured (Layer 6)"));
    }
    Ok(Output::Json(status))
}

/// One diagnostic line: a named check, a status, and a human-readable detail.
/// `status` is "ok" | "warn" | "error"; an "error" makes the whole report not-ok.
fn check(name: &str, status: &str, detail: String) -> serde_json::Value {
    serde_json::json!({ "name": name, "status": status, "detail": detail })
}

fn cmd_doctor() -> Result<Output> {
    let s = config::open_store()?;
    let paths = config::resolve()?;
    let mut checks = Vec::new();

    // 1. Database schema — the store opened (migrations ran); confirm the stamp.
    match s.meta_get("schema_version")? {
        Some(v) => checks.push(check(
            "database",
            "ok",
            format!("schema v{v} at {}", paths.db.display()),
        )),
        None => checks.push(check(
            "database",
            "error",
            "schema_version missing — run `haven setup`".into(),
        )),
    }

    // 2–4. Install wiring: MCP stanza, skill snapshot, binary on PATH.
    match config::install_check() {
        Ok(w) => {
            checks.push(if w.mcp_registered {
                check(
                    "mcp",
                    "ok",
                    format!(
                        "`haven` server registered in {}",
                        w.mcp_config_path.display()
                    ),
                )
            } else {
                check(
                    "mcp",
                    "warn",
                    format!(
                        "not registered in {} — run `haven setup`",
                        w.mcp_config_path.display()
                    ),
                )
            });

            checks.push(if w.skill_present && w.skill_current {
                check(
                    "skill",
                    "ok",
                    format!("up to date at {}", w.skill_dir.display()),
                )
            } else if w.skill_present {
                check(
                    "skill",
                    "warn",
                    "installed but stale vs this binary — run `haven skill install`".into(),
                )
            } else {
                check(
                    "skill",
                    "warn",
                    format!(
                        "missing {} — run `haven skill install`",
                        w.missing_skill_files.join(", ")
                    ),
                )
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

    // 5. Auth/sync — not part of the local (no-accounts) install.
    checks.push(check("auth", "skip", "not configured (cloud half)".into()));
    checks.push(check("sync", "skip", "not configured (cloud half)".into()));

    let ok = !checks
        .iter()
        .any(|c| c["status"] == "error" || c["status"] == "warn");
    let mut report = serde_json::json!({
        "ok": ok,
        "schema_version": s.meta_get("schema_version")?,
        "device_id": s.meta_get("device_id")?,
        "checks": checks,
    });
    if !ok {
        report["hint"] =
            serde_json::json!("`haven setup` re-wires MCP + skill; warnings above are non-fatal");
    }
    Ok(Output::Json(report))
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
        ProjectCmd::List => Ok(Output::Projects(s.list_projects()?)),
        ProjectCmd::Get { key } => Ok(Output::Project(s.get_project(key)?)),
        ProjectCmd::Use { key } => {
            s.use_project(key)?;
            Ok(Output::Message(format!("current project: {key}")))
        }
    }
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
                status: opt_parse(&a.status, Status::parse)?,
                priority: a.priority,
                commit: a.commit,
                assign: opt_parse(&a.assign, OwnerKind::parse)?,
                parent: a.parent.clone(),
                depends_on: a.depends_on.clone(),
                group: a.group.clone(),
                metadata: None,
            };
            Ok(Output::Item(s.add_item(project, new)?))
        }
        ItemCmd::List(a) => {
            let filter = ItemFilter {
                status: opt_parse(&a.status, Status::parse)?,
                node_type: opt_parse(&a.node_type, NodeType::parse)?,
                owner: opt_parse(&a.owner, OwnerKind::parse)?,
                committed: if a.committed { Some(true) } else { None },
                icebox: a.icebox,
                group: a.group.clone(),
                wait: opt_parse(&a.wait, WaitState::parse)?,
                stale_days: a.stale,
            };
            Ok(Output::Items(s.list_items(project, &filter)?))
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
            let wait = match a.wait.as_deref() {
                None => None,
                Some("none") => Some(WaitUpdate::Clear),
                Some(w) => Some(WaitUpdate::Set(WaitState::parse(w)?)),
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
        EvolveCmd::Resolve { reference } => Ok(Output::Items(s.resolve_live(project, reference)?)),
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
