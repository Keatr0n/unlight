use crate::{
    Catalog, Label, PhotoId, Result,
    error::io_error,
    keywords::{keyword_paths, replace_keywords},
    model::FileStamp,
    photos::load_photo,
    variants::{
        commit_step, current_recipe_json, master_variant, recipe_json, set_flag, set_label,
        set_rating,
    },
    xmp::{XmpData, read_sidecar, sidecar_path, write_sidecar},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    fs,
    path::{Path, PathBuf},
};
use unlight_recipe::Recipe;

fn file_stamp(path: &Path) -> Result<FileStamp> {
    let metadata = fs::metadata(path).map_err(io_error(path))?;
    Ok(FileStamp::of(&metadata))
}

fn sidecar_contents(conn: &Connection, photo: PhotoId) -> Result<XmpData> {
    let variant = master_variant(conn, photo)?;
    let (rating, flag, label) = conn
        .prepare_cached("SELECT rating, flag, label FROM variants WHERE id = ?1")?
        .query_row([variant], |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, Label>(2)?)))?;
    let recipe: Recipe = serde_json::from_str(&current_recipe_json(conn, variant)?)?;
    Ok(XmpData {
        rating: Some(rating),
        flag: Some(flag),
        label: (label != Label::None).then_some(label),
        keywords: keyword_paths(conn, variant)?,
        recipe: (recipe != Recipe::default()).then_some(recipe),
    })
}

/// Marks the catalog and the sidecar as agreeing, as of this version of the file.
pub(crate) fn record_synced(tx: &Transaction, photo: PhotoId, stamp: FileStamp) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO sidecars (photo_id, size, mtime_ns, dirty, conflict) VALUES (?1, ?2, ?3, 0, 0)
         ON CONFLICT (photo_id) DO UPDATE SET
             size = excluded.size, mtime_ns = excluded.mtime_ns, dirty = 0, conflict = 0",
    )?
    .execute(params![photo, stamp.size, stamp.mtime_ns])?;
    Ok(())
}

/// Notes that the file changed while the catalog had unsaved changes of its own.
pub(crate) fn record_conflict(tx: &Transaction, photo: PhotoId, stamp: FileStamp) -> Result<()> {
    tx.prepare_cached(
        "UPDATE sidecars SET size = ?2, mtime_ns = ?3, conflict = 1 WHERE photo_id = ?1",
    )?
    .execute(params![photo, stamp.size, stamp.mtime_ns])?;
    Ok(())
}

pub(crate) fn is_dirty(conn: &Connection, photo: PhotoId) -> Result<bool> {
    let dirty = conn
        .prepare_cached("SELECT dirty FROM sidecars WHERE photo_id = ?1")?
        .query_row([photo], |row| row.get(0))
        .optional()?;
    Ok(dirty.unwrap_or(false))
}

/// Copies a sidecar's metadata onto the photo's master variant. The sidecar's keywords
/// replace the variant's. A recipe joins the history when there is one to join, so it can be
/// undone.
pub(crate) fn apply_sidecar(tx: &Transaction, photo: PhotoId, data: &XmpData) -> Result<()> {
    let variant = master_variant(tx, photo)?;
    if let Some(rating) = data.rating {
        set_rating(tx, &[variant], rating)?;
    }
    if let Some(flag) = data.flag {
        set_flag(tx, &[variant], flag)?;
    }
    if let Some(label) = data.label {
        set_label(tx, &[variant], label)?;
    }
    replace_keywords(tx, variant, &data.keywords)?;
    if let Some(recipe) = &data.recipe {
        commit_step(tx, variant, "Read from sidecar", &recipe_json(recipe)?)?;
    }
    Ok(())
}

fn photos_where(conn: &Connection, sql: &str) -> Result<Vec<PhotoId>> {
    let mut statement = conn.prepare_cached(sql)?;
    let photos = statement.query_map([], |row| row.get(0))?;
    Ok(photos.collect::<rusqlite::Result<_>>()?)
}

impl Catalog {
    /// Writes the master variant's metadata and recipe to the photo's sidecar, keeping any
    /// foreign content, and returns the sidecar's path. The catalog wins any conflict.
    /// Virtual copies stay in the catalog.
    pub fn write_sidecar(&self, photo: PhotoId) -> Result<PathBuf> {
        self.write(|tx| {
            let raw = load_photo(tx, photo)?.path;
            let path = sidecar_path(&raw);
            write_sidecar(&path, &sidecar_contents(tx, photo)?)?;
            record_synced(tx, photo, file_stamp(&path)?)?;
            Ok(path)
        })
    }

    /// Replaces the master variant's metadata with the photo's sidecar. The file wins any
    /// conflict.
    pub fn read_sidecar(&self, photo: PhotoId) -> Result<()> {
        let raw = self.photo(photo)?.path;
        let path = sidecar_path(&raw);
        let data = read_sidecar(&path)?;
        let stamp = file_stamp(&path)?;
        self.write(|tx| {
            apply_sidecar(tx, photo, &data)?;
            record_synced(tx, photo, stamp)
        })
    }

    /// Photos whose catalog metadata has changed since their sidecar was last in sync.
    pub fn dirty_sidecars(&self) -> Result<Vec<PhotoId>> {
        self.read(|conn| {
            photos_where(conn, "SELECT photo_id FROM sidecars WHERE dirty = 1 ORDER BY photo_id")
        })
    }

    /// Photos whose sidecar changed on disk while the catalog also had unsaved changes.
    /// Resolve one with [`Catalog::write_sidecar`] or [`Catalog::read_sidecar`].
    pub fn sidecar_conflicts(&self) -> Result<Vec<PhotoId>> {
        self.read(|conn| {
            photos_where(conn, "SELECT photo_id FROM sidecars WHERE conflict = 1 ORDER BY photo_id")
        })
    }
}
