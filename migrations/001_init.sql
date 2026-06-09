-- Haven v1 local schema (SPEC §1). The structure layer.
-- Note: PRAGMA foreign_keys / journal_mode are set on the connection in code
-- (db.rs), not here — they cannot run inside rusqlite_migration's transaction.

-- ============================================================
-- Projects (namespacing; one per product/repo)
-- ============================================================
CREATE TABLE projects (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,   -- local only
  public_id   TEXT NOT NULL UNIQUE,                -- UUID, stable sync identity
  key         TEXT NOT NULL UNIQUE,                -- slug, e.g. "haven"
  ref_prefix  TEXT NOT NULL,                       -- e.g. "HV" — used to mint node refs
  ref_counter INTEGER NOT NULL DEFAULT 0,          -- monotonic; last minted node number
  title       TEXT NOT NULL,
  description TEXT,
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
  -- sync metadata
  client_id   TEXT NOT NULL,                       -- idempotency key (UUID)
  revision    INTEGER NOT NULL DEFAULT 1,          -- monotonic LWW
  sync_state  TEXT NOT NULL DEFAULT 'local'
    CHECK (sync_state IN ('local','synced','failed')),
  last_synced_at TEXT
);

-- ============================================================
-- Nodes (the one unified work object).
-- Valid with NO edges, NO priority, uncommitted: the default
-- "floating in the icebox" state.
-- ============================================================
CREATE TABLE nodes (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,  -- local only
  public_id     TEXT NOT NULL UNIQUE,               -- UUID, STABLE FOREVER
  project_id    INTEGER NOT NULL,
  ref           TEXT NOT NULL,                      -- "HV-42", unique per project locally

  title         TEXT NOT NULL,
  body          TEXT,                               -- short summary only; real content = artifacts

  type          TEXT NOT NULL DEFAULT 'task'
    CHECK (type IN ('task','code','research','data','design','admin',
                    'release','phase','gate')),     -- release/phase/gate = container nodes

  -- maturity axis (how well-defined)
  status        TEXT NOT NULL DEFAULT 'discovery'
    CHECK (status IN ('discovery','definition','ready','in_progress',
                      'blocked','done','superseded','archived')),

  -- ownership: who executes it
  owner_kind    TEXT CHECK (owner_kind IN ('human','ai')),   -- NULL = unassigned
  assignee      TEXT,                               -- optional actor: "ai:claude", "human:tom"

  -- why parked, if it is (orthogonal to status; set when blocked or waiting)
  wait_state    TEXT CHECK (wait_state IN ('on_human','on_dependency','on_external')),

  -- commitment axis (whether/when) — independent of maturity
  committed     INTEGER NOT NULL DEFAULT 0,         -- 0 = icebox/floating, 1 = committed
  priority      INTEGER CHECK (priority BETWEEN 0 AND 4),  -- NULL = unprioritised
  sort_key      TEXT,                               -- LexoRank-style fine rank within band

  metadata      TEXT NOT NULL DEFAULT '{}',         -- JSON: custom fields, dispatch hints,
                                                    -- muxra session handle for ai nodes
  created_at    TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at    TEXT NOT NULL DEFAULT (datetime('now')),
  archived_at   TEXT,

  -- sync metadata (servo pattern)
  client_id     TEXT NOT NULL,                      -- idempotency
  revision      INTEGER NOT NULL DEFAULT 1,         -- monotonic LWW on mutable fields
  sync_state    TEXT NOT NULL DEFAULT 'local'
    CHECK (sync_state IN ('local','synced','failed')),
  sync_attempts INTEGER NOT NULL DEFAULT 0,
  last_sync_error TEXT,
  last_synced_at TEXT,

  FOREIGN KEY (project_id) REFERENCES projects(id),
  UNIQUE (project_id, ref),
  UNIQUE (project_id, client_id)
);

CREATE INDEX idx_nodes_project   ON nodes(project_id);
CREATE INDEX idx_nodes_status    ON nodes(project_id, status);
CREATE INDEX idx_nodes_committed ON nodes(project_id, committed, priority);
CREATE INDEX idx_nodes_sync      ON nodes(sync_state) WHERE sync_state <> 'synced';

-- ============================================================
-- Edge layer 1: DECOMPOSITION — "what is this part of"
--   DAG; a node may have multiple parents.
-- ============================================================
CREATE TABLE decomposition_edges (
  parent_id   INTEGER NOT NULL,
  child_id    INTEGER NOT NULL,
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),
  client_id   TEXT NOT NULL,
  sync_state  TEXT NOT NULL DEFAULT 'local'
    CHECK (sync_state IN ('local','synced','failed')),
  PRIMARY KEY (parent_id, child_id),
  FOREIGN KEY (parent_id) REFERENCES nodes(id) ON DELETE CASCADE,
  FOREIGN KEY (child_id)  REFERENCES nodes(id) ON DELETE CASCADE,
  CHECK (parent_id <> child_id),
  UNIQUE (client_id)                                 -- sync idempotency / mark-synced key
);

-- ============================================================
-- Edge layer 2: DEPENDENCY — "what blocks this"
-- ============================================================
CREATE TABLE dependency_edges (
  node_id        INTEGER NOT NULL,                 -- the blocked node
  depends_on_id  INTEGER NOT NULL,                 -- the prerequisite
  created_at     TEXT NOT NULL DEFAULT (datetime('now')),
  client_id      TEXT NOT NULL,
  sync_state     TEXT NOT NULL DEFAULT 'local'
    CHECK (sync_state IN ('local','synced','failed')),
  PRIMARY KEY (node_id, depends_on_id),
  FOREIGN KEY (node_id)       REFERENCES nodes(id) ON DELETE CASCADE,
  FOREIGN KEY (depends_on_id) REFERENCES nodes(id) ON DELETE CASCADE,
  CHECK (node_id <> depends_on_id),
  UNIQUE (client_id)                                 -- sync idempotency / mark-synced key
);

-- ============================================================
-- Edge layer 3: GROUPING — "which release/phase is this in"
--   group_id is a node of type release/phase/gate. Members
--   re-batch freely without touching work-breakdown.
-- ============================================================
CREATE TABLE grouping_edges (
  group_id    INTEGER NOT NULL,
  member_id   INTEGER NOT NULL,
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),
  client_id   TEXT NOT NULL,
  sync_state  TEXT NOT NULL DEFAULT 'local'
    CHECK (sync_state IN ('local','synced','failed')),
  PRIMARY KEY (group_id, member_id),
  FOREIGN KEY (group_id)  REFERENCES nodes(id) ON DELETE CASCADE,
  FOREIGN KEY (member_id) REFERENCES nodes(id) ON DELETE CASCADE,
  CHECK (group_id <> member_id),
  UNIQUE (client_id)                                 -- sync idempotency / mark-synced key
);

-- ============================================================
-- Edge layer 4: LINEAGE — "what did this become" — APPEND-ONLY
--   events = what/why/when/who; edges = from->to graph.
--   Never UPDATEd or DELETEd. This is the immutable core log.
-- ============================================================
CREATE TABLE lineage_events (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  public_id    TEXT NOT NULL UNIQUE,               -- UUID
  project_id   INTEGER NOT NULL,
  event_type   TEXT NOT NULL
    CHECK (event_type IN ('split','merge','supersede','update','archive','reopen')),
  rationale    TEXT,                               -- why
  triggered_by TEXT,                               -- "human:tom" / "ai:claude"
  context      TEXT NOT NULL DEFAULT '{}',         -- JSON: diffs, refs
  created_at   TEXT NOT NULL DEFAULT (datetime('now')),
  client_id    TEXT NOT NULL,
  sync_state   TEXT NOT NULL DEFAULT 'local'
    CHECK (sync_state IN ('local','synced','failed')),
  FOREIGN KEY (project_id) REFERENCES projects(id),
  UNIQUE (project_id, client_id)
);

CREATE TABLE lineage_edges (
  event_id     INTEGER NOT NULL,
  from_node_id INTEGER NOT NULL,
  to_node_id   INTEGER NOT NULL,
  PRIMARY KEY (event_id, from_node_id, to_node_id),
  FOREIGN KEY (event_id)     REFERENCES lineage_events(id) ON DELETE CASCADE,
  FOREIGN KEY (from_node_id) REFERENCES nodes(id),
  FOREIGN KEY (to_node_id)   REFERENCES nodes(id)
);

CREATE INDEX idx_lineage_from ON lineage_edges(from_node_id);
CREATE INDEX idx_lineage_to   ON lineage_edges(to_node_id);

-- ============================================================
-- Artifacts: typed references from a node to content.
--   Content lives as FILES under ~/.haven/<project>/...; this
--   is the typed, queryable pointer.
-- ============================================================
CREATE TABLE artifacts (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  public_id    TEXT NOT NULL UNIQUE,               -- UUID
  node_id      INTEGER NOT NULL,

  role         TEXT NOT NULL
    CHECK (role IN ('spec','research','design','handoff','decision',
                    'scratch','source','delivery','vision')),
  kind         TEXT NOT NULL DEFAULT 'file'
    CHECK (kind IN ('file','external','delivery')),

  path         TEXT,                               -- relative to ~/.haven/<project>/ (kind=file)
  uri          TEXT,                               -- external URL / obsidian:// / ticket url
  title        TEXT,
  excerpt      TEXT,

  -- handoff-specific (role='handoff'): the baton-pass
  from_owner   TEXT CHECK (from_owner IN ('human','ai')),
  to_owner     TEXT CHECK (to_owner   IN ('human','ai')),

  -- content-sync metadata (kind=file blobs sync to Storage, lazy download)
  content_hash TEXT,                               -- sha256 of file bytes at last sync
  remote_path  TEXT,                               -- Storage object key once uploaded

  metadata     TEXT NOT NULL DEFAULT '{}',
  created_at   TEXT NOT NULL DEFAULT (datetime('now')),
  created_by   TEXT,
  client_id    TEXT NOT NULL,
  revision     INTEGER NOT NULL DEFAULT 1,
  sync_state   TEXT NOT NULL DEFAULT 'local'
    CHECK (sync_state IN ('local','synced','failed')),
  last_synced_at TEXT,

  FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE
);

CREATE INDEX idx_artifacts_node ON artifacts(node_id);
CREATE INDEX idx_artifacts_role ON artifacts(node_id, role);

-- ============================================================
-- Full-text search over node title/body (FTS5, external content)
-- ============================================================
CREATE VIRTUAL TABLE node_fts USING fts5(
  title, body,
  content='nodes', content_rowid='id',
  tokenize='porter'
);

CREATE TRIGGER nodes_fts_ai AFTER INSERT ON nodes BEGIN
  INSERT INTO node_fts(rowid, title, body) VALUES (new.id, new.title, new.body);
END;
CREATE TRIGGER nodes_fts_ad AFTER DELETE ON nodes BEGIN
  INSERT INTO node_fts(node_fts, rowid, title, body) VALUES('delete', old.id, old.title, old.body);
END;
CREATE TRIGGER nodes_fts_au AFTER UPDATE ON nodes BEGIN
  INSERT INTO node_fts(node_fts, rowid, title, body) VALUES('delete', old.id, old.title, old.body);
  INSERT INTO node_fts(rowid, title, body) VALUES (new.id, new.title, new.body);
END;

-- ============================================================
-- Local-only key/value config + sync bookkeeping
-- ============================================================
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

INSERT INTO meta (key, value) VALUES ('schema_version', '1');
