-- HV-124: promote `context-pack` to a first-class artifact role.
--
-- A context pack was previously stored as a `spec` artifact disambiguated only by
-- the magic filename `context-pack.md` + the node's grouping position — so a spec
-- on a pure decomposition parent governed nothing, and one node could carry both a
-- real spec.md and a context-pack.md under the same role. This adds `context-pack`
-- to the artifacts.role CHECK and reclassifies the existing magic-filename rows, so
-- resolution (content.rs) keys on the role, not a filename.
--
-- SQLite cannot modify a CHECK constraint in place, so this rebuilds `artifacts`
-- (the same rename-rebuild pattern as 006, but standalone — nothing FK-references
-- artifacts, so there is no child-table dance). Every column is carried forward;
-- then the reclassify UPDATE flips `spec` context-pack.md rows to `context-pack`.

ALTER TABLE artifacts RENAME TO artifacts_old;

CREATE TABLE artifacts (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  public_id    TEXT NOT NULL UNIQUE,
  node_id      INTEGER NOT NULL,
  role         TEXT NOT NULL
    CHECK (role IN ('spec','research','design','handoff','decision',
                    'scratch','source','delivery','vision','context-pack')),
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
INSERT INTO artifacts
SELECT id, public_id, node_id, role, kind, path, uri, title, excerpt, from_owner,
       to_owner, content_hash, remote_path, metadata, created_at, created_by,
       client_id, revision, sync_state, last_synced_at
FROM artifacts_old;
DROP TABLE artifacts_old;
CREATE INDEX idx_artifacts_node ON artifacts(node_id);
CREATE INDEX idx_artifacts_role ON artifacts(node_id, role);

-- Reclassify the magic-filename packs to the new first-class role. Matches the old
-- resolution predicate (role='spec' + the canonical context-pack.md filename), so
-- exactly the rows content.rs used to treat as packs become role='context-pack'.
UPDATE artifacts
   SET role = 'context-pack', sync_state = 'local', revision = revision + 1
 WHERE role = 'spec' AND path LIKE '%/context-pack.md';
