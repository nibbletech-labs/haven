//! Connection + schema migrations.
//!
//! The full local DDL (SPEC §1) lives in `migrations/001_init.sql` at the
//! workspace root and is embedded at compile time. PRAGMAs that cannot run
//! inside a migration transaction (`foreign_keys`, `journal_mode`) are applied
//! to the connection here.

use std::path::Path;
use std::sync::OnceLock;

use rusqlite::Connection;
use rusqlite_migration::{Migrations, M};

use crate::error::{HavenError, Result};

const MIGRATION_001: &str = include_str!("../../../migrations/001_init.sql");
const MIGRATION_002: &str = include_str!("../../../migrations/002_acceptance.sql");
const MIGRATION_003: &str = include_str!("../../../migrations/003_anchor_type.sql");
const MIGRATION_004: &str = include_str!("../../../migrations/004_due_at.sql");
const MIGRATION_005: &str = include_str!("../../../migrations/005_owner_eligible.sql");
const MIGRATION_006: &str = include_str!("../../../migrations/006_drop_owner_eligible.sql");
const MIGRATION_007: &str = include_str!("../../../migrations/007_context_pack_role.sql");

/// The ordered migration SQL, embedded at compile time. Adding a migration here
/// is the only edit needed: the supported schema version is this list's length,
/// so it can never drift from `migrations()` (no hand-bumped constant to forget).
const MIGRATION_SQL: &[&str] = &[
    MIGRATION_001,
    MIGRATION_002,
    MIGRATION_003,
    MIGRATION_004,
    MIGRATION_005,
    MIGRATION_006,
    MIGRATION_007,
];

/// Highest `user_version` this binary can open, derived from `MIGRATION_SQL`.
pub fn latest_schema_migration() -> i64 {
    MIGRATION_SQL.len() as i64
}

fn migrations() -> &'static Migrations<'static> {
    static MIGRATIONS: OnceLock<Migrations<'static>> = OnceLock::new();
    MIGRATIONS.get_or_init(|| Migrations::new(MIGRATION_SQL.iter().copied().map(M::up).collect()))
}

/// Open (creating if needed) the SQLite database at `path`, apply connection
/// PRAGMAs, and run any pending migrations. Returns a ready-to-use connection.
pub fn open<P: AsRef<Path>>(path: P) -> Result<Connection> {
    let path = path.as_ref();
    let mut conn = Connection::open(path)?;
    configure(&conn, /* wal */ true)?;
    ensure_supported_schema_version(&conn, Some(path))?;
    migrations().to_latest(&mut conn)?;
    Ok(conn)
}

/// Open an in-memory database with the schema applied — used in tests. WAL is
/// not requested: SQLite silently keeps in-memory DBs in `memory` journal mode,
/// so asking for WAL there would be a meaningless no-op.
pub fn open_in_memory() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    configure(&conn, /* wal */ false)?;
    ensure_supported_schema_version(&conn, None)?;
    migrations().to_latest(&mut conn)?;
    Ok(conn)
}

fn ensure_supported_schema_version(conn: &Connection, path: Option<&Path>) -> Result<()> {
    let db_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if db_version > latest_schema_migration() {
        return Err(HavenError::StoreTooNew {
            path: path
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<memory>".into()),
            db_version,
            supported_version: latest_schema_migration(),
        });
    }
    Ok(())
}

fn configure(conn: &Connection, wal: bool) -> Result<()> {
    // Foreign keys for the edge integrity the schema relies on; busy_timeout so a
    // background sync pass and a foreground CLI call don't trip over each other.
    // Both are per-connection and re-applied on every open.
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    if wal {
        // journal_mode is a set-and-report pragma: it returns the resulting mode
        // and silently stays on the old mode if the filesystem can't support WAL
        // (read-only / some network mounts). Confirm it actually took rather than
        // proceeding under a false assumption of concurrent-read safety.
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(HavenError::Invalid(format!(
                "could not enable WAL journal mode (got {mode:?}); \
                 this database's filesystem may not support it"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_valid() {
        // rusqlite_migration validates that the set parses and is monotonic.
        assert_eq!(migrations().validate(), Ok(()));
    }

    #[test]
    fn supported_version_matches_migration_count() {
        // The supported schema version is derived from the migration list, so it
        // cannot silently desync. The literal below is a deliberate tripwire:
        // adding a migration forces a one-line edit here, a moment to confirm
        // intent (and to bump the release/version if needed).
        assert_eq!(latest_schema_migration(), MIGRATION_SQL.len() as i64);
        assert_eq!(latest_schema_migration(), 7);
    }

    /// HV-66: the migration-005 `nodes` REBUILD must preserve every row's
    /// integer id and carry forward EVERY existing column — including HV-67's
    /// `due_at` (added in place at 004), `done_looks_like`, and `why` — while the
    /// new `owner_eligible` defaults NULL (no backfill). This is the test that
    /// catches a botched rebuild: it stages a row at schema v4, then migrates the
    /// SAME connection to v5 and asserts nothing was dropped.
    #[test]
    fn migration_005_rebuild_preserves_ids_and_carries_columns_forward() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();

        // Apply migrations 001..=004 only (the pre-005 schema: `due_at` exists,
        // `owner_eligible` does not).
        let to_v4 = Migrations::new(
            MIGRATION_SQL[..4]
                .iter()
                .copied()
                .map(M::up)
                .collect::<Vec<_>>(),
        );
        to_v4.to_latest(&mut conn).unwrap();
        let v4: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v4, 4, "staged at schema v4");
        // The column being added by 005 must NOT exist yet.
        assert!(
            conn.prepare("SELECT owner_eligible FROM nodes").is_err(),
            "owner_eligible must not exist before migration 005"
        );

        // Stage a project + a fully-populated node, capturing its assigned id.
        conn.execute(
            "INSERT INTO projects (public_id, key, ref_prefix, title, client_id)
             VALUES ('p-uuid', 'haven', 'HV', 'Haven', 'pc')",
            [],
        )
        .unwrap();
        let project_id: i64 = conn
            .query_row("SELECT id FROM projects WHERE key='haven'", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO nodes
                (public_id, project_id, ref, title, body, type, status, owner_kind,
                 committed, priority, client_id, done_looks_like, why, due_at)
             VALUES ('n-uuid', ?1, 'HV-1', 'Carry me', 'a body', 'code', 'ready',
                     'ai', 1, 2, 'nc', 'ship it', 'because reasons', '2026-07-01')",
            [project_id],
        )
        .unwrap();
        let node_id: i64 = conn
            .query_row("SELECT id FROM nodes WHERE ref='HV-1'", [], |r| r.get(0))
            .unwrap();

        // Now migrate the SAME connection through 005 (the rebuild under test).
        // Pinned to v5 deliberately: 006 later DROPS `owner_eligible`, so going to
        // latest would invalidate the column assertions below — this test owns the
        // 005-era invariant, and `migration_006_*` owns its removal.
        let to_v5 = Migrations::new(
            MIGRATION_SQL[..5]
                .iter()
                .copied()
                .map(M::up)
                .collect::<Vec<_>>(),
        );
        to_v5.to_latest(&mut conn).unwrap();
        let v5: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v5, 5);

        // The row survived: id PRESERVED, no column dropped, owner_eligible NULL.
        // Read each carried column individually and assert nothing was lost.
        let col = |sql: &str| -> Option<String> {
            conn.query_row(sql, [], |r| r.get::<_, Option<String>>(0))
                .unwrap()
        };
        let id: i64 = conn
            .query_row("SELECT id FROM nodes WHERE ref='HV-1'", [], |r| r.get(0))
            .unwrap();
        let committed: i64 = conn
            .query_row("SELECT committed FROM nodes WHERE ref='HV-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let priority: Option<i64> = conn
            .query_row("SELECT priority FROM nodes WHERE ref='HV-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            id, node_id,
            "integer id MUST be preserved across the rebuild"
        );
        assert_eq!(
            col("SELECT title FROM nodes WHERE ref='HV-1'").as_deref(),
            Some("Carry me")
        );
        assert_eq!(
            col("SELECT body FROM nodes WHERE ref='HV-1'").as_deref(),
            Some("a body")
        );
        assert_eq!(
            col("SELECT status FROM nodes WHERE ref='HV-1'").as_deref(),
            Some("ready")
        );
        assert_eq!(
            col("SELECT owner_kind FROM nodes WHERE ref='HV-1'").as_deref(),
            Some("ai")
        );
        assert_eq!(committed, 1);
        assert_eq!(priority, Some(2));
        assert_eq!(
            col("SELECT done_looks_like FROM nodes WHERE ref='HV-1'").as_deref(),
            Some("ship it"),
            "done_looks_like NOT dropped"
        );
        assert_eq!(
            col("SELECT why FROM nodes WHERE ref='HV-1'").as_deref(),
            Some("because reasons"),
            "why NOT dropped"
        );
        assert_eq!(
            col("SELECT due_at FROM nodes WHERE ref='HV-1'").as_deref(),
            Some("2026-07-01"),
            "due_at NOT dropped"
        );
        assert_eq!(
            col("SELECT owner_eligible FROM nodes WHERE ref='HV-1'"),
            None,
            "owner_eligible defaults NULL (no backfill)"
        );

        // The new CHECK constraint is live post-rebuild: a bad value is rejected,
        // a valid one accepted.
        assert!(
            conn.execute(
                "UPDATE nodes SET owner_eligible='nonsense' WHERE ref='HV-1'",
                []
            )
            .is_err(),
            "the owner_eligible CHECK must reject an out-of-domain value"
        );
        conn.execute("UPDATE nodes SET owner_eligible='any' WHERE ref='HV-1'", [])
            .unwrap();

        // FTS survived the rebuild — the row is searchable by title.
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM node_fts WHERE node_fts MATCH 'Carry'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "FTS index must be rebuilt and populated");
    }

    /// HV-125: the migration-006 `nodes` REBUILD must DROP `owner_eligible` while
    /// preserving every row's integer id, carrying forward every remaining column
    /// (esp. `due_at`), and keeping the edge tables + FTS intact across the
    /// child-FK-table rebuild. This is the in-memory analog of the live-DB safety
    /// gate: stage a populated row + a decomposition edge at v5, migrate to v6, and
    /// assert the column is gone but nothing else was lost.
    #[test]
    fn migration_006_drops_owner_eligible_and_preserves_rows_edges_and_fts() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();

        // Stage at schema v5 (the pre-006 schema: `owner_eligible` exists).
        let to_v5 = Migrations::new(
            MIGRATION_SQL[..5]
                .iter()
                .copied()
                .map(M::up)
                .collect::<Vec<_>>(),
        );
        to_v5.to_latest(&mut conn).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            5,
            "staged at schema v5"
        );
        // The column being dropped by 006 must exist at v5.
        assert!(
            conn.prepare("SELECT owner_eligible FROM nodes").is_ok(),
            "owner_eligible must exist before migration 006"
        );

        // Stage a project + two fully-populated nodes (one with owner_eligible set,
        // due_at, done_looks_like, why), and a decomposition edge between them.
        conn.execute(
            "INSERT INTO projects (public_id, key, ref_prefix, title, client_id)
             VALUES ('p-uuid', 'haven', 'HV', 'Haven', 'pc')",
            [],
        )
        .unwrap();
        let project_id: i64 = conn
            .query_row("SELECT id FROM projects WHERE key='haven'", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO nodes
                (public_id, project_id, ref, title, body, type, status, owner_kind,
                 committed, priority, client_id, done_looks_like, why, due_at, owner_eligible)
             VALUES ('n1-uuid', ?1, 'HV-1', 'Carry me', 'a body', 'code', 'ready',
                     'ai', 1, 2, 'nc1', 'ship it', 'because reasons', '2026-07-01', 'ai')",
            [project_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nodes
                (public_id, project_id, ref, title, type, status, client_id)
             VALUES ('n2-uuid', ?1, 'HV-2', 'Child node', 'code', 'ready', 'nc2')",
            [project_id],
        )
        .unwrap();
        let parent_id: i64 = conn
            .query_row("SELECT id FROM nodes WHERE ref='HV-1'", [], |r| r.get(0))
            .unwrap();
        let child_id: i64 = conn
            .query_row("SELECT id FROM nodes WHERE ref='HV-2'", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO decomposition_edges (parent_id, child_id, client_id)
             VALUES (?1, ?2, 'edge-cid')",
            [parent_id, child_id],
        )
        .unwrap();

        // Migrate the SAME connection through 006 (the rebuild under test). Pinned
        // to v6: later migrations (007+) don't touch nodes, so this test owns the
        // 006-era invariant and migration_007_* owns its own.
        let to_v6 = Migrations::new(
            MIGRATION_SQL[..6]
                .iter()
                .copied()
                .map(M::up)
                .collect::<Vec<_>>(),
        );
        to_v6.to_latest(&mut conn).unwrap();
        let v6: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v6, 6);

        // The column is GONE.
        assert!(
            conn.prepare("SELECT owner_eligible FROM nodes").is_err(),
            "owner_eligible must not exist after migration 006"
        );

        // Every other column carried forward, id preserved.
        let col = |sql: &str| -> Option<String> {
            conn.query_row(sql, [], |r| r.get::<_, Option<String>>(0))
                .unwrap()
        };
        let id: i64 = conn
            .query_row("SELECT id FROM nodes WHERE ref='HV-1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            id, parent_id,
            "integer id MUST be preserved across the rebuild"
        );
        assert_eq!(
            col("SELECT title FROM nodes WHERE ref='HV-1'").as_deref(),
            Some("Carry me")
        );
        assert_eq!(
            col("SELECT owner_kind FROM nodes WHERE ref='HV-1'").as_deref(),
            Some("ai")
        );
        assert_eq!(
            col("SELECT done_looks_like FROM nodes WHERE ref='HV-1'").as_deref(),
            Some("ship it"),
            "done_looks_like NOT dropped"
        );
        assert_eq!(
            col("SELECT why FROM nodes WHERE ref='HV-1'").as_deref(),
            Some("because reasons"),
            "why NOT dropped"
        );
        assert_eq!(
            col("SELECT due_at FROM nodes WHERE ref='HV-1'").as_deref(),
            Some("2026-07-01"),
            "due_at NOT dropped"
        );

        // The decomposition edge survived the child-FK-table rebuild, still
        // pointing at the preserved ids.
        let edges: i64 = conn
            .query_row(
                "SELECT count(*) FROM decomposition_edges WHERE parent_id=?1 AND child_id=?2",
                [parent_id, child_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(edges, 1, "the decomposition edge must survive the rebuild");

        // FTS survived the rebuild — both rows are searchable.
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM node_fts WHERE node_fts MATCH 'Carry OR Child'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            hits, 2,
            "FTS index must be rebuilt and populated for both rows"
        );
    }

    /// HV-124: migration 007 adds the `context-pack` role and reclassifies exactly
    /// the magic-filename packs (role='spec' + a `context-pack.md` path) — leaving a
    /// real spec.md and every other role untouched, preserving all rows, and making
    /// the expanded CHECK accept `context-pack` while still rejecting a bad role.
    #[test]
    fn migration_007_reclassifies_context_pack_artifacts() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();

        // Stage at schema v6 (pre-007: no `context-pack` role).
        let to_v6 = Migrations::new(
            MIGRATION_SQL[..6]
                .iter()
                .copied()
                .map(M::up)
                .collect::<Vec<_>>(),
        );
        to_v6.to_latest(&mut conn).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            6,
            "staged at schema v6"
        );

        conn.execute(
            "INSERT INTO projects (public_id, key, ref_prefix, title, client_id)
             VALUES ('p', 'haven', 'HV', 'Haven', 'pc')",
            [],
        )
        .unwrap();
        let pid: i64 = conn
            .query_row("SELECT id FROM projects WHERE key='haven'", [], |r| {
                r.get(0)
            })
            .unwrap();
        // A container node + a leaf node.
        conn.execute(
            "INSERT INTO nodes (public_id, project_id, ref, title, type, client_id)
             VALUES ('n1', ?1, 'HV-1', 'phase', 'phase', 'nc1'),
                    ('n2', ?1, 'HV-2', 'leaf', 'task', 'nc2')",
            [pid],
        )
        .unwrap();
        let container: i64 = conn
            .query_row("SELECT id FROM nodes WHERE ref='HV-1'", [], |r| r.get(0))
            .unwrap();
        let leaf: i64 = conn
            .query_row("SELECT id FROM nodes WHERE ref='HV-2'", [], |r| r.get(0))
            .unwrap();
        // Three artifacts, all role='spec' at v6: a context-pack.md on the container
        // (to be reclassified), a real spec.md on the leaf (must stay spec), and a
        // design artifact (a different role — must be untouched).
        conn.execute(
            "INSERT INTO artifacts (public_id, node_id, role, path, client_id) VALUES
                ('a1', ?1, 'spec',   'items/HV-1/context-pack.md', 'ac1'),
                ('a2', ?2, 'spec',   'items/HV-2/spec.md',         'ac2'),
                ('a3', ?2, 'design', 'items/HV-2/design.md',       'ac3')",
            [container, leaf],
        )
        .unwrap();

        // Migrate to latest (runs 007).
        migrations().to_latest(&mut conn).unwrap();
        let v7: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v7, latest_schema_migration());
        assert_eq!(v7, 7);

        let role_of = |pubid: &str| -> String {
            conn.query_row(
                "SELECT role FROM artifacts WHERE public_id = ?1",
                [pubid],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            role_of("a1"),
            "context-pack",
            "the context-pack.md pack must be reclassified"
        );
        assert_eq!(role_of("a2"), "spec", "a real spec.md must stay spec");
        assert_eq!(role_of("a3"), "design", "an unrelated role is untouched");
        // All three rows preserved.
        let count: i64 = conn
            .query_row("SELECT count(*) FROM artifacts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3, "no artifact row dropped by the rebuild");

        // The expanded CHECK now accepts `context-pack`...
        conn.execute(
            "INSERT INTO artifacts (public_id, node_id, role, path, client_id)
             VALUES ('a4', ?1, 'context-pack', 'items/HV-1/context-pack.md', 'ac4')",
            [container],
        )
        .unwrap();
        // ...and still rejects a bad role.
        assert!(
            conn.execute(
                "INSERT INTO artifacts (public_id, node_id, role, client_id)
                 VALUES ('a5', ?1, 'nonsense', 'ac5')",
                [container],
            )
            .is_err(),
            "the role CHECK must still reject an out-of-domain value"
        );
    }

    #[test]
    fn schema_applies_and_seeds_version() {
        let conn = open_in_memory().unwrap();
        let user_version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(user_version, latest_schema_migration());
        let v: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, "1");
    }

    #[test]
    fn newer_database_gets_actionable_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("haven.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", latest_schema_migration() + 1)
                .unwrap();
        }

        let err = open(&path).unwrap_err();
        match err {
            HavenError::StoreTooNew {
                path: err_path,
                db_version,
                supported_version,
            } => {
                assert_eq!(err_path, path.display().to_string());
                assert_eq!(db_version, latest_schema_migration() + 1);
                assert_eq!(supported_version, latest_schema_migration());
            }
            other => panic!("expected StoreTooNew, got {other:?}"),
        }
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let conn = open_in_memory().unwrap();
        // Inserting a node referencing a non-existent project must fail the FK.
        let res = conn.execute(
            "INSERT INTO nodes (public_id, project_id, ref, title, client_id)
             VALUES ('p', 999, 'X-1', 't', 'c')",
            [],
        );
        assert!(res.is_err(), "expected FK violation, got {res:?}");
    }

    #[test]
    fn file_db_enables_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("haven.db");
        let conn = open(&path).unwrap();
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn fts_triggers_index_nodes() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO projects (public_id, key, ref_prefix, title, client_id)
             VALUES ('proj-uuid', 'haven', 'HV', 'Haven', 'cid-proj')",
            [],
        )
        .unwrap();
        let project_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO nodes (public_id, project_id, ref, title, body, client_id)
             VALUES ('node-uuid', ?1, 'HV-1', 'Authentication flow', 'token refresh logic', 'cid-node')",
            [project_id],
        )
        .unwrap();

        // INSERT trigger populated the FTS index.
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM node_fts WHERE node_fts MATCH 'authentication'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);

        // Porter stemming: 'refresh' matches 'refresh logic' in the body.
        let body_hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM node_fts WHERE node_fts MATCH 'refresh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(body_hits, 1);

        // UPDATE trigger keeps it in sync.
        conn.execute(
            "UPDATE nodes SET title = 'Billing flow' WHERE ref = 'HV-1'",
            [],
        )
        .unwrap();
        let stale: i64 = conn
            .query_row(
                "SELECT count(*) FROM node_fts WHERE node_fts MATCH 'authentication'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stale, 0,
            "FTS should no longer match old title after update"
        );
    }
}
