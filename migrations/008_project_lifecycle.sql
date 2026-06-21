-- HV-112: add project lifecycle to `projects` — soft-archive (status/archived_at/
-- archived_reason) + a first-class delete tombstone (deleted_at). All in-place
-- ADD COLUMNs (no table rebuild, cf. 004_due_at.sql).
-- NOTE: no PRAGMA here. rusqlite_migration runs each migration inside a
-- transaction, and `foreign_keys`/`journal_mode` PRAGMAs cannot be toggled inside
-- that transaction (db.rs:4-6) — they are set on the connection in db.rs. A
-- constant DEFAULT 'active' with a self-contained CHECK is accepted in place, so
-- no swap window is needed.
ALTER TABLE projects ADD COLUMN status TEXT NOT NULL DEFAULT 'active'
  CHECK (status IN ('active','archived'));      -- binary project lifecycle (constant default → in-place OK)
ALTER TABLE projects ADD COLUMN archived_at TEXT;       -- nullable; set on archive, NULL on reopen/active
ALTER TABLE projects ADD COLUMN archived_reason TEXT;   -- nullable rationale (no lineage edge target for projects)
ALTER TABLE projects ADD COLUMN deleted_at TEXT;        -- nullable delete tombstone; NULL on every live row

-- Helps default listings drop archived/tombstoned projects.
CREATE INDEX idx_projects_status ON projects(status);
