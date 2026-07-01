//! Path resolution for the `~/.haven` root and the SQLite database.
//!
//! The root is `$HAVEN_HOME` when set (used by tests and headless contexts),
//! else `~/.haven`. The DB lives at `<root>/haven.db`. The content tree under
//! `<root>/<project>/...` is the Layer 4 concern; here we just resolve paths and
//! open the store.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use haven_auth::AuthConfig;
use haven_core::{HavenError, Result, Store};
use haven_sync::SyncConfig;

pub struct Paths {
    pub root: PathBuf,
    pub db: PathBuf,
}

pub fn resolve() -> Result<Paths> {
    let root = match std::env::var_os("HAVEN_HOME") {
        Some(h) => PathBuf::from(h),
        None => directories::BaseDirs::new()
            .map(|b| b.home_dir().join(".haven"))
            .ok_or_else(|| HavenError::Invalid("could not determine home directory".into()))?,
    };
    Ok(Paths {
        db: root.join("haven.db"),
        root,
    })
}

/// Ensure the root exists, then open the store (running migrations). Used by all
/// commands that touch the work-graph.
pub fn open_store() -> Result<Store> {
    let paths = resolve()?;
    std::fs::create_dir_all(&paths.root)?;
    Store::open(&paths.db, &paths.root)
}

/// Resolve a setting from `meta` (set via `haven config set`), falling back to
/// an environment variable, erroring if neither is present.
fn setting(store: &Store, meta_key: &str, env: &str) -> Result<String> {
    if let Some(v) = store.meta_get(meta_key)? {
        return Ok(v);
    }
    if let Some(v) = std::env::var_os(env) {
        return Ok(v.to_string_lossy().into_owned());
    }
    Err(HavenError::Invalid(format!(
        "missing config '{meta_key}': set it with `haven config set {meta_key} <value>` or ${env}"
    )))
}

/// Resolve an optional setting: `meta`, then the env var, else `None`.
fn setting_opt(store: &Store, meta_key: &str, env: &str) -> Result<Option<String>> {
    if let Some(v) = store.meta_get(meta_key)? {
        return Ok(Some(v));
    }
    Ok(std::env::var_os(env).map(|v| v.to_string_lossy().into_owned()))
}

/// Auth0 tenant config for the CLI app (SPEC §6). The audience is optional:
/// the ID-token flow Supabase third-party auth uses doesn't need one.
pub fn auth_config(store: &Store) -> Result<AuthConfig> {
    Ok(AuthConfig::new(
        setting(store, "auth0_domain", "HAVEN_AUTH0_DOMAIN")?,
        setting(store, "auth0_client_id", "HAVEN_AUTH0_CLIENT_ID")?,
        setting_opt(store, "auth0_audience", "HAVEN_AUTH0_AUDIENCE")?,
    ))
}

/// Supabase project config for sync (SPEC §5).
pub fn sync_config(store: &Store) -> Result<SyncConfig> {
    Ok(SyncConfig::new(
        setting(store, "supabase_url", "HAVEN_SUPABASE_URL")?,
        setting(store, "supabase_anon_key", "HAVEN_SUPABASE_ANON_KEY")?,
    ))
}

/// The Claude config dir (`~/.claude`), overridable via `$HAVEN_CLAUDE_DIR` so
/// `haven setup` in tests/headless runs never touches a developer's real config.
fn claude_dir() -> Result<PathBuf> {
    match std::env::var_os("HAVEN_CLAUDE_DIR") {
        Some(d) => Ok(PathBuf::from(d)),
        None => directories::BaseDirs::new()
            .map(|b| b.home_dir().join(".claude"))
            .ok_or_else(|| HavenError::Invalid("could not determine home directory".into())),
    }
}

/// The Claude Code **user-scope** config file (`~/.claude.json`) — the file
/// whose top-level `mcpServers` map Claude Code actually reads for user-wide
/// MCP servers. (Not `~/.claude/mcp.json`: Claude Code ignores that path —
/// found live when `claude mcp list` showed no `haven` after `setup`.) With
/// `$HAVEN_CLAUDE_DIR` set, the file lives *inside* that dir so tests stay
/// hermetic.
fn claude_mcp_config_path() -> Result<PathBuf> {
    match std::env::var_os("HAVEN_CLAUDE_DIR") {
        Some(d) => Ok(PathBuf::from(d).join(".claude.json")),
        None => directories::BaseDirs::new()
            .map(|b| b.home_dir().join(".claude.json"))
            .ok_or_else(|| HavenError::Invalid("could not determine home directory".into())),
    }
}

/// The Codex config dir (`~/.codex`), overridable via `$HAVEN_CODEX_DIR` so tests
/// and headless installs never mutate a developer's real config.
fn codex_dir() -> Result<PathBuf> {
    match std::env::var_os("HAVEN_CODEX_DIR") {
        Some(d) => Ok(PathBuf::from(d)),
        None => directories::BaseDirs::new()
            .map(|b| b.home_dir().join(".codex"))
            .ok_or_else(|| HavenError::Invalid("could not determine home directory".into())),
    }
}

fn codex_mcp_config_path() -> Result<PathBuf> {
    Ok(codex_dir()?.join("config.toml"))
}

/// Open Agent Skills user dir (`~/.agents`), overridable via `$HAVEN_AGENTS_DIR`.
/// Codex reads `.agents/skills`, `~/.agents/skills`, and `/etc/codex/skills`;
/// Haven writes the user-scope path by default.
fn agents_dir() -> Result<PathBuf> {
    match std::env::var_os("HAVEN_AGENTS_DIR") {
        Some(d) => Ok(PathBuf::from(d)),
        None => directories::BaseDirs::new()
            .map(|b| b.home_dir().join(".agents"))
            .ok_or_else(|| HavenError::Invalid("could not determine home directory".into())),
    }
}

/// A skill shipped in the binary, versioned with the CLI/MCP surface it documents
/// (ARCHITECTURE §14.3) — no runtime dependency on the repo. `files` are
/// `(relative path under the skill dir, contents)`.
struct Skill {
    name: &'static str,
    files: &'static [(&'static str, &'static str)],
}

const HAVEN_SKILL_FILES: &[(&str, &str)] = &[
    ("SKILL.md", include_str!("../../../skill/haven/SKILL.md")),
    (
        "references/workflows.md",
        include_str!("../../../skill/haven/references/workflows.md"),
    ),
    (
        "references/surface-map.md",
        include_str!("../../../skill/haven/references/surface-map.md"),
    ),
    (
        "references/spec-quality.md",
        include_str!("../../../skill/haven/references/spec-quality.md"),
    ),
    (
        "references/parallel-dev.md",
        include_str!("../../../skill/haven/references/parallel-dev.md"),
    ),
    (
        "references/external-handoff.md",
        include_str!("../../../skill/haven/references/external-handoff.md"),
    ),
    (
        "references/running-work.md",
        include_str!("../../../skill/haven/references/running-work.md"),
    ),
    (
        "agents/openai.yaml",
        include_str!("../../../skill/haven/agents/openai.yaml"),
    ),
];

const ORCHESTRATE_PLAN_SKILL_FILES: &[(&str, &str)] = &[
    (
        "SKILL.md",
        include_str!("../../../skill/orchestrate-plan/SKILL.md"),
    ),
    (
        "references/decomposition.md",
        include_str!("../../../skill/orchestrate-plan/references/decomposition.md"),
    ),
    (
        "references/tick-ops.md",
        include_str!("../../../skill/orchestrate-plan/references/tick-ops.md"),
    ),
    (
        "references/value-density.md",
        include_str!("../../../skill/orchestrate-plan/references/value-density.md"),
    ),
    (
        "agents/openai.yaml",
        include_str!("../../../skill/orchestrate-plan/agents/openai.yaml"),
    ),
];

const CREATE_CONTEXT_PACK_SKILL_FILES: &[(&str, &str)] = &[
    (
        "SKILL.md",
        include_str!("../../../skill/create-context-pack/SKILL.md"),
    ),
    (
        "references/pack-template.md",
        include_str!("../../../skill/create-context-pack/references/pack-template.md"),
    ),
    (
        "references/verify-ops.md",
        include_str!("../../../skill/create-context-pack/references/verify-ops.md"),
    ),
    (
        "agents/openai.yaml",
        include_str!("../../../skill/create-context-pack/agents/openai.yaml"),
    ),
];

const ORCHESTRATE_RUN_SKILL_FILES: &[(&str, &str)] = &[
    (
        "SKILL.md",
        include_str!("../../../skill/orchestrate-run/SKILL.md"),
    ),
    (
        "references/tick-ops.md",
        include_str!("../../../skill/orchestrate-run/references/tick-ops.md"),
    ),
    (
        "references/worktree-merge.md",
        include_str!("../../../skill/orchestrate-run/references/worktree-merge.md"),
    ),
    (
        "references/dispatch-policy.md",
        include_str!("../../../skill/orchestrate-run/references/dispatch-policy.md"),
    ),
    (
        "references/executor-discipline.md",
        include_str!("../../../skill/orchestrate-run/references/executor-discipline.md"),
    ),
    (
        "agents/openai.yaml",
        include_str!("../../../skill/orchestrate-run/agents/openai.yaml"),
    ),
];

const VERIFY_ACCEPTANCE_SKILL_FILES: &[(&str, &str)] = &[
    (
        "SKILL.md",
        include_str!("../../../skill/verify-acceptance/SKILL.md"),
    ),
    (
        "references/verdict-contract.md",
        include_str!("../../../skill/verify-acceptance/references/verdict-contract.md"),
    ),
    (
        "references/verify-ops.md",
        include_str!("../../../skill/verify-acceptance/references/verify-ops.md"),
    ),
    (
        "references/evaluation-lens.md",
        include_str!("../../../skill/verify-acceptance/references/evaluation-lens.md"),
    ),
    (
        "agents/openai.yaml",
        include_str!("../../../skill/verify-acceptance/agents/openai.yaml"),
    ),
];

/// Every skill `haven setup` / `haven skill install` lay down. Add a skill by
/// adding its files const + an entry here; install / refresh / doctor all
/// iterate this registry.
const SKILL_REGISTRY: &[Skill] = &[
    Skill {
        name: "haven",
        files: HAVEN_SKILL_FILES,
    },
    Skill {
        name: "orchestrate-plan",
        files: ORCHESTRATE_PLAN_SKILL_FILES,
    },
    Skill {
        name: "create-context-pack",
        files: CREATE_CONTEXT_PACK_SKILL_FILES,
    },
    Skill {
        name: "orchestrate-run",
        files: ORCHESTRATE_RUN_SKILL_FILES,
    },
    Skill {
        name: "verify-acceptance",
        files: VERIFY_ACCEPTANCE_SKILL_FILES,
    },
];

/// Skill names shipped in this binary, in registry order.
pub fn skill_names() -> impl Iterator<Item = &'static str> {
    SKILL_REGISTRY.iter().map(|s| s.name)
}

/// The embedded files for `skill_name`, or `None` if it isn't a shipped skill.
fn skill_files(skill_name: &str) -> Option<&'static [(&'static str, &'static str)]> {
    SKILL_REGISTRY
        .iter()
        .find(|s| s.name == skill_name)
        .map(|s| s.files)
}

/// Relative paths shipped for `skill_name` (for `haven skill install` output).
pub fn skill_file_list(skill_name: &str) -> Vec<&'static str> {
    skill_files(skill_name)
        .map(|fs| fs.iter().map(|(rel, _)| *rel).collect())
        .unwrap_or_default()
}

/// Install a skill's embedded snapshot to `<claude>/skills/<skill_name>/`.
/// Idempotent — overwrites, since it's a versioned snapshot, not user-editable
/// state. Returns the installed skill directory.
pub fn ensure_skill_installed(skill_name: &str) -> Result<PathBuf> {
    let skill_dir = claude_dir()?.join("skills").join(skill_name);
    write_skill_snapshot(skill_name, &skill_dir)?;
    Ok(skill_dir)
}

pub fn ensure_codex_skill_installed(skill_name: &str) -> Result<PathBuf> {
    let skill_dir = agents_dir()?.join("skills").join(skill_name);
    write_skill_snapshot(skill_name, &skill_dir)?;
    Ok(skill_dir)
}

fn write_skill_snapshot(skill_name: &str, skill_dir: &Path) -> Result<()> {
    let files = skill_files(skill_name)
        .ok_or_else(|| HavenError::Invalid(format!("unknown skill: {skill_name}")))?;
    for (rel, contents) in files {
        let dest = skill_dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, contents)?;
    }
    Ok(())
}

/// Refresh previously-installed skill snapshots that have drifted from the one
/// embedded in this binary — the self-heal that lets a binary upgrade carry the
/// skill with it (no manual `haven skill install` after e.g. `brew upgrade`).
/// Only rewrites skill dirs that already exist, so a `--no-skill` or
/// single-agent setup stays respected. Best-effort by design: a failure is
/// reported on stderr and must never stop the caller (the MCP server).
/// Returns the dirs it refreshed.
pub fn refresh_stale_skill_snapshots() -> Vec<PathBuf> {
    let bases = [claude_dir(), agents_dir()];
    let mut refreshed = Vec::new();
    for base in bases.into_iter().flatten() {
        for skill in SKILL_REGISTRY {
            let skill_dir = base.join("skills").join(skill.name);
            if !skill_dir.is_dir() {
                continue; // never installed here (or opted out) — not ours to create
            }
            let (_, current) = skill_snapshot_check(skill.name, &skill_dir);
            if current {
                continue;
            }
            match write_skill_snapshot(skill.name, &skill_dir) {
                Ok(()) => refreshed.push(skill_dir),
                Err(e) => {
                    eprintln!(
                        "haven: skill refresh skipped ({}): {e}",
                        skill_dir.display()
                    )
                }
            }
        }
    }
    refreshed
}

/// What `haven setup` is responsible for wiring on a machine: the MCP server
/// registration in the Claude config, and the embedded skill snapshot on disk.
/// `haven doctor` reads this to tell a botched install from a healthy one.
pub struct InstallCheck {
    pub claude_mcp_config_path: PathBuf,
    pub claude_mcp_registered: bool,
    /// Per-skill snapshot status under `<claude>/skills/<name>/`, keyed by name.
    pub claude_skills: BTreeMap<String, SkillInstallStatus>,
    pub codex_mcp_config_path: PathBuf,
    pub codex_mcp_registered: bool,
    /// Per-skill snapshot status under `<agents>/skills/<name>/`, keyed by name.
    pub codex_skills: BTreeMap<String, SkillInstallStatus>,
    /// `haven` resolved on `$PATH` — what the MCP `command: "haven"` stanza needs
    /// to actually launch. `None` if the binary isn't reachable by that name.
    pub haven_on_path: Option<PathBuf>,
}

/// One installed skill's snapshot health, for one agent.
pub struct SkillInstallStatus {
    pub dir: PathBuf,
    /// Every embedded skill file is present on disk.
    pub present: bool,
    /// …and its bytes match the snapshot baked into this binary (no drift).
    pub current: bool,
    /// Embedded skill files absent from disk (subset that fails `present`).
    pub missing_files: Vec<String>,
}

/// Inspect the install wiring without mutating anything (the read side of
/// `ensure_mcp_wiring` / `ensure_skill_installed`).
pub fn install_check() -> Result<InstallCheck> {
    let claude_mcp_config_path = claude_mcp_config_path()?;
    let claude_mcp_registered = std::fs::read_to_string(&claude_mcp_config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("mcpServers")
                .and_then(|m| m.get("haven"))
                .map(|h| h.is_object())
        })
        .unwrap_or(false);

    let codex_mcp_config_path = codex_mcp_config_path()?;
    let codex_mcp_registered = std::fs::read_to_string(&codex_mcp_config_path)
        .map(|s| codex_config_has_haven(&s))
        .unwrap_or(false);

    // Per-skill snapshot health, for each shipped skill, on each agent.
    let claude_skills_base = claude_dir()?;
    let codex_skills_base = agents_dir()?;
    let mut claude_skills = BTreeMap::new();
    let mut codex_skills = BTreeMap::new();
    for skill in SKILL_REGISTRY {
        let cdir = claude_skills_base.join("skills").join(skill.name);
        let (cmissing, ccurrent) = skill_snapshot_check(skill.name, &cdir);
        claude_skills.insert(
            skill.name.to_string(),
            SkillInstallStatus {
                present: cmissing.is_empty(),
                current: ccurrent,
                missing_files: cmissing,
                dir: cdir,
            },
        );
        let xdir = codex_skills_base.join("skills").join(skill.name);
        let (xmissing, xcurrent) = skill_snapshot_check(skill.name, &xdir);
        codex_skills.insert(
            skill.name.to_string(),
            SkillInstallStatus {
                present: xmissing.is_empty(),
                current: xcurrent,
                missing_files: xmissing,
                dir: xdir,
            },
        );
    }
    Ok(InstallCheck {
        claude_mcp_config_path,
        claude_mcp_registered,
        claude_skills,
        codex_mcp_config_path,
        codex_mcp_registered,
        codex_skills,
        haven_on_path: haven_on_path(),
    })
}

fn skill_snapshot_check(skill_name: &str, skill_dir: &Path) -> (Vec<String>, bool) {
    let Some(files) = skill_files(skill_name) else {
        return (vec![format!("unknown skill: {skill_name}")], false);
    };
    let mut missing_skill_files = Vec::new();
    let mut skill_current = true;
    for (rel, contents) in files {
        match std::fs::read_to_string(skill_dir.join(rel)) {
            Ok(on_disk) if on_disk == *contents => {}
            Ok(_) => skill_current = false,
            Err(_) => {
                missing_skill_files.push((*rel).to_string());
                skill_current = false;
            }
        }
    }
    (missing_skill_files, skill_current)
}

/// Find a `haven` executable on `$PATH` (the name the MCP stanza invokes). Mirrors
/// a shell's lookup: first match wins; no executable-bit check (good enough for a
/// diagnostic, and portable).
fn haven_on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("haven"))
        .find(|candidate| candidate.is_file())
}

/// Idempotently register the `haven` MCP server in the Claude config, merging
/// into any existing `mcpServers` map (SPEC §7). Returns the config path.
pub fn ensure_mcp_wiring() -> Result<PathBuf> {
    let path = claude_mcp_config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut root: serde_json::Value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&path)?)
            .unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    if !root.is_object() {
        root = serde_json::json!({});
    }
    let obj = root.as_object_mut().unwrap();
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    // If an existing `mcpServers` is some non-object shape, replace it rather
    // than silently failing to register (the registration must not be a no-op).
    if !servers.is_object() {
        *servers = serde_json::json!({});
    }
    servers.as_object_mut().unwrap().insert(
        "haven".into(),
        serde_json::json!({ "command": "haven", "args": ["mcp"] }),
    );
    std::fs::write(&path, serde_json::to_string_pretty(&root)?)?;
    Ok(path)
}

/// Idempotently register the `haven` MCP server in Codex config TOML:
///
/// [mcp_servers.haven]
/// command = "haven"
/// args = ["mcp"]
pub fn ensure_codex_mcp_wiring() -> Result<PathBuf> {
    let path = codex_mcp_config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = upsert_toml_table(
        &raw,
        "mcp_servers.haven",
        &["command = \"haven\"", "args = [\"mcp\"]"],
    );
    std::fs::write(&path, updated)?;
    Ok(path)
}

fn codex_config_has_haven(raw: &str) -> bool {
    let section = find_toml_section(raw, "mcp_servers.haven");
    let Some(section) = section else {
        return false;
    };
    let has_command = section
        .lines()
        .any(|line| line.trim() == "command = \"haven\"");
    let has_args = section
        .lines()
        .any(|line| line.trim() == "args = [\"mcp\"]");
    has_command && has_args
}

fn find_toml_section<'a>(raw: &'a str, header: &str) -> Option<&'a str> {
    let target = format!("[{header}]");
    let start = raw.lines().position(|line| line.trim() == target)?;
    let mut byte_start = 0;
    for line in raw.lines().take(start + 1) {
        byte_start += line.len() + 1;
    }
    let mut byte_end = raw.len();
    let mut cursor = byte_start;
    for line in raw.lines().skip(start + 1) {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            byte_end = cursor;
            break;
        }
        cursor += line.len() + 1;
    }
    raw.get(byte_start..byte_end)
}

fn upsert_toml_table(raw: &str, header: &str, body: &[&str]) -> String {
    let target = format!("[{header}]");
    let mut out = String::new();
    let mut lines = raw.lines().peekable();
    let mut replaced = false;
    while let Some(line) = lines.next() {
        if line.trim() == target {
            push_toml_table(&mut out, header, body);
            replaced = true;
            while let Some(next) = lines.peek() {
                let trimmed = next.trim();
                if trimmed.starts_with('[') && trimmed.ends_with(']') {
                    break;
                }
                lines.next();
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !replaced {
        if !out.trim().is_empty() {
            out.push('\n');
        }
        push_toml_table(&mut out, header, body);
    }
    out
}

fn push_toml_table(out: &mut String, header: &str, body: &[&str]) {
    out.push('[');
    out.push_str(header);
    out.push_str("]\n");
    for line in body {
        out.push_str(line);
        out.push('\n');
    }
}

pub struct LinkResult {
    pub workspace: PathBuf,
    pub backlog: PathBuf,
    pub canonical_backlog: PathBuf,
    pub items: PathBuf,
    pub canonical_items: PathBuf,
    pub docs: PathBuf,
    pub git_exclude: Option<PathBuf>,
    pub binding: PathBuf,
}

pub struct UnlinkResult {
    pub workspace: PathBuf,
    pub removed_workspace: bool,
    pub binding: PathBuf,
    pub removed_binding: bool,
    pub git_exclude: Option<PathBuf>,
}

/// The repo-local binding marker `haven link` writes (and [`repo_binding`] reads):
/// a one-line file naming the project this repo is bound to.
pub const BINDING_FILE: &str = ".haven-project";

/// Default visible-workspace directory name for `haven link`/`unlink`.
const DEFAULT_WORKSPACE: &str = "_haven";

/// Marker line written into a projection's `README.md`. `link`/`unlink` use it to
/// recognise (and refuse to clobber) a Haven projection.
const PROJECTION_MARKER: &str = "Haven workspace projection";

pub fn link_workspace(store: &Store, project: Option<&str>, name: &Path) -> Result<LinkResult> {
    let canonical_backlog = store.render(project)?;
    // The project this repo binds to — `render` above already validated it exists.
    let key = match project {
        Some(p) => p.to_string(),
        None => store.current_project()?.ok_or_else(|| {
            HavenError::Invalid(
                "no project selected to link — pass -p <key> or run `haven project use`".into(),
            )
        })?,
    };
    let root = std::env::current_dir()?;
    let workspace = root.join(name);
    prepare_projection_workspace(&workspace)?;
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(
        workspace.join("README.md"),
        "Haven workspace projection. Canonical graph/content lives under ~/.haven.\n\
         `backlog.md` is generated; `items/` and `docs/` alias canonical content.\n",
    )?;
    let backlog = workspace.join("backlog.md");
    replace_backlog_alias(&canonical_backlog, &backlog)?;
    let canonical_items = store.content_root().join(&key).join("items");
    std::fs::create_dir_all(&canonical_items)?;
    let items = workspace.join("items");
    replace_path_alias(&canonical_items, &items)?;
    let docs = workspace.join("docs");
    rebuild_docs_projection(store, &key, &docs)?;
    // Bind this repo to the project so CLI writes run here can't silently mis-file
    // into a different (e.g. concurrently-flipped) current project — HV-147.
    let binding = root.join(BINDING_FILE);
    std::fs::write(&binding, format!("{key}\n"))?;
    let git_exclude = exclude_workspace_from_git(&workspace)?;
    append_git_exclude(&format!("/{BINDING_FILE}"))?;
    Ok(LinkResult {
        workspace,
        backlog,
        canonical_backlog,
        items,
        canonical_items,
        docs,
        git_exclude,
        binding,
    })
}

pub fn unlink_workspace(name: Option<&Path>) -> Result<UnlinkResult> {
    let root = std::env::current_dir()?;
    let workspace = match name {
        Some(n) => root.join(n),
        // No explicit name: prefer the default, else discover the lone projection
        // dir, so a workspace created with `link --name X` still unlinks cleanly
        // (and clears *its* git-exclude entry) without re-passing the name.
        None => discover_projection_workspace(&root)?,
    };
    let removed_workspace = if std::fs::symlink_metadata(&workspace).is_ok() {
        ensure_projection_workspace(&workspace)?;
        remove_existing_path(&workspace)?;
        true
    } else {
        false
    };

    let binding = root.join(BINDING_FILE);
    let removed_binding = if std::fs::symlink_metadata(&binding).is_ok() {
        std::fs::remove_file(&binding)?;
        true
    } else {
        false
    };

    let git_exclude = remove_git_exclude_entries(&[
        format!(
            "/{}/",
            workspace
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(DEFAULT_WORKSPACE)
        ),
        format!("/{BINDING_FILE}"),
    ])?;

    Ok(UnlinkResult {
        workspace,
        removed_workspace,
        binding,
        removed_binding,
        git_exclude,
    })
}

/// Resolve which workspace `unlink` targets when no `--name` is given: the
/// default `_haven` when it's a projection, otherwise the *sole* repo-local
/// directory carrying the projection marker (so a `link --name X` round-trips
/// without re-passing the name). Falls back to `_haven` when nothing — or more
/// than one candidate — is found, leaving the ambiguous case to an explicit
/// `--name` rather than guessing which to delete.
fn discover_projection_workspace(root: &Path) -> Result<PathBuf> {
    let default = root.join(DEFAULT_WORKSPACE);
    if is_projection_dir(&default) {
        return Ok(default);
    }
    let mut found: Option<PathBuf> = None;
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if is_projection_dir(&path) {
            if found.is_some() {
                return Ok(default); // ambiguous — require an explicit --name
            }
            found = Some(path);
        }
    }
    Ok(found.unwrap_or(default))
}

/// True when `path` is a directory carrying the `link` projection marker.
fn is_projection_dir(path: &Path) -> bool {
    path.is_dir()
        && std::fs::read_to_string(path.join("README.md"))
            .map(|s| s.contains(PROJECTION_MARKER))
            .unwrap_or(false)
}

/// The project this repo is bound to, if any: the first [`BINDING_FILE`] found
/// walking up from the current directory. None when unset — callers then fall
/// back to the usual project resolution, so this is purely additive.
pub fn repo_binding() -> Result<Option<String>> {
    let mut dir = match std::env::current_dir() {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };
    loop {
        let marker = dir.join(BINDING_FILE);
        if marker.is_file() {
            let key = std::fs::read_to_string(&marker)?.trim().to_string();
            return Ok((!key.is_empty()).then_some(key));
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

fn replace_backlog_alias(canonical: &Path, link: &Path) -> Result<()> {
    replace_path_alias(canonical, link)
}

fn rebuild_docs_projection(store: &Store, project_key: &str, docs: &Path) -> Result<()> {
    if std::fs::symlink_metadata(docs).is_ok() {
        remove_existing_path(docs)?;
    }
    std::fs::create_dir_all(docs)?;
    for anchor in store.docs(Some(project_key))? {
        let target = store
            .content_root()
            .join(project_key)
            .join("items")
            .join(&anchor.item.reference);
        if target.is_dir() {
            replace_path_alias(&target, &docs.join(&anchor.item.reference))?;
        }
    }
    Ok(())
}

fn replace_path_alias(canonical: &Path, link: &Path) -> Result<()> {
    if link.exists() || std::fs::symlink_metadata(link).is_ok() {
        remove_existing_path(link)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(canonical, link)?;
    }
    #[cfg(windows)]
    {
        let meta = std::fs::metadata(canonical)?;
        if meta.is_dir() {
            std::os::windows::fs::symlink_dir(canonical, link)?;
        } else {
            std::os::windows::fs::symlink_file(canonical, link)?;
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let meta = std::fs::metadata(canonical)?;
        if meta.is_dir() {
            std::fs::create_dir_all(link)?;
        } else {
            std::fs::copy(canonical, link)?;
        }
    }
    Ok(())
}

fn remove_existing_path(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn prepare_projection_workspace(workspace: &Path) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(workspace) else {
        return Ok(());
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(HavenError::Invalid(format!(
            "{} exists but is not a directory; refusing to replace it with a Haven projection",
            workspace.display()
        )));
    }
    let readme = workspace.join("README.md");
    if readme.is_file() {
        ensure_projection_workspace(workspace)?;
        return Ok(());
    }
    for entry in std::fs::read_dir(workspace)? {
        let entry = entry?;
        if entry.file_name() != ".DS_Store" {
            return Err(HavenError::Invalid(format!(
                "{} exists and does not look like a Haven projection; refusing to modify it",
                workspace.display()
            )));
        }
    }
    Ok(())
}

fn ensure_projection_workspace(workspace: &Path) -> Result<()> {
    let readme = workspace.join("README.md");
    let marker = std::fs::read_to_string(&readme).unwrap_or_default();
    if marker.contains(PROJECTION_MARKER) {
        return Ok(());
    }
    Err(HavenError::Invalid(format!(
        "{} does not look like a Haven projection; refusing to remove it",
        workspace.display()
    )))
}

fn exclude_workspace_from_git(workspace: &Path) -> Result<Option<PathBuf>> {
    let name = workspace
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(DEFAULT_WORKSPACE);
    append_git_exclude(&format!("/{name}/"))
}

/// Append a single line to the repo's `.git/info/exclude` (idempotent). Returns
/// the exclude file path, or None when not inside a git repo.
fn append_git_exclude(entry: &str) -> Result<Option<PathBuf>> {
    let Some(git_dir) = find_git_dir(std::env::current_dir()?) else {
        return Ok(None);
    };
    let exclude = git_dir.join("info").join("exclude");
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut raw = std::fs::read_to_string(&exclude).unwrap_or_default();
    if !raw.lines().any(|line| line.trim() == entry) {
        if !raw.ends_with('\n') && !raw.is_empty() {
            raw.push('\n');
        }
        raw.push_str(entry);
        raw.push('\n');
        std::fs::write(&exclude, raw)?;
    }
    Ok(Some(exclude))
}

fn remove_git_exclude_entries(entries: &[String]) -> Result<Option<PathBuf>> {
    let Some(git_dir) = find_git_dir(std::env::current_dir()?) else {
        return Ok(None);
    };
    let exclude = git_dir.join("info").join("exclude");
    if !exclude.exists() {
        return Ok(Some(exclude));
    }
    let raw = std::fs::read_to_string(&exclude).unwrap_or_default();
    let filtered: Vec<&str> = raw
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !entries.iter().any(|entry| entry == trimmed)
        })
        .collect();
    let mut next = filtered.join("\n");
    if !next.is_empty() {
        next.push('\n');
    }
    std::fs::write(&exclude, next)?;
    Ok(Some(exclude))
}

fn find_git_dir(mut dir: PathBuf) -> Option<PathBuf> {
    loop {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

// ---- self install / self update ------------------------------------------

/// Resolve the directory to install the `haven` binary into, mirroring
/// `packaging/install.sh`: an explicit `--dir`, else the first writable of
/// `$HAVEN_BIN_DIR`, `/usr/local/bin`, `~/.local/bin`.
pub fn resolve_install_dir(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = explicit {
        std::fs::create_dir_all(dir)?;
        if !is_writable_dir(dir) {
            return Err(HavenError::Invalid(format!(
                "{} is not writable",
                dir.display()
            )));
        }
        return Ok(dir.to_path_buf());
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("HAVEN_BIN_DIR") {
        candidates.push(PathBuf::from(d));
    }
    candidates.push(PathBuf::from("/usr/local/bin"));
    if let Some(b) = directories::BaseDirs::new() {
        candidates.push(b.home_dir().join(".local/bin"));
    }
    for dir in candidates {
        if std::fs::create_dir_all(&dir).is_ok() && is_writable_dir(&dir) {
            return Ok(dir);
        }
    }
    Err(HavenError::Invalid(
        "no writable install dir found — set $HAVEN_BIN_DIR and re-run".into(),
    ))
}

/// True if a file can actually be created (and removed) in `dir` — more honest
/// than inspecting permission bits.
fn is_writable_dir(dir: &Path) -> bool {
    let probe = dir.join(".haven-write-probe");
    match std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn temp_sibling(dest: &Path) -> PathBuf {
    let name = dest.file_name().and_then(|s| s.to_str()).unwrap_or("haven");
    dest.with_file_name(format!(".{name}.haven-tmp-{}", std::process::id()))
}

/// Whether `dir` is one of the entries on `$PATH`.
pub fn dir_on_path(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d == dir))
        .unwrap_or(false)
}

/// Atomically point `dest` at `source` with a symlink (Unix). The link is built
/// under a temp name in the same directory then renamed over `dest`, so a
/// concurrent PATH lookup sees the old or the new target, never a missing file
/// (`haven` is on the execution hot path). A running MCP server keeps executing
/// its old inode until it exits; the next launch resolves the new link.
#[cfg(unix)]
pub fn install_link(source: &Path, dest: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;
    let tmp = temp_sibling(dest);
    let _ = std::fs::remove_file(&tmp);
    symlink(source, &tmp)?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn install_link(source: &Path, dest: &Path) -> Result<()> {
    // No reliable unprivileged symlink on Windows — copy instead.
    eprintln!("haven: --link is unsupported on this platform; copying instead");
    install_copy(source, dest)
}

/// Copy `source` to `dest` as a 0755 executable, atomically (temp sibling, set
/// mode, rename over `dest`) so `dest` is never a torn binary.
pub fn install_copy(source: &Path, dest: &Path) -> Result<()> {
    let bytes = std::fs::read(source)?;
    atomic_write_executable(dest, &bytes)
}

/// Write `bytes` to `dest` as a 0755 executable, atomically (temp sibling in the
/// same dir, set mode, rename over `dest`). Shared by `install_copy` and the
/// binary self-update swap: renaming over a *running* binary is safe on Unix —
/// the live process keeps its old inode, and the next exec resolves the new one.
pub fn atomic_write_executable(dest: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = temp_sibling(dest);
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// How this running `haven` was installed — drives the `self update` advice.
#[derive(Debug, Clone)]
pub enum InstallMethod {
    /// Invoked through a symlink (the `self install --link` dev path).
    DevSymlink { target: PathBuf },
    /// A Homebrew-installed binary (path under a Cellar).
    Homebrew,
    /// A copied binary under `~/.local/bin` / `/usr/local/bin` (install.sh).
    InstallSh,
    /// Anything else.
    Unknown { exe: PathBuf },
}

impl InstallMethod {
    pub fn tag(&self) -> &'static str {
        match self {
            InstallMethod::DevSymlink { .. } => "dev-symlink",
            InstallMethod::Homebrew => "homebrew",
            InstallMethod::InstallSh => "install.sh",
            InstallMethod::Unknown { .. } => "unknown",
        }
    }
}

pub fn detect_install_method() -> Result<InstallMethod> {
    let raw = std::env::current_exe().map_err(HavenError::Io)?;
    // Symlink check first, on the *pre*-canonicalize path — canonicalize would
    // erase the very link we're trying to detect.
    if std::fs::symlink_metadata(&raw)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        let target = std::fs::canonicalize(&raw).unwrap_or_else(|_| raw.clone());
        return Ok(InstallMethod::DevSymlink { target });
    }
    let exe = std::fs::canonicalize(&raw).unwrap_or(raw);
    if brew_prefix_containing(&exe).is_some() {
        return Ok(InstallMethod::Homebrew);
    }
    if under_known_copy_dir(&exe) {
        return Ok(InstallMethod::InstallSh);
    }
    Ok(InstallMethod::Unknown { exe })
}

/// Detect a Homebrew install by path shape — every prefix (`/opt/homebrew`,
/// Intel `/usr/local`, Linuxbrew) has a `/Cellar/` segment. Avoids shelling out
/// to `brew`, which may not be on PATH in an agent/MCP environment.
fn brew_prefix_containing(exe: &Path) -> Option<PathBuf> {
    let mut acc = PathBuf::new();
    for comp in exe.components() {
        if comp.as_os_str() == "Cellar" {
            return Some(acc);
        }
        acc.push(comp);
    }
    std::env::var_os("HOMEBREW_PREFIX")
        .map(PathBuf::from)
        .filter(|p| exe.starts_with(p))
}

fn under_known_copy_dir(exe: &Path) -> bool {
    let haven_bin_dir = std::env::var_os("HAVEN_BIN_DIR").map(PathBuf::from);
    under_known_copy_dir_with_env(exe, haven_bin_dir.as_deref())
}

fn under_known_copy_dir_with_env(exe: &Path, haven_bin_dir: Option<&Path>) -> bool {
    if under_install_dir(exe, Path::new("/usr/local/bin")) {
        return true;
    }
    if let Some(b) = directories::BaseDirs::new() {
        if under_install_dir(exe, &b.home_dir().join(".local/bin")) {
            return true;
        }
    }
    haven_bin_dir
        .map(|d| under_install_dir(exe, d))
        .unwrap_or(false)
}

fn under_install_dir(exe: &Path, dir: &Path) -> bool {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    exe.starts_with(dir)
}

/// Per-method guidance string for `self update`.
pub fn update_advice(method: &InstallMethod) -> String {
    match method {
        InstallMethod::DevSymlink { target, .. } => format!(
            "Dev install (symlink → {}). Rebuild to update: `cargo build --release` — \
             the symlink picks it up. Skill snapshots are re-synced for you.",
            target.display()
        ),
        InstallMethod::Homebrew => "Homebrew install. Update with: \
             `brew upgrade nibbletech-labs/tap/haven` (pass --run to have haven run it)."
            .into(),
        InstallMethod::InstallSh => "Installed via install.sh. Update by re-running: \
             `curl -fsSL https://raw.githubusercontent.com/nibbletech-labs/haven/main/packaging/install.sh | sh`."
            .into(),
        InstallMethod::Unknown { exe } => format!(
            "Couldn't determine how `haven` was installed ({}). Reinstall with your \
             original method, or use install.sh.",
            exe.display()
        ),
    }
}

/// Best-effort: ask GitHub for the latest published version. Returns `None` on
/// any failure (offline, rate-limit, no releases yet) — never an error, so
/// `self update` works fully offline. The only network path in the CLI.
pub fn latest_release_version() -> Option<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    rt.block_on(fetch_latest_release_version())
}

async fn fetch_latest_release_version() -> Option<String> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("haven/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(4))
        .build()
        .ok()?;
    // Prefer the latest release; fall back to the newest tag when there are no
    // releases yet (the v0.1.0 bootstrap case — best-effort, not semver-sorted).
    if let Some(tag) = github_json(
        &client,
        "https://api.github.com/repos/nibbletech-labs/haven/releases/latest",
    )
    .await
    .and_then(|v| v.get("tag_name").and_then(|t| t.as_str()).map(String::from))
    {
        return Some(tag);
    }
    github_json(
        &client,
        "https://api.github.com/repos/nibbletech-labs/haven/tags",
    )
    .await
    .and_then(|v| v.as_array().and_then(|a| a.first()).cloned())
    .and_then(|v| v.get("name").and_then(|t| t.as_str()).map(String::from))
}

async fn github_json(client: &reqwest::Client, url: &str) -> Option<serde_json::Value> {
    let resp = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<serde_json::Value>().await.ok()
}

/// Semver-aware "is `latest` strictly newer than `current`?". Both may carry a
/// leading `v`. Returns `None` if either fails to parse — the caller treats an
/// unparseable comparison as "don't claim an update" (offline-/garbage-safe).
pub fn is_newer(current: &str, latest: &str) -> Option<bool> {
    let c = semver::Version::parse(current.trim_start_matches('v')).ok()?;
    let l = semver::Version::parse(latest.trim_start_matches('v')).ok()?;
    Some(l > c)
}

/// The Rust target triple this running binary corresponds to, used to select the
/// matching release asset. `None` on a platform we don't publish prebuilts for
/// (the caller falls back to install-method advice). Mirrors the `uname`→triple
/// mapping in `packaging/install.sh` and the release workflow's build matrix.
pub fn current_target_triple() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        _ => return None,
    })
}

/// Download the prebuilt release tarball for `tag`/`target`, verify it against
/// the published `.sha256` sidecar, and return the extracted `haven` binary
/// bytes. Network and checksum failures are hard errors — we never hand back an
/// unverified binary to swap. `version` is `tag` with any leading `v` stripped
/// (the asset filename embeds it). Synchronous wrapper over a one-shot runtime,
/// matching `latest_release_version`.
pub fn fetch_release_binary(tag: &str, version: &str, target: &str) -> Result<Vec<u8>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(HavenError::Io)?;
    rt.block_on(fetch_release_binary_async(tag, version, target))
}

async fn fetch_release_binary_async(tag: &str, version: &str, target: &str) -> Result<Vec<u8>> {
    let asset = format!("haven-{version}-{target}.tar.gz");
    let url = format!("https://github.com/nibbletech-labs/haven/releases/download/{tag}/{asset}");
    // Generous timeout: this pulls a ~10–20 MB tarball, not a JSON blob.
    let client = reqwest::Client::builder()
        .user_agent(concat!("haven/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| HavenError::Invalid(format!("http client: {e}")))?;

    // Checksum sidecar is "<hex>  <filename>" (shasum -c form); take the hash.
    let sha_text = http_get_text(&client, &format!("{url}.sha256"))
        .await
        .map_err(|e| HavenError::Invalid(format!("fetch checksum for {asset}: {e}")))?;
    let expected = sha_text
        .split_whitespace()
        .next()
        .ok_or_else(|| HavenError::Invalid(format!("empty checksum for {asset}")))?
        .to_ascii_lowercase();

    let tarball = http_get_bytes(&client, &url)
        .await
        .map_err(|e| HavenError::Invalid(format!("download {asset}: {e}")))?;

    let actual = sha256_hex(&tarball);
    if actual != expected {
        return Err(HavenError::Invalid(format!(
            "checksum mismatch for {asset}: expected {expected}, got {actual}"
        )));
    }

    extract_haven_binary(&tarball)
        .ok_or_else(|| HavenError::Invalid(format!("no `haven` binary inside {asset}")))
}

async fn http_get_text(client: &reqwest::Client, url: &str) -> std::result::Result<String, String> {
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.text().await.map_err(|e| e.to_string())
}

async fn http_get_bytes(
    client: &reqwest::Client,
    url: &str,
) -> std::result::Result<Vec<u8>, String> {
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    Ok(resp.bytes().await.map_err(|e| e.to_string())?.to_vec())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Pull the top-level regular file `haven` out of a gzip-compressed tar archive,
/// returning its bytes. `None` if the archive is unreadable or has no such file.
/// Matches only the root entry (`haven` or `./haven`, as our `tar -C dir .`
/// tarballs store it) — never a nested `sub/haven` — so a crafted archive can't
/// smuggle the binary out of a decoy path.
fn extract_haven_binary(tar_gz: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let dec = flate2::read::GzDecoder::new(std::io::Cursor::new(tar_gz));
    let mut archive = tar::Archive::new(dec);
    for entry in archive.entries().ok()? {
        let mut entry = entry.ok()?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let is_haven = match entry.path() {
            Ok(p) => {
                let rel = p.strip_prefix("./").unwrap_or(&p);
                rel == Path::new("haven")
            }
            Err(_) => false,
        };
        if is_haven {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).ok()?;
            return Some(buf);
        }
    }
    None
}

#[cfg(test)]
mod update_tests {
    use super::*;

    #[test]
    fn is_newer_is_semver_aware() {
        assert_eq!(is_newer("0.1.0", "0.1.1"), Some(true));
        assert_eq!(is_newer("0.1.0", "v0.1.1"), Some(true)); // leading v tolerated
        assert_eq!(is_newer("0.1.1", "0.1.1"), Some(false)); // equal
        assert_eq!(is_newer("0.2.0", "0.1.9"), Some(false)); // ahead of release
        assert_eq!(is_newer("0.1.9", "0.1.10"), Some(true)); // numeric, not lexical
                                                             // prerelease precedence: rc sorts before its release, after the prior one.
        assert_eq!(is_newer("0.1.0", "0.1.1-rc.1"), Some(true));
        assert_eq!(is_newer("0.1.1", "0.1.1-rc.1"), Some(false));
        // unparseable → None, so callers never claim a bogus update.
        assert_eq!(is_newer("0.1.0", "not-a-version"), None);
    }

    #[test]
    fn sha256_hex_matches_known_vectors() {
        // `printf '' | shasum -a 256` and `printf 'abc' | shasum -a 256`.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut out, flate2::Compression::fast());
            let mut builder = tar::Builder::new(enc);
            for (name, data) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder.append_data(&mut header, name, *data).unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap();
        }
        out
    }

    #[test]
    fn extract_haven_binary_finds_the_binary_past_decoys() {
        let payload = b"\x7fELF not-really-but-close";
        let archive = tar_gz(&[("README.md", b"readme"), ("haven", payload)]);
        assert_eq!(
            extract_haven_binary(&archive).as_deref(),
            Some(&payload[..])
        );
    }

    #[test]
    fn extract_haven_binary_handles_dot_slash_prefix() {
        // Real tarballs are built with `tar -C stage .`, so the entry is ./haven.
        let payload = b"binary-bytes";
        let archive = tar_gz(&[("./haven", payload)]);
        assert_eq!(
            extract_haven_binary(&archive).as_deref(),
            Some(&payload[..])
        );
    }

    #[test]
    fn extract_haven_binary_ignores_nested_haven() {
        // A nested `decoy/haven` shares the file_name "haven" but is not the
        // top-level entry — it must not be extracted (defense in depth; the real
        // gate is the upstream sha256 check).
        let archive = tar_gz(&[("decoy/haven", b"evil"), ("README.md", b"r")]);
        assert!(extract_haven_binary(&archive).is_none());
    }

    #[test]
    fn extract_haven_binary_absent_is_none() {
        let archive = tar_gz(&[("LICENSE", b"mit"), ("README.md", b"readme")]);
        assert!(extract_haven_binary(&archive).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_executable_swaps_content_and_sets_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("haven");
        std::fs::write(&dest, b"old-binary").unwrap(); // simulate an in-place swap
        atomic_write_executable(&dest, b"new-binary").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new-binary");
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn under_known_copy_dir_accepts_symlinked_haven_bin_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let real_bin = tmp.path().join("real/bin");
        std::fs::create_dir_all(&real_bin).unwrap();
        let exe = real_bin.join("haven");
        std::fs::write(&exe, b"binary").unwrap();

        let linked_bin = tmp.path().join("linked-bin");
        std::os::unix::fs::symlink(&real_bin, &linked_bin).unwrap();

        let canonical_exe = std::fs::canonicalize(&exe).unwrap();
        assert!(under_known_copy_dir_with_env(
            &canonical_exe,
            Some(&linked_bin)
        ));
        assert!(!under_known_copy_dir_with_env(
            &canonical_exe,
            Some(&tmp.path().join("missing-bin"))
        ));
    }
}
