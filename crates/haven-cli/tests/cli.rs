//! End-to-end CLI tests driving the real `haven` binary against a temp
//! `HAVEN_HOME`, asserting on JSON stdout and the error envelope / exit codes.

use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// A test harness binding the binary to an isolated HAVEN_HOME.
struct Haven {
    _home: TempDir,
    home: std::path::PathBuf,
}

impl Haven {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        let path = home.path().join("haven");
        std::fs::create_dir_all(&path).unwrap();
        Haven {
            home: path,
            _home: home,
        }
    }

    /// Run `haven <args>`, expect success, parse stdout as JSON.
    fn json(&self, args: &[&str]) -> Value {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "command {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "bad json from {args:?}: {e}\n{}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }

    fn json_in_dir(&self, args: &[&str], dir: &std::path::Path) -> Value {
        let out = self.cmd(args).current_dir(dir).output().unwrap();
        assert!(
            out.status.success(),
            "command {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "bad json from {args:?}: {e}\n{}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }

    /// Run `haven <args>`, expect failure, parse the error envelope from stderr.
    /// The per-call telemetry line (HV-166) also rides stderr, so strip it before
    /// parsing the pretty-printed envelope — exactly what a real stderr consumer
    /// of the envelope does.
    fn fail(&self, args: &[&str]) -> Value {
        let out = self.cmd(args).output().unwrap();
        assert!(
            !out.status.success(),
            "command {args:?} unexpectedly succeeded"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        let envelope: String = stderr
            .lines()
            .filter(|l| !l.trim_start().starts_with("haven-telemetry "))
            .collect::<Vec<_>>()
            .join("\n");
        serde_json::from_str(&envelope)
            .unwrap_or_else(|e| panic!("bad error envelope from {args:?}: {e}\n{stderr}"))
    }

    fn ok(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "command {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Run `haven <args>`, expect success, return raw stdout (not JSON) — for
    /// commands that emit a plain text block, e.g. `haven prime` (HV-23).
    fn text(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "command {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Run `haven <args>` and return (stdout, stderr) regardless of exit status —
    /// used to assert the per-call telemetry line on stderr (HV-166).
    fn run_capturing(&self, args: &[&str]) -> (String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::cargo_bin("haven").unwrap();
        c.env("HAVEN_HOME", &self.home);
        // Keep `setup`'s MCP-config write inside the temp tree, never ~/.claude.
        c.env("HAVEN_CLAUDE_DIR", self.home.join(".claude"));
        c.env("HAVEN_CODEX_DIR", self.home.join(".codex"));
        c.env("HAVEN_AGENTS_DIR", self.home.join(".agents"));
        c.env("HAVEN_CLOUD_SYNC_PREVIEW", "0");
        c.current_dir(&self.home);
        c.args(args);
        c
    }
}

fn status_of(report: &Value, name: &str) -> String {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no `{name}` check in {report}"))["status"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn cli_priority_and_rank_rationale_round_trip_in_lineage() {
    let h = Haven::new();
    h.ok(&[
        "project", "add", "--key", "haven", "--title", "Haven", "--prefix", "HV",
    ]);
    h.ok(&["project", "use", "haven"]);
    h.ok(&[
        "item",
        "add",
        "First",
        "--status",
        "ready",
        "--done-looks-like",
        "done",
        "--commit",
        "--priority",
        "2",
    ]);
    h.ok(&[
        "item",
        "add",
        "Second",
        "--status",
        "ready",
        "--done-looks-like",
        "done",
        "--commit",
        "--priority",
        "2",
    ]);

    h.ok(&[
        "item",
        "update",
        "HV-1",
        "--priority",
        "1",
        "--rationale",
        "Needed for the release",
    ]);
    h.ok(&[
        "item",
        "rank",
        "HV-2",
        "--before",
        "HV-1",
        "--rationale",
        "Second should lead the band",
    ]);

    let first = h.json(&["item", "get", "HV-1", "--include", "lineage"]);
    assert_eq!(first["lineage"][0]["event_type"], "update");
    assert_eq!(first["lineage"][0]["rationale"], "Needed for the release");
    assert_eq!(
        first["lineage"][0]["context"]["operation"],
        "priority_update"
    );
    assert_eq!(first["lineage"][0]["context"]["old_priority"], 2);
    assert_eq!(first["lineage"][0]["context"]["new_priority"], 1);

    let second = h.json(&["item", "get", "HV-2", "--include", "lineage"]);
    assert_eq!(second["lineage"][0]["event_type"], "update");
    assert_eq!(
        second["lineage"][0]["rationale"],
        "Second should lead the band"
    );
    assert_eq!(second["lineage"][0]["context"]["operation"], "rank");
    assert_eq!(second["lineage"][0]["context"]["target"], "HV-1");

    h.ok(&["item", "add", "Third"]);
    h.ok(&[
        "item",
        "commit",
        "HV-3",
        "--priority",
        "1",
        "--rationale",
        "Bring into scope",
    ]);
    h.ok(&[
        "item",
        "uncommit",
        "HV-3",
        "--rationale",
        "Park until the blocker clears",
    ]);
    let third = h.json(&["item", "get", "HV-3", "--include", "lineage"]);
    assert_eq!(third["lineage"].as_array().unwrap().len(), 2);
    assert_eq!(third["lineage"][0]["context"]["operation"], "commit");
    assert_eq!(third["lineage"][0]["rationale"], "Bring into scope");
    assert_eq!(third["lineage"][0]["context"]["old_committed"], false);
    assert_eq!(third["lineage"][0]["context"]["new_priority"], 1);
    assert_eq!(third["lineage"][1]["context"]["operation"], "uncommit");
    assert_eq!(
        third["lineage"][1]["rationale"],
        "Park until the blocker clears"
    );
    assert_eq!(third["lineage"][1]["context"]["old_priority"], 1);
}

#[test]
fn skill_install_and_setup_write_the_snapshot() {
    let h = Haven::new();
    let skill_dir = h.home.join(".claude/skills/haven");

    // Explicit install writes the embedded snapshot.
    let out = h.json(&["skill", "install"]);
    assert!(out["installed"]["claude_haven"]
        .as_str()
        .unwrap()
        .ends_with("skills/haven"));
    assert!(skill_dir.join("SKILL.md").exists());
    assert!(skill_dir.join("references/workflows.md").exists());
    assert!(skill_dir.join("references/surface-map.md").exists());
    assert!(skill_dir.join("references/spec-quality.md").exists());
    assert!(skill_dir.join("references/parallel-dev.md").exists());
    assert!(skill_dir.join("agents/openai.yaml").exists());
    // It's the real skill (frontmatter name), not an empty file.
    let body = std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap();
    assert!(body.contains("name: haven"));

    // The same install lays down every shipped skill, not just haven.
    assert!(out["installed"]["claude_orchestrate-plan"]
        .as_str()
        .unwrap()
        .ends_with("skills/orchestrate-plan"));
    let op_dir = h.home.join(".claude/skills/orchestrate-plan");
    assert!(op_dir.join("SKILL.md").exists());
    assert!(op_dir.join("references/decomposition.md").exists());
    assert!(op_dir.join("references/tick-ops.md").exists());
    assert!(op_dir.join("agents/openai.yaml").exists());
    assert!(std::fs::read_to_string(op_dir.join("SKILL.md"))
        .unwrap()
        .contains("name: orchestrate-plan"));

    // …and the third skill, create-context-pack.
    assert!(out["installed"]["claude_create-context-pack"]
        .as_str()
        .unwrap()
        .ends_with("skills/create-context-pack"));
    let ccp_dir = h.home.join(".claude/skills/create-context-pack");
    assert!(ccp_dir.join("SKILL.md").exists());
    assert!(ccp_dir.join("references/pack-template.md").exists());
    assert!(ccp_dir.join("references/verify-ops.md").exists());
    assert!(ccp_dir.join("agents/openai.yaml").exists());
    assert!(std::fs::read_to_string(ccp_dir.join("SKILL.md"))
        .unwrap()
        .contains("name: create-context-pack"));

    // …and the fourth skill, orchestrate-run.
    assert!(out["installed"]["claude_orchestrate-run"]
        .as_str()
        .unwrap()
        .ends_with("skills/orchestrate-run"));
    let orun_dir = h.home.join(".claude/skills/orchestrate-run");
    assert!(orun_dir.join("SKILL.md").exists());
    assert!(orun_dir.join("references/tick-ops.md").exists());
    assert!(orun_dir.join("references/worktree-merge.md").exists());
    assert!(orun_dir.join("references/dispatch-policy.md").exists());
    assert!(orun_dir.join("agents/openai.yaml").exists());
    assert!(std::fs::read_to_string(orun_dir.join("SKILL.md"))
        .unwrap()
        .contains("name: orchestrate-run"));

    // …and the fifth skill, verify-acceptance.
    assert!(out["installed"]["claude_verify-acceptance"]
        .as_str()
        .unwrap()
        .ends_with("skills/verify-acceptance"));
    let verify_dir = h.home.join(".claude/skills/verify-acceptance");
    assert!(verify_dir.join("SKILL.md").exists());
    assert!(verify_dir.join("references/verdict-contract.md").exists());
    assert!(verify_dir.join("references/verify-ops.md").exists());
    assert!(verify_dir.join("agents/openai.yaml").exists());
    assert!(std::fs::read_to_string(verify_dir.join("SKILL.md"))
        .unwrap()
        .contains("name: verify-acceptance"));

    let codex = h.json(&["skill", "install", "--agent", "codex"]);
    assert!(codex["installed"]["codex_haven"]
        .as_str()
        .unwrap()
        .ends_with("skills/haven"));
    assert!(codex["installed"]["codex_orchestrate-plan"]
        .as_str()
        .unwrap()
        .ends_with("skills/orchestrate-plan"));
    assert!(codex["installed"]["codex_create-context-pack"]
        .as_str()
        .unwrap()
        .ends_with("skills/create-context-pack"));
    assert!(codex["installed"]["codex_orchestrate-run"]
        .as_str()
        .unwrap()
        .ends_with("skills/orchestrate-run"));
    assert!(codex["installed"]["codex_verify-acceptance"]
        .as_str()
        .unwrap()
        .ends_with("skills/verify-acceptance"));
    assert!(h.home.join(".agents/skills/haven/SKILL.md").exists());
    assert!(h
        .home
        .join(".agents/skills/orchestrate-plan/SKILL.md")
        .exists());
    assert!(h
        .home
        .join(".agents/skills/create-context-pack/SKILL.md")
        .exists());
    assert!(h
        .home
        .join(".agents/skills/orchestrate-run/SKILL.md")
        .exists());
    assert!(h
        .home
        .join(".agents/skills/verify-acceptance/SKILL.md")
        .exists());
    // Skill *content* validity (description cap, frontmatter, name/composition)
    // and exhaustive coverage are checked by `every_shipped_skill_is_valid_and_covered`
    // (HV-204); this test only asserts the install mechanics land files correctly.

    // `setup` installs both default agent skills (alongside MCP wiring) — unless --no-skill.
    let fresh = Haven::new();
    let setup = fresh.json(&["setup"]);
    assert!(setup["skill"].as_str().unwrap().ends_with("skills/haven"));
    assert!(fresh.home.join(".claude/skills/haven/SKILL.md").exists());
    assert!(fresh.home.join(".agents/skills/haven/SKILL.md").exists());
    assert!(!fresh.home.join("AGENTS.md").exists());
    assert_eq!(setup["agents_md"], "skipped (--agents-md not requested)");
    // A plain `setup` (no --project-key) creates no project — a fresh install
    // starts with none; one is created when the user first names some work.
    assert_eq!(setup["current_project"], serde_json::Value::Null);
    assert_eq!(setup["project_created"], false);
    let codex_config = std::fs::read_to_string(fresh.home.join(".codex/config.toml")).unwrap();
    assert!(codex_config.contains("[mcp_servers.haven]"));
    assert!(codex_config.contains("command = \"haven\""));
    assert!(codex_config.contains("args = [\"mcp\"]"));

    let skipped = Haven::new();
    let out = skipped.json(&["setup", "--no-skill"]);
    assert_eq!(out["skill"], "skipped (--no-skill)");
    assert!(!skipped.home.join(".claude/skills/haven/SKILL.md").exists());
    assert!(!skipped.home.join(".agents/skills/haven/SKILL.md").exists());
}

#[test]
fn setup_and_doctor_are_stable_across_current_directories() {
    let h = Haven::new();
    let dir_a = h.home.join("dir-a");
    let dir_b = h.home.join("dir-b");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();

    h.json_in_dir(&["setup"], &dir_a);
    assert!(
        !dir_a.join("AGENTS.md").exists(),
        "plain setup must not write cwd-local AGENTS.md"
    );
    assert!(!dir_b.join("AGENTS.md").exists());

    let bin = assert_cmd::cargo::cargo_bin("haven");
    let bindir = bin.parent().unwrap();
    let out = h
        .cmd(&["doctor"])
        .current_dir(&dir_b)
        .env("PATH", bindir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doctor: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status_of(&doctor, "agents_md"), "skip");
    assert_eq!(
        doctor["ok"], true,
        "doctor should stay green from an unrelated cwd: {doctor}"
    );
}

#[test]
fn setup_agents_md_flag_writes_and_updates_repo_stanza() {
    let h = Haven::new();
    let repo = h.home.join("repo");
    let subdir = repo.join("nested");
    std::fs::create_dir_all(repo.join(".git/info")).unwrap();
    std::fs::create_dir_all(&subdir).unwrap();
    std::fs::write(
        repo.join("AGENTS.md"),
        "# Existing instructions\n\n<!-- HAVEN:BEGIN -->\nstale\n<!-- HAVEN:END -->\n\nKeep this line.\n",
    )
    .unwrap();

    let out = h.json_in_dir(&["setup", "--agents-md"], &subdir);
    let actual_agents_md = std::path::PathBuf::from(out["agents_md"].as_str().unwrap());
    assert_eq!(
        std::fs::canonicalize(actual_agents_md).unwrap(),
        std::fs::canonicalize(repo.join("AGENTS.md")).unwrap()
    );
    let raw = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
    assert!(raw.contains("# Existing instructions"));
    assert!(raw.contains("Core local verbs:"));
    assert!(raw.contains("Keep this line."));
    assert!(!raw.contains("\nstale\n"));

    let bin = assert_cmd::cargo::cargo_bin("haven");
    let bindir = bin.parent().unwrap();
    let out = h
        .cmd(&["doctor"])
        .current_dir(&subdir)
        .env("PATH", bindir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doctor: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status_of(&doctor, "agents_md"), "ok");
    assert_eq!(
        doctor["ok"], true,
        "doctor should pass in the linked repo: {doctor}"
    );
}

#[test]
fn cloud_sync_preview_is_hidden_by_default() {
    let h = Haven::new();
    let setup = h.json(&[
        "setup",
        "--no-skill",
        "--project-key",
        "haven",
        "--project-title",
        "Haven",
        "--prefix",
        "HV",
    ]);
    assert!(setup.get("note").is_none());

    h.json(&["item", "add", "First local item"]);

    let status = h.json(&["status"]);
    assert!(status.get("sync_pending").is_none());
    assert!(status.get("auth").is_none());

    let prime = h.text(&["prime"]);
    let first_line = prime.lines().next().unwrap_or_default();
    assert!(
        !first_line.contains("sync"),
        "prime should not surface Cloud Sync by default: {first_line}"
    );

    let doctor = h.json(&["doctor"]);
    let checks = doctor["checks"].as_array().unwrap();
    assert!(!checks.iter().any(|c| c["name"] == "auth"));
    assert!(!checks.iter().any(|c| c["name"] == "sync"));

    let help = h.text(&["--help"]);
    assert!(!help.contains("Auth0 sign-in"));
    assert!(!help.contains("Sync with the cloud"));

    let sync_err = h.fail(&["sync", "status"]);
    assert!(sync_err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("HAVEN_CLOUD_SYNC_PREVIEW=1"));
    let auth_err = h.fail(&["auth", "status"]);
    assert!(auth_err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("HAVEN_CLOUD_SYNC_PREVIEW=1"));
}

#[test]
fn cloud_sync_preview_flag_restores_sync_status() {
    let h = Haven::new();
    h.json(&[
        "setup",
        "--no-skill",
        "--project-key",
        "haven",
        "--project-title",
        "Haven",
        "--prefix",
        "HV",
    ]);
    h.json(&["item", "add", "First local item"]);

    let out = h
        .cmd(&["status"])
        .env("HAVEN_CLOUD_SYNC_PREVIEW", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(status["sync_pending"].as_i64().unwrap() > 0);
    assert_eq!(status["auth"], "not configured (Cloud Sync preview)");

    let out = h
        .cmd(&["sync", "status"])
        .env("HAVEN_CLOUD_SYNC_PREVIEW", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "sync status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sync: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(sync["sync_pending"].as_i64().unwrap() > 0);
    assert_eq!(sync["watch_supported"], false);
}

/// Fold a SKILL.md YAML frontmatter `description` (a `>-` block scalar) into the
/// single string that Codex / Open Agent Skills measure against the 1024-byte
/// cap: trimmed, non-empty continuation lines joined with single spaces.
fn skill_description(skill_md: &str) -> String {
    let mut lines = skill_md.lines();
    assert_eq!(
        lines.next(),
        Some("---"),
        "SKILL.md must open with frontmatter"
    );
    let mut parts: Vec<String> = Vec::new();
    let mut in_desc = false;
    for line in lines {
        if line == "---" {
            break;
        }
        if in_desc {
            if line.is_empty() || line.starts_with(' ') {
                parts.push(line.trim().to_string());
                continue;
            }
            in_desc = false;
        }
        if let Some(rest) = line.strip_prefix("description:") {
            let rest = rest.trim();
            if matches!(rest, "" | ">-" | ">" | "|" | "|-") {
                in_desc = true;
            } else {
                parts.push(rest.trim_matches(|c| c == '"' || c == '\'').to_string());
            }
        }
    }
    parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Frontmatter keys the Open Agent Skills spec permits (per skill-creator's
/// `quick_validate.py`); anything else fails validation.
const ALLOWED_SKILL_FRONTMATTER_KEYS: &[&str] = &[
    "name",
    "description",
    "license",
    "allowed-tools",
    "metadata",
    "compatibility",
];

/// Validate one shipped skill against the Open Agent Skills rule set (a faithful
/// port of skill-creator's `quick_validate.py`) plus a composition check that the
/// skill's identity is consistent across SKILL.md, the directory name, and
/// `agents/openai.yaml`. Returns `Err(reason)` on the first violation. HV-204.
fn validate_skill_dir(dir: &std::path::Path) -> Result<(), String> {
    let id = dir.file_name().unwrap().to_string_lossy().to_string();
    let content = std::fs::read_to_string(dir.join("SKILL.md"))
        .map_err(|_| format!("{id}: SKILL.md not found"))?;
    if !content.starts_with("---") {
        return Err(format!("{id}: no YAML frontmatter (must start with `---`)"));
    }
    // Frontmatter is the text between the first two `---` lines.
    let fm = content
        .strip_prefix("---\n")
        .and_then(|rest| rest.split("\n---").next())
        .ok_or_else(|| format!("{id}: invalid frontmatter format"))?;

    // Top-level keys are the column-0 `key:` lines; nested/indented keys (e.g.
    // under `metadata`) and block-scalar body lines are indented, so excluded.
    let keys: Vec<String> = fm
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with(char::is_whitespace))
        .filter_map(|l| l.split_once(':').map(|(k, _)| k.trim().to_string()))
        .collect();
    if let Some(bad) = keys
        .iter()
        .find(|k| !ALLOWED_SKILL_FRONTMATTER_KEYS.contains(&k.as_str()))
    {
        return Err(format!(
            "{id}: unexpected frontmatter key `{bad}` (allowed: {})",
            ALLOWED_SKILL_FRONTMATTER_KEYS.join(", ")
        ));
    }
    for required in ["name", "description"] {
        if !keys.iter().any(|k| k == required) {
            return Err(format!(
                "{id}: missing required `{required}` in frontmatter"
            ));
        }
    }

    // name: kebab-case, no edge/doubled hyphens, <= 64 chars, == directory name.
    let name = fm
        .lines()
        .find_map(|l| l.strip_prefix("name:"))
        .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        .unwrap_or_default();
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(format!(
            "{id}: name `{name}` must be kebab-case ([a-z0-9-])"
        ));
    }
    if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return Err(format!(
            "{id}: name `{name}` cannot start/end with `-` or contain `--`"
        ));
    }
    if name.chars().count() > 64 {
        return Err(format!(
            "{id}: name is {} chars (> 64 max)",
            name.chars().count()
        ));
    }
    if name != id {
        return Err(format!(
            "{id}: SKILL.md name `{name}` != directory name `{id}`"
        ));
    }

    // description: the folded value carries no angle brackets and stays <= 1024.
    let desc = skill_description(&content);
    if desc.contains('<') || desc.contains('>') {
        return Err(format!(
            "{id}: description must not contain angle brackets (< or >)"
        ));
    }
    if desc.len() > 1024 {
        return Err(format!(
            "{id}: description is {} bytes (> 1024 Open Agent Skills / Codex limit)",
            desc.len()
        ));
    }

    // Composition: the Codex manifest's identity must match the skill it adapts.
    let manifest = std::fs::read_to_string(dir.join("agents/openai.yaml"))
        .map_err(|_| format!("{id}: agents/openai.yaml not found"))?;
    for field in ["name", "skill"] {
        let val = manifest
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{field}:")))
            .map(|v| v.trim().to_string())
            .unwrap_or_default();
        if val != id {
            return Err(format!("{id}: openai.yaml {field} `{val}` != `{id}`"));
        }
    }
    Ok(())
}

/// Every skill must satisfy the Open Agent Skills rules so it loads in Codex as
/// well as Claude Code — and the coverage must be exhaustive: no skill folder
/// goes unvalidated, and the folder set exactly matches what the binary actually
/// ships (the embedded `SKILL_REGISTRY`). HV-204.
#[test]
fn every_shipped_skill_is_valid_and_covered() {
    use std::collections::BTreeSet;

    // Source of truth #1 — the skill/ folder. EVERY directory under it must be a
    // real skill (a missing SKILL.md fails loudly; nothing is silently skipped)
    // and must pass validation.
    let skill_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skill");
    let mut folder = BTreeSet::new();
    for entry in std::fs::read_dir(&skill_root).expect("skill/ dir is readable") {
        let path = entry.unwrap().path();
        if !path.is_dir() {
            continue; // stray files (e.g. .DS_Store) are not skills
        }
        let id = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            path.join("SKILL.md").exists(),
            "skill/{id}/ has no SKILL.md — every directory under skill/ must be a skill"
        );
        validate_skill_dir(&path).unwrap_or_else(|e| panic!("skill validation failed — {e}"));
        folder.insert(id);
    }
    assert!(
        folder.len() >= 5,
        "expected at least the five shipped skills, only found {folder:?}"
    );

    // Source of truth #2 — what the binary actually installs (the embedded
    // SKILL_REGISTRY). The two sets must agree exactly, so a skill can never be
    // added to the folder without shipping, nor ship without a validated folder.
    let h = Haven::new();
    h.json(&["skill", "install"]);
    let installed: BTreeSet<String> = std::fs::read_dir(h.home.join(".claude/skills"))
        .expect("installed skills dir is readable")
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        folder, installed,
        "skill/ folder and the embedded SKILL_REGISTRY disagree — a skill is in one but not the other"
    );
}

#[test]
fn skill_install_can_target_one_skill() {
    let h = Haven::new();
    let out = h.json(&["skill", "install", "--skill", "orchestrate-plan"]);
    assert!(out["installed"]["claude_orchestrate-plan"]
        .as_str()
        .unwrap()
        .ends_with("skills/orchestrate-plan"));
    // --skill scopes the install: the other skills are NOT touched.
    assert!(out["installed"].get("claude_haven").is_none());
    assert!(out["installed"].get("claude_create-context-pack").is_none());
    assert!(h
        .home
        .join(".claude/skills/orchestrate-plan/SKILL.md")
        .exists());
    assert!(!h.home.join(".claude/skills/haven").exists());
    assert!(!h.home.join(".claude/skills/create-context-pack").exists());

    // An unknown skill name is a clean error envelope, not a panic.
    let err = h.fail(&["skill", "install", "--skill", "nope"]);
    assert!(err.to_string().contains("unknown skill"));
}

#[test]
fn mcp_startup_refreshes_stale_skill_snapshot() {
    let h = Haven::new();
    // Claude-only install: the Codex skill dir must stay absent throughout.
    h.json(&["setup", "--agent", "claude"]);
    let skill_md = h.home.join(".claude/skills/haven/SKILL.md");
    let pristine = std::fs::read_to_string(&skill_md).unwrap();

    // Simulate drift (an old binary's snapshot, or a hand-edit).
    std::fs::write(&skill_md, "stale").unwrap();

    // `haven mcp` with immediate stdin EOF: serves nothing, but self-heals first.
    let out = h
        .cmd(&["mcp"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "mcp failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(std::fs::read_to_string(&skill_md).unwrap(), pristine);
    assert!(String::from_utf8_lossy(&out.stderr).contains("refreshed skill snapshot"));
    // Respects the single-agent setup: no Codex dir conjured into existence.
    assert!(!h.home.join(".agents/skills/haven").exists());
}

#[test]
fn doctor_reports_install_health() {
    let h = Haven::new();

    // Before setup the store still opens (migrations run, schema stamped), but the
    // MCP stanza and skill snapshot aren't wired — doctor flags exactly that.
    let before = h.json(&["doctor"]);
    assert_eq!(before["ok"], false);
    assert_eq!(status_of(&before, "database"), "ok");

    // Schema line: store's applied version vs what this binary supports (offline).
    assert_eq!(status_of(&before, "schema"), "ok");
    let schema_detail = before["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "schema")
        .unwrap()["detail"]
        .as_str()
        .unwrap();
    // Derive the expected version from the binary so this never drifts when a
    // migration is added (mirrors db.rs's "no hand-bumped constant" philosophy).
    let supported = haven_core::db::latest_schema_migration();
    assert!(
        schema_detail.contains(&format!("binary supports v{supported}")),
        "schema detail was: {schema_detail}"
    );
    assert_eq!(status_of(&before, "claude_mcp"), "warn");
    assert_eq!(status_of(&before, "claude_skill_haven"), "warn");
    assert_eq!(status_of(&before, "claude_skill_orchestrate-plan"), "warn");
    assert_eq!(
        status_of(&before, "claude_skill_create-context-pack"),
        "warn"
    );
    assert_eq!(status_of(&before, "claude_skill_orchestrate-run"), "warn");
    assert_eq!(status_of(&before, "claude_skill_verify-acceptance"), "warn");
    assert_eq!(status_of(&before, "codex_mcp"), "warn");
    assert_eq!(status_of(&before, "codex_skill_haven"), "warn");
    assert_eq!(status_of(&before, "codex_skill_orchestrate-plan"), "warn");
    assert_eq!(
        status_of(&before, "codex_skill_create-context-pack"),
        "warn"
    );
    assert_eq!(status_of(&before, "codex_skill_orchestrate-run"), "warn");
    assert_eq!(status_of(&before, "codex_skill_verify-acceptance"), "warn");
    assert_eq!(status_of(&before, "agents_md"), "skip");

    // After setup, MCP + skill are green. Put the built binary on $PATH so the
    // `path` check can resolve `haven` (it isn't there by default in the test env).
    h.ok(&["setup"]);
    let bin = assert_cmd::cargo::cargo_bin("haven");
    let bindir = bin.parent().unwrap();
    let out = Command::cargo_bin("haven")
        .unwrap()
        .env("HAVEN_HOME", &h.home)
        .env("HAVEN_CLAUDE_DIR", h.home.join(".claude"))
        .env("HAVEN_CODEX_DIR", h.home.join(".codex"))
        .env("HAVEN_AGENTS_DIR", h.home.join(".agents"))
        .current_dir(&h.home)
        .env("PATH", bindir)
        .arg("doctor")
        .output()
        .unwrap();
    let after: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status_of(&after, "claude_mcp"), "ok");
    assert_eq!(status_of(&after, "claude_skill_haven"), "ok");
    assert_eq!(status_of(&after, "claude_skill_orchestrate-plan"), "ok");
    assert_eq!(status_of(&after, "claude_skill_create-context-pack"), "ok");
    assert_eq!(status_of(&after, "claude_skill_orchestrate-run"), "ok");
    assert_eq!(status_of(&after, "claude_skill_verify-acceptance"), "ok");
    assert_eq!(status_of(&after, "codex_mcp"), "ok");
    assert_eq!(status_of(&after, "codex_skill_haven"), "ok");
    assert_eq!(status_of(&after, "codex_skill_orchestrate-plan"), "ok");
    assert_eq!(status_of(&after, "codex_skill_create-context-pack"), "ok");
    assert_eq!(status_of(&after, "codex_skill_orchestrate-run"), "ok");
    assert_eq!(status_of(&after, "codex_skill_verify-acceptance"), "ok");
    assert_eq!(status_of(&after, "agents_md"), "skip");
    assert_eq!(status_of(&after, "path"), "ok");
    assert_eq!(
        after["ok"], true,
        "after setup doctor should be green: {after}"
    );
}

#[test]
fn doctor_flags_context_pack_tombstone() {
    let h = Haven::new();
    let status_of = |report: &Value, name: &str| -> String {
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("no `{name}` check in {report}"))["status"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // Bootstrap + select a project, then build the HV-59 shape: a broad phase whose
    // context-pack.md is a MOVED tombstone, with a still-grouped member.
    h.ok(&["setup", "--project-key", "demo", "--prefix", "DM"]);
    h.json(&["item", "add", "broad phase", "--type", "phase"]); // DM-1
    h.json(&["item", "add", "member", "--group", "DM-1"]); // DM-2
    h.ok(&[
        "artifact",
        "add",
        "DM-1",
        "--role",
        "context-pack",
        "--content",
        "MOVED: pack now lives on the build batch. See HV-73.",
        "--name",
        "context-pack.md",
    ]);

    // doctor flags the tombstone (warn flips the global report not-ok too).
    let dirty = h.json(&["doctor"]);
    assert_eq!(
        status_of(&dirty, "context_pack_integrity"),
        "warn",
        "tombstone should be flagged: {dirty}"
    );

    // Removing the tombstone clears the check (one row → --name is unambiguous).
    h.ok(&["artifact", "rm", "DM-1", "--name", "context-pack.md"]);
    let clean = h.json(&["doctor"]);
    assert_eq!(
        status_of(&clean, "context_pack_integrity"),
        "ok",
        "clean store should pass: {clean}"
    );
}

#[test]
fn setup_creates_no_project_by_default_but_can_bootstrap_one() {
    // A plain `setup` wires up skills/MCP but creates NO project — a fresh install
    // starts with none. A project is created when the user (or their AI) first
    // names some work, or up front with `--project-key`.
    let h = Haven::new();
    let out = h.json(&["setup"]);
    assert_eq!(out["current_project"], serde_json::Value::Null);
    assert_eq!(out["project_created"], false);
    assert_eq!(
        h.json(&["project", "list"]).as_array().unwrap().len(),
        0,
        "plain setup must not create a default project"
    );
    // With no current project, item ops fail rather than filing into a default.
    h.fail(&["item", "add", "First item"]);

    // doctor is still green on a fresh, project-less install.
    let bin = assert_cmd::cargo::cargo_bin("haven");
    let bindir = bin.parent().unwrap();
    let out = h.cmd(&["doctor"]).env("PATH", bindir).output().unwrap();
    assert!(
        out.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doctor: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        doctor["ok"], true,
        "fresh setup should leave doctor green even with no project: {doctor}"
    );

    // `--project-key` bootstraps and selects a named project up front.
    let custom = Haven::new();
    let out = custom.json(&[
        "setup",
        "--project-key",
        "demo",
        "--project-title",
        "Demo",
        "--prefix",
        "DM",
    ]);
    assert_eq!(out["current_project"], "demo");
    assert_eq!(out["project_created"], true);
    let item = custom.json(&["item", "add", "First item"]);
    assert_eq!(item["ref"], "DM-1");
}

#[test]
fn docs_lists_anchor_artifacts_without_dispatching_them() {
    let h = Haven::new();
    h.ok(&[
        "setup",
        "--project-key",
        "haven",
        "--project-title",
        "Haven",
        "--prefix",
        "HV",
    ]);
    h.json(&[
        "item",
        "add",
        "Haven docs",
        "--type",
        "anchor",
        "--status",
        "ready",
        "--done-looks-like",
        "docs landed",
        "--commit",
        "--assign",
        "ai",
    ]);
    h.json(&[
        "artifact",
        "add",
        "HV-1",
        "--role",
        "vision",
        "--content",
        "Project vision",
    ]);

    let docs = h.json(&["docs"]);
    assert_eq!(docs.as_array().unwrap().len(), 1);
    assert_eq!(docs[0]["ref"], "HV-1");
    assert_eq!(docs[0]["type"], "anchor");
    assert_eq!(docs[0]["artifacts"][0]["role"], "vision");
    assert!(h
        .json(&["next", "--owner", "ai"])
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        h.fail(&["item", "archive", "HV-1", "--rationale", "not work"])["error"]["code"],
        "invalid"
    );
}

#[test]
fn link_creates_visible_workspace_projection_and_local_git_exclude() {
    let h = Haven::new();
    std::fs::create_dir_all(h.home.join(".git/info")).unwrap();
    h.ok(&[
        "setup",
        "--project-key",
        "demo",
        "--project-title",
        "Demo",
        "--prefix",
        "DM",
    ]);
    h.json(&[
        "item",
        "add",
        "Codex can read the projection",
        "--commit",
        "--status",
        "ready",
        "--done-looks-like",
        "visible in _haven/backlog.md",
    ]);
    h.json(&["item", "add", "Demo docs", "--type", "anchor"]);
    let src = h.home.join("doc.md");
    std::fs::write(&src, b"# Demo docs\n").unwrap();
    h.json(&[
        "artifact",
        "add",
        "DM-2",
        "--role",
        "vision",
        "--file",
        src.to_str().unwrap(),
    ]);

    let out = h.json(&["link"]);
    assert!(out["workspace"].as_str().unwrap().ends_with("/_haven"));
    assert!(out["items"].as_str().unwrap().ends_with("/_haven/items"));
    assert!(out["docs"].as_str().unwrap().ends_with("/_haven/docs"));
    assert!(h.home.join("_haven/README.md").exists());
    assert!(h.home.join("_haven/docs").is_dir());
    assert!(h.home.join("_haven/items/DM-2/doc.md").exists());
    assert!(h.home.join("_haven/docs/DM-2/doc.md").exists());
    let backlog = std::fs::read_to_string(h.home.join("_haven/backlog.md")).unwrap();
    assert!(backlog.contains("Codex can read the projection"));
    let exclude = std::fs::read_to_string(h.home.join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|line| line.trim() == "/_haven/"));
}

#[test]
fn link_upgrades_old_workspace_projection_and_is_idempotent() {
    let h = Haven::new();
    std::fs::create_dir_all(h.home.join(".git/info")).unwrap();
    h.ok(&[
        "setup",
        "--project-key",
        "demo",
        "--project-title",
        "Demo",
        "--prefix",
        "DM",
    ]);
    h.json(&["item", "add", "Demo docs", "--type", "anchor"]);
    let src = h.home.join("doc.md");
    std::fs::write(&src, b"# Demo docs\n").unwrap();
    h.json(&[
        "artifact",
        "add",
        "DM-1",
        "--role",
        "vision",
        "--file",
        src.to_str().unwrap(),
    ]);

    let workspace = h.home.join("_haven");
    std::fs::create_dir_all(workspace.join("docs")).unwrap();
    std::fs::write(
        workspace.join("README.md"),
        b"Haven workspace projection. Canonical graph/content lives under ~/.haven.\n",
    )
    .unwrap();
    std::fs::write(workspace.join("backlog.md"), b"old backlog\n").unwrap();
    std::fs::write(workspace.join("docs/stale.md"), b"stale\n").unwrap();

    h.json(&["link"]);
    h.json(&["link"]);

    assert!(h.home.join("_haven/items/DM-1/doc.md").exists());
    assert!(h.home.join("_haven/docs/DM-1/doc.md").exists());
    assert!(!h.home.join("_haven/docs/stale.md").exists());

    let exclude = std::fs::read_to_string(h.home.join(".git/info/exclude")).unwrap();
    assert_eq!(
        exclude
            .lines()
            .filter(|line| line.trim() == "/_haven/")
            .count(),
        1
    );
    assert_eq!(
        exclude
            .lines()
            .filter(|line| line.trim() == "/.haven-project")
            .count(),
        1
    );
}

#[test]
fn link_refuses_to_clobber_non_projection_workspace() {
    let h = Haven::new();
    h.ok(&[
        "setup",
        "--project-key",
        "demo",
        "--project-title",
        "Demo",
        "--prefix",
        "DM",
    ]);
    std::fs::create_dir_all(h.home.join("_haven/docs")).unwrap();
    std::fs::write(h.home.join("_haven/docs/keep.md"), b"do not delete\n").unwrap();

    let err = h.fail(&["link"]);
    assert_eq!(err["error"]["code"], "invalid");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("refusing to modify"));
    assert!(h.home.join("_haven/docs/keep.md").exists());
}

#[test]
fn unlink_removes_local_projection_without_touching_canonical_content() {
    let h = Haven::new();
    std::fs::create_dir_all(h.home.join(".git/info")).unwrap();
    h.ok(&[
        "setup",
        "--project-key",
        "demo",
        "--project-title",
        "Demo",
        "--prefix",
        "DM",
    ]);
    h.json(&["item", "add", "Demo docs", "--type", "anchor"]);
    h.json(&["link"]);

    let canonical_backlog = h.home.join("demo/backlog.md");
    assert!(canonical_backlog.exists());
    assert!(h.home.join("_haven").exists());
    assert!(h.home.join(".haven-project").exists());

    let out = h.json(&["unlink"]);
    assert_eq!(out["removed_workspace"], true);
    assert_eq!(out["removed_binding"], true);
    assert!(!h.home.join("_haven").exists());
    assert!(!h.home.join(".haven-project").exists());
    assert!(canonical_backlog.exists());

    let exclude = std::fs::read_to_string(h.home.join(".git/info/exclude")).unwrap();
    assert!(!exclude.lines().any(|line| line.trim() == "/_haven/"));
    assert!(!exclude.lines().any(|line| line.trim() == "/.haven-project"));

    let out = h.json(&["unlink"]);
    assert_eq!(out["removed_workspace"], false);
    assert_eq!(out["removed_binding"], false);
}

#[test]
fn unlink_discovers_custom_named_projection_without_name_arg() {
    let h = Haven::new();
    std::fs::create_dir_all(h.home.join(".git/info")).unwrap();
    h.ok(&[
        "setup",
        "--project-key",
        "demo",
        "--project-title",
        "Demo",
        "--prefix",
        "DM",
    ]);
    // Link into a non-default workspace name.
    let linked = h.json(&["link", "--name", "Workspace"]);
    assert!(linked["workspace"]
        .as_str()
        .unwrap()
        .ends_with("/Workspace"));
    assert!(h.home.join("Workspace").exists());
    let exclude = std::fs::read_to_string(h.home.join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|line| line.trim() == "/Workspace/"));

    // `unlink` with no --name still discovers it via the projection marker,
    // removes it, and clears *its* git-exclude entry (not a stale /_haven/).
    let out = h.json(&["unlink"]);
    assert_eq!(out["removed_workspace"], true);
    assert!(out["workspace"].as_str().unwrap().ends_with("/Workspace"));
    assert!(!h.home.join("Workspace").exists());
    let exclude = std::fs::read_to_string(h.home.join(".git/info/exclude")).unwrap();
    assert!(!exclude.lines().any(|line| line.trim() == "/Workspace/"));
    assert!(!exclude.lines().any(|line| line.trim() == "/.haven-project"));
}

#[test]
fn batch_commit_and_archive_take_multiple_refs() {
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "haven", "--prefix", "HV"]);
    h.json(&["item", "add", "A"]);
    h.json(&["item", "add", "B"]);
    h.json(&["item", "add", "C"]);

    // "commit these two" — one command, an array back.
    let committed = h.json(&["item", "commit", "HV-1", "HV-2", "--priority", "2"]);
    assert_eq!(committed.as_array().unwrap().len(), 2);

    // "archive those" — one command.
    let archived = h.json(&["item", "archive", "HV-1", "HV-3", "--rationale", "groomed"]);
    let refs: Vec<&str> = archived
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["ref"].as_str().unwrap())
        .collect();
    assert_eq!(refs, ["HV-1", "HV-3"]);
    assert!(archived
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["status"] == "archived"));
}

#[test]
fn item_list_limit_and_offset_slice() {
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "haven", "--prefix", "HV"]);
    h.json(&["item", "add", "A"]);
    h.json(&["item", "add", "B"]);
    h.json(&["item", "add", "C"]);

    let refs = |v: &Value| {
        v.as_array()
            .unwrap()
            .iter()
            .map(|i| i["ref"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };

    // No flags → all three (default is unbounded, unchanged).
    assert_eq!(h.json(&["item", "list"]).as_array().unwrap().len(), 3);
    // --limit bounds the result (parity with `next`).
    assert_eq!(
        refs(&h.json(&["item", "list", "--limit", "2"])),
        ["HV-1", "HV-2"]
    );
    // --offset paginates.
    assert_eq!(
        refs(&h.json(&["item", "list", "--limit", "2", "--offset", "2"])),
        ["HV-3"]
    );
}

#[test]
fn full_lifecycle() {
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "haven", "--prefix", "HV"]);

    // Add a committed, ready, AI-owned item and an uncommitted prereq.
    let item = h.json(&[
        "item",
        "add",
        "Draft the spec",
        "--status",
        "ready",
        "--done-looks-like",
        "spec drafted",
        "--commit",
        "--priority",
        "1",
        "--assign",
        "ai",
    ]);
    assert_eq!(item["ref"], "HV-1");
    assert_eq!(item["committed"], true);
    assert_eq!(item["owner_kind"], "ai");

    h.json(&["item", "add", "Set up CI"]); // HV-2

    // next returns only the dispatchable item.
    let next = h.json(&["next"]);
    assert_eq!(next.as_array().unwrap().len(), 1);
    assert_eq!(next[0]["ref"], "HV-1");

    let explain = h.json(&["next", "--explain", "--owner", "human"]);
    assert_eq!(explain["dispatchable"], 0);
    assert_eq!(explain["counts"]["owner_mismatch"], 1);

    // dispatch is the richer, still-bounded "what should I work on?" read.
    h.json(&["item", "add", "App 2.0", "--type", "phase"]); // HV-3
    h.json(&[
        "item",
        "add",
        "Quick-log",
        "--status",
        "ready",
        "--done-looks-like",
        "quick-log works",
        "--commit",
        "--assign",
        "ai",
    ]); // HV-4
    h.json(&[
        "item",
        "add",
        "Outside scope",
        "--status",
        "ready",
        "--done-looks-like",
        "outside works",
        "--commit",
        "--assign",
        "ai",
    ]); // HV-5
    h.ok(&["decompose", "HV-3", "--into", "HV-4"]);
    h.json(&[
        "artifact",
        "add",
        "HV-4",
        "--role",
        "spec",
        "--content",
        "Quick-log spec",
    ]);
    let dispatch = h.json(&["dispatch", "--owner", "ai", "--scope", "HV-3", "--explain"]);
    assert_eq!(dispatch["project"], "haven");
    assert_eq!(dispatch["scope"]["ref"], "HV-3");
    assert_eq!(dispatch["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(dispatch["candidates"][0]["ref"], "HV-4");
    assert_eq!(dispatch["candidates"][0]["parents"][0]["ref"], "HV-3");
    assert_eq!(dispatch["candidates"][0]["artifacts"][0]["role"], "spec");
    assert_eq!(dispatch["recommendation"]["ref"], "HV-4");
    assert_eq!(dispatch["explain"]["scope"]["ref"], "HV-3");

    // Decomposition + dependency edges, read back via include.
    h.ok(&["item", "add", "Frontend"]); // HV-6
    h.ok(&["decompose", "HV-1", "--into", "HV-6"]);
    h.ok(&["depend", "HV-6", "--on", "HV-2"]);
    let full = h.json(&["item", "get", "HV-1", "--include", "edges"]);
    assert_eq!(full["edges"]["children"][0], "HV-6");
    let many = h.json(&["item", "get", "HV-2", "HV-1"]);
    let many = many.as_array().unwrap();
    assert_eq!(many[0]["ref"], "HV-2");
    assert_eq!(many[1]["ref"], "HV-1");

    // Evolve split supersedes the source and resolves forward.
    let split = h.json(&[
        "evolve",
        "split",
        "HV-2",
        "--into",
        "API",
        "--into",
        "DB",
        "--rationale",
        "too big",
    ]);
    assert_eq!(split["superseded"][0], "HV-2");
    assert_eq!(split["new"].as_array().unwrap().len(), 2);
    let superseded = h.json(&["item", "get", "HV-2"]);
    assert_eq!(superseded["status"], "superseded");

    // Search hits the new node.
    let hits = h.json(&["search", "API"]);
    assert_eq!(hits.as_array().unwrap().len(), 1);

    // Status reports counts.
    let status = h.json(&["status"]);
    assert!(status["total"].as_i64().unwrap() >= 4);
}

#[test]
fn error_envelope_and_exit_code() {
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "haven", "--prefix", "HV"]);

    let err = h.fail(&["item", "get", "HV-999"]);
    assert_eq!(err["error"]["code"], "not_found");

    h.json(&["item", "add", "A"]);
    h.json(&["item", "add", "B"]);
    h.ok(&["decompose", "HV-1", "--into", "HV-2"]);
    let cycle = h.fail(&["decompose", "HV-2", "--into", "HV-1"]);
    assert_eq!(cycle["error"]["code"], "graph_rule");
}

/// HV-24: `item claim` atomically flips a ready item to in_progress + ai in one
/// op; claiming an already-claimed item fails non-zero with a clear conflict.
#[test]
fn item_claim_takes_then_clashes() {
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "haven", "--prefix", "HV"]);

    let item = h.json(&[
        "item",
        "add",
        "Pick me up",
        "--status",
        "ready",
        "--done-looks-like",
        "done",
        "--commit",
    ]);
    assert_eq!(item["ref"], "HV-1");

    // First claim wins: one op sets owner=ai AND status=in_progress.
    let claimed = h.json(&["item", "claim", "HV-1"]);
    assert_eq!(claimed["status"], "in_progress");
    assert_eq!(claimed["owner_kind"], "ai");

    // Second claim is the clash: non-zero exit, conflict envelope.
    let clash = h.fail(&["item", "claim", "HV-1"]);
    assert_eq!(clash["error"]["code"], "conflict");
    assert!(
        clash["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already claimed"),
        "clash names the cause: {clash}"
    );

    // The held item is untouched by the losing claim.
    let after = h.json(&["item", "get", "HV-1"]);
    assert_eq!(after["status"], "in_progress");
    assert_eq!(after["owner_kind"], "ai");
}

#[test]
fn content_layer_artifact_note_render() {
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "haven", "--prefix", "HV"]);
    h.json(&[
        "item",
        "add",
        "Write spec",
        "--commit",
        "--status",
        "ready",
        "--done-looks-like",
        "spec written",
    ]);

    // Register a file artifact and read it back.
    let src = h.home.join("spec.md");
    std::fs::create_dir_all(&h.home).unwrap();
    std::fs::write(&src, b"# Spec\nbody\n").unwrap();
    let art = h.json(&[
        "artifact",
        "add",
        "HV-1",
        "--role",
        "spec",
        "--file",
        src.to_str().unwrap(),
    ]);
    assert_eq!(art["path"], "items/HV-1/spec.md");
    let got = h.json(&["artifact", "get", "HV-1", "--role", "spec"]);
    assert_eq!(got["content"], "# Spec\nbody\n");

    // A note appends without creating a DB row.
    h.json(&["note", "HV-1", "a scratch thought"]);
    assert!(
        h.json(&["artifact", "list", "HV-1"])
            .as_array()
            .unwrap()
            .len()
            == 1
    );

    // backlog.md is auto-rendered after mutations.
    let backlog = h.home.join("haven/backlog.md");
    let body = std::fs::read_to_string(&backlog).unwrap();
    assert!(body.contains("## Committed"));
    assert!(body.contains("HV-1"));
}

#[test]
fn no_project_selected_is_a_clean_error() {
    let h = Haven::new();
    h.ok(&["init"]);
    let err = h.fail(&["item", "add", "orphan"]);
    assert_eq!(err["error"]["code"], "invalid");
}

// ---- HV-17: idempotent capture + bulk import -------------------------------

#[test]
fn item_add_if_absent_round_trips() {
    let h = Haven::new();
    h.ok(&[
        "setup",
        "--no-skill",
        "--project-key",
        "demo",
        "--prefix",
        "DM",
    ]);

    let first = h.json(&["item", "add", "Setup CI"]);
    assert_eq!(first["ref"], "DM-1");
    // The clean-create response carries neither guard field.
    assert!(first.get("existing").is_none());
    assert!(first.get("similar").is_none());

    let second = h.json(&["item", "add", "  setup  ci.", "--if-absent"]);
    assert_eq!(second["existing"], true);
    assert_eq!(second["ref"], "DM-1");

    // A near-duplicate created without the guard carries a similar warning.
    let third = h.json(&["item", "add", "Setup CI runners"]);
    assert!(third["similar"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["ref"] == "DM-1"));
}

#[test]
fn import_creates_a_wired_batch_in_one_transaction() {
    let h = Haven::new();
    h.ok(&[
        "setup",
        "--no-skill",
        "--project-key",
        "demo",
        "--prefix",
        "DM",
    ]);

    let file = h.home.join("batch.json");
    std::fs::write(
        &file,
        serde_json::json!([
            // Born `ready` is fine WITH acceptance; engaged born-states
            // (in_progress/blocked/done, commit:true) are rejected (HV-159).
            {"id": "api", "title": "Build API", "parent": "epic", "status": "ready", "done_looks_like": "it works"},
            {"id": "ui", "title": "Build UI", "depends_on": ["api"]},
            {"id": "epic", "title": "Auth epic"}
        ])
        .to_string(),
    )
    .unwrap();

    let out = h.json(&["import", file.to_str().unwrap()]);
    let outcomes = out.as_array().unwrap();
    assert_eq!(outcomes.len(), 3);
    assert_eq!(outcomes[0]["id"], "api");
    assert_eq!(outcomes[0]["ref"], "DM-1");
    assert_eq!(outcomes[2]["ref"], "DM-3");

    // Edges round-trip through the graph read; the projection regenerated.
    let g = h.json(&["graph"]);
    let edges = g["edges"].as_array().unwrap();
    let has = |kind: &str, from: &str, to: &str| {
        edges
            .iter()
            .any(|e| e["kind"] == kind && e["from"] == from && e["to"] == to)
    };
    assert!(has("decomposition", "DM-3", "DM-1"));
    assert!(has("dependency", "DM-2", "DM-1"));
    assert!(h.home.join("demo/backlog.md").exists());

    // A failing batch (in-batch cycle) leaves the store untouched.
    std::fs::write(
        &file,
        serde_json::json!([
            {"id": "a", "title": "First", "depends_on": ["b"]},
            {"id": "b", "title": "Second", "depends_on": ["a"]}
        ])
        .to_string(),
    )
    .unwrap();
    let err = h.fail(&["import", file.to_str().unwrap()]);
    assert_eq!(err["error"]["code"], "graph_rule");
    assert_eq!(h.json(&["item", "list"]).as_array().unwrap().len(), 3);

    // Unreadable / invalid files produce the `invalid` envelope.
    let err = h.fail(&["import", "/nonexistent/batch.json"]);
    assert_eq!(err["error"]["code"], "invalid");
    std::fs::write(&file, "not json").unwrap();
    let err = h.fail(&["import", file.to_str().unwrap()]);
    assert_eq!(err["error"]["code"], "invalid");
}

// ---- self install / self update ------------------------------------------

#[cfg(unix)]
#[test]
fn self_install_link_creates_symlink_to_build() {
    let h = Haven::new();
    let dir = TempDir::new().unwrap();
    let dir_s = dir.path().to_str().unwrap();

    let out = h.json(&["self", "install", "--link", "--dir", dir_s]);
    assert_eq!(out["mode"], "link");

    let dest = dir.path().join("haven");
    let meta = std::fs::symlink_metadata(&dest).unwrap();
    assert!(
        meta.file_type().is_symlink(),
        "expected a symlink at {dest:?}"
    );

    // The link resolves to the very binary under test (canonicalize the footgun).
    let target = std::fs::canonicalize(&dest).unwrap();
    let bin = std::fs::canonicalize(assert_cmd::cargo::cargo_bin("haven")).unwrap();
    assert_eq!(target, bin);
}

#[cfg(unix)]
#[test]
fn self_install_link_is_idempotent_noop() {
    let h = Haven::new();
    let dir = TempDir::new().unwrap();
    let dir_s = dir.path().to_str().unwrap();

    h.json(&["self", "install", "--link", "--dir", dir_s]);
    // Re-running through the same dir already resolves to this build → no-op,
    // not a self-referential link (the canonicalize guard).
    let again = h.json(&["self", "install", "--link", "--dir", dir_s]);
    assert_eq!(again["noop"], true);
}

#[cfg(unix)]
#[test]
fn self_install_copy_writes_executable() {
    use std::os::unix::fs::PermissionsExt;
    let h = Haven::new();
    let dir = TempDir::new().unwrap();
    let dir_s = dir.path().to_str().unwrap();

    let out = h.json(&["self", "install", "--dir", dir_s]);
    assert_eq!(out["mode"], "copy");

    let dest = dir.path().join("haven");
    let meta = std::fs::symlink_metadata(&dest).unwrap();
    assert!(
        !meta.file_type().is_symlink(),
        "copy should be a regular file"
    );
    assert_eq!(meta.permissions().mode() & 0o777, 0o755);
}

#[test]
fn self_install_copy_needs_force_to_clobber() {
    let h = Haven::new();
    let dir = TempDir::new().unwrap();
    let dir_s = dir.path().to_str().unwrap();

    h.json(&["self", "install", "--dir", dir_s]);
    // A second copy over a different existing binary is refused without --force.
    let err = h.fail(&["self", "install", "--dir", dir_s]);
    assert_eq!(err["error"]["code"], "invalid");
    // --force overwrites.
    h.ok(&["self", "install", "--dir", dir_s, "--force"]);
}

#[cfg(unix)]
#[test]
fn self_install_non_writable_dir_errors() {
    use std::os::unix::fs::PermissionsExt;
    let h = Haven::new();
    let dir = TempDir::new().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let dir_s = dir.path().to_str().unwrap();

    let err = h.fail(&["self", "install", "--link", "--dir", dir_s]);
    assert_eq!(err["error"]["code"], "invalid");

    // Restore so the TempDir can clean itself up.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn self_update_check_is_offline_safe() {
    let h = Haven::new();
    // No store, possibly no network: must still succeed and report the running
    // version. `latest` may be null (offline / repo not published yet).
    let out = h.json(&["self", "update", "--check"]);
    assert_eq!(out["current"], env!("CARGO_PKG_VERSION"));
    assert!(out["method"].is_string());
}

#[cfg(unix)]
#[test]
fn self_update_check_detects_symlinked_haven_bin_dir_as_install_sh() {
    use std::os::unix::fs::PermissionsExt;

    let h = Haven::new();
    let dir = TempDir::new().unwrap();
    let real_bin = dir.path().join("real/bin");
    std::fs::create_dir_all(&real_bin).unwrap();

    let copied = real_bin.join("haven");
    std::fs::copy(assert_cmd::cargo::cargo_bin("haven"), &copied).unwrap();
    std::fs::set_permissions(&copied, std::fs::Permissions::from_mode(0o755)).unwrap();

    let linked_bin = dir.path().join("linked-bin");
    std::os::unix::fs::symlink(&real_bin, &linked_bin).unwrap();

    let out = Command::new(&copied)
        .env("HAVEN_HOME", &h.home)
        .env("HAVEN_CLAUDE_DIR", h.home.join(".claude"))
        .env("HAVEN_CODEX_DIR", h.home.join(".codex"))
        .env("HAVEN_AGENTS_DIR", h.home.join(".agents"))
        .env("HAVEN_BIN_DIR", &linked_bin)
        .env("HAVEN_CLOUD_SYNC_PREVIEW", "0")
        .current_dir(&h.home)
        .args(["self", "update", "--check"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "self update --check failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out["method"], "install.sh");
}

#[test]
fn backup_now_list_verify_and_restore_round_trip() {
    let h = Haven::new();
    h.ok(&[
        "setup",
        "--no-skill",
        "--project-key",
        "haven",
        "--prefix",
        "HV",
    ]);
    h.json(&["item", "add", "First item"]);

    // Take a snapshot now.
    let now = h.json(&["backup", "now"]);
    assert_eq!(now["integrity"], "ok");
    let id = now["id"].as_str().unwrap().to_string();
    assert!(now["projects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["key"] == "haven"));

    // List shows it, newest first, not frozen.
    let list = h.json(&["backup", "list"]);
    assert_eq!(list["frozen"], false);
    let backups = list["backups"].as_array().unwrap();
    assert_eq!(backups[0]["id"], id.as_str());
    assert_eq!(backups[0]["integrity"], "ok");

    // Verify the latest, and by id.
    assert_eq!(h.json(&["backup", "verify"])["integrity"], "ok");
    assert_eq!(h.json(&["backup", "verify", &id])["integrity"], "ok");

    // Restore is gated behind --yes.
    assert_eq!(
        h.fail(&["backup", "restore", &id])["error"]["code"],
        "invalid"
    );

    // Restore round-trips; the graph still has the item afterwards.
    let restore = h.json(&["backup", "restore", &id, "--yes"]);
    assert_eq!(restore["restored"], id.as_str());
    assert!(!restore["safety_snapshot"].as_str().unwrap().is_empty());

    let graph = h.json(&["graph"]);
    assert!(graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["title"] == "First item"));

    // status surfaces backup state.
    let status = h.json(&["status"]);
    assert!(status["backups"]["count"].as_u64().unwrap() >= 1);
    assert_eq!(status["backups"]["frozen"], false);
}

#[test]
fn read_only_command_triggers_daily_backup_once(/* HV-89 */) {
    let h = Haven::new();

    // Fresh home, no backup taken yet today.
    assert!(h.json(&["backup", "list"])["backups"]
        .as_array()
        .unwrap()
        .is_empty());

    // A single READ-ONLY command (`status` is not in `mutates`) takes exactly one
    // opportunistic snapshot — the gap HV-89 closes: before, only DB-mutating
    // commands fired the daily backup, so a day of pure read-only use (or direct
    // content-file edits) took none. 0 -> 1.
    h.json(&["status"]);
    assert_eq!(
        h.json(&["backup", "list"])["backups"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "a read-only command should take one daily backup"
    );

    // A SECOND read-only command the same day adds none — the `last_backup` date
    // marker caps it at one snapshot/day regardless of command count. Still 1.
    // (It must be a command that *succeeds*: only the `Ok` arm reaches the backup
    // trigger, so a no-op here proves the marker gate, not a failed command.)
    h.json(&["status"]);
    assert_eq!(
        h.json(&["backup", "list"])["backups"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "a second same-day read-only command must not take another backup"
    );
}

#[test]
fn repo_binding_gates_writes_and_warns_reads_on_project_mismatch() {
    let h = Haven::new();
    // Two projects; bind this repo (cwd = HAVEN_HOME) to `haven`.
    h.json(&[
        "project", "add", "--key", "haven", "--title", "Haven", "--prefix", "HV",
    ]);
    h.ok(&["project", "use", "haven"]);
    h.json(&[
        "project", "add", "--key", "other", "--title", "Other", "--prefix", "OT",
    ]);
    let linked = h.json(&["link", "-p", "haven", "--name", "Workspace"]);
    assert!(linked["binding"]
        .as_str()
        .unwrap()
        .ends_with(".haven-project"));

    // Flip the global current project away from the binding.
    h.ok(&["project", "use", "other"]);

    // A write with no -p resolves to `other` != bound `haven` → BLOCKED (mis-file guard).
    let err = h.fail(&["item", "add", "Mistake", "--type", "task"]);
    assert_eq!(err["error"]["code"], "invalid");
    let msg = err["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("linked to project 'haven'"),
        "blocked msg: {msg}"
    );
    assert!(msg.contains("-p haven"), "should guide to -p: {msg}");

    // Explicit -p matching the binding proceeds (no mismatch).
    h.ok(&["item", "add", "Right", "--type", "task", "-p", "haven"]);
    // Explicit -p to another project is the deliberate override → proceeds (with a warn).
    let cross = h
        .cmd(&["item", "add", "Cross", "--type", "task", "-p", "other"])
        .output()
        .unwrap();
    assert!(
        cross.status.success(),
        "explicit -p override should proceed"
    );
    assert!(String::from_utf8_lossy(&cross.stderr).contains("-p override"));

    // A read with the mismatched current project warns but still succeeds.
    let read = h.cmd(&["next"]).output().unwrap();
    assert!(read.status.success(), "read must not be blocked");
    assert!(
        String::from_utf8_lossy(&read.stderr).contains("linked project 'haven'"),
        "read should warn on mismatch"
    );

    // Sanity: the deliberate -p writes actually landed in their target projects.
    let haven_items = h.json(&["item", "list", "-p", "haven"]);
    assert!(haven_items
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["title"] == "Right"));
    let other_items = h.json(&["item", "list", "-p", "other"]);
    assert!(other_items
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["title"] == "Cross"));
    assert!(!other_items
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["title"] == "Mistake"));
}

// ───────────────────────── HV-158: verb-divergence ─────────────────────────

/// A scratch project with the binding stubbed out (cwd not bound), so plain
/// `-p demo` ops run without the repo-binding guard interfering.
fn demo() -> Haven {
    let h = Haven::new();
    h.json(&[
        "project", "add", "--key", "demo", "--title", "Demo", "--prefix", "DM",
    ]);
    h.ok(&["project", "use", "demo"]);
    h
}

#[test]
fn list_items_errors_with_item_list_corrective() {
    let h = demo();
    let err = h.fail(&["list-items", "-p", "demo"]);
    assert_eq!(err["error"]["code"], "invalid");
    let msg = err["error"]["message"].as_str().unwrap();
    assert!(msg.contains("haven item list"), "msg: {msg}");
}

#[test]
fn top_level_mcp_flat_names_error_with_item_verb() {
    let h = demo();
    for (verb, want) in [
        ("get", "haven item get"),
        ("add", "haven item add"),
        ("archive", "haven item archive"),
        ("handoff", "haven item handoff"),
    ] {
        let err = h.fail(&[verb, "-p", "demo"]);
        let msg = err["error"]["message"].as_str().unwrap();
        assert!(
            msg.contains(want),
            "top-level `{verb}` should tip to `{want}`, got: {msg}"
        );
    }
}

#[test]
fn item_update_commit_flag_errors_naming_commit_verb() {
    let h = demo();
    let item = h.json(&["item", "add", "Thing", "-p", "demo"]);
    let r = item["ref"].as_str().unwrap();
    let err = h.fail(&["item", "update", r, "--commit", "-p", "demo"]);
    let msg = err["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains(&format!("haven item commit {r}")),
        "msg: {msg}"
    );
}

#[test]
fn status_positional_key_acts_like_p() {
    let h = demo();
    // `status demo` should report the same project as `status -p demo`.
    let via_positional = h.json(&["status", "demo"]);
    let via_flag = h.json(&["status", "-p", "demo"]);
    assert_eq!(via_positional["project"], via_flag["project"]);
    assert_eq!(via_positional["project"], "demo");
}

#[test]
fn prime_emits_all_sections() {
    let h = demo();
    // A committed-ready, AI-owned dispatch-eligible item (queue + next-flagged).
    h.json(&[
        "item",
        "add",
        "Ship the API",
        "--status",
        "ready",
        "--done-looks-like",
        "returns 200",
        "--commit",
        "--assign",
        "ai",
        "-p",
        "demo",
    ]); // DM-1
        // An in-progress, human-owned item (in-progress/waiting section). Committed +
        // with acceptance — a real in-flight item, so it is NOT an untriaged floater.
    h.json(&[
        "item",
        "add",
        "Refactor core",
        "--status",
        "ready",
        "--done-looks-like",
        "core slimmed",
        "--commit",
        "-p",
        "demo",
    ]); // DM-2
    h.ok(&["item", "assign", "DM-2", "--to", "human", "-p", "demo"]);
    h.ok(&[
        "item",
        "update",
        "DM-2",
        "--status",
        "in_progress",
        "-p",
        "demo",
    ]);
    // An untriaged floater (uncommitted, no acceptance) for the inbox section.
    h.json(&["item", "add", "Loose idea", "-p", "demo"]); // DM-3

    // The positional key resolves like `-p` (mirrors `status`).
    let block = h.text(&["prime", "demo"]);

    // §1 project state.
    assert!(block.contains("PROJECT demo (DM)"), "block:\n{block}");
    // §2 committed queue with next-eligible flagged.
    assert!(block.contains("QUEUE"), "block:\n{block}");
    assert!(block.contains("> DM-1"), "next-eligible flag:\n{block}");
    assert!(block.contains("Ship the API"), "block:\n{block}");
    // §3 in-progress / waiting with owner.
    assert!(block.contains("IN-PROGRESS / WAITING"), "block:\n{block}");
    assert!(
        block.contains("DM-2") && block.contains("human"),
        "in-progress owner:\n{block}"
    );
    // §4 conventions.
    assert!(block.contains("CONVENTIONS"), "block:\n{block}");
    // §5 untriaged inbox (HV-82 reuse).
    assert!(block.contains("INBOX (untriaged: 1)"), "block:\n{block}");
    assert!(block.contains("DM-3"), "inbox floater:\n{block}");

    // The positional and the -p flag resolve to the same block.
    assert_eq!(block, h.text(&["prime", "-p", "demo"]));
}

// ───────────────── HV-53: live-only graph & item list views ─────────────────

/// Build a project with one live item and one archived + one superseded item,
/// returning the harness and the live item's ref.
fn store_with_dead_items() -> Haven {
    let h = demo();
    h.json(&["item", "add", "Alive", "-p", "demo"]); // DM-1
    h.json(&["item", "add", "Gone", "-p", "demo"]); // DM-2
    h.json(&["item", "add", "Old", "-p", "demo"]); // DM-3
    h.json(&["item", "add", "New", "-p", "demo"]); // DM-4
    h.ok(&["item", "archive", "DM-2", "-p", "demo"]);
    // Supersede DM-3 with DM-4 → DM-3 becomes superseded.
    h.json(&[
        "evolve",
        "supersede",
        "DM-3",
        "--with",
        "DM-4",
        "-p",
        "demo",
    ]);
    h
}

#[test]
fn graph_excludes_archived_superseded_by_default_all_includes() {
    let h = store_with_dead_items();
    let g = h.json(&["graph", "-p", "demo"]);
    let refs: Vec<&str> = g["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["ref"].as_str().unwrap())
        .collect();
    assert!(refs.contains(&"DM-1"), "live node present: {refs:?}");
    assert!(!refs.contains(&"DM-2"), "archived hidden: {refs:?}");
    assert!(!refs.contains(&"DM-3"), "superseded hidden: {refs:?}");

    let all = h.json(&["graph", "--all", "-p", "demo"]);
    let all_refs: Vec<&str> = all["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["ref"].as_str().unwrap())
        .collect();
    assert!(
        all_refs.contains(&"DM-2"),
        "--all surfaces archived: {all_refs:?}"
    );
    assert!(
        all_refs.contains(&"DM-3"),
        "--all surfaces superseded: {all_refs:?}"
    );
}

#[test]
fn item_list_hides_archived_superseded_by_default_all_includes() {
    let h = store_with_dead_items();
    let live = h.json(&["item", "list", "-p", "demo"]);
    let titles: Vec<&str> = live
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["title"].as_str().unwrap())
        .collect();
    assert!(titles.contains(&"Alive"), "live present: {titles:?}");
    assert!(!titles.contains(&"Gone"), "archived hidden: {titles:?}");
    assert!(!titles.contains(&"Old"), "superseded hidden: {titles:?}");

    let all = h.json(&["item", "list", "--all", "-p", "demo"]);
    let all_titles: Vec<&str> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["title"].as_str().unwrap())
        .collect();
    assert!(
        all_titles.contains(&"Gone"),
        "--all surfaces archived: {all_titles:?}"
    );
    assert!(
        all_titles.contains(&"Old"),
        "--all surfaces superseded: {all_titles:?}"
    );
}

#[test]
fn item_list_status_filter_still_reaches_dead_items() {
    // An explicit --status archived must still find archived items even though
    // the default now hides them (the filter is the deliberate ask).
    let h = store_with_dead_items();
    let archived = h.json(&["item", "list", "--status", "archived", "-p", "demo"]);
    let titles: Vec<&str> = archived
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["title"].as_str().unwrap())
        .collect();
    assert!(
        titles.contains(&"Gone"),
        "explicit status reaches archived: {titles:?}"
    );
}

/// Pull the single `haven-telemetry {...}` line out of captured stderr (HV-166).
fn telemetry_obj(stderr: &str) -> Value {
    let line = stderr
        .lines()
        .find(|l| l.trim_start().starts_with("haven-telemetry "))
        .unwrap_or_else(|| panic!("no telemetry line in stderr:\n{stderr}"));
    let payload = line.trim_start().strip_prefix("haven-telemetry ").unwrap();
    serde_json::from_str(payload).unwrap_or_else(|e| panic!("bad telemetry json: {e}\n{line}"))
}

#[test]
fn cli_item_op_emits_telemetry_line_on_stderr() {
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "demo", "--prefix", "DM"]);
    // A known item op with an explicit project: stdout stays clean JSON, stderr
    // carries exactly one well-formed telemetry line.
    let (stdout, stderr) = h.run_capturing(&["item", "add", "A telemetered task", "-p", "demo"]);
    // stdout is the structured Output channel — it must remain parseable JSON,
    // i.e. the telemetry line is NOT on stdout.
    let _: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not clean JSON (telemetry leaked?): {e}\n{stdout}"));
    assert!(
        !stdout.contains("haven-telemetry"),
        "telemetry must never be on stdout:\n{stdout}"
    );
    let v = telemetry_obj(&stderr);
    assert_eq!(v["tool"], "item.add");
    assert_eq!(v["project_passed"], "demo");
    assert_eq!(v["project_resolved"], "demo");
    assert_eq!(v["error_class"], "ok");
    assert!(
        v["latency_ms"].is_u64(),
        "latency_ms numeric: {:?}",
        v["latency_ms"]
    );
}

#[test]
fn cli_telemetry_surfaces_sticky_project_drift() {
    // No -p: the op resolves via the sticky current_project. project_passed (null)
    // != project_resolved ("demo") must be readable from the line — HV-153 drift.
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "demo", "--prefix", "DM"]);
    let (_stdout, stderr) = h.run_capturing(&["item", "list"]);
    let v = telemetry_obj(&stderr);
    assert_eq!(v["tool"], "item.list");
    assert!(v["project_passed"].is_null(), "no -p was passed: {v}");
    assert_eq!(v["project_resolved"], "demo");
    assert_ne!(
        v["project_passed"], v["project_resolved"],
        "the drift must be observable from the line"
    );
}

#[test]
fn cli_telemetry_error_class_buckets_a_not_found() {
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "demo", "--prefix", "DM"]);
    // Completing a nonexistent ref fails NotFound → error_class "not_found".
    let (_stdout, stderr) = h.run_capturing(&[
        "item",
        "complete",
        "DM-9999",
        "--evidence",
        "x",
        "-p",
        "demo",
    ]);
    let v = telemetry_obj(&stderr);
    assert_eq!(v["tool"], "item.complete");
    assert_eq!(v["error_class"], "not_found");
    assert_eq!(v["project_resolved"], "demo");
}

// ---- HV-123: project archive / reopen / list -----------------------------

#[test]
fn cli_project_archive_reopen_roundtrip() {
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "demo", "--prefix", "DM"]);
    // Mint a ref so ref_counter is non-zero (proves reservation).
    h.ok(&["item", "add", "First", "-p", "demo"]);

    // Archive: status flips, reason recorded, namespace preserved.
    let arch = h.json(&[
        "project",
        "archive",
        "demo",
        "--rationale",
        "winding down",
        "--by",
        "alice",
    ]);
    assert_eq!(arch["status"], "archived");
    assert_eq!(arch["archived_reason"], "winding down");
    assert_eq!(arch["ref_prefix"], "DM");
    assert_eq!(arch["ref_counter"], 1);

    // Default listing hides it; --include-archived shows it.
    let listed = h.json(&["project", "list"]);
    let keys: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["key"].as_str().unwrap())
        .collect();
    assert!(
        !keys.contains(&"demo"),
        "archived project hidden by default"
    );

    let all = h.json(&["project", "list", "--include-archived"]);
    let all_keys: Vec<&str> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["key"].as_str().unwrap())
        .collect();
    assert!(all_keys.contains(&"demo"), "--include-archived shows it");

    // get still resolves an archived project.
    let got = h.json(&["project", "get", "demo"]);
    assert_eq!(got["status"], "archived");

    // A mutating op into the archived project is refused.
    let err = h.fail(&["item", "add", "Blocked", "-p", "demo"]);
    assert_eq!(err["error"]["code"], "invalid");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("archived"));

    // `project use` of an archived project is refused.
    let use_err = h.fail(&["project", "use", "demo"]);
    assert_eq!(use_err["error"]["code"], "invalid");

    // Reopen restores it; the next ref continues from the preserved counter.
    let reopened = h.json(&["project", "reopen", "demo"]);
    assert_eq!(reopened["status"], "active");
    assert_eq!(reopened["ref_counter"], 1);
    let next = h.json(&["item", "add", "Second", "-p", "demo"]);
    assert_eq!(next["ref"], "DM-2");
}

#[test]
fn cli_project_reopen_of_active_errors() {
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "demo", "--prefix", "DM"]);
    let err = h.fail(&["project", "reopen", "demo"]);
    assert_eq!(err["error"]["code"], "invalid");
}

#[test]
fn cli_project_archive_missing_key_is_not_found() {
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "demo", "--prefix", "DM"]);
    let err = h.fail(&["project", "archive", "ghost"]);
    assert_eq!(err["error"]["code"], "not_found");
}

#[test]
fn cli_project_list_pretty_has_status_column() {
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "demo", "--prefix", "DM"]);
    let (stdout, _stderr) = h.run_capturing(&["project", "list", "--pretty"]);
    assert!(stdout.contains("STATUS"), "STATUS column present: {stdout}");
    assert!(stdout.contains("active"), "active status shown: {stdout}");
}

#[test]
fn cli_project_archive_reason_alias_accepted() {
    let h = Haven::new();
    h.ok(&["setup", "--project-key", "demo", "--prefix", "DM"]);
    // `--reason` is a clap alias of `--rationale`.
    let arch = h.json(&["project", "archive", "demo", "--reason", "alias works"]);
    assert_eq!(arch["status"], "archived");
    assert_eq!(arch["archived_reason"], "alias works");
}

// ---- item-level external references (HV-226) + artifact xref write (HV-229) ---

fn project_with_items(h: &Haven, n: usize) {
    h.ok(&[
        "project", "add", "--key", "haven", "--title", "Haven", "--prefix", "HV",
    ]);
    h.ok(&["project", "use", "haven"]);
    for i in 0..n {
        h.ok(&["item", "add", &format!("Ship {i}")]);
    }
}

#[test]
fn cli_extref_add_list_find_rm_round_trip() {
    let h = Haven::new();
    project_with_items(&h, 2); // HV-1, HV-2

    // add: records the locator AND flips in_progress by default.
    let added = h.json(&[
        "item",
        "extref",
        "add",
        "HV-1",
        "--store",
        "jira",
        "--target",
        "PROJ-9",
        "--url",
        "https://x/9",
        "--canonical",
    ]);
    assert_eq!(added["status"], "in_progress");
    assert_eq!(added["metadata"]["external_refs"][0]["target"], "PROJ-9");
    assert_eq!(
        added["metadata"]["external_refs"][0]["execution_canonical"],
        true
    );

    // list
    let list = h.json(&["item", "extref", "list", "HV-1"]);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["store"], "jira");

    // find: reverse lookup from the external id back to the Haven item.
    let found = h.json(&["item", "extref", "find", "--target", "PROJ-9"]);
    let arr = found.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["ref"], "HV-1");

    // rm
    h.ok(&["item", "extref", "rm", "HV-1", "--target", "PROJ-9"]);
    assert!(h
        .json(&["item", "extref", "list", "HV-1"])
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn cli_extref_no_in_progress_keeps_status_and_target_required() {
    let h = Haven::new();
    project_with_items(&h, 1); // HV-1 (discovery)

    let added = h.json(&[
        "item",
        "extref",
        "add",
        "HV-1",
        "--store",
        "github",
        "--target",
        "o/r#1",
        "--no-in-progress",
    ]);
    assert_eq!(added["status"], "discovery");

    // missing --target is a clap usage error (non-zero exit).
    let out = h
        .cmd(&["item", "extref", "add", "HV-1", "--store", "jira"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "missing --target should fail");
}

#[test]
fn cli_artifact_add_with_xref_round_trips_via_xref_read() {
    // HV-229: an artifact xref is writable from the public CLI and reads back
    // through `haven xref`.
    let h = Haven::new();
    project_with_items(&h, 1); // HV-1
    h.ok(&[
        "artifact",
        "add",
        "HV-1",
        "--role",
        "design",
        "--content",
        "doc",
        "--name",
        "d.md",
        "--xref-relation",
        "mirror",
        "--xref-store",
        "github",
        "--xref-target",
        "o/r#1",
    ]);
    let report = h.json(&["xref", "HV-1"]);
    assert_eq!(report["outbound"][0]["target"], "o/r#1");
    assert_eq!(report["outbound"][0]["relation"], "mirror");
}
