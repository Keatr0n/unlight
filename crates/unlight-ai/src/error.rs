use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the model manifest is invalid: {0}")]
    Manifest(String),
    #[error("no model is called {0:?}")]
    UnknownModel(String),
    #[error("{0} isn't downloaded yet")]
    ModelMissing(String),
    #[error("{} doesn't match its manifest: expected SHA-256 {expected}, got {actual}", path.display())]
    HashMismatch { path: PathBuf, expected: String, actual: String },
    #[error("{url} sent more than the {expected} bytes the manifest lists")]
    TooLarge { url: String, expected: u64 },
    #[error("{url} ended after {received} of {expected} bytes")]
    Truncated { url: String, received: u64, expected: u64 },
    #[error("downloading {url} failed: {message}")]
    Download { url: String, message: String },
    #[error("{0} is already being downloaded")]
    DownloadInProgress(String),
    #[error("cancelled")]
    Cancelled,
    #[error("ONNX Runtime isn't available: {0}")]
    RuntimeMissing(String),
    #[error("inference failed: {0}")]
    Inference(String),
    #[error("the model produced {0:?}, which isn't a single mask")]
    UnexpectedOutput(Vec<i64>),
    #[error("{0:?} isn't a mask id")]
    InvalidMaskId(String),
    #[error("mask {0} is damaged, its contents don't match its name")]
    DamagedMask(String),
    #[error("mask image {path} can't be used: {message}", path = path.display())]
    MaskFormat { path: PathBuf, message: String },
    #[error("resizing failed: {0}")]
    Resize(String),
    #[error("neither XDG_DATA_HOME nor HOME is set")]
    NoDataDirectory,
    #[error("{path}: {source}", path = path.display())]
    Io { path: PathBuf, source: std::io::Error },
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io_error(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.into();
    move |source| Error::Io { path, source }
}
