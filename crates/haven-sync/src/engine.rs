//! HTTP transport against Supabase PostgREST + Storage (SPEC §5).
//!
//! **Written; push is live-verified, pull is unit-tested against a local DB.**
//! Both passes are implemented: push uploads unsynced rows; pull fetches the
//! remote snapshot and reconciles it locally (reverse FK translation + revision
//! LWW, in [`crate::local::apply_snapshot`]). The HTTP transport itself needs a
//! real Supabase project + a valid Auth0 token to exercise end-to-end.

use std::path::Path;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use rusqlite::Connection;
use serde_json::Value;

use crate::local;
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
    /// Postgres schema PostgREST should target. `None` = the default `public`
    /// schema; `Some("haven")` routes via the `Accept-Profile`/`Content-Profile`
    /// headers so Haven's objects can live in a dedicated schema on a shared
    /// project. The Storage path is schema-agnostic and ignores this.
    pub schema: Option<String>,
}

impl SyncConfig {
    pub fn new(api_url: impl Into<String>, anon_key: impl Into<String>) -> Self {
        SyncConfig {
            api_url: api_url.into(),
            anon_key: anon_key.into(),
            bucket: "haven-content".into(),
            schema: None,
        }
    }

    /// Target a non-default Postgres schema (e.g. `haven`) for PostgREST calls.
    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = Some(schema.into());
        self
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
        // Route to a non-default Postgres schema when configured. PostgREST reads
        // Accept-Profile on GET/HEAD and Content-Profile on write verbs; sending
        // both on every request is correct and simplest.
        if let Some(schema) = &self.config.schema {
            if let Ok(v) = HeaderValue::from_str(schema) {
                h.insert("Accept-Profile", v.clone());
                h.insert("Content-Profile", v);
            }
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
            // PostgREST returns 409 for unique violations (23505 — the row
            // already landed: success) AND for FK violations (23503 — a parent
            // row is missing remotely: a real failure that must not be
            // swallowed, found live when a wiped remote made every child push
            // "succeed"). Disambiguate by the Postgres error code in the body.
            ErrorClass::Duplicate => {
                let body = resp.text().await.unwrap_or_default();
                if body.contains("23503") {
                    Err(SyncError::Permanent(format!(
                        "{table}: HTTP {status} (foreign key violation — parent row missing remotely): {body}"
                    )))
                } else {
                    Ok(())
                }
            }
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
    /// table is marked `synced` locally only after its push succeeds. Returns
    /// how many content blobs were uploaded.
    pub async fn push_pass(
        &self,
        conn: &Connection,
        content_root: &Path,
    ) -> Result<usize, SyncError> {
        // 1. Content blobs → Storage, BEFORE collecting the row batch: uploading
        //    stamps `remote_path`/`content_hash` (+ revision bump) on the rows,
        //    and the batch below must carry those values.
        let uploaded = self.upload_changed_content(conn, content_root).await?;

        let batch = local::collect_push_batch(conn)?;

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

        Ok(uploaded)
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

    /// Headers for Storage object requests: same apikey + bearer as PostgREST,
    /// but raw bytes instead of JSON, and `x-upsert` so a re-upload of a changed
    /// file overwrites its blob (allowed by the Storage UPDATE policy).
    fn storage_headers(&self) -> HeaderMap {
        let mut h = self.headers();
        h.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        h.insert("x-upsert", HeaderValue::from_static("true"));
        h
    }

    fn object_url(&self, key: &str) -> String {
        format!(
            "{}/storage/v1/object/{}/{}",
            self.config.api_url, self.config.bucket, key
        )
    }

    /// Upload each changed `kind='file'` artifact blob to Storage under
    /// `<sub>/<project-key>/<rel_path>` (= `…/items/<ref>/<file>`). The key's
    /// first segment must equal the Auth0 subject — enforced by Storage RLS, and
    /// derived here from the access token's `sub` claim. Selection is by hash
    /// comparison ([`local::needs_upload`]); each success stamps the row's
    /// `remote_path`/`content_hash` so the row push that follows carries them.
    /// Returns the number of blobs uploaded.
    async fn upload_changed_content(
        &self,
        conn: &Connection,
        content_root: &Path,
    ) -> Result<usize, SyncError> {
        let candidates = local::collect_content_candidates(conn)?;
        if candidates.is_empty() {
            return Ok(0);
        }
        let sub = crate::jwt_sub(&self.access_token)?;
        let mut uploaded = 0;
        for c in &candidates {
            let Some((bytes, hash)) = local::needs_upload(c, content_root) else {
                continue;
            };
            let key = format!("{sub}/{}/{}", c.project_key, c.rel_path);
            let resp = self
                .client
                .post(self.object_url(&key))
                .headers(self.storage_headers())
                .body(bytes)
                .send()
                .await?;
            let status = resp.status().as_u16();
            if !resp.status().is_success() {
                return match classify_status(status) {
                    // Duplicate = the blob already landed (e.g. a retried pass
                    // racing x-upsert) — record and continue.
                    ErrorClass::Duplicate => {
                        local::record_uploaded(conn, &c.client_id, &key, &hash)?;
                        uploaded += 1;
                        continue;
                    }
                    ErrorClass::Unauthorized => Err(SyncError::Unauthorized),
                    ErrorClass::Transient => Err(SyncError::Transient(format!(
                        "storage {key}: HTTP {status}"
                    ))),
                    ErrorClass::Permanent => {
                        let body = resp.text().await.unwrap_or_default();
                        Err(SyncError::Permanent(format!(
                            "storage {key}: HTTP {status}: {body}"
                        )))
                    }
                };
            }
            local::record_uploaded(conn, &c.client_id, &key, &hash)?;
            uploaded += 1;
        }
        Ok(uploaded)
    }

    /// Download one Storage object by key (authenticated; RLS scopes reads to
    /// the token's subject).
    pub async fn download_object(&self, key: &str) -> Result<Vec<u8>, SyncError> {
        let url = format!(
            "{}/storage/v1/object/authenticated/{}/{}",
            self.config.api_url, self.config.bucket, key
        );
        let resp = self.client.get(url).headers(self.headers()).send().await?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            return match classify_status(status) {
                ErrorClass::Unauthorized => Err(SyncError::Unauthorized),
                ErrorClass::Transient => Err(SyncError::Transient(format!(
                    "storage get {key}: HTTP {status}"
                ))),
                _ => Err(SyncError::Permanent(format!(
                    "storage get {key}: HTTP {status}"
                ))),
            };
        }
        Ok(resp.bytes().await?.to_vec())
    }

    /// Lazy-pull one artifact's content (SPEC §5): download `remote_path` from
    /// Storage, verify it against `expected_hash` when known, and cache it at
    /// `<content_root>/<project_key>/<rel_path>` so subsequent reads are local.
    pub async fn hydrate(
        &self,
        content_root: &Path,
        project_key: &str,
        rel_path: &str,
        remote_path: &str,
        expected_hash: Option<&str>,
    ) -> Result<std::path::PathBuf, SyncError> {
        let bytes = self.download_object(remote_path).await?;
        local::write_hydrated(content_root, project_key, rel_path, &bytes, expected_hash)
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

    /// Run one pull pass: fetch the remote snapshot and reconcile it into the
    /// local store (reverse FK translation + revision LWW, all in one
    /// transaction; stale content caches under `content_root` are invalidated).
    /// Returns what changed locally.
    pub async fn pull_pass(
        &self,
        conn: &Connection,
        content_root: &Path,
    ) -> Result<crate::local::ReconcileStats, SyncError> {
        let snapshot = self.pull_remote().await?;
        crate::local::apply_snapshot(conn, &snapshot, content_root)
    }

    /// Fetch every remote table into a [`RemoteSnapshot`] (full reconcile, no
    /// cursor in v1 — single-user volume is small, SPEC §5). The
    /// reverse-translation + LWW apply lives in [`crate::local::apply_snapshot`];
    /// [`Self::pull_pass`] chains the two.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `schema = Some("haven")` emits BOTH PostgREST profile headers so reads
    /// (Accept-Profile) and writes (Content-Profile) route to that schema.
    #[test]
    fn schema_set_emits_both_profile_headers() {
        let cfg = SyncConfig::new("https://x.supabase.co", "anon").with_schema("haven");
        let engine = SyncEngine::new(cfg, "token");
        let h = engine.headers();
        assert_eq!(
            h.get("Accept-Profile").map(|v| v.to_str().unwrap()),
            Some("haven"),
        );
        assert_eq!(
            h.get("Content-Profile").map(|v| v.to_str().unwrap()),
            Some("haven"),
        );
    }

    /// `schema = None` (the default) emits neither header — PostgREST falls back
    /// to the `public` schema, preserving today's behaviour.
    #[test]
    fn schema_none_emits_no_profile_headers() {
        let cfg = SyncConfig::new("https://x.supabase.co", "anon");
        assert!(cfg.schema.is_none());
        let engine = SyncEngine::new(cfg, "token");
        let h = engine.headers();
        assert!(h.get("Accept-Profile").is_none());
        assert!(h.get("Content-Profile").is_none());
    }
}
