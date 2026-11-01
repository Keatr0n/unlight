//! Schema migrations, tracked in `PRAGMA user_version`. Each entry takes the schema from the
//! version equal to its index to the next one. The SQL stays in Rust because the Nix source
//! filter only keeps Cargo sources.

use crate::{Error, Result};
use rusqlite::{Connection, TransactionBehavior};

const V1: &str = "
CREATE TABLE folders (
    id        INTEGER PRIMARY KEY,
    parent_id INTEGER REFERENCES folders(id) ON DELETE CASCADE,
    path      TEXT NOT NULL UNIQUE
);
CREATE INDEX folders_parent ON folders(parent_id);

CREATE TABLE photos (
    id            INTEGER PRIMARY KEY,
    folder_id     INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    file_name     TEXT NOT NULL,
    size          INTEGER NOT NULL,
    mtime_ns      INTEGER NOT NULL,
    fingerprint   BLOB NOT NULL,
    status        INTEGER NOT NULL DEFAULT 0,
    status_detail TEXT,
    camera        TEXT NOT NULL DEFAULT '',
    lens          TEXT NOT NULL DEFAULT '',
    iso           REAL,
    shutter       REAL,
    aperture      REAL,
    focal_length  REAL,
    capture_time  INTEGER,
    width         INTEGER NOT NULL DEFAULT 0,
    height        INTEGER NOT NULL DEFAULT 0,
    orientation   INTEGER NOT NULL DEFAULT 0,
    UNIQUE (folder_id, file_name)
);
CREATE INDEX photos_fingerprint ON photos(fingerprint, size);
CREATE INDEX photos_capture_time ON photos(capture_time, file_name COLLATE NOCASE);
CREATE INDEX photos_file_name ON photos(file_name COLLATE NOCASE);
CREATE INDEX photos_camera ON photos(camera);
CREATE INDEX photos_lens ON photos(lens);
CREATE INDEX photos_status ON photos(status);

-- The recipe lives in its own table so that edited rows don't bloat the pages every library
-- filter scans. A variant with no row here has the default recipe.
CREATE TABLE variants (
    id               INTEGER PRIMARY KEY,
    photo_id         INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
    copy_name        TEXT,
    rating           INTEGER NOT NULL DEFAULT 0 CHECK (rating BETWEEN 0 AND 5),
    flag             INTEGER NOT NULL DEFAULT 0 CHECK (flag BETWEEN -1 AND 1),
    label            INTEGER NOT NULL DEFAULT 0 CHECK (label BETWEEN 0 AND 5),
    edited_at        INTEGER,
    history_position INTEGER
);
CREATE UNIQUE INDEX variants_master ON variants(photo_id) WHERE copy_name IS NULL;
CREATE INDEX variants_photo ON variants(photo_id);
CREATE INDEX variants_rating ON variants(rating);
CREATE INDEX variants_flag ON variants(flag);
CREATE INDEX variants_label ON variants(label);
CREATE INDEX variants_edited_at ON variants(edited_at);

CREATE TABLE variant_recipes (
    variant_id INTEGER PRIMARY KEY REFERENCES variants(id) ON DELETE CASCADE,
    recipe     TEXT NOT NULL
);

CREATE TABLE history (
    id         INTEGER PRIMARY KEY,
    variant_id INTEGER NOT NULL REFERENCES variants(id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    label      TEXT NOT NULL,
    recipe     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (variant_id, seq)
);

CREATE TABLE snapshots (
    id         INTEGER PRIMARY KEY,
    variant_id INTEGER NOT NULL REFERENCES variants(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    recipe     TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX snapshots_variant ON snapshots(variant_id);

CREATE TABLE keywords (
    id        INTEGER PRIMARY KEY,
    parent_id INTEGER REFERENCES keywords(id) ON DELETE CASCADE,
    name      TEXT NOT NULL COLLATE NOCASE
);
CREATE UNIQUE INDEX keywords_path ON keywords(ifnull(parent_id, 0), name);
CREATE INDEX keywords_parent ON keywords(parent_id);

CREATE TABLE variant_keywords (
    variant_id INTEGER NOT NULL REFERENCES variants(id) ON DELETE CASCADE,
    keyword_id INTEGER NOT NULL REFERENCES keywords(id) ON DELETE CASCADE,
    PRIMARY KEY (variant_id, keyword_id)
) WITHOUT ROWID;
CREATE INDEX variant_keywords_keyword ON variant_keywords(keyword_id);

CREATE TABLE collections (
    id    INTEGER PRIMARY KEY,
    name  TEXT NOT NULL,
    rules TEXT
);

CREATE TABLE collection_variants (
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    variant_id    INTEGER NOT NULL REFERENCES variants(id) ON DELETE CASCADE,
    PRIMARY KEY (collection_id, variant_id)
) WITHOUT ROWID;
CREATE INDEX collection_variants_variant ON collection_variants(variant_id);

CREATE TABLE presets (
    id         INTEGER PRIMARY KEY,
    group_name TEXT NOT NULL,
    name       TEXT NOT NULL,
    settings   TEXT NOT NULL,
    UNIQUE (group_name, name)
);

CREATE TABLE sidecars (
    photo_id INTEGER PRIMARY KEY REFERENCES photos(id) ON DELETE CASCADE,
    size     INTEGER,
    mtime_ns INTEGER,
    dirty    INTEGER NOT NULL DEFAULT 0,
    conflict INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX sidecars_dirty ON sidecars(dirty) WHERE dirty = 1;
CREATE INDEX sidecars_conflict ON sidecars(conflict) WHERE conflict = 1;
";

const MIGRATIONS: &[&str] = &[V1];

pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    apply(conn, MIGRATIONS)
}

/// Runs one transaction per step, so a failing step leaves the catalog at the last good
/// version. Taking the write lock before reading the version lets two processes open the
/// same catalog at once.
fn apply(conn: &mut Connection, migrations: &[&str]) -> Result<()> {
    let supported = migrations.len() as u32;
    loop {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > supported {
            return Err(Error::SchemaTooNew { found: version, supported });
        }
        let Some(migration) = migrations.get(version as usize) else {
            return Ok(());
        };
        tx.execute_batch(migration)?;
        tx.pragma_update(None, "user_version", version + 1)?;
        tx.commit()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(conn: &Connection) -> u32 {
        conn.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap()
    }

    fn tables(conn: &Connection) -> Vec<String> {
        let mut statement = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        let rows = statement.query_map([], |row| row.get(0)).unwrap();
        rows.map(|row| row.unwrap()).collect()
    }

    #[test]
    fn a_new_catalog_reaches_the_latest_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(version(&conn), MIGRATIONS.len() as u32);
        assert!(tables(&conn).contains(&"variants".to_owned()));
    }

    #[test]
    fn migrating_twice_changes_nothing() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        let before = tables(&conn);
        migrate(&mut conn).unwrap();
        assert_eq!(tables(&conn), before);
    }

    #[test]
    fn only_pending_steps_run_and_data_survives() {
        let mut conn = Connection::open_in_memory().unwrap();
        let first = ["CREATE TABLE t (a INTEGER); INSERT INTO t VALUES (7);"];
        apply(&mut conn, &first).unwrap();
        let both = [first[0], "ALTER TABLE t ADD COLUMN b INTEGER DEFAULT 9;"];
        apply(&mut conn, &both).unwrap();
        assert_eq!(version(&conn), 2);
        let row: (i64, i64) =
            conn.query_row("SELECT a, b FROM t", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(row, (7, 9));
    }

    #[test]
    fn a_failing_step_rolls_back_to_the_last_good_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        let steps = ["CREATE TABLE good (a);", "CREATE TABLE half (a); THIS IS NOT SQL;"];
        assert!(apply(&mut conn, &steps).is_err());
        assert_eq!(version(&conn), 1);
        assert_eq!(tables(&conn), ["good"]);
    }

    #[test]
    fn a_catalog_from_a_newer_build_is_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 999).unwrap();
        let error = migrate(&mut conn).unwrap_err();
        assert!(matches!(error, Error::SchemaTooNew { found: 999, .. }));
    }
}
