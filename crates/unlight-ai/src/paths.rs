use crate::error::{Error, Result};
use std::{
    env,
    path::{Path, PathBuf},
};

/// `$XDG_DATA_HOME/unlight`, or `~/.local/share/unlight` when that is unset. The spec says a
/// relative value must be ignored.
pub fn data_directory() -> Result<PathBuf> {
    let base = env::var_os("XDG_DATA_HOME")
        .filter(|value| Path::new(value).is_absolute())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")))
        .ok_or(Error::NoDataDirectory)?;
    Ok(base.join("unlight"))
}
