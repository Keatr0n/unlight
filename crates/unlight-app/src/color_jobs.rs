//! Colour work too slow for a frame: baking a proof table, and reading folders of profiles.

use crate::state::proof::{BakeKey, IccProfile};
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};
use unlight_engine::{ProfileLibrary, ProofLut, profile_directory};
use unlight_output::{bake_proof, proof_profile_name, read_proof_profile};

pub enum ColorEvent {
    Baked { key: BakeKey, outcome: Result<Arc<ProofLut>, String> },
    ProofProfiles(Vec<IccProfile>),
    /// A profile the user picked by hand, or why it can't be used.
    ChosenProofProfile(Result<IccProfile, String>),
    ProfilesRescanned { found: usize, rejected: usize },
}

pub struct ColorJobs {
    sender: Sender<ColorEvent>,
    receiver: Receiver<ColorEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

/// Where ICC profiles live: this app's own folder, then the freedesktop colour directories.
pub fn icc_directories() -> Vec<PathBuf> {
    let home = env::var_os("HOME").map(PathBuf::from);
    let data_home = env::var_os("XDG_DATA_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".local/share")));
    let mut directories: Vec<PathBuf> = Vec::new();
    directories.extend(data_home.iter().map(|data| data.join("unlight/icc")));
    directories.extend(data_home.iter().map(|data| data.join("color/icc")));
    directories.extend(data_home.iter().map(|data| data.join("icc")));
    directories.extend(home.iter().map(|home| home.join(".color/icc")));
    let shared = env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    directories.extend(shared.split(':').filter(|dir| !dir.is_empty()).map(|dir| Path::new(dir).join("color/icc")));
    directories
}

fn is_icc(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("icc") || extension.eq_ignore_ascii_case("icm")
    })
}

fn read_profile(path: &Path) -> Result<IccProfile, String> {
    let bytes = read_proof_profile(path).map_err(|error| error.to_string())?;
    let name = proof_profile_name(&bytes).map_err(|error| error.to_string())?;
    let fallback = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = if name.is_empty() { fallback.into_owned() } else { name };
    Ok(IccProfile { path: path.to_owned(), name })
}

/// Every readable profile under some folders. A file that isn't one is simply not listed.
pub fn find_profiles(directories: &[PathBuf]) -> Vec<IccProfile> {
    let mut found: Vec<IccProfile> = Vec::new();
    for directory in directories {
        let files = walkdir::WalkDir::new(directory).follow_links(true).into_iter().flatten();
        let paths = files.filter(|entry| is_icc(entry.path()));
        found.extend(paths.filter_map(|entry| read_profile(entry.path()).ok()));
    }
    found.sort_by_key(|profile| profile.name.to_lowercase());
    found.dedup_by(|a, b| a.path == b.path);
    found
}

impl ColorJobs {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self { sender, receiver, wake }
    }

    pub fn try_next(&self) -> Option<ColorEvent> {
        self.receiver.try_recv().ok()
    }

    fn spawn(&self, name: &str, job: impl FnOnce() -> Option<ColorEvent> + Send + 'static) {
        let (sender, wake) = (self.sender.clone(), self.wake.clone());
        let spawned = thread::Builder::new().name(name.to_owned()).spawn(move || {
            if let Some(event) = job() {
                let _ = sender.send(event);
                wake();
            }
        });
        if let Err(error) = spawned {
            eprintln!("couldn't start {name}: {error}");
        }
    }

    pub fn bake(&self, key: BakeKey) {
        self.spawn("unlight-proof", move || {
            let table = read_proof_profile(&key.0).and_then(|bytes| bake_proof(&bytes, key.1));
            let outcome = table.map(Arc::new).map_err(|error| error.to_string());
            Some(ColorEvent::Baked { key, outcome })
        });
    }

    pub fn search_proof_profiles(&self) {
        self.spawn("unlight-icc", || Some(ColorEvent::ProofProfiles(find_profiles(&icc_directories()))));
    }

    /// The portal dialog blocks until closed, so it runs beside the UI.
    pub fn choose_proof_profile(&self) {
        self.spawn("unlight-choose", || {
            let path = rfd::FileDialog::new().add_filter("ICC profile", &["icc", "icm"]).pick_file()?;
            Some(ColorEvent::ChosenProofProfile(read_profile(&path)))
        });
    }

    pub fn rescan_profiles(&self) {
        self.spawn("unlight-profiles", || {
            let library = ProfileLibrary::rescan();
            Some(ColorEvent::ProfilesRescanned { found: library.profiles.len(), rejected: library.rejected.len() })
        });
    }
}

/// Shows the profile folder in the file manager, making it first so there's somewhere to drop
/// files. Returns the folder, or why it couldn't be opened and where it is.
pub fn open_profile_folder() -> Result<PathBuf, String> {
    let Some(directory) = profile_directory() else {
        return Err("There's no home folder to keep profiles in".to_owned());
    };
    let where_it_is = directory.display().to_string();
    std::fs::create_dir_all(&directory).map_err(|error| format!("Couldn't make {where_it_is}: {error}"))?;
    let mut opener = Command::new("xdg-open")
        .arg(&directory)
        .spawn()
        .map_err(|_| format!("Camera profiles live in {where_it_is}"))?;
    thread::spawn(move || opener.wait());
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_output::{OutputSpace, icc_profile};

    #[test]
    fn profiles_are_found_in_subfolders_and_other_files_are_passed_over() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("printers/epson");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("adobe.icc"), icc_profile(OutputSpace::AdobeRgb).unwrap()).unwrap();
        std::fs::write(directory.path().join("srgb.ICM"), icc_profile(OutputSpace::Srgb).unwrap()).unwrap();
        std::fs::write(directory.path().join("broken.icc"), b"nonsense").unwrap();
        std::fs::write(directory.path().join("notes.txt"), b"hello").unwrap();
        let found = find_profiles(&[directory.path().to_owned()]);
        let names: Vec<&str> = found.iter().map(|profile| profile.name.as_str()).collect();
        assert_eq!(names, ["Compatible with Adobe RGB (1998)", "sRGB"]);
    }

    #[test]
    fn a_missing_folder_finds_nothing() {
        assert!(find_profiles(&["/nonexistent/unlight/icc".into()]).is_empty());
    }
}
