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

    /// Run `haven <args>`, expect failure, parse the error envelope from stderr.
    fn fail(&self, args: &[&str]) -> Value {
        let out = self.cmd(args).output().unwrap();
        assert!(
            !out.status.success(),
            "command {args:?} unexpectedly succeeded"
        );
        serde_json::from_slice(&out.stderr).unwrap()
    }

    fn ok(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "command {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::cargo_bin("haven").unwrap();
        c.env("HAVEN_HOME", &self.home);
        // Keep `setup`'s MCP-config write inside the temp tree, never ~/.claude.
        c.env("HAVEN_CLAUDE_DIR", self.home.join(".claude"));
        c.args(args);
        c
    }
}

#[test]
fn skill_install_and_setup_write_the_snapshot() {
    let h = Haven::new();
    let skill_dir = h.home.join(".claude/skills/haven");

    // Explicit install writes the embedded snapshot.
    let out = h.json(&["skill", "install"]);
    assert!(out["installed"].as_str().unwrap().ends_with("skills/haven"));
    assert!(skill_dir.join("SKILL.md").exists());
    assert!(skill_dir.join("references/workflows.md").exists());
    assert!(skill_dir.join("references/surface-map.md").exists());
    // It's the real skill (frontmatter name), not an empty file.
    let body = std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap();
    assert!(body.contains("name: haven"));

    // `setup` installs it too (alongside the MCP wiring) — unless --no-skill.
    let fresh = Haven::new();
    let setup = fresh.json(&["setup"]);
    assert!(setup["skill"].as_str().unwrap().ends_with("skills/haven"));
    assert!(fresh.home.join(".claude/skills/haven/SKILL.md").exists());

    let skipped = Haven::new();
    let out = skipped.json(&["setup", "--no-skill"]);
    assert_eq!(out["skill"], "skipped (--no-skill)");
    assert!(!skipped.home.join(".claude/skills/haven/SKILL.md").exists());
}

#[test]
fn doctor_reports_install_health() {
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

    // Before setup the store still opens (migrations run, schema stamped), but the
    // MCP stanza and skill snapshot aren't wired — doctor flags exactly that.
    let before = h.json(&["doctor"]);
    assert_eq!(before["ok"], false);
    assert_eq!(status_of(&before, "database"), "ok");
    assert_eq!(status_of(&before, "mcp"), "warn");
    assert_eq!(status_of(&before, "skill"), "warn");

    // After setup, MCP + skill are green. Put the built binary on $PATH so the
    // `path` check can resolve `haven` (it isn't there by default in the test env).
    h.ok(&["setup"]);
    let bin = assert_cmd::cargo::cargo_bin("haven");
    let bindir = bin.parent().unwrap();
    let out = Command::cargo_bin("haven")
        .unwrap()
        .env("HAVEN_HOME", &h.home)
        .env("HAVEN_CLAUDE_DIR", h.home.join(".claude"))
        .env("PATH", bindir)
        .arg("doctor")
        .output()
        .unwrap();
    let after: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status_of(&after, "mcp"), "ok");
    assert_eq!(status_of(&after, "skill"), "ok");
    assert_eq!(status_of(&after, "path"), "ok");
    assert_eq!(
        after["ok"], true,
        "after setup doctor should be green: {after}"
    );
}

#[test]
fn setup_can_bootstrap_first_project() {
    let h = Haven::new();
    let out = h.json(&[
        "setup",
        "--project-key",
        "haven",
        "--project-title",
        "Haven",
        "--prefix",
        "HV",
    ]);
    assert_eq!(out["current_project"], "haven");
    assert_eq!(out["project_created"], true);

    let item = h.json(&["item", "add", "Draft the spec"]);
    assert_eq!(item["ref"], "HV-1");
}

#[test]
fn batch_commit_and_archive_take_multiple_refs() {
    let h = Haven::new();
    h.ok(&["setup"]);
    h.ok(&[
        "project", "add", "--key", "haven", "--title", "Haven", "--prefix", "HV",
    ]);
    h.ok(&["project", "use", "haven"]);
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
fn full_lifecycle() {
    let h = Haven::new();
    h.ok(&["setup"]);
    h.ok(&[
        "project", "add", "--key", "haven", "--title", "Haven", "--prefix", "HV",
    ]);
    h.ok(&["project", "use", "haven"]);

    // Add a committed, ready, AI-owned item and an uncommitted prereq.
    let item = h.json(&[
        "item",
        "add",
        "Draft the spec",
        "--status",
        "ready",
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

    // Decomposition + dependency edges, read back via include.
    h.ok(&["item", "add", "Frontend"]); // HV-3
    h.ok(&["decompose", "HV-1", "--into", "HV-3"]);
    h.ok(&["depend", "HV-3", "--on", "HV-2"]);
    let full = h.json(&["item", "get", "HV-1", "--include", "edges"]);
    assert_eq!(full["edges"]["children"][0], "HV-3");

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
    h.ok(&["setup"]);
    h.ok(&[
        "project", "add", "--key", "haven", "--title", "Haven", "--prefix", "HV",
    ]);
    h.ok(&["project", "use", "haven"]);

    let err = h.fail(&["item", "get", "HV-999"]);
    assert_eq!(err["error"]["code"], "not_found");

    h.json(&["item", "add", "A"]);
    h.json(&["item", "add", "B"]);
    h.ok(&["decompose", "HV-1", "--into", "HV-2"]);
    let cycle = h.fail(&["decompose", "HV-2", "--into", "HV-1"]);
    assert_eq!(cycle["error"]["code"], "graph_rule");
}

#[test]
fn content_layer_artifact_note_render() {
    let h = Haven::new();
    h.ok(&["setup"]);
    h.ok(&[
        "project", "add", "--key", "haven", "--title", "Haven", "--prefix", "HV",
    ]);
    h.ok(&["project", "use", "haven"]);
    h.json(&["item", "add", "Write spec", "--commit", "--status", "ready"]);

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
    h.ok(&["setup"]);
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
            {"id": "api", "title": "Build API", "parent": "epic", "status": "ready", "commit": true},
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
