use crate::{Catalog, Error, KeywordId, Result, VariantId, variants::mark_sidecar_dirty};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyword {
    pub id: KeywordId,
    pub parent: Option<KeywordId>,
    pub name: String,
}

fn find_keyword(
    conn: &Connection,
    parent: Option<KeywordId>,
    name: &str,
) -> Result<Option<KeywordId>> {
    let id = conn
        .prepare_cached("SELECT id FROM keywords WHERE ifnull(parent_id, 0) = ?1 AND name = ?2")?
        .query_row(params![parent.map_or(0, |id| id.0), name], |row| row.get(0))
        .optional()?;
    Ok(id)
}

fn ensure_keyword(tx: &Transaction, parent: Option<KeywordId>, name: &str) -> Result<KeywordId> {
    if let Some(id) = find_keyword(tx, parent, name)? {
        return Ok(id);
    }
    tx.prepare_cached("INSERT INTO keywords (parent_id, name) VALUES (?1, ?2)")?
        .execute(params![parent, name])?;
    Ok(KeywordId(tx.last_insert_rowid()))
}

/// Finds or creates each level of `path`, root first, and returns the leaf.
pub(crate) fn ensure_keyword_path(tx: &Transaction, path: &[String]) -> Result<Option<KeywordId>> {
    let mut parent = None;
    for name in path {
        parent = Some(ensure_keyword(tx, parent, name)?);
    }
    Ok(parent)
}

fn attach(tx: &Transaction, variant: VariantId, keyword: KeywordId) -> Result<()> {
    tx.prepare_cached(
        "INSERT OR IGNORE INTO variant_keywords (variant_id, keyword_id) VALUES (?1, ?2)",
    )?
    .execute(params![variant, keyword])?;
    Ok(())
}

pub(crate) fn copy_keywords(tx: &Transaction, source: VariantId, target: VariantId) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO variant_keywords (variant_id, keyword_id)
         SELECT ?2, keyword_id FROM variant_keywords WHERE variant_id = ?1",
    )?
    .execute(params![source, target])?;
    Ok(())
}

/// Replaces a variant's keywords with these paths, used when a sidecar is the source of truth.
pub(crate) fn replace_keywords(
    tx: &Transaction,
    variant: VariantId,
    paths: &[Vec<String>],
) -> Result<()> {
    tx.prepare_cached("DELETE FROM variant_keywords WHERE variant_id = ?1")?.execute([variant])?;
    for path in paths {
        if let Some(keyword) = ensure_keyword_path(tx, path)? {
            attach(tx, variant, keyword)?;
        }
    }
    Ok(())
}

/// Each of a variant's keywords as a path from the root, sorted.
pub(crate) fn keyword_paths(conn: &Connection, variant: VariantId) -> Result<Vec<Vec<String>>> {
    let mut statement = conn.prepare_cached(
        "WITH RECURSIVE path(leaf, id, parent_id, names) AS (
             SELECT k.id, k.id, k.parent_id, k.name
             FROM keywords k JOIN variant_keywords vk ON vk.keyword_id = k.id
             WHERE vk.variant_id = ?1
             UNION ALL
             SELECT p.leaf, k.id, k.parent_id, k.name || char(31) || p.names
             FROM path p JOIN keywords k ON k.id = p.parent_id
         )
         SELECT names FROM path WHERE parent_id IS NULL ORDER BY names",
    )?;
    let names = statement.query_map([variant], |row| row.get::<_, String>(0))?;
    let mut paths = Vec::new();
    for joined in names {
        paths.push(joined?.split('\u{1f}').map(str::to_owned).collect());
    }
    Ok(paths)
}

impl Catalog {
    /// Finds or creates a keyword under `parent`.
    pub fn create_keyword(&self, parent: Option<KeywordId>, name: &str) -> Result<KeywordId> {
        self.write(|tx| ensure_keyword(tx, parent, name))
    }

    /// All keywords. A parent always comes before its children.
    pub fn keywords(&self) -> Result<Vec<Keyword>> {
        self.read(|conn| {
            let mut statement = conn.prepare_cached(
                "WITH RECURSIVE tree(id, parent_id, name, depth) AS (
                     SELECT id, parent_id, name, 0 FROM keywords WHERE parent_id IS NULL
                     UNION ALL
                     SELECT k.id, k.parent_id, k.name, t.depth + 1
                     FROM keywords k JOIN tree t ON k.parent_id = t.id
                 )
                 SELECT id, parent_id, name FROM tree ORDER BY depth, name",
            )?;
            let keywords = statement.query_map([], |row| {
                Ok(Keyword { id: row.get(0)?, parent: row.get(1)?, name: row.get(2)? })
            })?;
            Ok(keywords.collect::<rusqlite::Result<_>>()?)
        })
    }

    pub fn keywords_of(&self, variant: VariantId) -> Result<Vec<Keyword>> {
        self.read(|conn| {
            let mut statement = conn.prepare_cached(
                "SELECT k.id, k.parent_id, k.name
                 FROM keywords k JOIN variant_keywords vk ON vk.keyword_id = k.id
                 WHERE vk.variant_id = ?1 ORDER BY k.name",
            )?;
            let keywords = statement.query_map([variant], |row| {
                Ok(Keyword { id: row.get(0)?, parent: row.get(1)?, name: row.get(2)? })
            })?;
            Ok(keywords.collect::<rusqlite::Result<_>>()?)
        })
    }

    pub fn add_keyword(&self, variants: &[VariantId], keyword: KeywordId) -> Result<()> {
        self.write(|tx| {
            for variant in variants {
                attach(tx, *variant, keyword)?;
                mark_sidecar_dirty(tx, *variant)?;
            }
            Ok(())
        })
    }

    pub fn remove_keyword(&self, variants: &[VariantId], keyword: KeywordId) -> Result<()> {
        self.write(|tx| {
            let mut statement = tx.prepare_cached(
                "DELETE FROM variant_keywords WHERE variant_id = ?1 AND keyword_id = ?2",
            )?;
            for variant in variants {
                statement.execute(params![variant, keyword])?;
                mark_sidecar_dirty(tx, *variant)?;
            }
            Ok(())
        })
    }

    pub fn rename_keyword(&self, keyword: KeywordId, name: &str) -> Result<()> {
        self.write(|tx| {
            let changed =
                tx.execute("UPDATE keywords SET name = ?2 WHERE id = ?1", params![keyword, name])?;
            if changed == 0 {
                return Err(Error::NotFound { kind: "keyword", id: keyword.0 });
            }
            Ok(())
        })
    }

    /// Deletes a keyword and everything below it.
    pub fn delete_keyword(&self, keyword: KeywordId) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM keywords WHERE id = ?1", [keyword])?;
            Ok(())
        })
    }
}
