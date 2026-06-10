//! Path resolution for the `~/.haven` root and the SQLite database.
//!
//! The root is `$HAVEN_HOME` when set (used by tests and headless contexts),
//! else `~/.haven`. The DB lives at `<root>/haven.db`. The content tree under
//! `<root>/<project>/...` is the Layer 4 concern; here we just resolve paths and
//! open the store.

use std::path::PathBuf;

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

/// Auth0 tenant config for the CLI app (SPEC §6).
pub fn auth_config(store: &Store) -> Result<AuthConfig> {
    Ok(AuthConfig::new(
        setting(store, "auth0_domain", "HAVEN_AUTH0_DOMAIN")?,
        setting(store, "auth0_client_id", "HAVEN_AUTH0_CLIENT_ID")?,
        setting(store, "auth0_audience", "HAVEN_AUTH0_AUDIENCE")?,
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

/// The `haven` Claude skill, embedded in the binary so it versions with the
/// CLI/MCP surface it documents (ARCHITECTURE §14.3) — no runtime dependency on
/// the repo. `(relative path under the skill dir, contents)`.
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
];

/// Install the embedded skill snapshot to `<claude>/skills/haven/`. Idempotent —
/// overwrites, since it's a versioned snapshot, not user-editable state. Returns
/// the installed skill directory.
pub fn ensure_skill_installed() -> Result<PathBuf> {
    let skill_dir = claude_dir()?.join("skills").join("haven");
    for (rel, contents) in SKILL_FILES {
        let dest = skill_dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, contents)?;
    }
    Ok(skill_dir)
}

/// What `haven setup` is responsible for wiring on a machine: the MCP server
/// registration in the Claude config, and the embedded skill snapshot on disk.
/// `haven doctor` reads this to tell a botched install from a healthy one.
pub struct InstallCheck {
    pub mcp_config_path: PathBuf,
    pub mcp_registered: bool,
    pub skill_dir: PathBuf,
    /// Every embedded skill file is present on disk.
    pub skill_present: bool,
    /// …and its bytes match the snapshot baked into this binary (no drift).
    pub skill_current: bool,
    /// Embedded skill files absent from disk (subset that fails `skill_present`).
    pub missing_skill_files: Vec<String>,
    /// `haven` resolved on `$PATH` — what the MCP `command: "haven"` stanza needs
    /// to actually launch. `None` if the binary isn't reachable by that name.
    pub haven_on_path: Option<PathBuf>,
}

/// Inspect the install wiring without mutating anything (the read side of
/// `ensure_mcp_wiring` / `ensure_skill_installed`).
pub fn install_check() -> Result<InstallCheck> {
    let mcp_config_path = claude_mcp_config_path()?;
    let mcp_registered = std::fs::read_to_string(&mcp_config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("mcpServers")
                .and_then(|m| m.get("haven"))
                .map(|h| h.is_object())
        })
        .unwrap_or(false);

    let skill_dir = claude_dir()?.join("skills").join("haven");
    let mut missing_skill_files = Vec::new();
    let mut skill_current = true;
    for (rel, contents) in SKILL_FILES {
        match std::fs::read_to_string(skill_dir.join(rel)) {
            Ok(on_disk) if on_disk == *contents => {}
            Ok(_) => skill_current = false, // present but drifted from the snapshot
            Err(_) => {
                missing_skill_files.push((*rel).to_string());
                skill_current = false;
            }
        }
    }

    Ok(InstallCheck {
        mcp_config_path,
        mcp_registered,
        skill_dir,
        skill_present: missing_skill_files.is_empty(),
        skill_current,
        missing_skill_files,
        haven_on_path: haven_on_path(),
    })
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
