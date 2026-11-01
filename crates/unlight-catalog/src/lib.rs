//! The library side of Unlight: a SQLite catalog of photos and their edits, folder import,
//! library queries, the preview cache and XMP sidecars.
//!
//! Everything here is synchronous. [`Catalog`] and [`PreviewCache`] are `Send + Sync`, so a
//! UI shares them behind an `Arc` and calls them from its worker threads.

mod clause;
mod collections;
mod db;
mod error;
mod files;
mod image;
mod import;
mod keywords;
mod migrations;
mod model;
mod photos;
mod presets;
mod previews;
mod query;
mod rules;
mod sidecars;
mod variants;
pub mod xmp;

pub use clause::{Comparison, TextOp};
pub use collections::Collection;
pub use db::Catalog;
pub use error::{Error, Result};
pub use image::RgbaImage;
pub use import::{CancelToken, RAW_EXTENSIONS, ScanFailure, ScanPhase, ScanProgress, ScanReport};
pub use keywords::Keyword;
pub use model::{
    CollectionId, Fingerprint, Flag, FolderId, KeywordId, Label, PhotoId, PhotoStatus, PresetId,
    SnapshotId, VariantId,
};
pub use photos::{Folder, Photo};
pub use presets::{PartialRecipe, Preset};
pub use previews::{
    DEFAULT_CAPACITY_BYTES, PreviewCache, PreviewSize, PreviewSource, default_cache_dir,
};
pub use query::{DateRange, Filter, FolderFilter, LibraryRow, Page, RatingFilter, Sort, SortKey};
pub use rules::{MatchMode, Rule, SmartRules};
pub use variants::{HISTORY_CAP, History, HistoryStep, Snapshot, Variant};
