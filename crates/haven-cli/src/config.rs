//! Path resolution for the `~/.haven` root and the SQLite database.
//!
//! The root is `$HAVEN_HOME` when set (used by tests and headless contexts),
//! else `~/.haven`. The DB lives at `<root>/haven.db`. The content tree under
//! `<root>/<project>/...` is the Layer 4 concern; here we just resolve paths and
//! open the store.

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

/// The `haven` skill, embedded in the binary so it versions with the CLI/MCP
/// surface it documents (ARCHITECTURE §14.3) — no runtime dependency on the repo.
/// `(relative path under the skill dir, contents)`.
const SKILL_FILES: &[(&str, &str)] = &[
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
        "agents/openai.yaml",
        include_str!("../../../skill/haven/agents/openai.yaml"),
    ),
];

pub const SKILL_FILE_LIST: &[&str] = &[
    "SKILL.md",
    "references/workflows.md",
    "references/surface-map.md",
    "agents/openai.yaml",
];

/// Install the embedded skill snapshot to `<claude>/skills/haven/`. Idempotent —
/// overwrites, since it's a versioned snapshot, not user-editable state. Returns
/// the installed skill directory.
pub fn ensure_skill_installed() -> Result<PathBuf> {
    let skill_dir = claude_dir()?.join("skills").join("haven");
    write_skill_snapshot(&skill_dir)?;
    Ok(skill_dir)
}

pub fn ensure_codex_skill_installed() -> Result<PathBuf> {
    let skill_dir = agents_dir()?.join("skills").join("haven");
    write_skill_snapshot(&skill_dir)?;
    Ok(skill_dir)
}

fn write_skill_snapshot(skill_dir: &Path) -> Result<()> {
    for (rel, contents) in SKILL_FILES {
        let dest = skill_dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, contents)?;
    }
    Ok(())
}

/// What `haven setup` is responsible for wiring on a machine: the MCP server
/// registration in the Claude config, and the embedded skill snapshot on disk.
/// `haven doctor` reads this to tell a botched install from a healthy one.
pub struct InstallCheck {
    pub claude_mcp_config_path: PathBuf,
    pub claude_mcp_registered: bool,
    pub claude_skill_dir: PathBuf,
    /// Every embedded skill file is present on disk.
    pub claude_skill_present: bool,
    /// …and its bytes match the snapshot baked into this binary (no drift).
    pub claude_skill_current: bool,
    /// Embedded skill files absent from disk (subset that fails `skill_present`).
    pub claude_missing_skill_files: Vec<String>,
    pub codex_mcp_config_path: PathBuf,
    pub codex_mcp_registered: bool,
    pub codex_skill_dir: PathBuf,
    pub codex_skill_present: bool,
    pub codex_skill_current: bool,
    pub codex_missing_skill_files: Vec<String>,
    pub agents_md_path: PathBuf,
    pub agents_md_present: bool,
    pub agents_md_current: bool,
    /// `haven` resolved on `$PATH` — what the MCP `command: "haven"` stanza needs
    /// to actually launch. `None` if the binary isn't reachable by that name.
    pub haven_on_path: Option<PathBuf>,
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

    let claude_skill_dir = claude_dir()?.join("skills").join("haven");
    let (claude_missing_skill_files, claude_skill_current) =
        skill_snapshot_check(&claude_skill_dir);
    let codex_mcp_config_path = codex_mcp_config_path()?;
    let codex_mcp_registered = std::fs::read_to_string(&codex_mcp_config_path)
        .map(|s| codex_config_has_haven(&s))
        .unwrap_or(false);
    let codex_skill_dir = agents_dir()?.join("skills").join("haven");
    let (codex_missing_skill_files, codex_skill_current) = skill_snapshot_check(&codex_skill_dir);
    let agents_md_path = std::env::current_dir()?.join("AGENTS.md");
    let agents_md = std::fs::read_to_string(&agents_md_path).ok();
    let agents_md_current = agents_md
        .as_deref()
        .map(agents_md_has_current_stanza)
        .unwrap_or(false);

    Ok(InstallCheck {
        claude_mcp_config_path,
        claude_mcp_registered,
        claude_skill_dir,
        claude_skill_present: claude_missing_skill_files.is_empty(),
        claude_skill_current,
        claude_missing_skill_files,
        codex_mcp_config_path,
        codex_mcp_registered,
        codex_skill_dir,
        codex_skill_present: codex_missing_skill_files.is_empty(),
        codex_skill_current,
        codex_missing_skill_files,
        agents_md_path,
        agents_md_present: agents_md.is_some(),
        agents_md_current,
        haven_on_path: haven_on_path(),
    })
}

fn skill_snapshot_check(skill_dir: &Path) -> (Vec<String>, bool) {
    let mut missing_skill_files = Vec::new();
    let mut skill_current = true;
    for (rel, contents) in SKILL_FILES {
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

const AGENTS_BEGIN: &str = "<!-- HAVEN:BEGIN -->";
const AGENTS_END: &str = "<!-- HAVEN:END -->";
const AGENTS_STANZA: &str = r#"<!-- HAVEN:BEGIN -->
## Haven

Haven is the canonical project work graph. Use `haven` CLI commands locally, or
the `haven_*` MCP tools when available. Keep structure in Haven: do not hand-edit
`backlog.md`; it is a generated projection.

Discovery:
- Canonical graph/content lives under `~/.haven`.
- Repo-local `Haven/` is a disposable visible workspace/projection when present.
- Codex MCP config is `~/.codex/config.toml` or trusted `.codex/config.toml`:
  `[mcp_servers.haven]` with `command = "haven"` and `args = ["mcp"]`.
- Codex/Open Agent Skills are read from `.agents/skills`, `~/.agents/skills`, or
  `/etc/codex/skills`; Claude skills live under `~/.claude/skills`.

Core local verbs:
- `haven project list` / `haven project use <key>` to select a backlog.
- `haven item get <ref> --include edges,artifacts,lineage` to inspect work.
- `haven item add "<title>" --if-absent` to capture without duplicating.
- `haven next --explain` to diagnose an empty dispatch queue.
- `haven item complete <ref> --evidence "<proof>"` to finish with evidence.
<!-- HAVEN:END -->
"#;

pub fn ensure_agents_md() -> Result<PathBuf> {
    let path = std::env::current_dir()?.join("AGENTS.md");
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = upsert_marked_block(&raw, AGENTS_BEGIN, AGENTS_END, AGENTS_STANZA);
    std::fs::write(&path, updated)?;
    Ok(path)
}

fn agents_md_has_current_stanza(raw: &str) -> bool {
    raw.contains(AGENTS_STANZA.trim())
}

fn upsert_marked_block(raw: &str, begin: &str, end: &str, block: &str) -> String {
    if let Some(start) = raw.find(begin) {
        if let Some(end_rel) = raw[start..].find(end) {
            let end_idx = start + end_rel + end.len();
            let mut out = String::new();
            out.push_str(raw[..start].trim_end());
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(block.trim());
            let suffix = raw[end_idx..].trim_start();
            if !suffix.is_empty() {
                out.push_str("\n\n");
                out.push_str(suffix);
            }
            out.push('\n');
            return out;
        }
    }
    let mut out = raw.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(block.trim());
    out.push('\n');
    out
}

pub struct LinkResult {
    pub workspace: PathBuf,
    pub backlog: PathBuf,
    pub canonical_backlog: PathBuf,
    pub git_exclude: Option<PathBuf>,
}

pub fn link_workspace(store: &Store, project: Option<&str>, name: &Path) -> Result<LinkResult> {
    let canonical_backlog = store.render(project)?;
    let workspace = std::env::current_dir()?.join(name);
    std::fs::create_dir_all(workspace.join("docs"))?;
    std::fs::write(
        workspace.join("README.md"),
        "Haven workspace projection. Canonical graph/content lives under ~/.haven.\n",
    )?;
    let backlog = workspace.join("backlog.md");
    replace_backlog_alias(&canonical_backlog, &backlog)?;
    let git_exclude = exclude_workspace_from_git(&workspace)?;
    Ok(LinkResult {
        workspace,
        backlog,
        canonical_backlog,
        git_exclude,
    })
}

fn replace_backlog_alias(canonical: &Path, link: &Path) -> Result<()> {
    if link.exists() || std::fs::symlink_metadata(link).is_ok() {
        let meta = std::fs::symlink_metadata(link)?;
        if meta.is_dir() && !meta.file_type().is_symlink() {
            return Err(HavenError::Invalid(format!(
                "{} is a directory; cannot replace with backlog projection",
                link.display()
            )));
        }
        std::fs::remove_file(link)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(canonical, link)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(canonical, link)?;
    }
    Ok(())
}

fn exclude_workspace_from_git(workspace: &Path) -> Result<Option<PathBuf>> {
    let Some(git_dir) = find_git_dir(std::env::current_dir()?) else {
        return Ok(None);
    };
    let exclude = git_dir.join("info").join("exclude");
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut raw = std::fs::read_to_string(&exclude).unwrap_or_default();
    let name = workspace
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("Haven");
    let entry = format!("/{name}/");
    if !raw.lines().any(|line| line.trim() == entry) {
        if !raw.ends_with('\n') && !raw.is_empty() {
            raw.push('\n');
        }
        raw.push_str(&entry);
        raw.push('\n');
        std::fs::write(&exclude, raw)?;
    }
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
