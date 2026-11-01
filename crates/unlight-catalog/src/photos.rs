use crate::{Catalog, Error, Fingerprint, FolderId, PhotoId, PhotoStatus, Result};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Folder {
    pub id: FolderId,
    pub parent: Option<FolderId>,
    pub path: PathBuf,
    /// Photos directly in this folder, not counting subfolders.
    pub photo_count: u64,
}

/// A photo's file and what its headers say.
#[derive(Clone, Debug, PartialEq)]
pub struct Photo {
    pub id: PhotoId,
    pub folder: FolderId,
    pub path: PathBuf,
    pub size: u64,
    pub fingerprint: Fingerprint,
    pub status: PhotoStatus,
    /// Why LibRaw rejected the file, for an unsupported photo.
    pub status_detail: Option<String>,
    pub camera: String,
    pub lens: String,
    pub iso: Option<f64>,
    pub shutter: Option<f64>,
    pub aperture: Option<f64>,
    pub focal_length: Option<f64>,
    /// Seconds since the Unix epoch.
    pub capture_time: Option<i64>,
    pub width: u32,
    pub height: u32,
    /// LibRaw flip code.
    pub orientation: i32,
}

const PHOTO_SELECT: &str = "
    SELECT p.id, p.folder_id, f.path, p.file_name, p.size, p.fingerprint, p.status,
           p.status_detail, p.camera, p.lens, p.iso, p.shutter, p.aperture, p.focal_length,
           p.capture_time, p.width, p.height, p.orientation
    FROM photos p JOIN folders f ON f.id = p.folder_id
    WHERE p.id = ?1";

fn photo_from_row(row: &Row) -> rusqlite::Result<Photo> {
    let folder_path: String = row.get(2)?;
    let file_name: String = row.get(3)?;
    Ok(Photo {
        id: row.get(0)?,
        folder: row.get(1)?,
        path: Path::new(&folder_path).join(file_name),
        size: row.get::<_, i64>(4)? as u64,
        fingerprint: row.get(5)?,
        status: row.get(6)?,
        status_detail: row.get(7)?,
        camera: row.get(8)?,
        lens: row.get(9)?,
        iso: row.get(10)?,
        shutter: row.get(11)?,
        aperture: row.get(12)?,
        focal_length: row.get(13)?,
        capture_time: row.get(14)?,
        width: row.get(15)?,
        height: row.get(16)?,
        orientation: row.get(17)?,
    })
}

pub(crate) fn load_photo(conn: &Connection, id: PhotoId) -> Result<Photo> {
    conn.prepare_cached(PHOTO_SELECT)?
        .query_row([id], photo_from_row)
        .optional()?
        .ok_or(Error::NotFound { kind: "photo", id: id.0 })
}

fn distinct_values(conn: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut statement = conn.prepare_cached(sql)?;
    let values = statement.query_map([], |row| row.get(0))?;
    Ok(values.collect::<rusqlite::Result<_>>()?)
}

impl Catalog {
    pub fn photo(&self, id: PhotoId) -> Result<Photo> {
        self.read(|conn| load_photo(conn, id))
    }

    pub fn folders(&self) -> Result<Vec<Folder>> {
        self.read(|conn| {
            let mut statement = conn.prepare_cached(
                "SELECT f.id, f.parent_id, f.path, count(p.id)
                 FROM folders f LEFT JOIN photos p ON p.folder_id = f.id
                 GROUP BY f.id ORDER BY f.path",
            )?;
            let folders = statement.query_map([], |row| {
                Ok(Folder {
                    id: row.get(0)?,
                    parent: row.get(1)?,
                    path: PathBuf::from(row.get::<_, String>(2)?),
                    photo_count: row.get::<_, i64>(3)? as u64,
                })
            })?;
            Ok(folders.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// Every camera in the catalog, for a filter menu.
    pub fn cameras(&self) -> Result<Vec<String>> {
        self.read(|conn| {
            distinct_values(
                conn,
                "SELECT DISTINCT camera FROM photos WHERE camera <> '' ORDER BY camera",
            )
        })
    }

    /// Every lens in the catalog, for a filter menu.
    pub fn lenses(&self) -> Result<Vec<String>> {
        self.read(|conn| {
            distinct_values(conn, "SELECT DISTINCT lens FROM photos WHERE lens <> '' ORDER BY lens")
        })
    }

    /// Removes photos and their edits from the catalog. Files on disk are left alone.
    pub fn remove_photos(&self, ids: &[PhotoId]) -> Result<()> {
        self.write(|tx| {
            let mut statement = tx.prepare_cached("DELETE FROM photos WHERE id = ?1")?;
            for id in ids {
                statement.execute([id])?;
            }
            Ok(())
        })
    }

    /// Removes a folder, its subfolders and their photos from the catalog. Files on disk are
    /// left alone.
    pub fn remove_folder(&self, id: FolderId) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM folders WHERE id = ?1", [id])?;
            Ok(())
        })
    }
}

pub(crate) fn set_status(conn: &Connection, id: PhotoId, status: PhotoStatus) -> Result<()> {
    conn.prepare_cached("UPDATE photos SET status = ?2 WHERE id = ?1")?
        .execute(params![id, status])?;
    Ok(())
}
