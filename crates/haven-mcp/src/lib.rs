//! `haven-mcp` — a hand-rolled stdio JSON-RPC 2.0 MCP server over the
//! `haven-core` `Store` (SPEC §3). Newline-delimited JSON messages (the MCP
//! stdio framing). Every `haven_*` tool is a thin wrapper over the exact same
//! `Store` method the CLI calls, so the two surfaces cannot drift.
//!
//! The protocol surface needed is tiny — `initialize`, `tools/list`,
//! `tools/call` (+ the `initialized` notification and `ping`) — so we implement
//! it directly rather than taking an SDK dependency.

use std::io::{self, BufRead, Write};

use haven_core::{
    ArtifactKind, ArtifactRole, CompleteInput, EdgeKind, HandoffInput, HavenError, Include,
    ItemFilter, ItemUpdate, LineageDirection, NewArtifact, NewItem, NodeType, OwnerKind, Result,
    Status, Store, WaitState, WaitUpdate,
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
/// and a `HavenError` as an `isError` tool result (so the model sees it).
fn handle_tool_call(store: &Store, id: Value, params: &Value) -> Response {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return error_response(id, -32602, "tools/call missing 'name'".into()),
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match call_tool(store, name, &args) {
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
    }
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

/// Dispatch a `haven_*` tool to the matching `Store` method. Returns the raw
/// JSON payload (wrapped into MCP content by the caller).
fn call_tool(store: &Store, name: &str, a: &Value) -> Result<Value> {
    let project = opt_str(a, "project");
    match name {
        "haven_list_items" => {
            let filter = ItemFilter {
                status: opt_str(a, "status").map(Status::parse).transpose()?,
                node_type: opt_str(a, "type").map(NodeType::parse).transpose()?,
                owner: opt_str(a, "owner").map(OwnerKind::parse).transpose()?,
                committed: opt_bool(a, "committed"),
                icebox: opt_bool(a, "icebox").unwrap_or(false),
                group: opt_str(a, "group").map(String::from),
                wait: opt_str(a, "wait").map(WaitState::parse).transpose()?,
                stale_days: opt_i64(a, "stale"),
            };
            to_value(store.list_items(project, &filter)?)
        }
        "haven_get_item" => {
            let includes = str_array(a, "include")
                .iter()
                .map(|s| Include::parse(s))
                .collect::<Result<Vec<_>>>()?;
            to_value(store.get_item(project, req_str(a, "ref")?, &includes)?)
        }
        "haven_next" => to_value(store.next(
            project,
            opt_str(a, "owner").map(OwnerKind::parse).transpose()?,
            opt_i64(a, "limit"),
        )?),
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
            let upd = ItemUpdate {
                title: opt_str(a, "title").map(String::from),
                body: opt_str(a, "body").map(String::from),
                done_looks_like: opt_str(a, "done_looks_like").map(String::from),
                why: opt_str(a, "why").map(String::from),
                status: opt_str(a, "status").map(Status::parse).transpose()?,
                priority: if commit == Some(true) { None } else { priority },
                node_type: opt_str(a, "type").map(NodeType::parse).transpose()?,
                wait,
            };
            let has_update = upd.title.is_some()
                || upd.body.is_some()
                || upd.done_looks_like.is_some()
                || upd.why.is_some()
                || upd.status.is_some()
                || upd.priority.is_some()
                || upd.node_type.is_some()
                || upd.wait.is_some();
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
            to_value(store.get_item(project, reference, &[])?)
        }
        "haven_add_edge" => {
            let kind = EdgeKind::parse(req_str(a, "kind")?)?;
            let remove = opt_bool(a, "remove").unwrap_or(false);
            store.add_edge(
                project,
                kind,
                req_str(a, "from")?,
                req_str(a, "to")?,
                remove,
            )?;
            Ok(json!({ "ok": true }))
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
                other => return Err(HavenError::Invalid(format!("unknown evolve op {other:?}"))),
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
        // item resolves to itself.
        "haven_resolve_live" => to_value(store.resolve_live(project, req_str(a, "ref")?)?),
        "haven_search" => {
            to_value(store.search(project, req_str(a, "query")?, opt_i64(a, "limit"))?)
        }
        // The whole project graph (all nodes + edges) in one read — for rendering
        // the graph or reasoning over the entire dependency structure at once.
        "haven_graph" => {
            to_value(store.project_graph(project, opt_bool(a, "lineage").unwrap_or(false))?)
        }
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
            };
            to_value(store.add_artifact(project, req_str(a, "ref")?, new)?)
        }
        "haven_status" => store.store_status(project),
        // Discover backlogs — a remote/headless client has no local `current_project`
        // to fall back on, so it lists, then selects by passing `project` per call.
        "haven_list_projects" => to_value(store.list_projects()?),
        // Start a new backlog remotely.
        "haven_add_project" => to_value(store.add_project(
            req_str(a, "key")?,
            opt_str(a, "prefix"),
            req_str(a, "title")?,
            opt_str(a, "description"),
        )?),
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
            to_value(store.complete_item(project, req_str(a, "ref")?, input)?)
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
    json!([
        { "name": "haven_list_items", "description": "List items in a project under filters. `wait` (on_human|on_dependency|on_external) answers 'what's waiting on me / stuck on X'; `stale` (days) surfaces items untouched for N+ days.",
          "inputSchema": obj(json!({"project":{"type":"string"},"status":{"type":"string"},"type":{"type":"string"},"owner":{"type":"string"},"committed":{"type":"boolean"},"icebox":{"type":"boolean"},"group":{"type":"string"},"wait":{"type":"string"},"stale":{"type":"integer"}}), json!([])) },
        { "name": "haven_get_item", "description": "Fetch one item, optionally with edges/artifacts/lineage.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"project":{"type":"string"},"include":{"type":"array","items":{"type":"string"}}}), json!(["ref"])) },
        { "name": "haven_next", "description": "Items ready to dispatch (committed, ready, unblocked).",
          "inputSchema": obj(json!({"project":{"type":"string"},"owner":{"type":"string"},"limit":{"type":"integer"}}), json!([])) },
        { "name": "haven_next_explain", "description": "Diagnose why the dispatch queue is empty: the dispatchable count plus a per-reason breakdown (owner-mismatch, blocked-by-dependency, waiting, committed-not-ready, ready-but-uncommitted) and a hint. Call when haven_next returns nothing — diagnose, don't invent work.",
          "inputSchema": obj(json!({"project":{"type":"string"},"owner":{"type":"string"}}), json!([])) },
        { "name": "haven_rank", "description": "Reorder an item within its priority band: place it immediately before or after another item (exactly one of `before`/`after`). Fine ordering for 'do X before Y' — use `haven_update_item {priority}` for coarse band moves.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"project":{"type":"string"},"before":{"type":"string"},"after":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_add_item", "description": "Create a work-graph item (node). `done_looks_like` is the acceptance statement output is verified against; `why` is a one-line provenance trace. Pass `if_absent: true` to return an existing live item with the same normalized title (marked `existing: true`) instead of creating a duplicate; responses may carry `similar` — up to 3 live items with overlapping titles (advisory).",
          "inputSchema": obj(json!({"title":{"type":"string"},"project":{"type":"string"},"type":{"type":"string"},"body":{"type":"string"},"done_looks_like":{"type":"string"},"why":{"type":"string"},"status":{"type":"string"},"priority":{"type":"integer"},"commit":{"type":"boolean"},"assign":{"type":"string"},"parent":{"type":"string"},"depends_on":{"type":"string"},"group":{"type":"string"},"if_absent":{"type":"boolean"}}), json!(["title"])) },
        { "name": "haven_update_item", "description": "Update maturity/commitment/ownership of an item. Set `done_looks_like` (acceptance) when it becomes ready so dispatch can verify against it.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"title":{"type":"string"},"body":{"type":"string"},"done_looks_like":{"type":"string"},"why":{"type":"string"},"status":{"type":"string"},"priority":{"type":"integer"},"type":{"type":"string"},"wait":{"type":"string"},"commit":{"type":"boolean"},"assign":{"type":"string"},"actor":{"type":"string"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_add_edge", "description": "Add/remove a decomposition|dependency|grouping edge.",
          "inputSchema": obj(json!({"kind":{"type":"string"},"from":{"type":"string"},"to":{"type":"string"},"remove":{"type":"boolean"},"project":{"type":"string"}}), json!(["kind","from","to"])) },
        { "name": "haven_evolve", "description": "Split/merge/supersede items (lineage).",
          "inputSchema": obj(json!({"op":{"type":"string"},"refs":{"type":"array","items":{"type":"string"}},"into":{"type":"array","items":{"type":"string"}},"with":{"type":"string"},"title":{"type":"string"},"rationale":{"type":"string"},"project":{"type":"string"}}), json!(["op","refs"])) },
        { "name": "haven_lineage", "description": "Lineage graph around an item.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"direction":{"type":"string"},"depth":{"type":"integer"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_resolve_live", "description": "Resolve a possibly superseded/archived item ref forward through lineage to its live descendant(s); a live item resolves to itself. Use to follow stale refs found in handoffs or docs.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_search", "description": "Full-text search over item title/body.",
          "inputSchema": obj(json!({"query":{"type":"string"},"project":{"type":"string"},"limit":{"type":"integer"}}), json!(["query"])) },
        { "name": "haven_graph", "description": "The whole project work-graph in one read: every node plus a flat edge list ({kind, from, to}, same shape as haven_add_edge), and optionally lineage links. Use to render the graph or reason over the entire dependency structure at once, instead of N+1 per-node fetches. Nodes include superseded/archived — filter client-side.",
          "inputSchema": obj(json!({"project":{"type":"string"},"lineage":{"type":"boolean"}}), json!([])) },
        { "name": "haven_get_artifact", "description": "Read an artifact's content (local or lazy-pulled).",
          "inputSchema": obj(json!({"ref":{"type":"string"},"role":{"type":"string"},"path":{"type":"string"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_add_artifact", "description": "Register an artifact on an item. Pass `content` to have the server write the file (the content channel for filesystem-less clients), or `path`/`uri` for a local file / external link.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"role":{"type":"string"},"kind":{"type":"string"},"content":{"type":"string"},"name":{"type":"string"},"path":{"type":"string"},"uri":{"type":"string"},"title":{"type":"string"},"from":{"type":"string"},"to":{"type":"string"},"project":{"type":"string"}}), json!(["ref","role"])) },
        { "name": "haven_status", "description": "Project counts and sync state.",
          "inputSchema": obj(json!({"project":{"type":"string"}}), json!([])) },
        { "name": "haven_list_projects", "description": "List all projects (backlogs). Use this to discover what's available; then target one by passing its `key` as the `project` arg on subsequent calls (selection is per-call, not a stored default).",
          "inputSchema": obj(json!({}), json!([])) },
        { "name": "haven_add_project", "description": "Create a new project (backlog / namespace). `key` is the slug used as the `project` arg; `prefix` (e.g. HV) seeds item refs and defaults to the first two letters of the key.",
          "inputSchema": obj(json!({"key":{"type":"string"},"title":{"type":"string"},"prefix":{"type":"string"},"description":{"type":"string"}}), json!(["key","title"])) },
        { "name": "haven_archive", "description": "Park an item: status→archived, emits an append-only lineage event. There is no hard-delete; this is how you 'drop' an item. Reversible via haven_reopen.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"rationale":{"type":"string"},"by":{"type":"string"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_reopen", "description": "Revive an archived/superseded item back into the maturity flow (status→discovery), emitting a lineage event.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"rationale":{"type":"string"},"by":{"type":"string"},"project":{"type":"string"}}), json!(["ref"])) },
        { "name": "haven_handoff", "description": "Atomic baton-pass (ai↔human): records a handoff note (stamped from/to), flips the owner, and sets wait/status in one call. To a human defaults to blocked + on_human; to ai clears the wait and unblocks. Prefer this over doing assign + update + add_artifact separately.",
          "inputSchema": obj(json!({"ref":{"type":"string"},"to":{"type":"string"},"from":{"type":"string"},"note":{"type":"string"},"status":{"type":"string"},"wait":{"type":"string"},"actor":{"type":"string"},"project":{"type":"string"}}), json!(["ref","to"])) },
        { "name": "haven_complete_item", "description": "Mark an item done: record `evidence` as an artifact (default role delivery), set status=done, and return the items/gates this unblocked (newly dispatchable). Warns if no acceptance (done_looks_like) was set. The reliable 'I finished this' path — prefer over a bare status update.",
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
        assert_eq!(tools.len(), 22);
        assert!(tools.iter().any(|t| t["name"] == "haven_next"));
        assert!(tools.iter().any(|t| t["name"] == "haven_next_explain"));
        assert!(tools.iter().any(|t| t["name"] == "haven_resolve_live"));
        assert!(tools.iter().any(|t| t["name"] == "haven_handoff"));
        assert!(tools.iter().any(|t| t["name"] == "haven_complete_item"));
        assert!(tools.iter().any(|t| t["name"] == "haven_graph"));
        assert!(tools.iter().any(|t| t["name"] == "haven_archive"));
        assert!(tools.iter().any(|t| t["name"] == "haven_list_projects"));
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
                    "arguments":{"title":"Dispatch me","status":"ready","commit":true,"assign":"ai"}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_next","arguments":{"owner":"ai"}
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
                    "arguments":{"title":"Build API","assign":"ai","status":"ready"}
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
    fn next_explain_and_resolve_live_via_tools() {
        let s = store();
        let out = session(
            &s,
            &[
                // Ready but uncommitted: nothing is dispatchable yet.
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Ready, not committed","status":"ready"}
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
                    "name":"haven_add_item","arguments":{"title":"First","status":"ready","commit":true,"priority":1}
                }}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"haven_add_item","arguments":{"title":"Second","status":"ready","commit":true,"priority":1}
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
}
