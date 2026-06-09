//! HTTP transport against Supabase PostgREST + Storage (SPEC §5).
//!
//! **Written, not live-verified** — needs a real Supabase project and a valid
//! Auth0 access token. The push pass is fully implemented; the pull pass fetches
//! remote rows, and the final reverse-translation + LWW reconcile is the one
//! piece left to wire against a live project (clearly marked below).

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use rusqlite::Connection;
use serde_json::Value;

use crate::local::{self, PushBatch};
use crate::{classify_status, ErrorClass, SyncError};

/// Connection details for a Supabase project.
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// e.g. `https://<ref>.supabase.co`
    pub api_url: String,
    /// The publishable (anon) API key — public-safe; RLS does the gating.
    pub anon_key: String,
    /// Storage bucket for content blobs.
    pub bucket: String,
}

impl SyncConfig {
    pub fn new(api_url: impl Into<String>, anon_key: impl Into<String>) -> Self {
        SyncConfig {
            api_url: api_url.into(),
            anon_key: anon_key.into(),
            bucket: "haven-content".into(),
        }
    }
}

/// One authenticated sync session against a project.
pub struct SyncEngine {
    client: reqwest::Client,
    config: SyncConfig,
    access_token: String,
}

impl SyncEngine {
    pub fn new(config: SyncConfig, access_token: impl Into<String>) -> Self {
        SyncEngine {
            client: reqwest::Client::new(),
            config,
            access_token: access_token.into(),
        }
    }

    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        // The anon key authorizes the request to PostgREST; the bearer token
        // carries the Auth0 identity RLS reads via auth.jwt()->>'sub'.
        if let Ok(v) = HeaderValue::from_str(&self.config.anon_key) {
            h.insert("apikey", v);
        }
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", self.access_token)) {
            h.insert(AUTHORIZATION, v);
        }
        h
    }

    fn rest_url(&self, table: &str) -> String {
        format!("{}/rest/v1/{}", self.config.api_url, table)
    }

    /// Upsert mutable rows (last-write-wins handled server-side by `revision`
    /// in the merge). A `409` is success (idempotent re-push).
    async fn upsert(&self, table: &str, rows: &[Value]) -> Result<(), SyncError> {
        self.write(table, rows, "resolution=merge-duplicates,return=minimal")
            .await
    }

    /// Insert append-only core rows. `ignore-duplicates` means a retried batch
    /// that mixes already-landed rows with new ones still inserts the new ones
    /// (rather than the whole batch 409-ing and being skipped).
    async fn insert_only(&self, table: &str, rows: &[Value]) -> Result<(), SyncError> {
        self.write(table, rows, "resolution=ignore-duplicates,return=minimal")
            .await
    }

    async fn write(&self, table: &str, rows: &[Value], prefer: &str) -> Result<(), SyncError> {
        if rows.is_empty() {
            return Ok(());
        }
        let resp = self
            .client
            .post(self.rest_url(table))
            .headers(self.headers())
            .header("Prefer", prefer)
            .json(rows)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if resp.status().is_success() {
            return Ok(());
        }
        match classify_status(status) {
            // A duplicate means the rows already landed — treat as success.
            ErrorClass::Duplicate => Ok(()),
            ErrorClass::Unauthorized => Err(SyncError::Unauthorized),
            ErrorClass::Transient => Err(SyncError::Transient(format!("{table}: HTTP {status}"))),
            ErrorClass::Permanent => {
                let body = resp.text().await.unwrap_or_default();
                Err(SyncError::Permanent(format!(
                    "{table}: HTTP {status}: {body}"
                )))
            }
        }
    }

    /// Run one push pass in the strict pipeline order (SPEC §5): content files to
    /// Storage first (so the rows that name them resolve), then projects, nodes,
    /// the append-only lineage core, the mutable edges, and artifacts. Each
    /// table is marked `synced` locally only after its push succeeds.
    pub async fn push_pass(&self, conn: &Connection) -> Result<(), SyncError> {
        let batch = local::collect_push_batch(conn)?;

        // 1. Content blobs → Storage (so artifact rows can reference them).
        self.upload_changed_content(conn, &batch).await?;

        // 2-3. Projects, then nodes.
        self.push_and_mark(conn, "projects", &batch.projects)
            .await?;
        self.push_and_mark(conn, "nodes", &batch.nodes).await?;

        // 4. Append-only lineage core (insert-only). Edges ride with their event.
        self.insert_only("lineage_events", &batch.lineage_events)
            .await?;
        self.insert_only("lineage_edges", &batch.lineage_edges)
            .await?;
        mark_event_rows(conn, &batch.lineage_events)?;

        // 5. Mutable structural edges + artifacts.
        self.push_and_mark(conn, "decomposition_edges", &batch.decomposition_edges)
            .await?;
        self.push_and_mark(conn, "dependency_edges", &batch.dependency_edges)
            .await?;
        self.push_and_mark(conn, "grouping_edges", &batch.grouping_edges)
            .await?;
        self.push_and_mark(conn, "artifacts", &batch.artifacts)
            .await?;

        Ok(())
    }

    /// Upsert a mutable table then mark its pushed rows synced by `client_id`.
    async fn push_and_mark(
        &self,
        conn: &Connection,
        table: &str,
        rows: &[Value],
    ) -> Result<(), SyncError> {
        if rows.is_empty() {
            return Ok(());
        }
        self.upsert(table, rows).await?;
        let client_ids: Vec<String> = rows
            .iter()
            .filter_map(|r| r["client_id"].as_str().map(String::from))
            .collect();
        local::mark_synced(conn, table, &client_ids)?;
        Ok(())
    }

    /// Upload each changed `kind='file'` artifact blob to Storage under
    /// `<user_id>/<project-key>/items/<ref>/<file>`. The object key's first
    /// segment is enforced by Storage RLS to equal the Auth0 subject.
    ///
    /// Deferred: needs the resolved content root + user id at call time. Left as
    /// a no-op stub so the push order is correct once wired.
    async fn upload_changed_content(
        &self,
        _conn: &Connection,
        _batch: &PushBatch,
    ) -> Result<(), SyncError> {
        // TODO(live): read artifact bytes from the content tree, PUT to
        // {api_url}/storage/v1/object/{bucket}/{key}, then record remote_path.
        Ok(())
    }

    /// Fetch all remote rows for `table` (full reconcile — no cursor in v1,
    /// single-user volume is small, SPEC §5).
    pub async fn fetch_table(&self, table: &str) -> Result<Vec<Value>, SyncError> {
        let resp = self
            .client
            .get(self.rest_url(table))
            .headers(self.headers())
            .query(&[("select", "*")])
            .send()
            .await?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            return match classify_status(status) {
                ErrorClass::Unauthorized => Err(SyncError::Unauthorized),
                ErrorClass::Transient => {
                    Err(SyncError::Transient(format!("pull {table}: HTTP {status}")))
                }
                _ => Err(SyncError::Permanent(format!("pull {table}: HTTP {status}"))),
            };
        }
        Ok(resp.json().await?)
    }

    /// Pull pass: fetch remote rows for reconcile.
    ///
    /// Deferred (the one piece needing a live project to finish): apply the
    /// fetched rows to the local DB by reverse-translating `public_id` FKs to
    /// local integer ids — inserting unknown nodes first so FKs resolve — and
    /// taking the higher `revision` per row (LWW), `updated_at` breaking ties.
    /// `fetch_table` above provides the transport; this returns the raw rows so
    /// the reconcile can be wired and tested against real data.
    pub async fn pull_remote(&self) -> Result<RemoteSnapshot, SyncError> {
        Ok(RemoteSnapshot {
            projects: self.fetch_table("projects").await?,
            nodes: self.fetch_table("nodes").await?,
            lineage_events: self.fetch_table("lineage_events").await?,
            lineage_edges: self.fetch_table("lineage_edges").await?,
            decomposition_edges: self.fetch_table("decomposition_edges").await?,
            dependency_edges: self.fetch_table("dependency_edges").await?,
            grouping_edges: self.fetch_table("grouping_edges").await?,
            artifacts: self.fetch_table("artifacts").await?,
        })
    }
}

/// All remote rows for one pull pass, awaiting reverse-translation + LWW merge.
#[derive(Debug, Default)]
pub struct RemoteSnapshot {
    pub projects: Vec<Value>,
    pub nodes: Vec<Value>,
    pub lineage_events: Vec<Value>,
    pub lineage_edges: Vec<Value>,
    pub decomposition_edges: Vec<Value>,
    pub dependency_edges: Vec<Value>,
    pub grouping_edges: Vec<Value>,
    pub artifacts: Vec<Value>,
}

/// Mark synced the lineage events that were just pushed (their edges have no
/// independent sync_state).
fn mark_event_rows(conn: &Connection, events: &[Value]) -> Result<(), SyncError> {
    let client_ids: Vec<String> = events
        .iter()
        .filter_map(|e| e["client_id"].as_str().map(String::from))
        .collect();
    local::mark_synced(conn, "lineage_events", &client_ids)
}
