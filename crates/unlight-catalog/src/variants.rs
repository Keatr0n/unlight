use crate::{
    Catalog, Error, Flag, Label, PhotoId, Result, SnapshotId, VariantId, keywords::copy_keywords,
    model::now_ms,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use unlight_recipe::Recipe;

/// Steps of history kept per variant. The oldest step left acts as the undo limit.
pub const HISTORY_CAP: i64 = 100;

const DEFAULT_RECIPE_JSON: &str = "{}";
const BASE_STEP_LABEL: &str = "Start";

/// One editable version of a photo. Every photo has a master; virtual copies are extra
/// variants of the same file.
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub id: VariantId,
    pub photo: PhotoId,
    /// `None` for the master.
    pub copy_name: Option<String>,
    pub rating: u8,
    pub flag: Flag,
    pub label: Label,
    /// Milliseconds since the Unix epoch. `None` while the recipe is the default.
    pub edited_at: Option<i64>,
    pub recipe: Recipe,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryStep {
    pub seq: i64,
    pub label: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
    pub steps: Vec<HistoryStep>,
    /// The `seq` of the step the variant is on, empty until the first edit.
    pub position: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub id: SnapshotId,
    pub variant: VariantId,
    pub name: String,
    pub recipe: Recipe,
    pub created_at: i64,
}

/// An unedited variant stores no JSON at all, so it picks up the current defaults, and a
/// library of untouched photos costs nothing.
pub(crate) fn recipe_json(recipe: &Recipe) -> Result<String> {
    if *recipe == Recipe::default() {
        return Ok(DEFAULT_RECIPE_JSON.to_owned());
    }
    let json = serde_json::to_string(recipe)?;
    serde_json::from_str::<Recipe>(&json).map_err(|_| Error::InvalidRecipe)?;
    Ok(json)
}

fn not_found(id: VariantId) -> Error {
    Error::NotFound { kind: "variant", id: id.0 }
}

pub(crate) fn current_recipe_json(conn: &Connection, id: VariantId) -> Result<String> {
    let json = conn
        .prepare_cached("SELECT recipe FROM variant_recipes WHERE variant_id = ?1")?
        .query_row([id], |row| row.get(0))
        .optional()?;
    Ok(json.unwrap_or_else(|| DEFAULT_RECIPE_JSON.to_owned()))
}

/// Writes the variant's current recipe. Only the recipe table holds JSON, so `edited_at`
/// always agrees with whether a row exists there.
pub(crate) fn store_recipe(tx: &Transaction, id: VariantId, json: &str) -> Result<()> {
    if json == DEFAULT_RECIPE_JSON {
        tx.prepare_cached("DELETE FROM variant_recipes WHERE variant_id = ?1")?.execute([id])?;
        tx.prepare_cached("UPDATE variants SET edited_at = NULL WHERE id = ?1")?.execute([id])?;
    } else {
        tx.prepare_cached(
            "INSERT INTO variant_recipes (variant_id, recipe) VALUES (?1, ?2)
             ON CONFLICT (variant_id) DO UPDATE SET recipe = excluded.recipe",
        )?
        .execute(params![id, json])?;
        tx.prepare_cached("UPDATE variants SET edited_at = ?2 WHERE id = ?1")?
            .execute(params![id, now_ms()])?;
    }
    mark_sidecar_dirty(tx, id)
}

pub(crate) fn mark_sidecar_dirty(tx: &Transaction, id: VariantId) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO sidecars (photo_id, dirty)
         SELECT photo_id, 1 FROM variants WHERE id = ?1 AND copy_name IS NULL
         ON CONFLICT (photo_id) DO UPDATE SET dirty = 1",
    )?
    .execute([id])?;
    Ok(())
}

pub(crate) fn master_variant(conn: &Connection, photo: PhotoId) -> Result<VariantId> {
    conn.prepare_cached("SELECT id FROM variants WHERE photo_id = ?1 AND copy_name IS NULL")?
        .query_row([photo], |row| row.get(0))
        .optional()?
        .ok_or(Error::NotFound { kind: "photo", id: photo.0 })
}

fn history_position(conn: &Connection, id: VariantId) -> Result<Option<i64>> {
    conn.prepare_cached("SELECT history_position FROM variants WHERE id = ?1")?
        .query_row([id], |row| row.get(0))
        .optional()?
        .ok_or(not_found(id))
}

fn insert_step(tx: &Transaction, id: VariantId, seq: i64, label: &str, json: &str) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO history (variant_id, seq, label, recipe, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![id, seq, label, json, now_ms()])?;
    Ok(())
}

fn move_to_step(tx: &Transaction, id: VariantId, seq: i64, json: &str) -> Result<()> {
    tx.prepare_cached("UPDATE variants SET history_position = ?2 WHERE id = ?1")?
        .execute(params![id, seq])?;
    store_recipe(tx, id, json)
}

/// The state before the first edit becomes a history step when that edit happens, so
/// untouched photos have no history rows and the first edit can still be undone.
pub(crate) fn commit_step(tx: &Transaction, id: VariantId, label: &str, json: &str) -> Result<()> {
    let position = history_position(tx, id)?;
    if current_recipe_json(tx, id)? == json {
        return Ok(());
    }
    let position = match position {
        Some(position) => position,
        None => {
            insert_step(tx, id, 1, BASE_STEP_LABEL, &current_recipe_json(tx, id)?)?;
            1
        }
    };
    tx.prepare_cached("DELETE FROM history WHERE variant_id = ?1 AND seq > ?2")?
        .execute(params![id, position])?;
    let seq = position + 1;
    insert_step(tx, id, seq, label, json)?;
    tx.prepare_cached("DELETE FROM history WHERE variant_id = ?1 AND seq <= ?2")?
        .execute(params![id, seq - HISTORY_CAP])?;
    move_to_step(tx, id, seq, json)
}

fn step_recipe(
    conn: &Connection,
    id: VariantId,
    sql: &str,
    seq: i64,
) -> Result<Option<(i64, String)>> {
    let step = conn
        .prepare_cached(sql)?
        .query_row(params![id, seq], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    Ok(step)
}

fn variant_from_row(row: &Row) -> rusqlite::Result<(Variant, Option<String>)> {
    let variant = Variant {
        id: row.get(0)?,
        photo: row.get(1)?,
        copy_name: row.get(2)?,
        rating: row.get(3)?,
        flag: row.get(4)?,
        label: row.get(5)?,
        edited_at: row.get(6)?,
        recipe: Recipe::default(),
    };
    Ok((variant, row.get(7)?))
}

fn with_recipe((mut variant, json): (Variant, Option<String>)) -> Result<Variant> {
    if let Some(json) = json {
        variant.recipe = serde_json::from_str(&json)?;
    }
    Ok(variant)
}

const VARIANT_SELECT: &str = "
    SELECT v.id, v.photo_id, v.copy_name, v.rating, v.flag, v.label, v.edited_at, r.recipe
    FROM variants v LEFT JOIN variant_recipes r ON r.variant_id = v.id";

impl Catalog {
    pub fn variant(&self, id: VariantId) -> Result<Variant> {
        let found = self.read(|conn| {
            let sql = format!("{VARIANT_SELECT} WHERE v.id = ?1");
            Ok(conn.prepare_cached(&sql)?.query_row([id], variant_from_row).optional()?)
        })?;
        with_recipe(found.ok_or(not_found(id))?)
    }

    /// A photo's variants, master first.
    pub fn variants_of(&self, photo: PhotoId) -> Result<Vec<Variant>> {
        let rows: Vec<_> = self.read(|conn| {
            let sql = format!(
                "{VARIANT_SELECT} WHERE v.photo_id = ?1 ORDER BY v.copy_name IS NOT NULL, v.id"
            );
            let mut statement = conn.prepare_cached(&sql)?;
            let rows = statement.query_map([photo], variant_from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        rows.into_iter().map(with_recipe).collect()
    }

    /// Adds a variant starting from `source`'s recipe, rating, label and keywords. It has its
    /// own flag and history, and shares nothing with `source` afterwards.
    pub fn create_virtual_copy(&self, source: VariantId) -> Result<VariantId> {
        self.write(|tx| {
            let photo: PhotoId = tx
                .query_row("SELECT photo_id FROM variants WHERE id = ?1", [source], |row| {
                    row.get(0)
                })
                .optional()?
                .ok_or(not_found(source))?;
            let copies: i64 = tx.query_row(
                "SELECT count(*) FROM variants WHERE photo_id = ?1 AND copy_name IS NOT NULL",
                [photo],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO variants (photo_id, copy_name, rating, label)
                 SELECT photo_id, ?2, rating, label FROM variants WHERE id = ?1",
                params![source, format!("Copy {}", copies + 1)],
            )?;
            let copy = VariantId(tx.last_insert_rowid());
            let json = current_recipe_json(tx, source)?;
            if json != DEFAULT_RECIPE_JSON {
                store_recipe(tx, copy, &json)?;
            }
            copy_keywords(tx, source, copy)?;
            Ok(copy)
        })
    }

    /// Deletes a virtual copy. A master can only go with its photo.
    pub fn delete_variant(&self, id: VariantId) -> Result<()> {
        self.write(|tx| {
            let copy_name: Option<String> = tx
                .query_row("SELECT copy_name FROM variants WHERE id = ?1", [id], |row| row.get(0))
                .optional()?
                .ok_or(not_found(id))?;
            if copy_name.is_none() {
                return Err(Error::MasterVariant);
            }
            tx.execute("DELETE FROM variants WHERE id = ?1", [id])?;
            Ok(())
        })
    }

    pub fn set_rating(&self, ids: &[VariantId], rating: u8) -> Result<()> {
        if rating > 5 {
            return Err(Error::InvalidRating(rating));
        }
        self.write(|tx| set_rating(tx, ids, rating))
    }

    pub fn set_flag(&self, ids: &[VariantId], flag: Flag) -> Result<()> {
        self.write(|tx| set_flag(tx, ids, flag))
    }

    pub fn set_label(&self, ids: &[VariantId], label: Label) -> Result<()> {
        self.write(|tx| set_label(tx, ids, label))
    }

    /// Makes `recipe` current and records it as a new history step, dropping any redo steps.
    pub fn save_recipe(&self, id: VariantId, recipe: &Recipe, label: &str) -> Result<()> {
        let json = recipe_json(recipe)?;
        self.write(|tx| commit_step(tx, id, label, &json))
    }

    /// Goes back one history step. Returns the recipe now current, or `None` at the oldest.
    pub fn undo(&self, id: VariantId) -> Result<Option<Recipe>> {
        self.step_through_history(
            id,
            "SELECT seq, recipe FROM history WHERE variant_id = ?1 AND seq < ?2
             ORDER BY seq DESC LIMIT 1",
        )
    }

    /// Goes forward one history step. Returns the recipe now current, or `None` at the newest.
    pub fn redo(&self, id: VariantId) -> Result<Option<Recipe>> {
        self.step_through_history(
            id,
            "SELECT seq, recipe FROM history WHERE variant_id = ?1 AND seq > ?2
             ORDER BY seq ASC LIMIT 1",
        )
    }

    fn step_through_history(&self, id: VariantId, sql: &str) -> Result<Option<Recipe>> {
        self.write(|tx| {
            let Some(position) = history_position(tx, id)? else {
                return Ok(None);
            };
            let Some((seq, json)) = step_recipe(tx, id, sql, position)? else {
                return Ok(None);
            };
            move_to_step(tx, id, seq, &json)?;
            Ok(Some(serde_json::from_str(&json)?))
        })
    }

    /// Jumps to a step listed by [`Catalog::history`], keeping the steps after it for redo.
    pub fn go_to_history_step(&self, id: VariantId, seq: i64) -> Result<Recipe> {
        self.write(|tx| {
            let (seq, json) = step_recipe(
                tx,
                id,
                "SELECT seq, recipe FROM history WHERE variant_id = ?1 AND seq = ?2",
                seq,
            )?
            .ok_or(Error::NotFound { kind: "history step", id: seq })?;
            move_to_step(tx, id, seq, &json)?;
            Ok(serde_json::from_str(&json)?)
        })
    }

    pub fn history(&self, id: VariantId) -> Result<History> {
        self.read(|conn| {
            let position = history_position(conn, id)?;
            let mut statement = conn.prepare_cached(
                "SELECT seq, label, created_at FROM history WHERE variant_id = ?1 ORDER BY seq",
            )?;
            let steps = statement.query_map([id], |row| {
                Ok(HistoryStep { seq: row.get(0)?, label: row.get(1)?, created_at: row.get(2)? })
            })?;
            Ok(History { steps: steps.collect::<rusqlite::Result<_>>()?, position })
        })
    }

    /// The recipe of every history step, by `seq`, so a UI can undo without waiting on a write.
    pub fn history_recipes(&self, id: VariantId) -> Result<Vec<(i64, Recipe)>> {
        let rows: Vec<(i64, String)> = self.read(|conn| {
            let mut statement = conn
                .prepare_cached("SELECT seq, recipe FROM history WHERE variant_id = ?1 ORDER BY seq")?;
            let rows = statement.query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        rows.into_iter().map(|(seq, json)| Ok((seq, serde_json::from_str(&json)?))).collect()
    }

    /// Saves the variant's current recipe under `name`.
    pub fn create_snapshot(&self, id: VariantId, name: &str) -> Result<SnapshotId> {
        self.write(|tx| {
            history_position(tx, id)?;
            tx.execute(
                "INSERT INTO snapshots (variant_id, name, recipe, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![id, name, current_recipe_json(tx, id)?, now_ms()],
            )?;
            Ok(SnapshotId(tx.last_insert_rowid()))
        })
    }

    pub fn snapshots(&self, id: VariantId) -> Result<Vec<Snapshot>> {
        let rows: Vec<(Snapshot, String)> = self.read(|conn| {
            let mut statement = conn.prepare_cached(
                "SELECT id, variant_id, name, recipe, created_at FROM snapshots
                 WHERE variant_id = ?1 ORDER BY created_at, id",
            )?;
            let rows = statement.query_map([id], |row| {
                let snapshot = Snapshot {
                    id: row.get(0)?,
                    variant: row.get(1)?,
                    name: row.get(2)?,
                    recipe: Recipe::default(),
                    created_at: row.get(4)?,
                };
                Ok((snapshot, row.get(3)?))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        rows.into_iter()
            .map(|(snapshot, json)| {
                Ok(Snapshot { recipe: serde_json::from_str(&json)?, ..snapshot })
            })
            .collect()
    }

    /// Restores a snapshot as a new history step, so applying one can be undone.
    pub fn apply_snapshot(&self, id: SnapshotId) -> Result<Recipe> {
        self.write(|tx| {
            let (variant, name, json): (VariantId, String, String) = tx
                .query_row(
                    "SELECT variant_id, name, recipe FROM snapshots WHERE id = ?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?
                .ok_or(Error::NotFound { kind: "snapshot", id: id.0 })?;
            commit_step(tx, variant, &format!("Snapshot: {name}"), &json)?;
            Ok(serde_json::from_str(&json)?)
        })
    }

    pub fn delete_snapshot(&self, id: SnapshotId) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM snapshots WHERE id = ?1", [id])?;
            Ok(())
        })
    }
}

macro_rules! bulk_setter {
    ($name:ident, $sql:literal, $value:ty) => {
        pub(crate) fn $name(tx: &Transaction, ids: &[VariantId], value: $value) -> Result<()> {
            let mut statement = tx.prepare_cached($sql)?;
            for id in ids {
                if statement.execute(params![id, value])? == 0 {
                    return Err(not_found(*id));
                }
                mark_sidecar_dirty(tx, *id)?;
            }
            Ok(())
        }
    };
}

bulk_setter!(set_rating, "UPDATE variants SET rating = ?2 WHERE id = ?1", u8);
bulk_setter!(set_flag, "UPDATE variants SET flag = ?2 WHERE id = ?1", Flag);
bulk_setter!(set_label, "UPDATE variants SET label = ?2 WHERE id = ?1", Label);
