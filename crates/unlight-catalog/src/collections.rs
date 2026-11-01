use crate::{Catalog, CollectionId, Error, Result, SmartRules, VariantId, clause::Clause};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq)]
pub struct Collection {
    pub id: CollectionId,
    pub name: String,
    /// `Some` for a smart collection, whose members come from these rules.
    pub rules: Option<SmartRules>,
}

fn not_found(id: CollectionId) -> Error {
    Error::NotFound { kind: "collection", id: id.0 }
}

/// Members of a manual collection, or a smart collection's compiled rules.
pub(crate) fn collection_clause(conn: &Connection, id: CollectionId) -> Result<Clause> {
    let rules: Option<String> = conn
        .prepare_cached("SELECT rules FROM collections WHERE id = ?1")?
        .query_row([id], |row| row.get(0))
        .optional()?
        .ok_or(not_found(id))?;
    match rules {
        Some(json) => Ok(serde_json::from_str::<SmartRules>(&json)?.clause()),
        None => Ok(Clause::new(
            "v.id IN (SELECT variant_id FROM collection_variants WHERE collection_id = ?)",
            [id.0.into()],
        )),
    }
}

impl Catalog {
    pub fn create_collection(&self, name: &str) -> Result<CollectionId> {
        self.write(|tx| {
            tx.execute("INSERT INTO collections (name) VALUES (?1)", [name])?;
            Ok(CollectionId(tx.last_insert_rowid()))
        })
    }

    pub fn create_smart_collection(&self, name: &str, rules: &SmartRules) -> Result<CollectionId> {
        let json = serde_json::to_string(rules)?;
        self.write(|tx| {
            tx.execute(
                "INSERT INTO collections (name, rules) VALUES (?1, ?2)",
                params![name, json],
            )?;
            Ok(CollectionId(tx.last_insert_rowid()))
        })
    }

    pub fn set_smart_rules(&self, id: CollectionId, rules: &SmartRules) -> Result<()> {
        let json = serde_json::to_string(rules)?;
        self.write(|tx| {
            let changed = tx.execute(
                "UPDATE collections SET rules = ?2 WHERE id = ?1 AND rules IS NOT NULL",
                params![id, json],
            )?;
            if changed == 0 {
                return Err(not_found(id));
            }
            Ok(())
        })
    }

    pub fn collections(&self) -> Result<Vec<Collection>> {
        let rows: Vec<(CollectionId, String, Option<String>)> = self.read(|conn| {
            let mut statement =
                conn.prepare_cached("SELECT id, name, rules FROM collections ORDER BY name, id")?;
            let rows =
                statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        rows.into_iter()
            .map(|(id, name, json)| {
                let rules = json.map(|json| serde_json::from_str(&json)).transpose()?;
                Ok(Collection { id, name, rules })
            })
            .collect()
    }

    pub fn rename_collection(&self, id: CollectionId, name: &str) -> Result<()> {
        self.write(|tx| {
            let changed =
                tx.execute("UPDATE collections SET name = ?2 WHERE id = ?1", params![id, name])?;
            if changed == 0 {
                return Err(not_found(id));
            }
            Ok(())
        })
    }

    pub fn delete_collection(&self, id: CollectionId) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM collections WHERE id = ?1", [id])?;
            Ok(())
        })
    }

    pub fn add_to_collection(&self, id: CollectionId, variants: &[VariantId]) -> Result<()> {
        self.write(|tx| {
            let mut statement = tx.prepare_cached(
                "INSERT OR IGNORE INTO collection_variants (collection_id, variant_id) VALUES (?1, ?2)",
            )?;
            for variant in variants {
                statement.execute(params![id, variant])?;
            }
            Ok(())
        })
    }

    pub fn remove_from_collection(&self, id: CollectionId, variants: &[VariantId]) -> Result<()> {
        self.write(|tx| {
            let mut statement = tx.prepare_cached(
                "DELETE FROM collection_variants WHERE collection_id = ?1 AND variant_id = ?2",
            )?;
            for variant in variants {
                statement.execute(params![id, variant])?;
            }
            Ok(())
        })
    }
}
