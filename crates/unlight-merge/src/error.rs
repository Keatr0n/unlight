use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error("the merge was cancelled")]
    Cancelled,
    #[error("a merge needs at least two photos")]
    TooFewFrames,
    #[error("{0} is a different size from the other photos")]
    SizeMismatch(PathBuf),
    #[error("{0} is already merged, so it can't be merged again")]
    AlreadyMerged(PathBuf),
    #[error("couldn't work out the exposure of {0}: its file doesn't say and it shares too little with the others")]
    UnknownExposure(PathBuf),
    #[error("these photos don't overlap enough to stitch")]
    NoOverlap,
    #[error("a {degrees:.0}° view is too wide for a perspective projection, use spherical or cylindrical")]
    FieldOfView { degrees: f64 },
    #[error("couldn't read a photo: {0}")]
    Source(String),
    #[error(transparent)]
    Raw(#[from] unlight_raw::Error),
    #[error("couldn't write the merged photo: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, MergeError>;
