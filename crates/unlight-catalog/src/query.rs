use crate::{
    Catalog, CollectionId, Comparison, Fingerprint, Flag, FolderId, KeywordId, Label, PhotoId,
    PhotoStatus, PreviewSource, Result, VariantId,
    clause::Clause,
    collections::collection_clause,
    rules::{edited_clause, text_search_clause},
};
use rusqlite::{Connection, Row, params_from_iter, types::Value};
use std::path::{Path, PathBuf};
use unlight_recipe::Orientation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RatingFilter {
    pub comparison: Comparison,
    pub value: u8,
}

/// Capture times from `from` up to but not including `until`, in Unix seconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DateRange {
    pub from: Option<i64>,
    pub until: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderFilter {
    pub folder: FolderId,
    pub include_subfolders: bool,
}

/// What the library shows. Every field set must match. A keyword also matches its
/// descendants, and each word of `text` must appear in the file name, camera, lens or a
/// keyword.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    pub rating: Option<RatingFilter>,
    pub flag: Option<Flag>,
    pub label: Option<Label>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub captured: Option<DateRange>,
    pub keyword: Option<KeywordId>,
    pub folder: Option<FolderFilter>,
    pub collection: Option<CollectionId>,
    pub text: Option<String>,
    pub edited: Option<bool>,
    pub status: Option<PhotoStatus>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortKey {
    #[default]
    CaptureTime,
    FileName,
    Rating,
    EditTime,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    pub offset: u64,
    pub limit: u64,
}

/// One grid cell's worth of data, with nothing that needs a second query.
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryRow {
    pub variant: VariantId,
    pub photo: PhotoId,
    pub path: PathBuf,
    pub fingerprint: Fingerprint,
    pub status: PhotoStatus,
    pub capture_time: Option<i64>,
    pub width: u32,
    pub height: u32,
    /// LibRaw flip code.
    pub orientation: i32,
    pub rating: u8,
    pub flag: Flag,
    pub label: Label,
    pub copy_name: Option<String>,
    pub edited: bool,
    /// How this variant's recipe turns the photo, on top of `orientation`.
    pub turned: Orientation,
}

impl LibraryRow {
    pub fn preview_source(&self) -> PreviewSource<'_> {
        PreviewSource {
            fingerprint: self.fingerprint,
            path: &self.path,
            orientation: self.orientation,
        }
    }
}

const FROM: &str = "FROM variants v JOIN photos p ON p.id = v.photo_id";
const ROW_COLUMNS: &str = "v.id, v.photo_id, f.path, p.file_name, p.fingerprint, p.status,
    p.capture_time, p.width, p.height, p.orientation, v.rating, v.flag, v.label, v.copy_name,
    v.edited_at,
    (SELECT json_extract(r.recipe, '$.orientation') FROM variant_recipes r WHERE r.variant_id = v.id)";

fn row_from_sql(row: &Row) -> rusqlite::Result<LibraryRow> {
    let folder: String = row.get(2)?;
    let file_name: String = row.get(3)?;
    Ok(LibraryRow {
        variant: row.get(0)?,
        photo: row.get(1)?,
        path: Path::new(&folder).join(file_name),
        fingerprint: row.get(4)?,
        status: row.get(5)?,
        capture_time: row.get(6)?,
        width: row.get(7)?,
        height: row.get(8)?,
        orientation: row.get(9)?,
        rating: row.get(10)?,
        flag: row.get(11)?,
        label: row.get(12)?,
        copy_name: row.get(13)?,
        edited: row.get::<_, Option<i64>>(14)?.is_some(),
        turned: row
            .get::<_, Option<String>>(15)?
            .and_then(|name| serde_json::from_value(name.into()).ok())
            .unwrap_or_default(),
    })
}

fn subtree_clause(column: &str, table: &str, id: i64) -> Clause {
    Clause::new(
        format!(
            "{column} IN (WITH RECURSIVE tree(id) AS (
                 SELECT ? UNION SELECT t.id FROM {table} t JOIN tree ON t.parent_id = tree.id
             ) SELECT id FROM tree)"
        ),
        [id.into()],
    )
}

impl Filter {
    /// A smart collection's rules live in the database, so compiling can need a query.
    fn clause(&self, conn: &Connection) -> Result<Clause> {
        let mut parts = self.column_clauses();
        if let Some(keyword) = self.keyword {
            let inner = subtree_clause("keyword_id", "keywords", keyword.0);
            let sql =
                format!("v.id IN (SELECT variant_id FROM variant_keywords WHERE {})", inner.sql);
            parts.push(Clause::new(sql, inner.params));
        }
        if let Some(filter) = self.folder {
            parts.push(if filter.include_subfolders {
                subtree_clause("p.folder_id", "folders", filter.folder.0)
            } else {
                Clause::compare("p.folder_id", Comparison::Equal, filter.folder.0)
            });
        }
        if let Some(collection) = self.collection {
            parts.push(collection_clause(conn, collection)?);
        }
        let terms = self.text.iter().flat_map(|text| text.split_whitespace());
        parts.extend(terms.map(text_search_clause));
        Ok(if parts.is_empty() { Clause::new("1", []) } else { Clause::join(parts, " AND ") })
    }

    fn column_clauses(&self) -> Vec<Clause> {
        let mut parts = Vec::new();
        if let Some(RatingFilter { comparison, value }) = self.rating {
            parts.push(Clause::compare("v.rating", comparison, value as i64));
        }
        if let Some(flag) = self.flag {
            parts.push(Clause::compare("v.flag", Comparison::Equal, flag.code()));
        }
        if let Some(label) = self.label {
            parts.push(Clause::compare("v.label", Comparison::Equal, label.code()));
        }
        if let Some(camera) = &self.camera {
            parts.push(Clause::compare("p.camera", Comparison::Equal, camera.clone()));
        }
        if let Some(lens) = &self.lens {
            parts.push(Clause::compare("p.lens", Comparison::Equal, lens.clone()));
        }
        if let Some(range) = self.captured {
            if let Some(from) = range.from {
                parts.push(Clause::compare("p.capture_time", Comparison::GreaterOrEqual, from));
            }
            if let Some(until) = range.until {
                parts.push(Clause::compare("p.capture_time", Comparison::Less, until));
            }
        }
        if let Some(edited) = self.edited {
            parts.push(edited_clause(edited));
        }
        if let Some(status) = self.status {
            parts.push(Clause::compare("p.status", Comparison::Equal, status.code()));
        }
        parts
    }
}

/// Capture times only have a resolution of one second and import order is arbitrary, so
/// shots from a burst need the file name to stay in sequence. The ids at the end keep paging
/// stable. All but the rating order come straight from an index, so a page stops early.
fn order_by(sort: Sort) -> String {
    let direction = if sort.descending { "DESC" } else { "ASC" };
    let chronological = format!(
        "p.capture_time {direction}, p.file_name COLLATE NOCASE {direction}, p.id {direction}, v.id {direction}"
    );
    match sort.key {
        SortKey::CaptureTime => chronological,
        SortKey::FileName => {
            format!("p.file_name COLLATE NOCASE {direction}, p.id {direction}, v.id {direction}")
        }
        SortKey::Rating => format!("v.rating {direction}, {chronological}"),
        SortKey::EditTime => format!("v.edited_at {direction}, v.id {direction}"),
    }
}

/// Finds the page's ids without touching anything the indexes don't cover. `OFFSET` steps
/// over every skipped row, so doing the full join here makes deep pages crawl.
fn page_ids_sql(where_sql: &str, sort: Sort) -> String {
    format!(
        "SELECT v.id AS page_id {FROM} WHERE {where_sql} ORDER BY {} LIMIT ? OFFSET ?",
        order_by(sort)
    )
}

fn page_sql(where_sql: &str, sort: Sort) -> String {
    format!(
        "SELECT {ROW_COLUMNS}
         FROM ({}) page
         JOIN variants v ON v.id = page.page_id
         JOIN photos p ON p.id = v.photo_id
         JOIN folders f ON f.id = p.folder_id
         ORDER BY {}",
        page_ids_sql(where_sql, sort),
        order_by(sort)
    )
}

impl Catalog {
    /// One page of matching variants.
    pub fn query(&self, filter: &Filter, sort: Sort, page: Page) -> Result<Vec<LibraryRow>> {
        self.read(|conn| {
            let clause = filter.clause(conn)?;
            let mut params = clause.params;
            params.push(Value::Integer(page.limit as i64));
            params.push(Value::Integer(page.offset as i64));
            let mut statement = conn.prepare_cached(&page_sql(&clause.sql, sort))?;
            let rows = statement.query_map(params_from_iter(params), row_from_sql)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// How many variants match.
    pub fn count(&self, filter: &Filter) -> Result<u64> {
        self.read(|conn| {
            let clause = filter.clause(conn)?;
            let sql = format!("SELECT count(*) {FROM} WHERE {}", clause.sql);
            let mut statement = conn.prepare_cached(&sql)?;
            let count: i64 =
                statement.query_row(params_from_iter(clause.params), |row| row.get(0))?;
            Ok(count as u64)
        })
    }

    /// The plan of the part of a query that selects and orders rows, for checking that
    /// indexes are in use.
    #[doc(hidden)]
    pub fn explain_query(&self, filter: &Filter, sort: Sort) -> Result<Vec<String>> {
        self.read(|conn| {
            let clause = filter.clause(conn)?;
            let sql = format!("EXPLAIN QUERY PLAN {}", page_ids_sql(&clause.sql, sort));
            let mut params = clause.params;
            params.extend([Value::Integer(100), Value::Integer(0)]);
            let mut statement = conn.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(params), |row| row.get(3))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
}
