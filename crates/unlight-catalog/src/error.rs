use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("catalog database: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error(transparent)]
    Raw(#[from] unlight_raw::Error),
    #[error("stored JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "catalog schema {found} is newer than version {supported}, which this build understands"
    )]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("{kind} {id} is not in the catalog")]
    NotFound { kind: &'static str, id: i64 },
    #[error("a rating runs 0 to 5, got {0}")]
    InvalidRating(u8),
    #[error("the master variant can't be deleted, remove the photo instead")]
    MasterVariant,
    #[error("the recipe holds a NaN or infinite value")]
    InvalidRecipe,
    #[error("path is not valid UTF-8: {}", .0.display())]
    NonUtf8Path(PathBuf),
    #[error("malformed XMP in {}: {detail}", path.display())]
    MalformedXmp { path: PathBuf, detail: String },
    #[error("can't decode preview: {0}")]
    Decode(String),
    #[error("can't encode preview: {0}")]
    Encode(String),
    #[error("neither XDG_CACHE_HOME nor HOME is set")]
    NoCacheDir,
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io_error(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io { path: path.to_owned(), source }
}
