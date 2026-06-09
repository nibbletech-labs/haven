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

fn migrations() -> &'static Migrations<'static> {
    static MIGRATIONS: OnceLock<Migrations<'static>> = OnceLock::new();
    MIGRATIONS.get_or_init(|| Migrations::new(vec![M::up(MIGRATION_001), M::up(MIGRATION_002)]))
}

/// Open (creating if needed) the SQLite database at `path`, apply connection
/// PRAGMAs, and run any pending migrations. Returns a ready-to-use connection.
pub fn open<P: AsRef<Path>>(path: P) -> Result<Connection> {
    let mut conn = Connection::open(path)?;
    configure(&conn, /* wal */ true)?;
    migrations().to_latest(&mut conn)?;
    Ok(conn)
}

/// Open an in-memory database with the schema applied — used in tests. WAL is
/// not requested: SQLite silently keeps in-memory DBs in `memory` journal mode,
/// so asking for WAL there would be a meaningless no-op.
pub fn open_in_memory() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    configure(&conn, /* wal */ false)?;
    migrations().to_latest(&mut conn)?;
    Ok(conn)
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
    fn schema_applies_and_seeds_version() {
        let conn = open_in_memory().unwrap();
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
