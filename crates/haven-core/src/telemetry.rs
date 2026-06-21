//! Per-call telemetry (HV-166).
//!
//! Every `haven_*` MCP `tools/call` and every CLI item op emits exactly ONE
//! structured line to **stderr** (stdout is the JSON-RPC channel for MCP and the
//! structured `Output` for the CLI — telemetry there would corrupt both). The
//! line carries the fields that make the HV-153 silent project-drift observable
//! even for the bare-array tools that left their wire shape unchanged:
//!
//! - `tool` — the tool / op name (`haven_next`, `item.complete`, …).
//! - `project_passed` — the `project` selector as the caller gave it (or null).
//! - `project_resolved` — the project key the op actually resolved to. When this
//!   differs from `project_passed` the sticky `current_project` fallback was in
//!   play — the drift HV-153 surfaced for objects, now visible for everything.
//! - `error_class` — `ok` on success, else the [`HavenError::code`] bucket
//!   (`not_found` / `invalid` / `conflict` / …).
//! - `latency_ms` — wall time of the call, measured with [`std::time::Instant`].
//!
//! The line is emitted as a single-line JSON object prefixed `haven-telemetry `
//! so it's both grep-able and machine-parseable. Emission is **always-on** to
//! stderr — the acceptance requires a test to assert the line is present, so the
//! default must satisfy "emits one structured line"; gating it behind an env var
//! would make the default silent. (Callers that want quiet interactive use can
//! redirect stderr; the line is one terse object, not a log spew.)

use std::io::Write;

use serde_json::json;

use crate::error::HavenError;

/// One per-call telemetry record. Construct it at the choke-point, then
/// [`emit`](TelemetryLine::emit) it (stderr) or [`write_to`](TelemetryLine::write_to)
/// it (any sink — the testable seam).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryLine {
    /// Tool / op name (`haven_next`, `item.complete`, …).
    pub tool: String,
    /// The `project` selector as passed by the caller (`None` ⇒ not given).
    pub project_passed: Option<String>,
    /// The project key actually resolved (`None` when resolution didn't run /
    /// failed — e.g. a global tool, or the op errored before resolving).
    pub project_resolved: Option<String>,
    /// `ok` on success, else the stable [`HavenError::code`] bucket.
    pub error_class: String,
    /// Wall-clock duration of the call in whole milliseconds.
    pub latency_ms: u128,
}

/// Bucket a result into the `error_class` field: `ok` for success, otherwise the
/// error's stable [`HavenError::code`] (closed-set `invalid`, `not_found`,
/// `conflict`, `graph_rule`, …). Reuses the existing taxonomy so the buckets
/// can't drift from the error envelope.
pub fn error_class<T>(result: &Result<T, HavenError>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(e) => e.code(),
    }
}

impl TelemetryLine {
    /// Build a line. `project_passed`/`project_resolved` are taken as-is so the
    /// caller controls exactly what each means at its choke-point.
    pub fn new(
        tool: impl Into<String>,
        project_passed: Option<String>,
        project_resolved: Option<String>,
        error_class: impl Into<String>,
        latency_ms: u128,
    ) -> Self {
        Self {
            tool: tool.into(),
            project_passed,
            project_resolved,
            error_class: error_class.into(),
            latency_ms,
        }
    }

    /// Render the single structured line (no trailing newline). A `haven-telemetry`
    /// prefix makes it grep-able; the rest is a compact JSON object so it parses.
    pub fn render(&self) -> String {
        let obj = json!({
            "tool": self.tool,
            "project_passed": self.project_passed,
            "project_resolved": self.project_resolved,
            "error_class": self.error_class,
            "latency_ms": self.latency_ms,
        });
        format!("haven-telemetry {obj}")
    }

    /// Write the line (with newline) to an arbitrary sink — the seam tests use to
    /// capture and parse the line without touching the real stderr.
    pub fn write_to(&self, w: &mut impl Write) -> std::io::Result<()> {
        writeln!(w, "{}", self.render())
    }

    /// Emit the line to **stderr** (the established telemetry/warning channel).
    /// Best-effort: a stderr write failure is swallowed — telemetry must never
    /// break the op it's measuring. Never writes to stdout.
    pub fn emit(&self) {
        let _ = self.write_to(&mut std::io::stderr());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_is_one_line_with_prefix_and_parseable_json() {
        let line = TelemetryLine::new(
            "haven_next",
            Some("haven".into()),
            Some("haven".into()),
            "ok",
            7,
        );
        let rendered = line.render();
        assert!(rendered.starts_with("haven-telemetry "));
        assert!(!rendered.contains('\n'), "must be a single line");
        let payload = rendered.strip_prefix("haven-telemetry ").unwrap();
        let v: serde_json::Value = serde_json::from_str(payload).expect("parseable JSON object");
        assert_eq!(v["tool"], "haven_next");
        assert_eq!(v["project_passed"], "haven");
        assert_eq!(v["project_resolved"], "haven");
        assert_eq!(v["error_class"], "ok");
        assert_eq!(v["latency_ms"], 7);
    }

    #[test]
    fn write_to_captures_into_a_sink() {
        let line = TelemetryLine::new("item.complete", None, Some("haven".into()), "ok", 0);
        let mut buf: Vec<u8> = Vec::new();
        line.write_to(&mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.ends_with('\n'));
        assert!(s.trim().starts_with("haven-telemetry "));
    }

    #[test]
    fn passed_none_renders_json_null_distinct_from_resolved() {
        // The HV-153 drift signal: no project passed, but a sticky key resolved —
        // the two fields differ and the difference is readable from the line.
        let line = TelemetryLine::new("haven_search", None, Some("haven".into()), "ok", 3);
        let payload = line.render();
        let payload = payload.strip_prefix("haven-telemetry ").unwrap();
        let v: serde_json::Value = serde_json::from_str(payload).unwrap();
        assert!(v["project_passed"].is_null());
        assert_eq!(v["project_resolved"], "haven");
        assert_ne!(v["project_passed"], v["project_resolved"]);
    }

    #[test]
    fn error_class_buckets_match_error_codes() {
        let nf: Result<(), HavenError> = Err(HavenError::NotFound("x".into()));
        assert_eq!(error_class(&nf), "not_found");
        let inv: Result<(), HavenError> = Err(HavenError::Invalid("x".into()));
        assert_eq!(error_class(&inv), "invalid");
        let ok: Result<(), HavenError> = Ok(());
        assert_eq!(error_class(&ok), "ok");
    }
}
