-- HV-125: REMOVE the `owner_eligible` eligibility axis (revert HV-66/005).
--
-- Readiness is already `status=ready` + `committed` (priority) + `done_looks_like`
-- (acceptance) — a different dimension from ownership. `owner_eligible` conflated
-- the two and added a redundant auto-pull gate; `next --owner` reverts to filtering
-- `owner_kind` (the assignment axis). This drops the now-unused column.
--
-- SQLite cannot DROP a column referenced by a CHECK constraint in place, so this
-- is the full rename-rebuild of `nodes` — the exact inverse of 005_owner_eligible
-- (and the same pattern as 003_anchor_type): drop FTS + triggers + indexes, rename
-- `nodes` aside, recreate it WITHOUT `owner_eligible`, copy every row preserving
-- integer ids, rebuild the child-FK tables whose foreign keys ALTER...RENAME
-- repointed at `nodes_old`, then recreate indexes, the FTS vtable, and triggers.
--
-- CRITICAL: the new CREATE and the INSERT...SELECT carry forward EVERY remaining
-- column — including `due_at` (004), `done_looks_like`, and `why` — or the rebuild
-- silently drops that data. `owner_eligible` is the ONLY column removed: it is
-- absent from both the new CREATE and the INSERT...SELECT, so the column (and any
-- values it held) is gone after the rebuild.

DROP TRIGGER IF EXISTS nodes_fts_ai;
DROP TRIGGER IF EXISTS nodes_fts_ad;
DROP TRIGGER IF EXISTS nodes_fts_au;
DROP TABLE IF EXISTS node_fts;
DROP INDEX IF EXISTS idx_nodes_project;
DROP INDEX IF EXISTS idx_nodes_status;
DROP INDEX IF EXISTS idx_nodes_committed;
DROP INDEX IF EXISTS idx_nodes_sync;

ALTER TABLE nodes RENAME TO nodes_old;

CREATE TABLE nodes (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  public_id     TEXT NOT NULL UNIQUE,
  project_id    INTEGER NOT NULL,
  ref           TEXT NOT NULL,
  title         TEXT NOT NULL,
  body          TEXT,
  type          TEXT NOT NULL DEFAULT 'task'
    CHECK (type IN ('task','code','research','data','design','admin',
                    'release','phase','gate','anchor')),
  status        TEXT NOT NULL DEFAULT 'discovery'
    CHECK (status IN ('discovery','definition','ready','in_progress',
                      'blocked','done','superseded','archived')),
  owner_kind    TEXT CHECK (owner_kind IN ('human','ai')),
  assignee      TEXT,
  wait_state    TEXT CHECK (wait_state IN ('on_human','on_dependency','on_external')),
  committed     INTEGER NOT NULL DEFAULT 0,
  priority      INTEGER CHECK (priority BETWEEN 0 AND 4),
  sort_key      TEXT,
  metadata      TEXT NOT NULL DEFAULT '{}',
  created_at    TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at    TEXT NOT NULL DEFAULT (datetime('now')),
  archived_at   TEXT,
  client_id     TEXT NOT NULL,
  revision      INTEGER NOT NULL DEFAULT 1,
  sync_state    TEXT NOT NULL DEFAULT 'local'
    CHECK (sync_state IN ('local','synced','failed')),
  sync_attempts INTEGER NOT NULL DEFAULT 0,
  last_sync_error TEXT,
  last_synced_at TEXT,
  done_looks_like TEXT,
  why TEXT,
  due_at TEXT,
  FOREIGN KEY (project_id) REFERENCES projects(id),
  UNIQUE (project_id, ref),
  UNIQUE (project_id, client_id)
);

INSERT INTO nodes (
  id, public_id, project_id, ref, title, body, type, status, owner_kind,
  assignee, wait_state, committed, priority, sort_key, metadata, created_at,
  updated_at, archived_at, client_id, revision, sync_state, sync_attempts,
  last_sync_error, last_synced_at, done_looks_like, why, due_at
)
SELECT
  id, public_id, project_id, ref, title, body, type, status, owner_kind,
  assignee, wait_state, committed, priority, sort_key, metadata, created_at,
  updated_at, archived_at, client_id, revision, sync_state, sync_attempts,
  last_sync_error, last_synced_at, done_looks_like, why, due_at
FROM nodes_old;

CREATE INDEX idx_nodes_project   ON nodes(project_id);
CREATE INDEX idx_nodes_status    ON nodes(project_id, status);
CREATE INDEX idx_nodes_committed ON nodes(project_id, committed, priority);
CREATE INDEX idx_nodes_sync      ON nodes(sync_state) WHERE sync_state <> 'synced';

CREATE TABLE decomposition_edges_new (
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
  UNIQUE (client_id)
);
INSERT INTO decomposition_edges_new
SELECT parent_id, child_id, created_at, client_id, sync_state FROM decomposition_edges;
DROP TABLE decomposition_edges;
ALTER TABLE decomposition_edges_new RENAME TO decomposition_edges;

CREATE TABLE dependency_edges_new (
  node_id        INTEGER NOT NULL,
  depends_on_id  INTEGER NOT NULL,
  created_at     TEXT NOT NULL DEFAULT (datetime('now')),
  client_id      TEXT NOT NULL,
  sync_state     TEXT NOT NULL DEFAULT 'local'
    CHECK (sync_state IN ('local','synced','failed')),
  PRIMARY KEY (node_id, depends_on_id),
  FOREIGN KEY (node_id)       REFERENCES nodes(id) ON DELETE CASCADE,
  FOREIGN KEY (depends_on_id) REFERENCES nodes(id) ON DELETE CASCADE,
  CHECK (node_id <> depends_on_id),
  UNIQUE (client_id)
);
INSERT INTO dependency_edges_new
SELECT node_id, depends_on_id, created_at, client_id, sync_state FROM dependency_edges;
DROP TABLE dependency_edges;
ALTER TABLE dependency_edges_new RENAME TO dependency_edges;

CREATE TABLE grouping_edges_new (
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
  UNIQUE (client_id)
);
INSERT INTO grouping_edges_new
SELECT group_id, member_id, created_at, client_id, sync_state FROM grouping_edges;
DROP TABLE grouping_edges;
ALTER TABLE grouping_edges_new RENAME TO grouping_edges;

CREATE TABLE lineage_edges_new (
  event_id     INTEGER NOT NULL,
  from_node_id INTEGER NOT NULL,
  to_node_id   INTEGER NOT NULL,
  PRIMARY KEY (event_id, from_node_id, to_node_id),
  FOREIGN KEY (event_id)     REFERENCES lineage_events(id) ON DELETE CASCADE,
  FOREIGN KEY (from_node_id) REFERENCES nodes(id),
  FOREIGN KEY (to_node_id)   REFERENCES nodes(id)
);
INSERT INTO lineage_edges_new
SELECT event_id, from_node_id, to_node_id FROM lineage_edges;
DROP TABLE lineage_edges;
ALTER TABLE lineage_edges_new RENAME TO lineage_edges;
CREATE INDEX idx_lineage_from ON lineage_edges(from_node_id);
CREATE INDEX idx_lineage_to   ON lineage_edges(to_node_id);

CREATE TABLE artifacts_new (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  public_id    TEXT NOT NULL UNIQUE,
  node_id      INTEGER NOT NULL,
  role         TEXT NOT NULL
    CHECK (role IN ('spec','research','design','handoff','decision',
                    'scratch','source','delivery','vision')),
  kind         TEXT NOT NULL DEFAULT 'file'
    CHECK (kind IN ('file','external','delivery')),
  path         TEXT,
  uri          TEXT,
  title        TEXT,
  excerpt      TEXT,
  from_owner   TEXT CHECK (from_owner IN ('human','ai')),
  to_owner     TEXT CHECK (to_owner   IN ('human','ai')),
  content_hash TEXT,
  remote_path  TEXT,
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
INSERT INTO artifacts_new
SELECT id, public_id, node_id, role, kind, path, uri, title, excerpt, from_owner,
       to_owner, content_hash, remote_path, metadata, created_at, created_by,
       client_id, revision, sync_state, last_synced_at
FROM artifacts;
DROP TABLE artifacts;
ALTER TABLE artifacts_new RENAME TO artifacts;
CREATE INDEX idx_artifacts_node ON artifacts(node_id);
CREATE INDEX idx_artifacts_role ON artifacts(node_id, role);

DROP TABLE nodes_old;

CREATE VIRTUAL TABLE node_fts USING fts5(
  title, body,
  content='nodes', content_rowid='id',
  tokenize='porter'
);
INSERT INTO node_fts(rowid, title, body)
SELECT id, title, body FROM nodes;

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
