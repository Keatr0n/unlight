//! Where the user left off, so reopening the app lands in the same place.

use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub variant: Option<i64>,
    pub develop: bool,
    /// A folder and whether it includes its subfolders.
    pub folder: Option<(i64, bool)>,
    pub collection: Option<i64>,
}

impl Session {
    /// A missing or unreadable file just means starting fresh.
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        fs::write(path, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_round_trips_and_survives_nonsense() {
        let directory = std::env::temp_dir().join(format!("unlight-session-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("session.json");
        let session = Session {
            variant: Some(5),
            develop: true,
            ..Session::default()
        };
        session.save(&path).unwrap();
        assert_eq!(Session::load(&path), session);
        fs::write(&path, b"not json").unwrap();
        assert_eq!(Session::load(&path), Session::default());
        fs::remove_dir_all(&directory).unwrap();
    }
}
