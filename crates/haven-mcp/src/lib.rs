//! `haven-mcp` — a hand-rolled stdio JSON-RPC 2.0 MCP server over the
//! `haven-core` `Store` (SPEC §3). Newline-delimited JSON messages (the MCP
//! stdio framing). Every `haven_*` tool is a thin wrapper over the exact same
//! `Store` method the CLI calls, so the two surfaces cannot drift.
//!
//! The protocol surface needed is tiny — `initialize`, `tools/list`,
//! `tools/call` (+ the `initialized` notification and `ping`) — so we implement
//! it directly rather than taking an SDK dependency.

// The tool registry is one large `json!([...])` literal; each tool adds another
// `serde_json::json!` expansion frame, and adding `haven_prime` (HV-23) tipped it
// past the default 128-frame limit. Lift the ceiling rather than splitting the
// single source-of-truth array.
#![recursion_limit = "256"]

use std::io::{self, BufRead, Write};
use std::time::Instant;

use haven_core::{
    telemetry::{self, TelemetryLine},
    Artifact, ArtifactKind, ArtifactRole, ArtifactSelector, CompleteInput, ContextPack, DueUpdate,
    EdgeKind, Edges, HandoffInput, HavenError, ImportItem, Include, Item, ItemFilter, ItemUpdate,
    LineageDirection, LineageEvent, NewArtifact, NewItem, NodeType, OwnerKind, Result, RollupState,
    StaleRef, Status, Store, WaitState, WaitUpdate,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Deserialize)]
struct Request {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct Response {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i64,
    message: String,
}

/// Default page size for `haven_list_items` when the caller passes no `limit`.
/// `total` is always returned, so a truncated page is never silent.
const DEFAULT_LIST_LIMIT: i64 = 100;

/// The MCP projection of an [`Item`] (SPEC §3). Deliberately leaner than the
/// `Item` the CLI serializes: the machine-only fields (`public_id`, `sync_state`,
/// `revision`, `sort_key`) are *always* dropped — an agent reasons in `ref`s, not
/// storage internals — and the `compact` form (list/next/resolve) further omits
/// the prose fields, timestamps and includes, which an agent pulls on demand via
/// `haven_get_item`. The `graph` view (`graph_node`) is compact plus
/// `done_looks_like`, so a whole-graph read can be triaged (e.g. tell a sealed
/// leaf from an unsealed one) without a per-node fetch. Borrows from the source
/// `Item`; the enums are `Copy`.
#[derive(Serialize)]
struct McpItem<'a> {
    #[serde(rename = "ref")]
    reference: &'a str,
    title: &'a str,
    #[serde(rename = "type")]
    node_type: NodeType,
    status: Status,
    committed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_kind: Option<OwnerKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wait_state: Option<WaitState>,

    // Full-only fields — `None` in the compact form. (`done_looks_like` is also
    // populated by the `graph` view — see `graph_node`.)
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    done_looks_like: Option<&'a str>,
    // Graph + full views only: the derived container rollup (containers only).
    #[serde(skip_serializing_if = "Option::is_none")]
    rollup_state: Option<RollupState>,
    // Sibling of `rollup_state`: live uncommitted work exists beneath the
    // container, so a `done` rollup is never silently misleading (HV-104).
    #[serde(skip_serializing_if = "Option::is_none")]
    has_uncommitted_descendants: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    why: Option<&'a str>,
    // Full-only: the optional `YYYY-MM-DD` deadline. Omitted when null; absent
    // from the lean compact/graph rows (HV-67).
    #[serde(skip_serializing_if = "Option::is_none")]
    due_at: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assignee: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archived_at: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    edges: Option<&'a Edges>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifacts: Option<&'a [Artifact]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lineage: Option<&'a [LineageEvent]>,

    // Graph + full views only: the context pack governing a leaf's build, so a
    // dispatcher reading a ready leaf sees its pack instead of building naked
    // (HV-75). Absent from compact (list/next) to keep it lean.
    #[serde(skip_serializing_if = "Option::is_none")]
    context_pack: Option<&'a ContextPack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_pack_clash: Option<&'a [String]>,
}

impl<'a> McpItem<'a> {
    /// Navigation view: identity + axes only. For list/next/resolve.
    fn compact(item: &'a Item) -> Self {
        McpItem {
            reference: &item.reference,
            title: &item.title,
            node_type: item.node_type,
            status: item.status,
            committed: item.committed,
            priority: item.priority,
            owner_kind: item.owner_kind,
            wait_state: item.wait_state,
            body: None,
            done_looks_like: None,
            rollup_state: None,
            has_uncommitted_descendants: None,
            why: None,
            due_at: None,
            assignee: None,
            created_at: None,
            updated_at: None,
            archived_at: None,
            metadata: None,
            edges: None,
            artifacts: None,
            lineage: None,
            context_pack: None,
            context_pack_clash: None,
        }
    }

    /// Graph view: the compact axes plus `done_looks_like`, so a single
    /// `haven_graph` read can triage and plan (e.g. tell a sealed leaf from an
    /// unsealed one) without N per-node detail fetches. list/next stay lean.
    fn graph_node(item: &'a Item) -> Self {
        McpItem {
            done_looks_like: item.done_looks_like.as_deref(),
            rollup_state: item.rollup_state,
            has_uncommitted_descendants: item.has_uncommitted_descendants,
            context_pack: item.context_pack.as_ref(),
            context_pack_clash: item.context_pack_clash.as_deref(),
            ..McpItem::compact(item)
        }
    }

    /// Detail view: prose, timestamps, non-empty metadata, and any includes that
    /// were loaded. Still drops the machine-only sync/storage fields.
    fn full(item: &'a Item) -> Self {
        McpItem {
            reference: &item.reference,
            title: &item.title,
            node_type: item.node_type,
            status: item.status,
            committed: item.committed,
            priority: item.priority,
            owner_kind: item.owner_kind,
            wait_state: item.wait_state,
            body: item.body.as_deref(),
            done_looks_like: item.done_looks_like.as_deref(),
            rollup_state: item.rollup_state,
            has_uncommitted_descendants: item.has_uncommitted_descendants,
            why: item.why.as_deref(),
            due_at: item.due_at.as_deref(),
            assignee: item.assignee.as_deref(),
            created_at: Some(&item.created_at),
            updated_at: Some(&item.updated_at),
            archived_at: item.archived_at.as_deref(),
            metadata: match &item.metadata {
                Value::Object(m) if !m.is_empty() => Some(&item.metadata),
                _ => None,
            },
            edges: item.edges.as_ref(),
            artifacts: item.artifacts.as_deref(),
            lineage: item.lineage.as_deref(),
            context_pack: item.context_pack.as_ref(),
            context_pack_clash: item.context_pack_clash.as_deref(),
        }
    }
}

/// Serve over stdin/stdout (blocking until EOF).
pub fn serve(store: &Store) -> io::Result<()> {
    let stdin = io::stdin().lock();
    let stdout = io::stdout().lock();
    serve_io(store, stdin, stdout)
}

/// Serve over arbitrary reader/writer — used by tests to pipe a session.
pub fn serve_io<R: BufRead, W: Write>(
    store: &Store,
    mut reader: R,
    mut writer: W,
) -> io::Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break; // EOF
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                write_message(
                    &mut writer,
                    &error_response(Value::Null, -32700, format!("parse error: {e}")),
                )?;
                continue;
            }
        };
        // A JSON-RPC notification is any message without an `id`; it gets no
        // response, whatever its method name. (`#[serde(default)]` makes a
        // missing id deserialize to `Value::Null`.)
        let is_notification = req.id.is_null();
        let response = dispatch(store, &req);
        if is_notification {
            continue;
        }
        if let Some(resp) = response {
            write_message(&mut writer, &resp)?;
        }
    }
    Ok(())
}

fn write_message<W: Write>(writer: &mut W, resp: &Response) -> io::Result<()> {
    writeln!(writer, "{}", serde_json::to_string(resp).unwrap())?;
    writer.flush()
}

fn ok_response(id: Value, result: Value) -> Response {
    Response {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

fn error_response(id: Value, code: i64, message: String) -> Response {
    Response {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(RpcError { code, message }),
    }
}

fn dispatch(store: &Store, req: &Request) -> Option<Response> {
    let id = req.id.clone();
    match req.method.as_str() {
        "initialize" => Some(ok_response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "haven", "version": env!("CARGO_PKG_VERSION") },
            }),
        )),
        "notifications/initialized" => None,
        "ping" => Some(ok_response(id, json!({}))),
        "tools/list" => Some(ok_response(id, json!({ "tools": tools_list() }))),
        "tools/call" => Some(handle_tool_call(store, id, &req.params)),
        other => Some(error_response(
            id,
            -32601,
            format!("method not found: {other}"),
        )),
    }
}

/// `tools/call` → run the tool, wrapping success as an MCP text-content result
/// and a `HavenError` as an `isError` tool result (so the model sees it). Emits
/// the per-call telemetry line to **stderr** (HV-166).
fn handle_tool_call(store: &Store, id: Value, params: &Value) -> Response {
    let (resp, telemetry) = handle_tool_call_inner(store, id, params);
    // stderr only — stdout is the JSON-RPC channel.
    if let Some(line) = telemetry {
        line.emit();
    }
    resp
}

/// The chokepoint that wraps EVERY `tools/call` (HV-166): times `call_tool`,
/// derives `{tool, project_passed, project_resolved, error_class, latency_ms}`,
/// and returns the JSON-RPC `Response` alongside the telemetry line so the public
/// wrapper can emit it (and tests can assert on it without intercepting stderr).
///
/// `project_resolved` is derived HERE for **every** project-scoped tool — including
/// the bare-array tools (`haven_next`/`haven_search`/`haven_lineage`) that HV-153
/// left wire-shape-unchanged. The telemetry line is how those tools' project drift
/// becomes observable (the agreed HV-153→HV-166 delegation): we resolve the key
/// at the chokepoint rather than only stamping object responses.
///
/// `telemetry` is `None` only for the malformed-call early return (no tool name),
/// which never reaches `call_tool`.
fn handle_tool_call_inner(
    store: &Store,
    id: Value,
    params: &Value,
) -> (Response, Option<TelemetryLine>) {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return (
                error_response(id, -32602, "tools/call missing 'name'".into()),
                None,
            )
        }
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    // The `project` selector as the caller gave it (absent ⇒ None).
    let project_passed = opt_str(&args, "project").map(str::to_string);

    // Time the actual call (Instant, not wall-clock formatting).
    let started = Instant::now();
    let result = call_tool(store, name, &args);
    let latency_ms = started.elapsed().as_millis();

    // Resolve the project key the op would use — for every project-scoped tool,
    // independent of the result's wire shape. Best-effort: on the rare resolution
    // miss (e.g. the call itself errored before resolving) we leave it `None`.
    let project_resolved = if is_project_scoped_tool(name) {
        store.resolve_project_key(opt_str(&args, "project")).ok()
    } else {
        None
    };

    let error_class = telemetry::error_class(&result);
    let line = TelemetryLine::new(
        name,
        project_passed,
        project_resolved,
        error_class,
        latency_ms,
    );

    let resp = match result {
        Ok(value) => ok_response(
            id,
            json!({
                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }],
                "isError": false,
            }),
        ),
        Err(e) => ok_response(
            id,
            json!({
                "content": [{ "type": "text", "text": json!({"error": {"code": e.code(), "message": e.to_string()}}).to_string() }],
                "isError": true,
            }),
        ),
    };
    (resp, Some(line))
}

// ---- argument helpers --------------------------------------------------------

fn opt_str<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(|x| x.as_str())
}
fn req_str<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    opt_str(v, k).ok_or_else(|| HavenError::Invalid(format!("missing required arg '{k}'")))
}
fn opt_i64(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(|x| x.as_i64())
}
fn opt_bool(v: &Value, k: &str) -> Option<bool> {
    v.get(k).and_then(|x| x.as_bool())
}
/// Build an [`ArtifactSelector`] from the mutually-exclusive `role`/`name`/`id`
/// args shared by `haven_rm_artifact` and `haven_mv_artifact`. Exactly one.
fn artifact_selector(a: &Value) -> Result<ArtifactSelector> {
    match (opt_str(a, "role"), opt_str(a, "name"), opt_str(a, "id")) {
        (Some(r), None, None) => Ok(ArtifactSelector::Role(ArtifactRole::parse(r)?)),
        (None, Some(n), None) => Ok(ArtifactSelector::Name(n.to_string())),
        (None, None, Some(i)) => Ok(ArtifactSelector::Id(i.to_string())),
        _ => Err(HavenError::Invalid(
            "provide exactly one of role, name, or id".into(),
        )),
    }
}
fn str_array(v: &Value, k: &str) -> Vec<String> {
    v.get(k)
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| e.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Attach a `stale_ref` advisory to a success payload when the resolved ref was
/// dead (superseded/archived) — HV-154. Rides the response object the same way
/// `similar`/`grooming_nudge` do: present only when there's something to say, so
/// the common live-ref path is unchanged. `out` must be a JSON object.
fn attach_stale_ref(out: &mut Value, stale: Option<StaleRef>) -> Result<()> {
    if let Some(stale) = stale {
        if let Value::Object(map) = out {
            map.insert("stale_ref".into(), to_value(&stale)?);
        }
    }
    Ok(())
}

/// Tools that mutate the work-graph — the choke point for the opportunistic
/// daily backup. The MCP server opens one `Store` for the whole session, so the
/// trigger must live per tools/call here (not in `serve`, which fires once).
fn is_mutating_tool(name: &str) -> bool {
    matches!(
        name,
        "haven_add_item"
            | "haven_update_item"
            | "haven_add_edge"
            | "haven_evolve"
            | "haven_rank"
            | "haven_add_artifact"
            | "haven_rm_artifact"
            | "haven_mv_artifact"
            | "haven_add_project"
            | "haven_archive_project"
            | "haven_reopen_project"
            | "haven_archive"
            | "haven_reopen"
            | "haven_handoff"
            | "haven_complete_item"
            | "haven_import"
    )
}

/// Tools that resolve a single project (explicit `project` arg, else the sticky
/// `current_project`). For these the success response echoes the resolved key,
/// and a sticky fallback rides a warning (HV-153). The GLOBAL tools
/// (`haven_list_projects`/`haven_add_project`) resolve no single project, so
/// they're excluded — their results carry no project echo. The lifecycle tools
/// (`haven_archive_project`/`haven_reopen_project`) take a required `key`
/// directly (operating on the namespace itself, like `haven_add_project`), so
/// they too resolve no `project` arg and are excluded (HV-123).
fn is_project_scoped_tool(name: &str) -> bool {
    !matches!(
        name,
        "haven_list_projects"
            | "haven_add_project"
            | "haven_archive_project"
            | "haven_reopen_project"
    )
}

/// The single response-builder chokepoint for the project echo (HV-153): on a
/// project-scoped tool's success, stamp the resolved project key onto the result
/// object, and — when the project was NOT passed explicitly (it came from the
/// sticky `current_project` fallback) — ride a warning naming it, so a drifting
/// sticky default is observable. Only stamps JSON *objects* (the array-shaped
/// results — `haven_next`, `haven_search`, … — are left as-is); the key is
/// otherwise observable on the abundant object-shaped responses.
fn attach_project_echo(out: &mut Value, resolved: &str, explicit: bool) {
    if let Value::Object(map) = out {
        map.insert("project_resolved".into(), json!(resolved));
        if !explicit {
            map.insert(
                "project_warning".into(),
                json!(format!(
                    "no `project` arg given — resolved to current_project {resolved:?} \
                     (sticky default); pass `project` explicitly to be sure"
                )),
            );
        }
    }
}

/// Dispatch a `haven_*` tool, then (for project-scoped tools) stamp the resolved
/// project echo onto the success payload — the HV-153 chokepoint, threaded once
/// here rather than per-tool.
fn call_tool(store: &Store, name: &str, a: &Value) -> Result<Value> {
    let explicit_project = opt_str(a, "project").is_some();
    let mut out = dispatch_tool(store, name, a)?;
    if is_project_scoped_tool(name) {
        // Resolve the key the dispatch actually used. Best-effort: a resolution
        // error here would already have surfaced from dispatch_tool, so on the
        // rare miss we simply skip the echo rather than mask the success.
        if let Ok(resolved) = store.resolve_project_key(opt_str(a, "project")) {
            attach_project_echo(&mut out, &resolved, explicit_project);
        }
    }
    Ok(out)
}

/// Dispatch a `haven_*` tool to the matching `Store` method. Returns the raw
/// JSON payload (wrapped into MCP content by the caller).
fn dispatch_tool(store: &Store, name: &str, a: &Value) -> Result<Value> {
    // Best-effort backup hooks; stderr only (stdout is the JSON-RPC channel).
    let backups = store.content_root().join("backups");
    // Quarantine warning on EVERY tools/call (incl. read-only), so a long
    // read-only agent session still sees the freeze alarm — "warns on every command".
    if let Ok(frozen) = Store::backups_frozen(&backups) {
        if !frozen.is_empty() {
            eprintln!(
                "haven: WARNING — {} quarantined backup(s) ({}); rotation + object GC frozen, \
                 run `haven backup clear <id>` (or remove the *-SUSPECT manifest/dir under {}).",
                frozen.len(),
                frozen.join(", "),
                backups.display(),
            );
        }
    }
    // Opportunistic ≤1/day snapshot only on mutating tools (the choke point).
    if is_mutating_tool(name) {
        let _ = store.maybe_daily_backup(&backups);
    }
    let project = opt_str(a, "project");
    match name {
        "haven_list_items" => {
            let filter = ItemFilter {
                status: opt_str(a, "status").map(Status::parse).transpose()?,
                node_type: opt_str(a, "type").map(NodeType::parse).transpose()?,
                owner: opt_str(a, "owner").map(OwnerKind::parse).transpose()?,
                committed: opt_bool(a, "committed"),
                icebox: opt_bool(a, "icebox").unwrap_or(false),
                inbox: false,
                group: opt_str(a, "group").map(String::from),
                wait: opt_str(a, "wait").map(WaitState::parse).transpose()?,
                stale_days: opt_i64(a, "stale"),
                // MCP `haven_list_items` keeps its existing contract (dead items
                // included); the live-only default is the CLI `item list` view
                // (HV-53), and dead nodes are filtered by `haven_graph`'s `all`.
                include_dead: true,
            };
            // Compact, paginated view: prose + machine fields stripped, bounded by
            // `limit` (default 100) from `offset`. `total` is the full match count,
            // so a truncated page is never silent.
            let all = store.list_items(project, &filter)?;
            let total = all.len();
            let offset = opt_i64(a, "offset").unwrap_or(0).max(0) as usize;
            let limit = opt_i64(a, "limit").unwrap_or(DEFAULT_LIST_LIMIT).max(0) as usize;
            let items: Vec<McpItem> = all
                .iter()
                .skip(offset)
                .take(limit)
                .map(McpItem::compact)
                .collect();
            Ok(json!({
                "total": total,
                "count": items.len(),
                "offset": offset,
                "items": items,
            }))
        }
        "haven_inbox" => {
            // Untriaged floaters: uncommitted, live, no acceptance yet — the
            // triage queue. Same compact, paginated envelope as haven_list_items.
            let filter = ItemFilter {
                owner: opt_str(a, "owner").map(OwnerKind::parse).transpose()?,
                inbox: true,
                ..Default::default()
            };
            let all = store.list_items(project, &filter)?;
            let total = all.len();
            let offset = opt_i64(a, "offset").unwrap_or(0).max(0) as usize;
            let limit = opt_i64(a, "limit").unwrap_or(DEFAULT_LIST_LIMIT).max(0) as usize;
            let items: Vec<McpItem> = all
                .iter()
                .skip(offset)
                .take(limit)
                .map(McpItem::compact)
                .collect();
            Ok(json!({
                "total": total,
                "count": items.len(),
                "offset": offset,
                "items": items,
            }))
        }
        // Cross-store links on a node's artifacts (HV-69): outbound xrefs + inbound
        // backlinks, as one deterministic, sorted report. Read-only; same core
        // method as CLI `haven xref`.
        "haven_xref" => to_value(store.xref(project, req_str(a, "ref")?)?),
        "haven_get_item" => {
            // HV-152: partial-accept — an invalid `include` rejects only the bad
            // key while honouring the valid ones (no whole-set short-circuit).
            // The valid keys load; the bad keys ride an `invalid_include` advisory
            // naming them + the legal set.
            let mut includes = Vec::new();
            let mut invalid: Vec<String> = Vec::new();
            for s in str_array(a, "include") {
                match Include::parse(&s) {
                    Ok(inc) => includes.push(inc),
                    Err(_) => invalid.push(s),
                }
            }
            // HV-154: a dead (superseded/archived) ref still returns the item,
            // but rides a `stale_ref{ref, resolved_to}` hint so the caller learns
            // where the live work moved instead of acting on the dead node.
            let (item, stale) = store.get_item_hinted(project, req_str(a, "ref")?, &includes)?;
            let mut out = to_value(McpItem::full(&item))?;
            attach_stale_ref(&mut out, stale)?;
            if !invalid.is_empty() {
                if let Value::Object(map) = &mut out {
                    map.insert(
                        "invalid_include".into(),
                        json!({ "keys": invalid, "valid": "edges, artifacts, lineage" }),
                    );
                }
            }
            Ok(out)
        }
        "haven_next" => {
            let items = store.next(
                project,
                opt_str(a, "owner").map(OwnerKind::parse).transpose()?,
                opt_i64(a, "limit"),
            )?;
            to_value(items.iter().map(McpItem::compact).collect::<Vec<_>>())
        }
        // Diagnose an empty queue. Returns the same dispatchable count `haven_next`
        // would, plus a per-reason breakdown — for the "next is empty" branch in
        // autonomous loops, so the agent diagnoses instead of inventing work.
        "haven_next_explain" => store.next_explain(
            project,
            opt_str(a, "owner").map(OwnerKind::parse).transpose()?,
        ),
        // Fine ordering within a priority band — exposed over MCP so a remote
        // client (phone/web) can reorder conversationally ("put X before Y"),
        // not just shuffle priority bands. Same core op as CLI `item rank`.
        "haven_rank" => to_value(store.rank_item(
            project,
            req_str(a, "ref")?,
            opt_str(a, "before"),
            opt_str(a, "after"),
        )?),
        "haven_add_item" => {
            let new = NewItem {
                title: req_str(a, "title")?.to_string(),
                node_type: opt_str(a, "type").map(NodeType::parse).transpose()?,
                body: opt_str(a, "body").map(String::from),
                done_looks_like: opt_str(a, "done_looks_like").map(String::from),
                why: opt_str(a, "why").map(String::from),
                due_at: opt_str(a, "due_at").map(String::from),
                status: opt_str(a, "status").map(Status::parse).transpose()?,
                priority: opt_i64(a, "priority"),
                commit: opt_bool(a, "commit").unwrap_or(false),
                assign: opt_str(a, "assign").map(OwnerKind::parse).transpose()?,
                parent: opt_str(a, "parent").map(String::from),
                depends_on: opt_str(a, "depends_on").map(String::from),
                group: opt_str(a, "group").map(String::from),
                metadata: a.get("metadata").cloned(),
            };
            let if_absent = opt_bool(a, "if_absent").unwrap_or(false);
            to_value(store.add_item_checked(project, new, if_absent)?)
        }
        // Bulk add: the CLI `haven import` envelope inline (a JSON array of items,
        // each carrying the item-add fields plus a temp `id` and ref-or-temp-id
        // `parent`/`depends_on`/`group`). One atomic call over the SHARED
        // Store::import_items — same temp-id/forward-ref resolution, if_absent
        // dedupe, all-or-nothing rollback, and HV-159 born-state guard as the CLI.
        "haven_import" => {
            let raw = a.get("items").ok_or_else(|| {
                HavenError::Invalid("missing required arg 'items' (a JSON array)".into())
            })?;
            let items: Vec<ImportItem> = serde_json::from_value(raw.clone())
                .map_err(|e| HavenError::Invalid(format!("invalid import envelope: {e}")))?;
            let if_absent = opt_bool(a, "if_absent").unwrap_or(false);
            to_value(store.import_items(project, items, if_absent)?)
        }
        "haven_update_item" => {
            let reference = req_str(a, "ref")?;
            let commit = opt_bool(a, "commit");
            let priority = opt_i64(a, "priority");
            // Maturity/content fields. When we're also committing, let
            // commit_item own `priority` so a single logical change is one write
            // (one revision bump), not two.
            let wait = match opt_str(a, "wait") {
                None => None,
                Some("none") => Some(WaitUpdate::Clear),
                Some(w) => Some(WaitUpdate::Set(WaitState::parse(w)?)),
            };
            let due = match opt_str(a, "due_at") {
                None => None,
                Some("none") => Some(DueUpdate::Clear),
                Some(d) => Some(DueUpdate::Set(d.to_string())),
            };
            let upd = ItemUpdate {
                title: opt_str(a, "title").map(String::from),
                body: opt_str(a, "body").map(String::from),
                done_looks_like: opt_str(a, "done_looks_like").map(String::from),
                why: opt_str(a, "why").map(String::from),
                status: opt_str(a, "status").map(Status::parse).transpose()?,
                priority: if commit == Some(true) { None } else { priority },
                node_type: opt_str(a, "type").map(NodeType::parse).transpose()?,
                wait,
                due,
            };
            let has_update = upd.title.is_some()
                || upd.body.is_some()
                || upd.done_looks_like.is_some()
                || upd.why.is_some()
                || upd.status.is_some()
                || upd.priority.is_some()
                || upd.node_type.is_some()
                || upd.wait.is_some()
                || upd.due.is_some();
            // HV-154: capture the stale-ref hint up front (this also validates
            // the ref exists, with the enriched not_found, before any write). The
            // update still applies to the dead node; the caller is told it moved.
            let (_node, stale) = store.get_item_hinted(project, reference, &[])?;
            if has_update {
                store.update_item(project, reference, upd)?;
            }
            // Commitment axis.
            match commit {
                Some(true) => {
                    store.commit_item(project, reference, priority)?;
                }
                Some(false) => {
                    store.uncommit_item(project, reference)?;
                }
                None => {}
            }
            // Ownership.
            if let Some(assign) = opt_str(a, "assign") {
                store.assign_item(
                    project,
                    reference,
                    OwnerKind::parse(assign)?,
                    opt_str(a, "actor"),
                )?;
            }
            // Grouping: add the item to a release/phase/gate container (mirrors
            // haven_add_item's `group`; the container is the group/`from` side).
            if let Some(group) = opt_str(a, "group") {
                store.group(project, group, reference, false)?;
            }
            let item = store.get_item(project, reference, &[])?;
            let mut out = to_value(McpItem::full(&item))?;
            attach_stale_ref(&mut out, stale)?;
            Ok(out)
        }
        "haven_add_edge" => {
            let kind = EdgeKind::parse(req_str(a, "kind")?)?;
            let remove = opt_bool(a, "remove").unwrap_or(false);
            // HV-154: a stale (superseded/archived) endpoint never lands silently
            // in the graph — the edge forms, and a `stale_ref` hint tells the
            // caller to re-point it at the live descendant.
            let stale = store.add_edge_hinted(
                project,
                kind,
                req_str(a, "from")?,
                req_str(a, "to")?,
                remove,
            )?;
            let mut out = json!({ "ok": true });
            attach_stale_ref(&mut out, stale)?;
            Ok(out)
        }
        "haven_evolve" => {
            let op = req_str(a, "op")?;
            let refs = str_array(a, "refs");
            let rationale = opt_str(a, "rationale");
            let by = opt_str(a, "by");
            let result = match op {
                "split" => {
                    let source = refs
                        .first()
                        .ok_or_else(|| HavenError::Invalid("split needs refs[0]".into()))?;
                    store.evolve_split(project, source, &str_array(a, "into"), rationale, by)?
                }
                "merge" => {
                    store.evolve_merge(project, &refs, req_str(a, "title")?, rationale, by)?
                }
                "supersede" => {
                    let source = refs
                        .first()
                        .ok_or_else(|| HavenError::Invalid("supersede needs refs[0]".into()))?;
                    store.evolve_supersede(project, source, req_str(a, "with")?, rationale, by)?
                }
                other => {
                    return Err(HavenError::Invalid(format!(
                        "unknown evolve op {other:?} — valid: split, merge, supersede"
                    )))
                }
            };
            to_value(result)
        }
        "haven_lineage" => {
            let dir = opt_str(a, "direction")
                .map(LineageDirection::parse)
                .transpose()?
                .unwrap_or(LineageDirection::Both);
            to_value(store.evolve_graph(project, req_str(a, "ref")?, dir, opt_i64(a, "depth"))?)
        }
        // Follow a possibly-stale ref forward through lineage to its live
        // descendant(s) — handoffs and docs often carry superseded refs. A live
        // item resolves to itself. DEPRECATED (HV-154): the read path now runs
        // this walk automatically and rides a `stale_ref` hint on
        // get_item/update_item/add_edge. Kept one release as a thin alias.
        "haven_resolve_live" => {
            #[allow(deprecated)]
            let items = store.resolve_live(project, req_str(a, "ref")?)?;
            to_value(items.iter().map(McpItem::compact).collect::<Vec<_>>())
        }
        "haven_search" => {
            to_value(store.search(project, req_str(a, "query")?, opt_i64(a, "limit"))?)
        }
        // The whole project graph (all nodes + edges) in one read — for rendering
        // the graph or reasoning over the entire dependency structure at once.
        // Nodes ride as compact-plus-acceptance items (`graph_node`: axes +
        // done_looks_like, the bulk of the payload); live-only by default (drop
        // superseded/archived nodes + any edge that would dangle onto one), with
        // `all:true` to include the dead nodes.
        "haven_graph" => {
            let g = store.project_graph(project, opt_bool(a, "lineage").unwrap_or(false))?;
            let all = opt_bool(a, "all").unwrap_or(false);
            let keep: std::collections::HashSet<&str> = g
                .nodes
                .iter()
                .filter(|n| all || !matches!(n.status, Status::Superseded | Status::Archived))
                .map(|n| n.reference.as_str())
                .collect();
            let nodes: Vec<McpItem> = g
                .nodes
                .iter()
                .filter(|n| keep.contains(n.reference.as_str()))
                .map(McpItem::graph_node)
                .collect();
            let edges: Vec<_> = g
                .edges
                .iter()
                .filter(|e| keep.contains(e.from.as_str()) && keep.contains(e.to.as_str()))
                .collect();
            let mut out = json!({ "project": g.project, "nodes": nodes, "edges": edges });
            // Preserve the original contract: lineage only when non-empty.
            if !g.lineage.is_empty() {
                out["lineage"] = to_value(&g.lineage)?;
            }
            // Grooming nudge (HV-82) rides along only when work has piled up, so
            // a planner reorienting via the graph is prompted to groom first.
            if let Some(nudge) = &g.grooming_nudge {
                out["grooming_nudge"] = json!(nudge);
            }
            Ok(out)
        }
        "haven_docs" => to_value(store.docs(project)?),
        "haven_get_artifact" => {
            let role = opt_str(a, "role").map(ArtifactRole::parse).transpose()?;
            let reference = req_str(a, "ref")?;
            let path = opt_str(a, "path");
            let got = match store.get_artifact(project, reference, role, path) {
                // Content synced to Storage but not on this machine: lazy-pull
                // it (SPEC §5), cache it in the content tree, and retry once.
                Err(HavenError::ContentNotLocal {
                    project: pkey,
                    rel_path,
                    remote_path,
                    content_hash,
                }) => {
                    hydrate_content(
                        store,
                        &pkey,
                        &rel_path,
                        &remote_path,
                        content_hash.as_deref(),
                    )?;
                    store.get_artifact(project, reference, role, path)?
                }
                other => other?,
            };
            to_value(got)
        }
        "haven_add_artifact" => {
            let role = ArtifactRole::parse(req_str(a, "role")?)?;
            // `content` is the write channel for filesystem-less clients; `path`
            // is a local source file (only meaningful to a local server).
            let has_file = opt_str(a, "path").is_some() || opt_str(a, "content").is_some();
            let kind = match opt_str(a, "kind") {
                Some(k) => ArtifactKind::parse(k)?,
                None if has_file => ArtifactKind::File,
                None => ArtifactKind::External,
            };
            let new = NewArtifact {
                role,
                kind,
                file: opt_str(a, "path").map(std::path::PathBuf::from),
                content: opt_str(a, "content").map(String::from),
                name: opt_str(a, "name").map(String::from),
                uri: opt_str(a, "uri").map(String::from),
                title: opt_str(a, "title").map(String::from),
                excerpt: opt_str(a, "excerpt").map(String::from),
                from_owner: opt_str(a, "from").map(OwnerKind::parse).transpose()?,
                to_owner: opt_str(a, "to").map(OwnerKind::parse).transpose()?,
                created_by: opt_str(a, "by").map(String::from),
                // No xref write flag yet — xref metadata is authored via the core
                // NewArtifact path (the read verb + doctor only need read). HV-69.
                metadata: None,
                replace: opt_bool(a, "replace").unwrap_or(false),
            };
            to_value(store.add_artifact(project, req_str(a, "ref")?, new)?)
        }
        "haven_rm_artifact" => {
            let selector = artifact_selector(a)?;
            to_value(store.remove_artifact(project, req_str(a, "ref")?, selector)?)
        }
        "haven_mv_artifact" => {
            let selector = artifact_selector(a)?;
            to_value(store.rename_artifact(
                project,
                req_str(a, "ref")?,
                selector,
                req_str(a, "new_name")?,
            )?)
        }
        "haven_status" => store.store_status(project),
        // One-shot session-context block (HV-23): project state, committed queue
        // (next flagged), in-progress/waiting, conventions, and the untriaged
        // inbox — what a fresh agent reads at session start instead of N discovery
        // calls. Returns the same rendered block the CLI prints, as a `prime`
        // string field (valid JSON for the content envelope). No sticky session:
        // `project` is a per-call arg like every other tool.
        "haven_prime" => Ok(json!({ "prime": store.prime(project)?.render() })),
        // Discover backlogs — a remote/headless client has no local `current_project`
        // to fall back on, so it lists, then selects by passing `project` per call.
        "haven_list_projects" => {
            to_value(store.list_projects(opt_bool(a, "include_archived").unwrap_or(false))?)
        }
        // Start a new backlog remotely.
        "haven_add_project" => to_value(store.add_project(
            req_str(a, "key")?,
            opt_str(a, "prefix"),
            req_str(a, "title")?,
            opt_str(a, "description"),
        )?),
        // Soft-archive a project (the reversible, namespace-reserving retire — the
        // project-level analogue of haven_archive). Required explicit `key`; raw
        // Project returned. There is NO hard-delete tool in this release.
        "haven_archive_project" => to_value(store.archive_project(
            req_str(a, "key")?,
            opt_str(a, "rationale"),
            opt_str(a, "by"),
        )?),
        // Reopen an archived project: total restore (refs continue from the
        // preserved counter). Required explicit `key`; raw Project returned.
        "haven_reopen_project" => {
            to_value(store.reopen_project(req_str(a, "key")?, opt_str(a, "by"))?)
        }
        // Park an item (never hard-delete): status → archived, emits an `archive`
        // lineage event. The MCP equivalent of `haven item archive`.
        "haven_archive" => to_value(store.archive_item(
            project,
            req_str(a, "ref")?,
            opt_str(a, "rationale"),
            opt_str(a, "by"),
        )?),
        // Revive an archived/superseded item back into the maturity flow.
        "haven_reopen" => to_value(store.reopen_item(
            project,
            req_str(a, "ref")?,
            opt_str(a, "rationale"),
            opt_str(a, "by"),
        )?),
        // Atomic baton-pass (ai↔human): record a handoff note, flip owner, set
        // wait/status in one call — the transition agents otherwise botch.
        "haven_handoff" => {
            let to = OwnerKind::parse(req_str(a, "to")?)?;
            let input = HandoffInput {
                from: opt_str(a, "from").map(OwnerKind::parse).transpose()?,
                note: opt_str(a, "note"),
                status: opt_str(a, "status").map(Status::parse).transpose()?,
                wait: opt_str(a, "wait").map(WaitState::parse).transpose()?,
                actor: opt_str(a, "actor"),
            };
            to_value(store.handoff(project, req_str(a, "ref")?, to, input)?)
        }
        // Atomic completion: record evidence, set status=done, and return what
        // this unblocked — the reliable "I finished this" path for agent loops.
        "haven_complete_item" => {
            let input = CompleteInput {
                evidence: opt_str(a, "evidence"),
                artifact_role: opt_str(a, "artifact_role")
                    .map(ArtifactRole::parse)
                    .transpose()?,
                by: opt_str(a, "by"),
            };
            // Re-emit so the (potentially long) `unblocked` list rides as compact
            // items; the completed item itself stays full (minus machine fields).
            let r = store.complete_item(project, req_str(a, "ref")?, input)?;
            Ok(json!({
                "item": McpItem::full(&r.item),
                "artifact": r.artifact,
                "unblocked": r.unblocked.iter().map(McpItem::compact).collect::<Vec<_>>(),
                "warnings": r.warnings,
            }))
        }
        other => Err(HavenError::Invalid(format!("unknown tool {other:?}"))),
    }
}

/// Resolve a remote setting the same way the CLI does (`haven-cli/config.rs`):
/// the `meta` table (set via `haven config set`), falling back to an env var.
fn remote_setting(store: &Store, meta_key: &str, env: &str) -> Result<String> {
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

/// Download one artifact's content from cloud Storage into the local content
/// tree — the lazy-pull half of the content channel (SPEC §5). Mirrors the CLI's
/// wiring: Supabase settings from `meta`/env, token from `$HAVEN_ACCESS_TOKEN`
/// or the keyring (auto-refreshing via Auth0). Without sync configured this
/// errors with the context to fix it, and the artifact read fails as before.
fn hydrate_content(
    store: &Store,
    project_key: &str,
    rel_path: &str,
    remote_path: &str,
    content_hash: Option<&str>,
) -> Result<()> {
    let sync_cfg = haven_sync::SyncConfig::new(
        remote_setting(store, "supabase_url", "HAVEN_SUPABASE_URL").map_err(|e| {
            HavenError::Invalid(format!(
                "content file {rel_path} is in cloud Storage but sync isn't configured here: {e}"
            ))
        })?,
        remote_setting(store, "supabase_anon_key", "HAVEN_SUPABASE_ANON_KEY")?,
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| HavenError::Invalid(format!("async runtime: {e}")))?;
    rt.block_on(async {
        let access = match std::env::var("HAVEN_ACCESS_TOKEN")
            .ok()
            .filter(|t| !t.is_empty())
        {
            Some(t) => t,
            None => {
                // Audience is optional — the ID-token flow doesn't use one
                // (mirrors `haven-cli/config.rs::auth_config`).
                let audience = match store.meta_get("auth0_audience")? {
                    Some(v) => Some(v),
                    None => std::env::var_os("HAVEN_AUTH0_AUDIENCE")
                        .map(|v| v.to_string_lossy().into_owned()),
                };
                let cfg = haven_auth::AuthConfig::new(
                    remote_setting(store, "auth0_domain", "HAVEN_AUTH0_DOMAIN")?,
                    remote_setting(store, "auth0_client_id", "HAVEN_AUTH0_CLIENT_ID")?,
                    audience,
                );
                haven_auth::current_bearer_token(&cfg, &haven_auth::TokenStore::new())
                    .await
                    .map_err(|e| HavenError::Invalid(format!("auth: {e}")))?
            }
        };
        let engine = haven_sync::SyncEngine::new(sync_cfg, access);
        engine
            .hydrate(
                store.content_root(),
                project_key,
                rel_path,
                remote_path,
                content_hash,
            )
            .await
            .map_err(|e| HavenError::Invalid(format!("sync: {e}")))?;
        Ok(())
    })
}

fn to_value<T: Serialize>(v: T) -> Result<Value> {
    Ok(serde_json::to_value(v)?)
}

/// The advertised tool catalogue (SPEC §3). Schemas are intentionally light —
/// enough for a client to know the argument names and which are required.
fn tools_list() -> Value {
    let obj = |props: Value, required: Value| json!({"type": "object", "properties": props, "required": required});
    // One element of `haven_import`'s `items` array — the `haven import` envelope
    // entry (item-add fields + temp `id` + ref-or-temp-id edge fields). Built
    // out here so the catalogue `json!([...])` below stays under the macro
    // recursion limit.
    let import_item_schema = json!({
        "type": "object",
        "properties": {
            "title": {"type": "string"},
            "id": {"type": "string", "description": "Temp id, local to this batch — referenceable by sibling edge fields."},
            "type": {"type": "string", "enum": ["task","code","research","data","design","admin","release","phase","gate","anchor"]},
            "body": {"type": "string"},
            "done_looks_like": {"type": "string"},
            "why": {"type": "string"},
            "status": {"type": "string", "enum": ["discovery","definition","ready","in_progress","blocked","done","superseded","archived"], "description": "Defaults to discovery; engaged states (in_progress/blocked/done) are rejected on import."},
            "priority": {"type": "integer", "minimum": 0, "maximum": 4},
            "commit": {"type": "boolean", "description": "Rejected if true — import mints uncommitted; commit afterwards."},
            "assign": {"type": "string", "enum": ["human","ai"]},
            "parent": {"type": "string", "description": "Decomposition parent: an existing ref or a temp id from this batch."},
            "depends_on": {"type": "array", "items": {"type": "string"}, "description": "Dependencies: existing refs or temp ids from this batch."},
            "group": {"type": "string", "description": "Grouping container (release/phase/gate): an existing ref or a temp id from this batch."}
        },
        "required": ["title"]
    });
    json!([
        { "name": "haven_list_items", "description": "List items in a project under filters. Returns a compact, paginated view {total, count, offset, items[]} — each item carries identity + axes only (ref, title, type, status, committed, owner, priority, wait); fetch prose/detail for one item with haven_get_item. Truncated to `limit` (default 100) from `offset`, in (priority, sort_key, created_at) order; `total` is the full match count. `wait` (on_human|on_dependency|on_external) answers 'what's waiting on me / stuck on X'; `stale` (days) surfaces items untouched for N+ days.",
          "inputSchema": obj(json!({"project":{"type":"string"},"status":{"type":"string","enum":["discovery","definition","ready","in_progress","blocked","done","superseded","archived"]},"type":{"type":"string","enum":["task","code","research","data","design","admin","release","phase","gate","anchor"]},"owner":{"type":"string","enum":["human","ai"]},"committed":{"type":"boolean"},"icebox":{"type":"boolean"},"group":{"type":"string"},"wait":{"type":"string","enum":["on_human","on_dependency","on_external"]},"stale":{"type":"integer"},"limit":{"type":"integer"},"offset":{"type":"integer"}}), json!([])) },
        { "name": "haven_inbox", "description": "Untriaged floaters: uncommitted, live (not archived/superseded), with no acceptance (done_looks_like) set yet — the triage queue behind capture→triage→next. Same compact, paginated {total, count, offset, items[]} envelope as haven_list_items.",
          "inputSchema": obj(json!({"project":{"type":"string"},"owner":{"type":"string","enum":["human","ai"]},"limit":{"type":"integer"},"offset":{"type":"integer"}}), json!([])) },
        { "name": "haven_xref", "description": "Cross-store links (HV-69) on a node's artifacts: a deterministic, sorted {node, outbound[], inbound[]} report. `outbound` is every typed xref {relation, store, target, canonical?} on this node's own artifacts; `inbound` is every other Haven artifact whose xref `target` resolves to this node (backlinks). Read-only. Cross-store targets are reported as-is; only Haven-ref targets resolve to nodes.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_get_item", "description": "Fetch one item in full (prose + requested edges/artifacts/lineage); internal sync fields (public_id/sync_state/revision) are omitted. The detail door for an item shown compactly by haven_list_items/haven_next. If the ref is superseded/archived the item still returns, but the response also carries `stale_ref` {ref, resolved_to:[live ref(s)]} — the work has moved; follow resolved_to. An unknown `include` key is rejected on its own (the valid keys still load) and reported under `invalid_include`.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"project":{"type":"string"},"include":{"type":"array","items":{"type":"string","enum":["edges","artifacts","lineage"]}}}), json!(["ref"])) },
        { "name": "haven_next", "description": "Items ready to dispatch (committed, ready, unblocked). Returns a compact view per item (identity + axes, no prose — fetch full via haven_get_item).",
          "inputSchema": obj(json!({"project":{"type":"string"},"owner":{"type":"string","enum":["human","ai"]},"limit":{"type":"integer"}}), json!([])) },
        { "name": "haven_next_explain", "description": "Diagnose why the dispatch queue is empty: the dispatchable count plus a per-reason breakdown (owner-mismatch, blocked-by-dependency, waiting, committed-not-ready, ready-but-uncommitted) and a hint. Call when haven_next returns nothing — diagnose, don't invent work.",
          "inputSchema": obj(json!({"project":{"type":"string"},"owner":{"type":"string","enum":["human","ai"]}}), json!([])) },
        { "name": "haven_rank", "description": "Reorder an item within its priority band: place it immediately before or after another item (exactly one of `before`/`after`). Fine ordering for 'do X before Y' — use `haven_update_item {priority}` for coarse band moves.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"project":{"type":"string"},"before":{"type":"string"},"after":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_add_item", "description": "Create a work-graph item (node). `done_looks_like` is the acceptance statement output is verified against; `why` is a one-line provenance trace. `due_at` is an optional deadline as a calendar date YYYY-MM-DD (no time/timezone), validated on write. Pass `if_absent: true` to return an existing live item with the same normalized title (marked `existing: true`) instead of creating a duplicate; responses may carry `similar` — up to 3 live items with overlapping titles (advisory).",
          "inputSchema": obj(json!({"title":{"type":"string"},"project":{"type":"string"},"type":{"type":"string","enum":["task","code","research","data","design","admin","release","phase","gate","anchor"],"description":"Node type. Leaves: task (default), code, research, data, design, admin. Containers (the only valid group targets): release, phase, gate. anchor = a long-lived project-docs / overview node."},"body":{"type":"string"},"done_looks_like":{"type":"string"},"why":{"type":"string"},"due_at":{"type":"string"},"status":{"type":"string","enum":["discovery","definition","ready","in_progress","blocked","done","superseded","archived"]},"priority":{"type":"integer","minimum":0,"maximum":4},"commit":{"type":"boolean"},"assign":{"type":"string","enum":["human","ai"]},"parent":{"type":"string"},"depends_on":{"type":"string"},"group":{"type":"string","description":"Add this new item to a release/phase/gate container (creates a grouping edge from that container to this item)."},"if_absent":{"type":"boolean"}}), json!(["title"])) },
        { "name": "haven_import", "description": "Bulk-add an N-node sub-graph in ONE atomic call — the `haven import` envelope inline. `items` is a JSON array; each element carries the haven_add_item fields (title*, type, body, done_looks_like, why, status, priority, commit, assign) PLUS a temp `id` (file-local, lets siblings reference it) and ref-or-temp-id edge fields `parent` / `depends_on` (array) / `group`. Edge targets may be an existing ref OR a temp id from this batch, including forward references (a target appearing later in the array). All-or-nothing: any failure — a bad edge target, a cycle, a born-engaged item — rolls the WHOLE batch back, ref counter included. `if_absent: true` skips items whose normalized title matches a live item (their temp ids resolve to the match). Like haven_add_item, items cannot be born in an engaged state (status in_progress/blocked/done or commit:true), and a `ready` item needs done_looks_like. Returns one outcome per input item (temp `id` echoed, the created/matched item, and `existing`).",
          "inputSchema": obj(json!({"items":{"type":"array","items":import_item_schema},"if_absent":{"type":"boolean"},"project":{"type":"string"}}), json!(["items"])) },
        { "name": "haven_update_item", "description": "Update maturity/commitment/ownership/grouping of an item. Set `done_looks_like` (acceptance) when it becomes ready so dispatch can verify against it. `due_at` sets the YYYY-MM-DD deadline (validated on write); pass `\"none\"` to clear it. Pass `group` to add the item to a release/phase/gate container (mirrors haven_add_item). Returns the updated item in full (same shape as haven_get_item). If the ref is superseded/archived the update still applies, but the response carries `stale_ref` {ref, resolved_to} — re-target the live item. To pass the work baton between ai and human (flip owner + record a note + set wait/status atomically), use haven_handoff, not a bare assign/update here.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"title":{"type":"string"},"body":{"type":"string"},"done_looks_like":{"type":"string"},"why":{"type":"string"},"due_at":{"type":"string"},"status":{"type":"string","enum":["discovery","definition","ready","in_progress","blocked","done","superseded","archived"]},"priority":{"type":"integer","minimum":0,"maximum":4},"type":{"type":"string","enum":["task","code","research","data","design","admin","release","phase","gate","anchor"],"description":"Node type. Leaves: task (default), code, research, data, design, admin. Containers (the only valid group targets): release, phase, gate. anchor = a long-lived project-docs / overview node."},"wait":{"type":"string","enum":["on_human","on_dependency","on_external","none"],"description":"none clears the wait"},"commit":{"type":"boolean"},"assign":{"type":"string","enum":["human","ai"]},"group":{"type":"string","description":"Add this item to a release/phase/gate container (creates a grouping edge from that container to this item). To remove, use haven_add_edge {kind:\"grouping\", remove:true}."},"actor":{"type":"string"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_add_edge", "description": "Add (or `remove:true`) a structural edge; direction matters. decomposition: from=parent → to=child. dependency: from=the blocked item → to=its blocker (the prerequisite). grouping: from=container → to=member, and the container (`from`) MUST be a release/phase/gate node. If an endpoint is superseded/archived the edge still forms, but the response carries `stale_ref` {ref, resolved_to} so you can re-point it at the live item.",
          "inputSchema": obj(json!({"kind":{"type":"string","enum":["decomposition","dependency","grouping"]},"from":{"type":"string"},"to":{"type":"string"},"remove":{"type":"boolean"},"project":{"type":"string"}}), json!(["kind","from","to"])) },
        { "name": "haven_evolve", "description": "Evolve items along lineage. `op` (split|merge|supersede) selects the operation, and which other args are required follows from it: split — refs[0] is the source, `into` lists the new child titles; merge — `refs` are the sources, `title` names the NEW node minted to replace them; supersede — refs[0] is the source folded into the EXISTING node named by `with`. So merge MINTS a new node (needs `title`) while supersede points at an existing one (needs `with`).",
          "inputSchema": obj(json!({"op":{"type":"string","enum":["split","merge","supersede"],"description":"split: refs[0] → new children in `into`. merge: `refs` → a NEW node (requires `title`). supersede: refs[0] folded into an EXISTING node (requires `with`)."},"refs":{"type":"array","items":{"type":"string"}},"into":{"type":"array","items":{"type":"string"},"description":"split only: titles of the new child items."},"with":{"type":"string","description":"supersede only: the EXISTING ref that refs[0] is folded into."},"title":{"type":"string","description":"merge only: the title of the NEW node minted from `refs`."},"rationale":{"type":"string"},"by":{"type":"string"},"project":{"type":"string"}}), json!(["op","refs"])) },
        { "name": "haven_lineage", "description": "Lineage graph around an item.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"direction":{"type":"string","enum":["ancestors","descendants","both"]},"depth":{"type":"integer"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_resolve_live", "description": "DEPRECATED (kept one release) — prefer the automatic stale_ref hint. haven_get_item/haven_update_item/haven_add_edge now resolve a superseded/archived ref forward through lineage automatically and ride a `stale_ref` {ref, resolved_to} field on the success response, so you rarely need to call this directly. Still resolves a possibly superseded/archived ref to its live descendant(s) (a live item resolves to itself); returns compact items.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_search", "description": "Full-text search over item title/body.",
          "inputSchema": obj(json!({"query":{"type":"string"},"project":{"type":"string"},"limit":{"type":"integer"}}), json!(["query"])) },
        { "name": "haven_graph", "description": "The whole project work-graph in one read: every node plus a flat edge list ({kind, from, to}, same shape as haven_add_edge), and optionally lineage links. Use to render the graph or reason over the entire dependency structure at once, instead of N+1 per-node fetches. Nodes are compact (identity + axes + done_looks_like; fetch other prose for one via haven_get_item) and live-only by default — pass `all:true` to include superseded/archived nodes (edges onto dropped nodes are omitted).",
          "inputSchema": obj(json!({"project":{"type":"string"},"lineage":{"type":"boolean"},"all":{"type":"boolean"}}), json!([])) },
        { "name": "haven_docs", "description": "List live project living-doc anchors and their artifacts. Use this instead of hard-coding a docs ref.",
          "inputSchema": obj(json!({"project":{"type":"string"}}), json!([])) },
        { "name": "haven_get_artifact", "description": "Read an artifact's content (local or lazy-pulled).",
          "inputSchema": obj(json!({"ref":{"type":"string"},"role":{"type":"string"},"path":{"type":"string"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_add_artifact", "description": "Register an artifact on an item. Pass `content` to have the server write the file (the content channel for filesystem-less clients), or `path`/`uri` for a local file / external link. `name` sets the destination filename (also for `path`). Re-adding the same filename errors unless `replace:true`, which overwrites in place.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"role":{"type":"string"},"kind":{"type":"string","enum":["file","external","delivery"]},"content":{"type":"string"},"name":{"type":"string"},"replace":{"type":"boolean"},"path":{"type":"string"},"uri":{"type":"string"},"title":{"type":"string"},"from":{"type":"string","enum":["human","ai"]},"to":{"type":"string","enum":["human","ai"]},"project":{"type":"string"}}), json!(["ref","role"])) },
        { "name": "haven_rm_artifact", "description": "Remove an artifact (DB row + backing file) from an item. Select by exactly one of `role`/`name`/`id`; a `role` matching more than one artifact is refused — disambiguate by `name` (the file's basename) or `id` (public_id).",
          "inputSchema": obj(json!({"ref":{"type":"string"},"role":{"type":"string"},"name":{"type":"string"},"id":{"type":"string"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_mv_artifact", "description": "Rename an artifact's backing file (role / history / created_at preserved). Select by exactly one of `role`/`name`/`id` (same ambiguity rule as haven_rm_artifact); `new_name` is a plain filename, rejected if it collides with another artifact's path on the item.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"new_name":{"type":"string"},"role":{"type":"string"},"name":{"type":"string"},"id":{"type":"string"},"project":{"type":"string"}}), json!(["ref","new_name"])) },
        { "name": "haven_status", "description": "Project counts and sync state.",
          "inputSchema": obj(json!({"project":{"type":"string"}}), json!([])) },
        { "name": "haven_prime", "description": "One-shot session-context block: ONE compact, token-budgeted read for session start, instead of separate status + next + list + inbox calls. Returns a `prime` text block covering — project state (key/prefix + counts/sync), the committed-ready queue with the dispatch-eligible (next) items flagged, in-progress/waiting items with owner + what they're waiting on, the load-bearing Haven conventions, and a compact untriaged-inbox view (count + top floaters to triage) so handoff-swept captures resurface. Per-call `project` (no sticky session).",
          "inputSchema": obj(json!({"project":{"type":"string"}}), json!([])) },
        { "name": "haven_list_projects", "description": "List all projects (backlogs). Use this to discover what's available; then target one by passing its `key` as the `project` arg on subsequent calls (selection is per-call, not a stored default). Hides archived projects unless `include_archived:true` (a deleted project is never listed).",
          "inputSchema": obj(json!({"include_archived":{"type":"boolean"}}), json!([])) },
        { "name": "haven_add_project", "description": "Create a new project (backlog / namespace). `key` is the slug used as the `project` arg; `prefix` (e.g. HV) seeds item refs and defaults to the first two letters of the key.",
          "inputSchema": obj(json!({"key":{"type":"string"},"title":{"type":"string"},"prefix":{"type":"string"},"description":{"type":"string"}}), json!(["key","title"])) },
        { "name": "haven_archive_project", "description": "Soft-archive a project: retire it (hidden from default listings, writes into it refused) while keeping its namespace fully RESERVED — key, ref_prefix and the ref counter are untouched, so refs are never reused. Reversible via haven_reopen_project. This is the everyday 'retire this project'; there is no hard-delete tool — archive IS how you drop a project. Required explicit `key` (operates on the namespace, never the implicit current project).",
          "inputSchema": obj(json!({"key":{"type":"string"},"rationale":{"type":"string"},"by":{"type":"string"}}), json!(["key"])) },
        { "name": "haven_reopen_project", "description": "Reopen an archived project: a total restore (nothing was destroyed) — refs continue minting from the preserved counter. Required explicit `key`. Errors if the project is not archived.",
          "inputSchema": obj(json!({"key":{"type":"string"},"by":{"type":"string"}}), json!(["key"])) },
        { "name": "haven_archive", "description": "Park an item: status→archived, emits an append-only lineage event. There is no hard-delete; this is how you 'drop' an item. Reversible via haven_reopen.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"rationale":{"type":"string"},"by":{"type":"string"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_reopen", "description": "Revive an archived/superseded item back into the maturity flow (status→discovery), emitting a lineage event.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"rationale":{"type":"string"},"by":{"type":"string"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_handoff", "description": "Atomic baton-pass (ai↔human): records a handoff note (stamped from/to), flips the owner, and sets wait/status in one call. To a human defaults to blocked + on_human; to ai clears the wait and unblocks. Prefer this over doing assign + update + add_artifact separately.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"to":{"type":"string","enum":["human","ai"]},"from":{"type":"string","enum":["human","ai"]},"note":{"type":"string"},"status":{"type":"string"},"wait":{"type":"string","enum":["on_human","on_dependency","on_external"]},"actor":{"type":"string"},"project":{"type":"string"}}), json!(["ref","to"])) },
        { "name": "haven_complete_item", "description": "Mark an item done: record `evidence` as an artifact (default role delivery), set status=done, and return the items/gates this unblocked (newly dispatchable, as compact items). Warns if no acceptance (done_looks_like) was set. The reliable 'I finished this' path — prefer over a bare status update.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"evidence":{"type":"string"},"artifact_role":{"type":"string"},"by":{"type":"string"},"project":{"type":"string"}}), json!(["ref"])) },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        // Reuse the CLI-independent path: a temp dir as content root + temp DB.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("haven.db");
        let s = Store::open(&db, dir.path()).unwrap();
        s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        s.use_project("haven").unwrap();
        // Keep the tempdir alive for the test's duration by leaking it.
        std::mem::forget(dir);
        s
    }

    /// Drive a session: feed requests as newline JSON, collect responses.
    fn session(store: &Store, requests: &[Value]) -> Vec<Value> {
        let input = requests
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let mut output = Vec::new();
        serve_io(store, io::Cursor::new(input.into_bytes()), &mut output).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn tool_payload(resp: &Value) -> Value {
        // Unwrap the MCP text-content envelope back into JSON.
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    /// Drive `tools/call` through the telemetry chokepoint and return the line.
    fn call_with_telemetry(store: &Store, name: &str, args: Value) -> TelemetryLine {
        let params = json!({ "name": name, "arguments": args });
        let (_resp, line) = handle_tool_call_inner(store, json!(1), &params);
        line.expect("a project/op tool call must produce a telemetry line")
    }

    /// Parse a rendered telemetry line back into its JSON object (HV-166).
    fn parse_line(line: &TelemetryLine) -> Value {
        let rendered = line.render();
        let payload = rendered
            .strip_prefix("haven-telemetry ")
            .expect("telemetry line carries the `haven-telemetry ` prefix");
        serde_json::from_str(payload).expect("telemetry payload is a JSON object")
    }

    #[test]
    fn telemetry_line_present_and_well_formed_for_a_known_call() {
        let s = store();
        // A bare-array tool (haven_next) — HV-153 left its wire shape unchanged, so
        // the telemetry line is the ONLY place its resolved project surfaces.
        let line = call_with_telemetry(&s, "haven_next", json!({ "project": "haven" }));
        let v = parse_line(&line);
        assert_eq!(v["tool"], "haven_next");
        assert_eq!(v["project_passed"], "haven");
        assert_eq!(v["project_resolved"], "haven");
        assert_eq!(v["error_class"], "ok");
        assert!(
            v["latency_ms"].is_u64(),
            "latency_ms must be a numeric millis value, got {:?}",
            v["latency_ms"]
        );
    }

    #[test]
    fn telemetry_surfaces_sticky_project_drift_for_array_tools() {
        // No `project` arg → resolves via the sticky current_project ("haven").
        // project_passed (null) != project_resolved ("haven") is readable from the
        // line — the HV-153 silent drift, now visible even for the array tools.
        let s = store();
        let line = call_with_telemetry(&s, "haven_search", json!({ "query": "x" }));
        let v = parse_line(&line);
        assert!(v["project_passed"].is_null(), "no project was passed");
        assert_eq!(v["project_resolved"], "haven");
        assert_ne!(
            v["project_passed"], v["project_resolved"],
            "the drift must be observable from the line"
        );
    }

    #[test]
    fn telemetry_error_class_buckets_a_not_found() {
        let s = store();
        // Getting a nonexistent ref errors NotFound → error_class "not_found".
        let line = call_with_telemetry(
            &s,
            "haven_get_item",
            json!({ "project": "haven", "ref": "HV-9999" }),
        );
        let v = parse_line(&line);
        assert_eq!(v["error_class"], "not_found");
        // The project still resolved even though the op errored.
        assert_eq!(v["project_resolved"], "haven");
    }

    #[test]
    fn initialize_and_tools_list() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
            ],
        );
        // initialize + tools/list responses; the notification produced none.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["result"]["serverInfo"]["name"], "haven");
        let tools = out[1]["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 31);
        assert!(tools.iter().any(|t| t["name"] == "haven_import"));
        assert!(tools.iter().any(|t| t["name"] == "haven_prime"));
        assert!(tools.iter().any(|t| t["name"] == "haven_inbox"));
        assert!(tools.iter().any(|t| t["name"] == "haven_xref"));
        assert!(tools.iter().any(|t| t["name"] == "haven_next"));
        assert!(tools.iter().any(|t| t["name"] == "haven_next_explain"));
        assert!(tools.iter().any(|t| t["name"] == "haven_resolve_live"));
        assert!(tools.iter().any(|t| t["name"] == "haven_handoff"));
        assert!(tools.iter().any(|t| t["name"] == "haven_complete_item"));
        assert!(tools.iter().any(|t| t["name"] == "haven_graph"));
        assert!(tools.iter().any(|t| t["name"] == "haven_docs"));
        assert!(tools.iter().any(|t| t["name"] == "haven_archive"));
        assert!(tools.iter().any(|t| t["name"] == "haven_list_projects"));
        assert!(tools.iter().any(|t| t["name"] == "haven_archive_project"));
        assert!(tools.iter().any(|t| t["name"] == "haven_reopen_project"));
    }

    #[test]
    fn schema_enums_match_accepted_enum_values() {
        // Every closed-set value enum advertised in a tool schema must be a value
        // the handler's Enum::parse actually accepts — so the schema can't drift
        // from the model (a typo'd enum value would make a valid call look invalid).
        let tools = tools_list();
        let tools = tools.as_array().unwrap();
        let props = |name: &str| -> Value {
            tools.iter().find(|t| t["name"] == name).unwrap()["inputSchema"]["properties"].clone()
        };
        let enum_of = |props: &Value, key: &str| -> Vec<String> {
            props[key]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{key} should carry an enum"))
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        // owner/assign enums are real OwnerKind values.
        for (tool, key) in [
            ("haven_add_item", "assign"),
            ("haven_update_item", "assign"),
            ("haven_next", "owner"),
            ("haven_list_items", "owner"),
            ("haven_add_artifact", "from"),
            ("haven_handoff", "to"),
        ] {
            for v in enum_of(&props(tool), key) {
                assert!(
                    OwnerKind::parse(&v).is_ok(),
                    "{tool}.{key} enum {v:?} not a real OwnerKind"
                );
            }
        }
        // wait enums are real WaitState values; the update_item SETTER also offers
        // the `none` clear-sentinel (the list/handoff filters do not).
        for v in enum_of(&props("haven_update_item"), "wait") {
            assert!(
                v == "none" || WaitState::parse(&v).is_ok(),
                "wait enum {v:?} not WaitState|none"
            );
        }
        for (tool, key) in [("haven_list_items", "wait"), ("haven_handoff", "wait")] {
            for v in enum_of(&props(tool), key) {
                assert!(
                    WaitState::parse(&v).is_ok(),
                    "{tool}.{key} filter enum {v:?} not a real WaitState"
                );
            }
        }
        // lineage direction + artifact kind enums are real.
        for v in enum_of(&props("haven_lineage"), "direction") {
            assert!(
                LineageDirection::parse(&v).is_ok(),
                "direction enum {v:?} not a real LineageDirection"
            );
        }
        for v in enum_of(&props("haven_add_artifact"), "kind") {
            assert!(
                ArtifactKind::parse(&v).is_ok(),
                "kind enum {v:?} not a real ArtifactKind"
            );
        }
        // priority bounds mirror the DB CHECK (priority BETWEEN 0 AND 4).
        let p = props("haven_add_item");
        assert_eq!(p["priority"]["minimum"], 0);
        assert_eq!(p["priority"]["maximum"], 4);
    }

    /// Guard against doc drift: the documented MCP catalogue in the skill's
    /// surface-map must list exactly the tools `tools/list` advertises — no
    /// undocumented tool, no stale doc row. Catalogue rows are table rows whose
    /// first cell is a `haven_*` code span (the CLI→MCP mapping rows lead with a
    /// CLI name, so they're skipped).
    #[test]
    fn surface_map_matches_tools_list() {
        use std::collections::BTreeSet;
        let advertised: BTreeSet<String> = tools_list()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skill/haven/references/surface-map.md"
        );
        let md = std::fs::read_to_string(path).unwrap();
        let documented: BTreeSet<String> = md
            .lines()
            .filter_map(|l| {
                let rest = l.trim_start().strip_prefix("| `haven_")?;
                Some(format!("haven_{}", rest.split('`').next()?))
            })
            .collect();

        assert_eq!(
            advertised,
            documented,
            "surface-map.md catalogue is out of sync with tools/list.\n  only in tools/list: {:?}\n  only in surface-map: {:?}",
            advertised.difference(&documented).collect::<Vec<_>>(),
            documented.difference(&advertised).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn add_item_and_next_via_tools() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item",
                    "arguments":{"title":"Dispatch me","status":"ready","commit":true,"assign":"ai","done_looks_like":"it works"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_next","arguments":{}
                }}),
            ],
        );
        let added = tool_payload(&out[0]);
        assert_eq!(added["ref"], "HV-1");
        assert_eq!(out[0]["result"]["isError"], false);

        let next = tool_payload(&out[1]);
        assert_eq!(next.as_array().unwrap().len(), 1);
        assert_eq!(next[0]["ref"], "HV-1");
    }

    /// HV-23: `haven_prime` returns the one-shot session block over MCP — the same
    /// rendered block the CLI prints, carrying all five sections.
    #[test]
    fn prime_via_tools_returns_the_block() {
        let s = store();
        let out = session(
            &s,
            &[
                // A committed-ready, dispatch-eligible item (queue + next-flagged).
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item",
                    "arguments":{"title":"Ship the API","status":"ready","commit":true,"assign":"ai","done_looks_like":"returns 200"}
                }}),
                // An untriaged floater for the inbox section.
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Loose idea"}
                }}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_prime","arguments":{}
                }}),
            ],
        );
        assert_eq!(out[2]["result"]["isError"], false);
        let payload = tool_payload(&out[2]);
        let block = payload["prime"].as_str().expect("prime is a text block");
        for marker in [
            "PROJECT haven (HV)",
            "QUEUE",
            "> HV-1",
            "IN-PROGRESS / WAITING",
            "CONVENTIONS",
            "INBOX (untriaged: 1)",
            "HV-2",
        ] {
            assert!(block.contains(marker), "missing {marker:?} in:\n{block}");
        }
    }

    /// HV-67: `due_at` over the MCP surface — set on add (full carries it),
    /// absent from the lean `next` compact row, cleared via `"none"`, and a
    /// malformed value rejected as an error.
    #[test]
    fn due_at_via_tools_full_carries_lean_omits_and_clear() {
        let s = store();
        let out = session(
            &s,
            &[
                // 1: add with a deadline → full response carries due_at.
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item",
                    "arguments":{"title":"Dated","status":"ready","commit":true,"assign":"ai","done_looks_like":"done","due_at":"2026-07-01"}
                }}),
                // 2: next → lean/compact row must NOT carry due_at.
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_next","arguments":{}
                }}),
                // 3: get_item full → carries due_at.
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_get_item","arguments":{"ref":"HV-1"}
                }}),
                // 4: clear via the `none` sentinel.
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
                    "name":"haven_update_item","arguments":{"ref":"HV-1","due_at":"none"}
                }}),
                // 5: malformed due_at on add → error.
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Bad","due_at":"2026-13-01"}
                }}),
            ],
        );

        // 1: add response (full shape) carries due_at.
        assert_eq!(out[0]["result"]["isError"], false);
        assert_eq!(tool_payload(&out[0])["due_at"], "2026-07-01");

        // 2: next's compact row omits due_at entirely.
        let next = tool_payload(&out[1]);
        assert_eq!(next[0]["ref"], "HV-1");
        assert!(
            next[0].get("due_at").is_none(),
            "lean next row must omit due_at, got: {}",
            next[0]
        );

        // 3: full get carries it.
        assert_eq!(tool_payload(&out[2])["due_at"], "2026-07-01");

        // 4: clearing drops the key (skip_serializing_if on null).
        assert_eq!(out[3]["result"]["isError"], false);
        assert!(
            tool_payload(&out[3]).get("due_at").is_none(),
            "cleared due_at must be absent from the full shape"
        );

        // 5: a calendar-impossible date is rejected.
        assert_eq!(out[4]["result"]["isError"], true);
    }

    /// HV-125: `haven_next --owner` filters the `owner_kind` (assignment) axis
    /// over the MCP surface — a planner-sealed `owner_kind=ai` leaf is dispatchable
    /// via `haven_next --owner ai`, an unassigned leaf is in NO owner query, and
    /// the full shape no longer carries an `owner_eligible` field.
    #[test]
    fn next_owner_dispatches_assigned_owner_kind() {
        let s = store();
        let out = session(
            &s,
            &[
                // 1: a planner-sealed ready+committed leaf, assigned to ai.
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item",
                    "arguments":{"title":"Sealed","status":"ready","commit":true,"done_looks_like":"done","assign":"ai"}
                }}),
                // 2: an unassigned ready+committed leaf (owner_kind NULL).
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item",
                    "arguments":{"title":"Unassigned","status":"ready","commit":true,"done_looks_like":"done"}
                }}),
                // 3: next --owner ai surfaces ONLY the ai-assigned leaf.
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_next","arguments":{"owner":"ai"}
                }}),
                // 4: next --owner human surfaces neither.
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
                    "name":"haven_next","arguments":{"owner":"human"}
                }}),
                // 5: bare next (no owner) surfaces BOTH ready leaves.
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
                    "name":"haven_next","arguments":{}
                }}),
                // 6: full get carries owner_kind, never owner_eligible.
                json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{
                    "name":"haven_get_item","arguments":{"ref":"HV-1"}
                }}),
            ],
        );

        // 3: the sealed ai leaf is dispatchable to --owner ai (the invariant the
        //    plan->run contract relies on); the unassigned one is not.
        let ai_next = tool_payload(&out[2]);
        assert_eq!(out[2]["result"]["isError"], false);
        assert_eq!(ai_next.as_array().unwrap().len(), 1);
        assert_eq!(ai_next[0]["ref"], "HV-1");
        // 4: never to human.
        assert!(tool_payload(&out[3]).as_array().unwrap().is_empty());
        // 5: bare next ignores ownership — both ready leaves surface.
        assert_eq!(tool_payload(&out[4]).as_array().unwrap().len(), 2);
        // 6: full get carries owner_kind and has no owner_eligible field at all.
        let full = tool_payload(&out[5]);
        assert_eq!(full["owner_kind"], "ai");
        assert!(
            full.get("owner_eligible").is_none(),
            "owner_eligible must be absent from the full shape (HV-125 removed it)"
        );
    }

    #[test]
    fn archive_then_reopen_round_trip() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Park me"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_archive","arguments":{"ref":"HV-1","rationale":"stale"}
                }}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_reopen","arguments":{"ref":"HV-1"}
                }}),
            ],
        );
        // Archive parks it.
        assert_eq!(out[1]["result"]["isError"], false);
        assert_eq!(tool_payload(&out[1])["status"], "archived");
        // Reopen revives it back into the maturity flow.
        assert_eq!(out[2]["result"]["isError"], false);
        assert_eq!(tool_payload(&out[2])["status"], "discovery");
    }

    #[test]
    fn handoff_via_tool() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item",
                    "arguments":{"title":"Build API","assign":"ai","status":"ready","done_looks_like":"it works"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_handoff",
                    "arguments":{"ref":"HV-1","to":"human","note":"please review"}
                }}),
            ],
        );
        assert_eq!(out[1]["result"]["isError"], false);
        let res = tool_payload(&out[1]);
        // One call flipped owner, parked it, and recorded the baton artifact.
        assert_eq!(res["item"]["owner_kind"], "human");
        assert_eq!(res["item"]["status"], "blocked");
        assert_eq!(res["item"]["wait_state"], "on_human");
        assert_eq!(res["artifact"]["role"], "handoff");
        assert_eq!(res["artifact"]["from_owner"], "ai");
        assert_eq!(res["artifact"]["to_owner"], "human");
    }

    #[test]
    fn complete_item_via_tool() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item",
                    "arguments":{"title":"Build it","done_looks_like":"tests pass"}
                }}),
                // A dependent that should become unblocked when HV-1 completes.
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item",
                    "arguments":{"title":"Ship it","depends_on":"HV-1"}
                }}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_complete_item",
                    "arguments":{"ref":"HV-1","evidence":"cargo test: ok"}
                }}),
            ],
        );
        assert_eq!(out[2]["result"]["isError"], false);
        let res = tool_payload(&out[2]);
        assert_eq!(res["item"]["status"], "done");
        assert_eq!(res["artifact"]["role"], "delivery");
        // Acceptance was set → no warnings; HV-2 is reported as unblocked.
        assert!(res["warnings"].as_array().map_or(true, |w| w.is_empty()));
        let unblocked: Vec<&str> = res["unblocked"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["ref"].as_str().unwrap())
            .collect();
        assert_eq!(unblocked, ["HV-2"]);
    }

    #[test]
    fn graph_via_tool() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"API"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"UI"}
                }}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_add_edge","arguments":{"kind":"dependency","from":"HV-2","to":"HV-1"}
                }}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
                    "name":"haven_graph","arguments":{}
                }}),
            ],
        );
        let g = tool_payload(&out[3]);
        assert_eq!(g["nodes"].as_array().unwrap().len(), 2);
        let edges = g["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 1);
        // The edge round-trips the {kind, from, to} shape add_edge took.
        assert_eq!(edges[0]["kind"], "dependency");
        assert_eq!(edges[0]["from"], "HV-2");
        assert_eq!(edges[0]["to"], "HV-1");
    }

    #[test]
    fn docs_via_tool_lists_anchor_artifacts() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item",
                    "arguments":{"title":"Haven docs","type":"anchor","status":"ready","commit":true,"done_looks_like":"docs landed"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_artifact",
                    "arguments":{"ref":"HV-1","role":"vision","content":"Project vision"}
                }}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_docs","arguments":{}
                }}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
                    "name":"haven_next","arguments":{}
                }}),
            ],
        );
        assert_eq!(out[2]["result"]["isError"], false);
        let docs = tool_payload(&out[2]);
        assert_eq!(docs.as_array().unwrap().len(), 1);
        assert_eq!(docs[0]["ref"], "HV-1");
        assert_eq!(docs[0]["type"], "anchor");
        assert_eq!(docs[0]["artifacts"][0]["role"], "vision");
        assert!(tool_payload(&out[3]).as_array().unwrap().is_empty());
    }

    #[test]
    fn next_explain_and_resolve_live_via_tools() {
        let s = store();
        let out = session(
            &s,
            &[
                // Ready but uncommitted: nothing is dispatchable yet.
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Ready, not committed","status":"ready","done_looks_like":"it works"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_next_explain","arguments":{}
                }}),
                // Supersede HV-1 with a fresh HV-2, then resolve the stale ref forward.
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Successor"}
                }}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
                    "name":"haven_evolve","arguments":{"op":"supersede","refs":["HV-1"],"with":"HV-2"}
                }}),
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
                    "name":"haven_resolve_live","arguments":{"ref":"HV-1"}
                }}),
            ],
        );
        // Explain diagnoses the empty queue rather than returning items.
        let explain = tool_payload(&out[1]);
        assert_eq!(explain["dispatchable"], 0);
        assert_eq!(explain["counts"]["ready_but_uncommitted"], 1);
        // The stale ref resolves forward to its live descendant.
        assert_eq!(out[4]["result"]["isError"], false);
        let live = tool_payload(&out[4]);
        let arr = live.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["ref"], "HV-2");
    }

    #[test]
    fn rank_via_tool_reorders_within_a_band() {
        let s = store();
        let out = session(
            &s,
            &[
                // Two committed P1 items; HV-1 sorts first by creation.
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"First","status":"ready","commit":true,"priority":1,"done_looks_like":"it works"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Second","status":"ready","commit":true,"priority":1,"done_looks_like":"it works"}
                }}),
                // Conversational reorder: put Second before First.
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_rank","arguments":{"ref":"HV-2","before":"HV-1"}
                }}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
                    "name":"haven_next","arguments":{}
                }}),
                // Exactly one of before/after is required.
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
                    "name":"haven_rank","arguments":{"ref":"HV-2"}
                }}),
            ],
        );
        assert_eq!(out[2]["result"]["isError"], false);
        let next = tool_payload(&out[3]);
        let refs: Vec<&str> = next
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["ref"].as_str().unwrap())
            .collect();
        assert_eq!(refs, ["HV-2", "HV-1"]);
        // Missing before/after surfaces as a tool error, not a crash.
        assert_eq!(out[4]["result"]["isError"], true);
    }

    #[test]
    fn list_and_add_projects_via_tools() {
        let s = store(); // seeds a "haven" project
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_list_projects","arguments":{}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_project","arguments":{"key":"glyph","title":"Glyph"}
                }}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_list_projects","arguments":{}
                }}),
            ],
        );
        // A remote client can discover the existing backlog.
        let before = tool_payload(&out[0]);
        assert_eq!(before.as_array().unwrap().len(), 1);
        assert_eq!(before[0]["key"], "haven");
        // Create a new one (prefix derives from the key).
        let added = tool_payload(&out[1]);
        assert_eq!(added["key"], "glyph");
        assert_eq!(added["ref_prefix"], "GL");
        // Now both are visible.
        assert_eq!(tool_payload(&out[2]).as_array().unwrap().len(), 2);
    }

    /// HV-123: archive/reopen a project over MCP, and the `include_archived` arg
    /// on `haven_list_projects`. Required explicit `key`; raw Project returned.
    #[test]
    fn archive_reopen_project_via_tools() {
        let s = store(); // seeds a "haven" project, prefix HV
        let out = session(
            &s,
            &[
                // Archive haven, with rationale + by.
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_archive_project",
                    "arguments":{"key":"haven","rationale":"done","by":"alice"}
                }}),
                // Default list hides the archived project.
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_list_projects","arguments":{}
                }}),
                // include_archived shows it.
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_list_projects","arguments":{"include_archived":true}
                }}),
                // Reopen restores it.
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
                    "name":"haven_reopen_project","arguments":{"key":"haven"}
                }}),
                // Now the default list shows it again.
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
                    "name":"haven_list_projects","arguments":{}
                }}),
            ],
        );

        // Archive: raw Project, status flipped, namespace + reason preserved.
        let arch = tool_payload(&out[0]);
        assert_eq!(out[0]["result"]["isError"], false);
        assert_eq!(arch["status"], "archived");
        assert_eq!(arch["archived_reason"], "done");
        assert_eq!(arch["ref_prefix"], "HV");

        assert_eq!(
            tool_payload(&out[1]).as_array().unwrap().len(),
            0,
            "archived hidden by default"
        );
        assert_eq!(
            tool_payload(&out[2]).as_array().unwrap().len(),
            1,
            "include_archived shows it"
        );

        let reopened = tool_payload(&out[3]);
        assert_eq!(reopened["status"], "active");
        assert_eq!(
            tool_payload(&out[4]).as_array().unwrap().len(),
            1,
            "reopened project back in the default list"
        );
    }

    /// HV-123: archive/reopen require an explicit `key` (no implicit current
    /// project); a missing key is a tool error, not a silent op on "haven".
    #[test]
    fn archive_project_requires_explicit_key() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_archive_project","arguments":{}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_reopen_project","arguments":{}
                }}),
            ],
        );
        assert_eq!(out[0]["result"]["isError"], true);
        assert_eq!(tool_payload(&out[0])["error"]["code"], "invalid");
        assert_eq!(out[1]["result"]["isError"], true);
    }

    #[test]
    fn tool_error_is_reported_as_is_error() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_get_item","arguments":{"ref":"HV-999"}
                }}),
            ],
        );
        assert_eq!(out[0]["result"]["isError"], true);
        let payload = tool_payload(&out[0]);
        assert_eq!(payload["error"]["code"], "not_found");
    }

    // ---- HV-154: stale-ref hint + enriched not_found over MCP ---------------

    /// get_item / update_item / add_edge on a SUPERSEDED ref ride a
    /// `stale_ref{ref, resolved_to}` hint on the success response (not a silent
    /// dead item).
    #[test]
    fn stale_ref_hint_rides_get_update_and_add_edge() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Old"}}}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"New"}}}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Other"}}}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
                    "name":"haven_evolve","arguments":{"op":"supersede","refs":["HV-1"],"with":"HV-2"}}}),
                // get on the stale ref: returns the item AND a stale_ref hint.
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
                    "name":"haven_get_item","arguments":{"ref":"HV-1"}}}),
                // update on the stale ref: also flagged.
                json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{
                    "name":"haven_update_item","arguments":{"ref":"HV-1","body":"touched"}}}),
                // add_edge with a stale endpoint: flagged.
                json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{
                    "name":"haven_add_edge","arguments":{"kind":"dependency","from":"HV-1","to":"HV-3"}}}),
                // a LIVE ref carries no hint.
                json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{
                    "name":"haven_get_item","arguments":{"ref":"HV-2"}}}),
            ],
        );

        let got = tool_payload(&out[4]);
        assert_eq!(got["ref"], "HV-1", "the (dead) item still resolves");
        assert_eq!(got["stale_ref"]["ref"], "HV-1");
        assert_eq!(got["stale_ref"]["resolved_to"][0], "HV-2");

        let upd = tool_payload(&out[5]);
        assert_eq!(upd["stale_ref"]["resolved_to"][0], "HV-2");

        let edge = tool_payload(&out[6]);
        assert_eq!(edge["ok"], true);
        assert_eq!(edge["stale_ref"]["ref"], "HV-1");
        assert_eq!(edge["stale_ref"]["resolved_to"][0], "HV-2");

        let live = tool_payload(&out[7]);
        assert!(
            live.get("stale_ref").is_none(),
            "a live ref must not carry a stale_ref hint, got: {live}"
        );
    }

    /// A not_found over MCP carries the nearest-live + prefix hint in the error
    /// message — the example wording from the acceptance.
    #[test]
    fn not_found_over_mcp_carries_nearest_live_and_prefix() {
        let s = store();
        let mut reqs: Vec<Value> = (1..=5)
            .map(|i| {
                json!({"jsonrpc":"2.0","id":i,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":format!("item {i}")}}})
            })
            .collect();
        reqs.push(
            json!({"jsonrpc":"2.0","id":99,"method":"tools/call","params":{
            "name":"haven_get_item","arguments":{"ref":"HV-91"}}}),
        );
        let out = session(&s, &reqs);
        let payload = tool_payload(out.last().unwrap());
        assert_eq!(payload["error"]["code"], "not_found");
        let msg = payload["error"]["message"].as_str().unwrap();
        assert!(msg.contains("closest live:"), "got: {msg}");
        assert!(msg.contains("refs here use prefix HV"), "got: {msg}");
    }

    #[test]
    fn unknown_method_is_jsonrpc_error() {
        let s = store();
        let out = session(
            &s,
            &[json!({"jsonrpc":"2.0","id":9,"method":"bogus","params":{}})],
        );
        assert_eq!(out[0]["error"]["code"], -32601);
    }

    #[test]
    fn add_item_if_absent_dedupes_and_warns_on_similar() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Setup CI"}}}),
                // Sloppier casing/whitespace/punctuation still hits the guard.
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"  setup  ci.","if_absent":true}}}),
                // A near-duplicate without the guard creates, with a warning.
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Setup CI runners"}}}),
            ],
        );
        let first = tool_payload(&out[0]);
        assert_eq!(first["ref"], "HV-1");
        assert!(first.get("existing").is_none());

        let second = tool_payload(&out[1]);
        assert_eq!(second["existing"], true);
        assert_eq!(second["ref"], "HV-1");

        let third = tool_payload(&out[2]);
        assert_eq!(third["ref"], "HV-2");
        assert!(third["similar"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["ref"] == "HV-1"));
    }

    #[test]
    fn list_items_compact_view_and_envelope() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item",
                    "arguments":{"title":"With prose","type":"task","body":"a long body","why":"because","status":"ready","commit":true,"priority":1,"assign":"ai","done_looks_like":"it works"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Second"}
                }}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_list_items","arguments":{}
                }}),
            ],
        );
        let res = tool_payload(&out[2]);
        // Self-describing envelope.
        assert_eq!(res["total"], 2);
        assert_eq!(res["count"], 2);
        assert_eq!(res["offset"], 0);
        let items = res["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        // Priority 1 (non-null) sorts ahead of the unprioritised item.
        let first = &items[0];
        assert_eq!(first["ref"], "HV-1");
        assert_eq!(first["title"], "With prose");
        assert_eq!(first["type"], "task");
        assert_eq!(first["status"], "ready");
        assert_eq!(first["committed"], true);
        assert_eq!(first["owner_kind"], "ai");
        // Prose + machine-only fields dropped from the compact view.
        for k in [
            "body",
            "why",
            "done_looks_like",
            "created_at",
            "updated_at",
            "public_id",
            "sync_state",
            "revision",
            "sort_key",
            "metadata",
            "context_pack",
            "context_pack_clash",
        ] {
            assert!(first.get(k).is_none(), "compact item should omit {k}");
        }
    }

    #[test]
    fn list_items_limit_and_offset_paginate() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"A"}}}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"B"}}}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"C"}}}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"haven_list_items","arguments":{"limit":2}}}),
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"haven_list_items","arguments":{"limit":2,"offset":2}}}),
            ],
        );
        let page1 = tool_payload(&out[3]);
        assert_eq!(page1["total"], 3);
        assert_eq!(page1["count"], 2);
        assert_eq!(page1["offset"], 0);
        let page2 = tool_payload(&out[4]);
        assert_eq!(page2["total"], 3);
        assert_eq!(page2["count"], 1);
        assert_eq!(page2["offset"], 2);
        // Pages are disjoint and together cover all three (ordering is deterministic).
        let refs = |v: &Value| {
            v["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["ref"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        let mut all = refs(&page1);
        all.extend(refs(&page2));
        all.sort();
        all.dedup();
        assert_eq!(all, ["HV-1", "HV-2", "HV-3"]);
    }

    #[test]
    fn inbox_tool_returns_untriaged_floaters_and_drops_on_triage() {
        let s = store();
        let out = session(
            &s,
            &[
                // An untriaged floater: uncommitted, no acceptance.
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"raw idea"}}}),
                // A sealed floater: already has acceptance — excluded from inbox.
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"scoped","done_looks_like":"ships"}}}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"haven_inbox","arguments":{}}}),
                // Triage the floater → it drops out of the inbox.
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"haven_update_item","arguments":{"ref":"HV-1","done_looks_like":"now defined"}}}),
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"haven_inbox","arguments":{}}}),
            ],
        );
        let before = tool_payload(&out[2]);
        assert_eq!(before["total"], 1);
        assert_eq!(before["items"][0]["ref"], "HV-1");
        let after = tool_payload(&out[4]);
        assert_eq!(after["total"], 0);
        assert!(after["items"].as_array().unwrap().is_empty());
    }

    #[test]
    fn graph_tool_carries_container_rollup() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"Phase","type":"phase"}}}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"child","parent":"HV-1","commit":true,"status":"in_progress"}}}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"haven_graph","arguments":{}}}),
            ],
        );
        let graph = tool_payload(&out[2]);
        let nodes = graph["nodes"].as_array().unwrap();
        let phase = nodes.iter().find(|n| n["ref"] == "HV-1").unwrap();
        assert_eq!(phase["rollup_state"], "active");
        // Leaves carry no rollup_state key.
        let child = nodes.iter().find(|n| n["ref"] == "HV-2").unwrap();
        assert!(child.get("rollup_state").is_none());
    }

    #[test]
    fn graph_tool_flags_uncommitted_descendants() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"Track","type":"phase"}}}),
                // Committed + done leaf → the committed subtree is all-done.
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"shipped","parent":"HV-1","commit":true,"status":"done"}}}),
                // Uncommitted floater → invisible to the rollup, but flips the flag.
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"todo","parent":"HV-1"}}}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"haven_graph","arguments":{}}}),
            ],
        );
        let graph = tool_payload(&out[3]);
        let nodes = graph["nodes"].as_array().unwrap();
        let phase = nodes.iter().find(|n| n["ref"] == "HV-1").unwrap();
        // A bare `done` never travels without the honesty flag beside it.
        assert_eq!(phase["rollup_state"], "done");
        assert_eq!(phase["has_uncommitted_descendants"], true);
        // Leaves carry neither derived key.
        let child = nodes.iter().find(|n| n["ref"] == "HV-2").unwrap();
        assert!(child.get("has_uncommitted_descendants").is_none());
    }

    #[test]
    fn context_pack_pointer_rides_get_item_and_graph_but_not_next() {
        let s = store();
        let out = session(
            &s,
            &[
                // A build-batch container that will carry the pack.
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"Checkout — dev batch","type":"phase"}}}),
                // A grouped, dispatchable leaf, plus an unrelated ungrouped one.
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"cart endpoint","commit":true,"status":"ready","assign":"ai","done_looks_like":"it works"}}}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"unrelated"}}}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"haven_add_edge","arguments":{"kind":"grouping","from":"HV-1","to":"HV-2"}}}),
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"haven_add_artifact","arguments":{"ref":"HV-1","role":"context-pack","name":"context-pack.md","content":"# pack"}}}),
                json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"haven_get_item","arguments":{"ref":"HV-2","include":["edges"]}}}),
                json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"haven_get_item","arguments":{"ref":"HV-3"}}}),
                json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"haven_graph","arguments":{}}}),
                json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"haven_next","arguments":{}}}),
            ],
        );
        // get_item on the packed leaf carries the one-hop pointer (container + name).
        let leaf = tool_payload(&out[5]);
        assert_eq!(leaf["context_pack"]["container"], "HV-1");
        assert_eq!(leaf["context_pack"]["artifact"], "context-pack.md");
        assert!(leaf.get("context_pack_clash").is_none());
        // The ungrouped leaf carries no pointer.
        let bare = tool_payload(&out[6]);
        assert!(bare.get("context_pack").is_none());
        // graph_node carries the pointer too, for whole-graph triage.
        let graph = tool_payload(&out[7]);
        let nodes = graph["nodes"].as_array().unwrap();
        let gleaf = nodes.iter().find(|n| n["ref"] == "HV-2").unwrap();
        assert_eq!(gleaf["context_pack"]["container"], "HV-1");
        // next stays lean — no pointer on the compact dispatch view.
        let next = tool_payload(&out[8]);
        let nitem = next
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["ref"] == "HV-2")
            .unwrap();
        assert!(
            nitem.get("context_pack").is_none(),
            "compact dispatch view (next) must stay lean"
        );
    }

    #[test]
    fn update_item_group_attaches_to_container_via_grouping_edge() {
        let s = store();
        let out = session(
            &s,
            &[
                // A phase container and a leaf, initially ungrouped.
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"Phase 1","type":"phase"}}}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"a leaf"}}}),
                // Post-hoc grouping via update_item {group} — symmetric with add_item.
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"haven_update_item","arguments":{"ref":"HV-2","group":"HV-1"}}}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"haven_graph","arguments":{}}}),
            ],
        );
        let g = tool_payload(&out[3]);
        let grouping: Vec<_> = g["edges"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "grouping")
            .collect();
        assert_eq!(
            grouping.len(),
            1,
            "update_item {{group}} must create exactly one grouping edge"
        );
        // The container is the `from`, the member the `to` — the rule the schema now documents.
        assert_eq!(grouping[0]["from"], "HV-1");
        assert_eq!(grouping[0]["to"], "HV-2");
    }

    #[test]
    fn grouping_onto_non_container_errors_with_type_and_direction() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"a task"}}}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"another"}}}),
                // Grouping with a non-container (task) as `from` is refused.
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"haven_add_edge","arguments":{"kind":"grouping","from":"HV-1","to":"HV-2"}}}),
            ],
        );
        assert_eq!(out[2]["result"]["isError"], true);
        let msg = tool_payload(&out[2])["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        // The error names both the container types and the direction (recovery aid).
        assert!(
            msg.contains("release/phase/gate"),
            "must name the container types: {msg}"
        );
        assert!(
            msg.contains("from"),
            "must name the direction (container is the `from`): {msg}"
        );
    }

    #[test]
    fn context_pack_clash_surfaces_both_containers_no_silent_pick() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"Batch A","type":"phase"}}}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"Batch B","type":"phase"}}}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"haven_add_item","arguments":{"title":"shared leaf"}}}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"haven_add_edge","arguments":{"kind":"grouping","from":"HV-1","to":"HV-3"}}}),
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"haven_add_edge","arguments":{"kind":"grouping","from":"HV-2","to":"HV-3"}}}),
                json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"haven_add_artifact","arguments":{"ref":"HV-1","role":"context-pack","name":"context-pack.md","content":"# a"}}}),
                json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"haven_add_artifact","arguments":{"ref":"HV-2","role":"context-pack","name":"context-pack.md","content":"# b"}}}),
                json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"haven_get_item","arguments":{"ref":"HV-3"}}}),
            ],
        );
        // Two packed containers claim one leaf → a clash, surfaced (not picked).
        let leaf = tool_payload(&out[7]);
        assert!(
            leaf.get("context_pack").is_none(),
            "a clash must not silently pick a pack"
        );
        let clash: Vec<&str> = leaf["context_pack_clash"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            clash.contains(&"HV-1") && clash.contains(&"HV-2"),
            "clash lists both containers, got {clash:?}"
        );
    }

    #[test]
    fn get_item_full_includes_prose_and_drops_machine_fields() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item",
                    "arguments":{"title":"Detailed","body":"the body","why":"the why","done_looks_like":"the acceptance"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_get_item","arguments":{"ref":"HV-1"}
                }}),
            ],
        );
        let item = tool_payload(&out[1]);
        // Prose present in the full view.
        assert_eq!(item["body"], "the body");
        assert_eq!(item["why"], "the why");
        assert_eq!(item["done_looks_like"], "the acceptance");
        assert!(item["created_at"].is_string());
        // Machine-only fields dropped even in full.
        for k in ["public_id", "sync_state", "revision", "sort_key"] {
            assert!(
                item.get(k).is_none(),
                "full item should omit machine field {k}"
            );
        }
    }

    #[test]
    fn next_returns_compact_items() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item",
                    "arguments":{"title":"Dispatch","body":"prose","status":"ready","commit":true,"assign":"ai","done_looks_like":"it works"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_next","arguments":{}
                }}),
            ],
        );
        let next = tool_payload(&out[1]);
        let arr = next.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["ref"], "HV-1");
        assert!(arr[0].get("body").is_none());
        assert!(arr[0].get("sync_state").is_none());
    }

    #[test]
    fn complete_unblocked_is_compact() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Build","done_looks_like":"tests pass"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Ship","depends_on":"HV-1"}
                }}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_complete_item","arguments":{"ref":"HV-1","evidence":"done"}
                }}),
            ],
        );
        let res = tool_payload(&out[2]);
        assert_eq!(res["item"]["status"], "done");
        let unblocked = res["unblocked"].as_array().unwrap();
        assert_eq!(unblocked.len(), 1);
        assert_eq!(unblocked[0]["ref"], "HV-2");
        assert!(unblocked[0].get("sync_state").is_none());
        assert!(unblocked[0].get("body").is_none());
    }

    #[test]
    fn graph_is_live_only_compact_with_all_opt_in() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Keep","body":"prose","done_looks_like":"ships"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Dead"}
                }}),
                // HV-1 depends on HV-2; archiving HV-2 should drop both it and the edge.
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_add_edge","arguments":{"kind":"dependency","from":"HV-1","to":"HV-2"}
                }}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
                    "name":"haven_archive","arguments":{"ref":"HV-2","rationale":"dead"}
                }}),
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
                    "name":"haven_graph","arguments":{}
                }}),
                json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{
                    "name":"haven_graph","arguments":{"all":true}
                }}),
            ],
        );
        // Default: archived HV-2 and the now-dangling edge are gone; node is compact.
        let live = tool_payload(&out[4]);
        let live_refs: Vec<&str> = live["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["ref"].as_str().unwrap())
            .collect();
        assert_eq!(live_refs, ["HV-1"]);
        assert!(live["edges"].as_array().unwrap().is_empty());
        assert!(live["nodes"][0].get("body").is_none());
        assert!(live["nodes"][0].get("sync_state").is_none());
        // Graph nodes carry done_looks_like (the planner's sealed-leaf test reads
        // it from one read) while prose like body stays dropped. list/next stay
        // lean — guarded by list_items_compact_view_and_envelope.
        assert_eq!(live["nodes"][0]["done_looks_like"], "ships");
        // all:true: the dead node and its edge come back.
        let full = tool_payload(&out[5]);
        assert_eq!(full["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(full["edges"].as_array().unwrap().len(), 1);
    }

    /// HV-95: the new rm/mv tools over MCP — add an artifact, rename it, remove it.
    #[test]
    fn rm_and_mv_artifact_via_tools() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Has artifacts"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_artifact",
                    "arguments":{"ref":"HV-1","role":"spec","content":"draft","name":"draft.md"}
                }}),
                // Rename draft.md → spec.md (select by name).
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_mv_artifact",
                    "arguments":{"ref":"HV-1","new_name":"spec.md","name":"draft.md"}
                }}),
                // Remove it (now the only spec → role selector is unambiguous).
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
                    "name":"haven_rm_artifact",
                    "arguments":{"ref":"HV-1","role":"spec"}
                }}),
                // Gone: get now errors.
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
                    "name":"haven_get_artifact","arguments":{"ref":"HV-1","role":"spec"}
                }}),
            ],
        );
        assert_eq!(out[2]["result"]["isError"], false);
        assert_eq!(tool_payload(&out[2])["path"], "items/HV-1/spec.md");
        assert_eq!(out[3]["result"]["isError"], false);
        assert_eq!(tool_payload(&out[3])["path"], "items/HV-1/spec.md");
        // After removal the artifact is gone.
        assert_eq!(out[4]["result"]["isError"], true);
    }

    // ---- HV-152: enum/closed-set messages + schema enums + include partial-accept

    /// An invalid `include` key rejects ONLY the bad key while honouring the
    /// valid ones (no whole-set short-circuit) — the valid include still loads,
    /// and an `invalid_include` advisory names the bad key + the legal set.
    #[test]
    fn get_item_invalid_include_honours_valid_keys_and_flags_the_bad_one() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Has edges"}}}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"child","parent":"HV-1"}}}),
                // include has one valid (edges) and one bogus (comments) key.
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_get_item","arguments":{"ref":"HV-1","include":["edges","comments"]}}}),
            ],
        );
        // The call SUCCEEDS (not a whole-set rejection) …
        assert_eq!(
            out[2]["result"]["isError"], false,
            "a bad include key must not fail the whole call"
        );
        let item = tool_payload(&out[2]);
        // … the valid include is honoured (edges loaded) …
        assert!(
            item.get("edges").is_some(),
            "the valid 'edges' include must still load: {item}"
        );
        // … and the bad key is signalled, naming the legal set.
        let inv = &item["invalid_include"];
        let bad: Vec<&str> = inv["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(bad, ["comments"], "names exactly the bad key: {item}");
        let msg = inv["valid"].as_str().unwrap();
        for v in ["edges", "artifacts", "lineage"] {
            assert!(msg.contains(v), "names the legal include set: {msg}");
        }
    }

    /// A get_item with ONLY a bad include key still returns the item (no valid
    /// keys to honour) and flags the bad one — never a silent empty whole-set drop.
    #[test]
    fn get_item_all_invalid_include_still_returns_item_and_flags() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Item"}}}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_get_item","arguments":{"ref":"HV-1","include":["bogus"]}}}),
            ],
        );
        assert_eq!(out[1]["result"]["isError"], false);
        let item = tool_payload(&out[1]);
        assert_eq!(item["ref"], "HV-1");
        assert_eq!(item["invalid_include"]["keys"][0], "bogus");
    }

    /// HV-152: get_item.include, add_item/update_item.type, and list/query status
    /// carry JSON-Schema enums (the three HV-146 left without), and every enum
    /// value is one the matching parser accepts (no schema↔model drift).
    #[test]
    fn type_status_include_carry_schema_enums() {
        let tools = tools_list();
        let tools = tools.as_array().unwrap();
        let props = |name: &str| -> Value {
            tools.iter().find(|t| t["name"] == name).unwrap()["inputSchema"]["properties"].clone()
        };
        let enum_of = |props: &Value, key: &str| -> Vec<String> {
            props[key]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{key} should carry an enum"))
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        // type enums are real NodeType values.
        for tool in ["haven_add_item", "haven_update_item"] {
            let vals = enum_of(&props(tool), "type");
            assert!(!vals.is_empty(), "{tool}.type enum is empty");
            for v in &vals {
                assert!(
                    NodeType::parse(v).is_ok(),
                    "{tool}.type enum {v:?} not a real NodeType"
                );
            }
        }
        // status enums are real Status values.
        for tool in ["haven_list_items", "haven_add_item", "haven_update_item"] {
            let vals = enum_of(&props(tool), "status");
            assert!(!vals.is_empty(), "{tool}.status enum is empty");
            for v in &vals {
                assert!(
                    Status::parse(v).is_ok(),
                    "{tool}.status enum {v:?} not a real Status"
                );
            }
        }
        // include is an ARRAY param — the enum constrains its items.
        let inc: Vec<String> = props("haven_get_item")["include"]["items"]["enum"]
            .as_array()
            .expect("get_item.include.items should carry an enum")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(!inc.is_empty(), "include item enum is empty");
        for v in &inc {
            assert!(
                Include::parse(v).is_ok(),
                "get_item.include enum {v:?} not a real Include"
            );
        }
    }

    /// HV-152 (folds HV-157): haven_evolve `op` is a closed enum [split|merge|
    /// supersede], the description distinguishes merge (new node, needs title)
    /// from supersede (existing node, needs `with`), and per-op required args are
    /// signalled in-schema.
    #[test]
    fn evolve_op_is_enum_with_merge_vs_supersede_described() {
        let tools = tools_list();
        let tools = tools.as_array().unwrap();
        let evolve = tools.iter().find(|t| t["name"] == "haven_evolve").unwrap();
        let op = &evolve["inputSchema"]["properties"]["op"];
        let vals: Vec<&str> = op["enum"]
            .as_array()
            .expect("evolve.op should carry an enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(vals, ["split", "merge", "supersede"]);
        let desc = op["description"].as_str().unwrap();
        // merge mints a NEW node (needs title); supersede folds into an EXISTING
        // node (needs `with`) — both distinctions named in the schema.
        assert!(desc.contains("merge") && desc.contains("supersede"));
        assert!(desc.contains("title"), "merge needs title: {desc}");
        assert!(desc.contains("with"), "supersede needs with: {desc}");
    }

    /// HV-152: a closed-set rejection surfaced over MCP carries the legal set in
    /// the error message (so an agent self-corrects without a tools/list round-trip).
    #[test]
    fn closed_set_rejection_over_mcp_names_the_legal_set() {
        let s = store();
        let out = session(
            &s,
            &[
                // bad status on add → error naming the set + the synonym hint.
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"X","status":"doing"}}}),
                // bad edge kind → error naming the set + the dependency synonym.
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Y"}}}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_add_edge","arguments":{"kind":"depends_on","from":"HV-1","to":"HV-2"}}}),
            ],
        );
        assert_eq!(out[0]["result"]["isError"], true);
        let msg = tool_payload(&out[0])["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(msg.contains("in_progress"), "status set named: {msg}");
        assert!(msg.contains("in_progress"), "synonym mapped: {msg}");

        assert_eq!(out[2]["result"]["isError"], true);
        let msg = tool_payload(&out[2])["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            msg.contains("dependency"),
            "edge set + synonym named: {msg}"
        );
    }

    // ---- HV-153: ref-omitting call echoes the resolved project; sticky warns ---

    /// A project-scoped success response echoes the resolved project key, and a
    /// call that fell back to the sticky `current_project` (no explicit `project`
    /// arg) ALSO carries a `project_warning` naming it — so a drifting sticky
    /// default is observable, not silent (HV-153). An explicit `project` echoes
    /// the key with NO warning.
    #[test]
    fn project_resolved_echoes_and_sticky_fallback_warns() {
        let s = store(); // seeds + selects "haven" as the sticky current_project
        let out = session(
            &s,
            &[
                // 1: explicit project → echoes project_resolved, NO warning.
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Explicit","project":"haven"}}}),
                // 2: omitted project → resolves via sticky, echoes + WARNS.
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Sticky"}}}),
                // 3: a read (get_item) omitting project also echoes + warns.
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"haven_get_item","arguments":{"ref":"HV-1"}}}),
                // 4: a read WITH explicit project echoes, no warning.
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
                    "name":"haven_get_item","arguments":{"ref":"HV-1","project":"haven"}}}),
                // 5: a list (compact envelope) omitting project echoes + warns.
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
                    "name":"haven_list_items","arguments":{}}}),
            ],
        );

        // 1: explicit → echo, no warning.
        let explicit = tool_payload(&out[0]);
        assert_eq!(explicit["project_resolved"], "haven");
        assert!(
            explicit.get("project_warning").is_none(),
            "explicit project must not warn: {explicit}"
        );

        // 2: sticky → echo + warning naming the resolved project.
        let sticky = tool_payload(&out[1]);
        assert_eq!(sticky["project_resolved"], "haven");
        let warn = sticky["project_warning"].as_str().unwrap();
        assert!(
            warn.contains("haven"),
            "sticky fallback warning must name the resolved project: {warn}"
        );

        // 3: read via sticky → echo + warn.
        let read_sticky = tool_payload(&out[2]);
        assert_eq!(read_sticky["project_resolved"], "haven");
        assert!(read_sticky.get("project_warning").is_some());

        // 4: read explicit → echo, no warn.
        let read_explicit = tool_payload(&out[3]);
        assert_eq!(read_explicit["project_resolved"], "haven");
        assert!(read_explicit.get("project_warning").is_none());

        // 5: list envelope (object) carries the echo + warning too.
        let list = tool_payload(&out[4]);
        assert_eq!(list["project_resolved"], "haven");
        assert!(list.get("project_warning").is_some());
    }

    /// The project echo/warning is NOT hard-requiring `project` — a ref-omitting
    /// call still succeeds (the footgun is mitigated, not removed), and the
    /// GLOBAL tools (list_projects/add_project) carry no project echo at all.
    #[test]
    fn project_echo_does_not_hard_require_and_skips_global_tools() {
        let s = store();
        let out = session(
            &s,
            &[
                // A project-less call still succeeds (no hard requirement).
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"No project arg"}}}),
                // A global tool carries no project_resolved/warning.
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_list_projects","arguments":{}}}),
            ],
        );
        assert_eq!(
            out[0]["result"]["isError"], false,
            "must not hard-require project"
        );
        // list_projects is global → returns a bare array, no project echo.
        let projects = tool_payload(&out[1]);
        assert!(
            projects.is_array(),
            "list_projects returns the project array"
        );
    }

    /// HV-155 round-trip: an import envelope (item-add fields + temp `id` and
    /// ref-or-temp-id edge fields) commits an N-node subgraph in one atomic
    /// `haven_import` call, reusing `Store::import_items` (same temp-id /
    /// forward-ref resolution as `haven import`). Verified envelope-in →
    /// graph-out via `haven_graph`.
    #[test]
    fn import_round_trips_subgraph_via_graph() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","method":"tools/call","id":1,"params":{
                "name":"haven_import","arguments":{"items":[
                    // Forward ref: parent "epic" appears later.
                    {"id":"api","title":"Build API","parent":"epic","depends_on":["ui"]},
                    {"id":"ui","title":"Build UI","group":"phase1"},
                    {"id":"epic","title":"Auth epic"},
                    {"id":"phase1","title":"Phase 1","type":"phase"}
                ]}}}),
                json!({"jsonrpc":"2.0","method":"tools/call","id":2,"params":{
                    "name":"haven_graph","arguments":{}}}),
            ],
        );

        assert_eq!(out[0]["result"]["isError"], false);
        let outcomes = tool_payload(&out[0]);
        let outcomes = outcomes.as_array().unwrap();
        assert_eq!(outcomes.len(), 4);
        // Temp ids are echoed back for correlation, sequential refs minted.
        assert_eq!(outcomes[0]["id"], "api");
        assert_eq!(outcomes[0]["ref"], "HV-1");
        assert_eq!(outcomes[3]["ref"], "HV-4");

        // Envelope-in → graph-out: every edge resolved (incl. temp-id +
        // forward-ref targets).
        let g = tool_payload(&out[1]);
        let edges = g["edges"].as_array().unwrap();
        let has = |kind: &str, from: &str, to: &str| {
            edges
                .iter()
                .any(|e| e["kind"] == kind && e["from"] == from && e["to"] == to)
        };
        assert!(has("decomposition", "HV-3", "HV-1"), "epic ⊃ api: {g}");
        assert!(has("dependency", "HV-1", "HV-2"), "api → ui: {g}");
        assert!(has("grouping", "HV-4", "HV-2"), "phase1 ∋ ui: {g}");
        assert_eq!(g["nodes"].as_array().unwrap().len(), 4);
    }

    /// HV-155 rollback: one bad edge target rolls the WHOLE batch back —
    /// node count AND the minted ref counter restored (the all-or-nothing
    /// transaction of `Store::import_items`, surfaced over MCP).
    #[test]
    fn import_rolls_back_on_bad_edge_target() {
        let s = store();
        let out = session(
            &s,
            &[
                // Establish a baseline node so ref_counter starts at 1.
                json!({"jsonrpc":"2.0","method":"tools/call","id":1,"params":{
                    "name":"haven_add_item","arguments":{"title":"Anchor"}}}),
                // One good item + one with a dangling edge target → whole batch fails.
                json!({"jsonrpc":"2.0","method":"tools/call","id":2,"params":{
                "name":"haven_import","arguments":{"items":[
                    {"id":"good","title":"Would-be item"},
                    {"title":"Dangling","depends_on":["nope"]}
                ]}}}),
                // The graph still holds only the anchor — nothing from the batch.
                json!({"jsonrpc":"2.0","method":"tools/call","id":3,"params":{
                    "name":"haven_graph","arguments":{}}}),
                // ref_counter restored: the next add mints HV-2, not HV-4.
                json!({"jsonrpc":"2.0","method":"tools/call","id":4,"params":{
                    "name":"haven_add_item","arguments":{"title":"After rollback"}}}),
            ],
        );

        assert_eq!(out[0]["result"]["isError"], false);
        assert_eq!(
            out[1]["result"]["isError"], true,
            "bad edge target must fail"
        );
        let g = tool_payload(&out[2]);
        assert_eq!(
            g["nodes"].as_array().unwrap().len(),
            1,
            "only the anchor survives: {g}"
        );
        // ref_counter rolled back → the post-rollback add is HV-2.
        assert_eq!(tool_payload(&out[3])["ref"], "HV-2");
    }

    /// HV-155 inherits HV-159: an engaged-state payload over `haven_import` is
    /// rejected by the SHARED `Store::import_items` guard (not re-implemented in
    /// the MCP layer) — no engaged-born item can be minted via the bulk op.
    #[test]
    fn import_inherits_born_state_guard() {
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","method":"tools/call","id":1,"params":{
                "name":"haven_import","arguments":{"items":[
                    {"id":"x","title":"Born running","status":"in_progress"}
                ]}}}),
                // commit:true is likewise refused.
                json!({"jsonrpc":"2.0","method":"tools/call","id":2,"params":{
                "name":"haven_import","arguments":{"items":[
                    {"id":"y","title":"Born committed","commit":true}
                ]}}}),
                // Nothing was minted by either rejected batch.
                json!({"jsonrpc":"2.0","method":"tools/call","id":3,"params":{
                    "name":"haven_graph","arguments":{}}}),
            ],
        );
        assert_eq!(out[0]["result"]["isError"], true, "engaged status rejected");
        assert_eq!(out[1]["result"]["isError"], true, "commit:true rejected");
        let g = tool_payload(&out[2]);
        assert_eq!(
            g["nodes"].as_array().unwrap().len(),
            0,
            "nothing minted: {g}"
        );
    }

    /// HV-155: the bulk op is mutating (it triggers the daily-backup chokepoint)
    /// and `if_absent` dedupe rides through to the shared core.
    #[test]
    fn import_is_mutating_and_dedupes_with_if_absent() {
        assert!(is_mutating_tool("haven_import"));
        let s = store();
        let out = session(
            &s,
            &[
                json!({"jsonrpc":"2.0","method":"tools/call","id":1,"params":{
                    "name":"haven_add_item","arguments":{"title":"Setup CI"}}}),
                json!({"jsonrpc":"2.0","method":"tools/call","id":2,"params":{
                "name":"haven_import","arguments":{"if_absent":true,"items":[
                    {"title":"setup  ci."},
                    {"title":"Run tests"}
                ]}}}),
            ],
        );
        assert_eq!(out[1]["result"]["isError"], false);
        let outcomes = tool_payload(&out[1]);
        let outcomes = outcomes.as_array().unwrap();
        // First matched the pre-existing node; only "Run tests" is new.
        // `existing` is skip_serializing_if-false, so a new item simply omits it.
        assert_eq!(outcomes[0]["existing"], true);
        assert_eq!(outcomes[0]["ref"], "HV-1");
        assert!(
            outcomes[1].get("existing").is_none(),
            "new item omits existing"
        );
        assert_eq!(outcomes[1]["ref"], "HV-2");
    }
}
