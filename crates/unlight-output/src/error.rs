use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("a {width}×{height} RGB image needs {expected} samples, got {actual}")]
    ImageSize {
        width: u32,
        height: u32,
        expected: usize,
        actual: usize,
    },
    #[error("the image is in {image} but the settings ask for {settings}")]
    ColorSpaceMismatch {
        image: &'static str,
        settings: &'static str,
    },
    #[error("invalid export settings: {0}")]
    Settings(String),
    #[error("filename template: {0}")]
    Template(String),
    /// Not a failure as such: the collision policy is skip and nothing was written.
    #[error("{} already exists", path.display())]
    Skipped { path: PathBuf },
    #[error("colour profile: {0}")]
    Icc(#[from] lcms2::Error),
    #[error("{format} encoder: {message}")]
    Encode {
        format: &'static str,
        message: String,
    },
    #[error("{kind} metadata is {size} bytes, more than {format} can hold")]
    MetadataTooLarge {
        kind: &'static str,
        format: &'static str,
        size: usize,
    },
    #[error("resize: {0}")]
    Resize(String),
    #[error("watermark: {0}")]
    Watermark(String),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
