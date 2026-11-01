//! Finds the camera profile for a photo: the one a recipe names, then one embedded in a DNG,
//! then one the user installed for that camera, then the plain calibration in the file.

use crate::{
    CameraProfile,
    dcp::{Dcp, DcpError},
};
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, PoisonError, RwLock},
};
use unlight_raw::{ColorData, RawImage};

/// What a recipe names to ask for the file's plain calibration and the default rendering,
/// whatever else is installed for the camera.
pub const BUILT_IN_PROFILE: &str = "Unlight Default";
/// What to call a DNG's own profile when it doesn't give itself a name.
const UNNAMED_EMBEDDED: &str = "Embedded";

/// Bodies sold under several names, which share a profile. The first name of each group stands
/// for the rest. Spacing and case don't need listing, since `camera_key` already ignores them.
const CAMERA_ALIASES: &[&[&str]] = &[
    &["Canon EOS 100D", "Canon EOS Rebel SL1", "Canon EOS Kiss X7"],
    &["Canon EOS 200D", "Canon EOS Rebel SL2", "Canon EOS Kiss X9"],
    &["Canon EOS 250D", "Canon EOS Rebel SL3", "Canon EOS Kiss X10", "Canon EOS 200D Mark II"],
    &["Canon EOS 450D", "Canon EOS Rebel XSi", "Canon EOS Kiss X2"],
    &["Canon EOS 500D", "Canon EOS Rebel T1i", "Canon EOS Kiss X3"],
    &["Canon EOS 550D", "Canon EOS Rebel T2i", "Canon EOS Kiss X4"],
    &["Canon EOS 600D", "Canon EOS Rebel T3i", "Canon EOS Kiss X5"],
    &["Canon EOS 650D", "Canon EOS Rebel T4i", "Canon EOS Kiss X6i"],
    &["Canon EOS 700D", "Canon EOS Rebel T5i", "Canon EOS Kiss X7i"],
    &["Canon EOS 750D", "Canon EOS Rebel T6i", "Canon EOS Kiss X8i"],
    &["Canon EOS 760D", "Canon EOS Rebel T6s", "Canon EOS 8000D"],
    &["Canon EOS 800D", "Canon EOS Rebel T7i", "Canon EOS Kiss X9i"],
    &["Canon EOS 850D", "Canon EOS Rebel T8i", "Canon EOS Kiss X10i"],
    &["Canon EOS 1000D", "Canon EOS Rebel XS", "Canon EOS Kiss F"],
    &["Canon EOS 1100D", "Canon EOS Rebel T3", "Canon EOS Kiss X50"],
    &["Canon EOS 1200D", "Canon EOS Rebel T5", "Canon EOS Kiss X70"],
    &["Canon EOS 1300D", "Canon EOS Rebel T6", "Canon EOS Kiss X80"],
    &["Canon EOS 2000D", "Canon EOS Rebel T7", "Canon EOS Kiss X90"],
    &["Canon EOS 4000D", "Canon EOS Rebel T100", "Canon EOS 3000D"],
    &["Canon EOS M50", "Canon EOS Kiss M"],
    &["Canon EOS M50 Mark II", "Canon EOS Kiss M2"],
    &["Nikon Z 6_2", "Nikon Z 6II", "Nikon Z 6 II"],
    &["Nikon Z 7_2", "Nikon Z 7II", "Nikon Z 7 II"],
    &["Nikon Z 6_3", "Nikon Z 6III", "Nikon Z 6 III"],
    &["Panasonic DMC-G7", "Panasonic DMC-G70"],
    &["Panasonic DMC-GX85", "Panasonic DMC-GX80", "Panasonic DMC-GX7MK2"],
    &["Panasonic DC-G90", "Panasonic DC-G91", "Panasonic DC-G95", "Panasonic DC-G99"],
    &["Panasonic DC-GX9", "Panasonic DC-GX7MK3"],
    &["Panasonic DC-TZ90", "Panasonic DC-TZ91", "Panasonic DC-ZS70"],
    &["Panasonic DC-FZ1000M2", "Panasonic DC-FZ10002"],
    &["Sony SLT-A99", "Sony SLT-A99V"],
];

/// One `.dcp` file. Only its identity is kept, since a library can hold hundreds of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledProfile {
    pub path: PathBuf,
    pub name: String,
    pub camera: String,
    camera_key: String,
}

/// The `.dcp` files in a directory, in file name order.
#[derive(Debug, Default)]
pub struct ProfileLibrary {
    pub profiles: Vec<InstalledProfile>,
    /// Files that were refused, with the reason.
    pub rejected: Vec<(PathBuf, DcpError)>,
    /// Goes up with each rescan, so a photo knows to look again.
    pub generation: u64,
}

/// `$XDG_DATA_HOME/unlight/profiles`, or `~/.local/share/unlight/profiles` when that is unset.
pub fn profile_directory() -> Option<PathBuf> {
    let base = env::var_os("XDG_DATA_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")))?;
    Some(base.join("unlight/profiles"))
}

fn plain_key(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut words = lower
        .split(|letter: char| letter.is_whitespace() || matches!(letter, '-' | '_' | ',' | '.'))
        .filter(|word| !word.is_empty() && !matches!(*word, "corporation" | "corp" | "co" | "ltd" | "inc"));
    let Some(make) = words.next() else {
        return String::new();
    };
    // Makers put their own name in the model too, as in "NIKON CORPORATION NIKON Z 6", and a
    // DNG's model can end with it, as in "Google-Pixel 4 XL-Google".
    let mut key = make.to_owned();
    words.filter(|word| *word != make).for_each(|word| key.push_str(word));
    key
}

/// What two names for one body have in common.
fn camera_key(name: &str) -> String {
    let key = plain_key(name);
    let group = CAMERA_ALIASES.iter().find(|group| group.iter().any(|alias| plain_key(alias) == key));
    group.map_or(key, |group| plain_key(group[0]))
}

static INSTALLED: RwLock<Option<Arc<ProfileLibrary>>> = RwLock::new(None);

impl ProfileLibrary {
    pub fn scan(directory: &Path) -> Self {
        let mut library = Self::default();
        let Ok(entries) = fs::read_dir(directory) else {
            return library;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("dcp"))
            })
            .collect();
        paths.sort();
        for path in paths {
            match Dcp::read(&path) {
                Ok(profile) => library.profiles.push(InstalledProfile {
                    name: if profile.name.is_empty() {
                        path.file_stem().unwrap_or_default().to_string_lossy().into_owned()
                    } else {
                        profile.name
                    },
                    camera_key: camera_key(&profile.unique_camera_model),
                    camera: profile.unique_camera_model,
                    path,
                }),
                Err(error) => library.rejected.push((path, error)),
            }
        }
        library
    }

    fn scan_installed() -> Self {
        profile_directory().map(|path| Self::scan(&path)).unwrap_or_default()
    }

    /// The user's library, read the first time it's asked for.
    pub fn installed() -> Arc<Self> {
        if let Some(library) = INSTALLED.read().unwrap_or_else(PoisonError::into_inner).as_ref() {
            return library.clone();
        }
        let mut slot = INSTALLED.write().unwrap_or_else(PoisonError::into_inner);
        slot.get_or_insert_with(|| Arc::new(Self::scan_installed())).clone()
    }

    /// Reads the user's library again. Open photos pick the change up at their next render.
    pub fn rescan() -> Arc<Self> {
        let scanned = Self::scan_installed();
        let mut slot = INSTALLED.write().unwrap_or_else(PoisonError::into_inner);
        let generation = slot.as_ref().map_or(0, |library| library.generation) + 1;
        let library = Arc::new(Self { generation, ..scanned });
        *slot = Some(library.clone());
        library
    }

    pub fn for_camera<'a>(&'a self, camera: &str) -> impl Iterator<Item = &'a InstalledProfile> {
        let key = camera_key(camera);
        self.profiles.iter().filter(move |profile| profile.camera_key == key)
    }

    /// A profile by name, this camera's first. Names like "Adobe Standard" repeat across every
    /// camera, and each one holds its own camera's matrices.
    fn named(&self, name: &str, camera: &str) -> Option<&InstalledProfile> {
        let mut same_name = self.profiles.iter().filter(|profile| profile.name == name);
        self.for_camera(camera).find(|profile| profile.name == name).or_else(|| same_name.next())
    }
}

/// The profiles a photo can be given.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileChoices {
    /// What a recipe naming no profile gets, or `None` for the built-in rendering.
    pub automatic: Option<String>,
    /// Names from the file, then from the library for this camera, without repeats.
    pub available: Vec<String>,
}

/// What a photo's profile can be chosen from, kept so a recipe can change the choice later.
#[derive(Clone, Debug)]
pub struct PhotoColor {
    color: ColorData,
    camera: String,
    embedded: Option<Arc<Dcp>>,
    extras: Vec<Arc<Dcp>>,
}

fn display_name(profile: &Dcp) -> &str {
    if profile.name.is_empty() { UNNAMED_EMBEDDED } else { &profile.name }
}

impl PhotoColor {
    pub fn new(raw: &RawImage) -> Self {
        let file = raw.color.dng.as_ref().and_then(|_| fs::read(&raw.metadata.path).ok()).unwrap_or_default();
        Self {
            color: raw.color.clone(),
            camera: raw.metadata.camera(),
            embedded: Dcp::parse(&file).ok().filter(Dcp::has_rendering).map(Arc::new),
            extras: Dcp::parse_extras(&file).into_iter().map(Arc::new).collect(),
        }
    }

    fn installed(path: &Path) -> Option<Arc<Dcp>> {
        Dcp::read(path).ok().map(Arc::new)
    }

    fn matching<'a>(&'a self, library: &'a ProfileLibrary) -> Option<&'a InstalledProfile> {
        library.for_camera(&self.camera).next()
    }

    fn stored_in_file(&self, name: &str) -> Option<CameraProfile> {
        if let Some(profile) = self.embedded.as_ref().filter(|profile| display_name(profile) == name) {
            return Some(CameraProfile::with_embedded(&self.color, profile.clone()));
        }
        let extra = self.extras.iter().find(|profile| display_name(profile) == name);
        extra.map(|profile| CameraProfile::with_dcp(&self.color, profile.clone()))
    }

    /// `requested` is a profile name from a recipe. A name that isn't installed falls through
    /// to the automatic choice, so a recipe from another machine still opens.
    pub fn profile(&self, requested: Option<&str>, library: &ProfileLibrary) -> CameraProfile {
        if requested == Some(BUILT_IN_PROFILE) {
            return CameraProfile::new(&self.color);
        }
        if let Some(profile) = requested.and_then(|name| self.stored_in_file(name)) {
            return profile;
        }
        let named = requested.and_then(|name| library.named(name, &self.camera));
        if let Some(profile) = named.and_then(|profile| Self::installed(&profile.path)) {
            return CameraProfile::with_dcp(&self.color, profile);
        }
        if let Some(profile) = &self.embedded {
            return CameraProfile::with_embedded(&self.color, profile.clone());
        }
        match self.matching(library).and_then(|profile| Self::installed(&profile.path)) {
            Some(profile) => CameraProfile::with_dcp(&self.color, profile),
            None => CameraProfile::new(&self.color),
        }
    }

    /// The profiles on offer for this photo.
    pub fn choices(&self, library: &ProfileLibrary) -> ProfileChoices {
        let automatic = self.embedded.as_deref().map(display_name).map(str::to_owned);
        let automatic = automatic.or_else(|| self.matching(library).map(|profile| profile.name.clone()));
        let from_file = self.embedded.iter().chain(&self.extras).map(|profile| display_name(profile).to_owned());
        let from_library = library.for_camera(&self.camera).map(|profile| profile.name.clone());
        let mut available: Vec<String> = Vec::new();
        for name in from_file.chain(from_library) {
            if !available.contains(&name) {
                available.push(name);
            }
        }
        ProfileChoices { automatic, available }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dcp::testing::{Builder, complete};
    use unlight_raw::{Cfa, DngColor, DngIlluminant, Metadata, Opcodes};

    fn same_camera(a: &str, b: &str) -> bool {
        camera_key(a) == camera_key(b)
    }

    fn raw_photo(camera_model: &str, path: PathBuf, dng: Option<DngColor>) -> RawImage {
        RawImage {
            width: 2,
            height: 2,
            samples: vec![0; 4],
            cfa: Cfa { period: 2, pattern: [[0; 6]; 6] },
            black: [[0.0; 6]; 6],
            white: 1.0,
            color: ColorData {
                xyz_to_camera: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                as_shot_multipliers: [2.0, 1.0, 1.5],
                daylight_multipliers: [2.0, 1.0, 1.5],
                baseline_exposure: 0.0,
                dng,
            },
            opcodes: Opcodes::default(),
            metadata: Metadata {
                make: "Test".into(),
                model: camera_model.into(),
                path,
                ..Metadata::default()
            },
            embedded_lens: None,
        }
    }

    fn dng_color() -> DngColor {
        DngColor {
            illuminants: vec![DngIlluminant {
                illuminant: 21,
                color_matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                forward_matrix: None,
                calibration: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            }],
            analog_balance: [1.0; 3],
            as_shot_neutral: None,
        }
    }

    #[test]
    fn scanning_skips_other_files_and_reports_broken_profiles() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("good.DCP"), complete(b"IIRC")).unwrap();
        fs::write(directory.path().join("broken.dcp"), b"not a profile").unwrap();
        fs::write(directory.path().join("notes.txt"), b"hello").unwrap();
        let library = ProfileLibrary::scan(directory.path());
        assert_eq!(library.profiles.len(), 1);
        assert_eq!(library.profiles[0].name, "Test Profile");
        assert_eq!(library.rejected.len(), 1);
    }

    #[test]
    fn a_missing_directory_is_an_empty_library() {
        let library = ProfileLibrary::scan(Path::new("/nonexistent/unlight/profiles"));
        assert!(library.profiles.is_empty() && library.rejected.is_empty());
    }

    #[test]
    fn a_camera_matches_whatever_the_case_and_spacing() {
        assert!(same_camera("NIKON  Z 6", "Nikon Z 6"));
        assert!(!same_camera("Nikon Z 6", "Nikon Z 7"));
    }

    #[test]
    fn makers_spell_their_own_name_several_ways() {
        assert!(same_camera("NIKON CORPORATION NIKON Z 6", "Nikon Z 6"));
        assert!(same_camera("Canon Canon EOS R", "Canon EOS R"));
        assert!(same_camera("Google-Pixel 4 XL-Google", "Google Pixel 4 XL"));
        assert!(same_camera("SONY ILCE-7RM4", "Sony ILCE-7RM4"));
        assert!(same_camera("OLYMPUS E-M1MarkII", "Olympus E-M1 Mark II"));
    }

    #[test]
    fn one_body_sold_under_several_names_shares_its_profiles() {
        assert!(same_camera("Canon EOS Rebel T7i", "Canon EOS 800D"));
        assert!(same_camera("Canon EOS Kiss X9i", "Canon EOS Rebel T7i"));
        assert!(same_camera("Nikon Z 6_2", "Nikon Z 6II"));
        assert!(same_camera("Panasonic DC-G95", "Panasonic DC-G90"));
        assert!(!same_camera("Canon EOS 800D", "Canon EOS 850D"));
        assert!(!same_camera("Nikon Z 6_2", "Nikon Z 6"), "the second generation is a different sensor");
    }

    fn library_of(profiles: &[(&str, &str)]) -> (tempfile::TempDir, ProfileLibrary) {
        let directory = tempfile::tempdir().unwrap();
        for (index, (name, camera)) in profiles.iter().enumerate() {
            let bytes = Builder::new(b"IIRC")
                .text(50708, camera)
                .text(50936, name)
                .floats(50940, &[0.0, 0.0, 1.0, 1.0])
                .build();
            fs::write(directory.path().join(format!("{index}.dcp")), bytes).unwrap();
        }
        let library = ProfileLibrary::scan(directory.path());
        (directory, library)
    }

    fn photo(model: &str, library: &ProfileLibrary) -> ProfileChoices {
        PhotoColor::new(&raw_photo(model, PathBuf::new(), None)).choices(library)
    }

    #[test]
    fn a_name_every_camera_shares_finds_this_cameras_copy() {
        let (_directory, library) =
            library_of(&[("Adobe Standard", "Test Other"), ("Adobe Standard", "Test Camera")]);
        let found = library.named("Adobe Standard", "Test Camera").unwrap();
        assert_eq!(found.camera, "Test Camera");
    }

    #[test]
    fn the_choices_are_this_cameras_profiles_only() {
        let (_directory, library) = library_of(&[
            ("Adobe Standard", "Test Camera"),
            ("Camera Vivid", "Test Camera"),
            ("Adobe Standard", "Test Other"),
            ("Portrait", "Test Other"),
        ]);
        let choices = photo("Camera", &library);
        assert_eq!(choices.available, ["Adobe Standard", "Camera Vivid"]);
        assert_eq!(choices.automatic.as_deref(), Some("Adobe Standard"));
    }

    #[test]
    fn a_camera_with_nothing_installed_is_on_the_built_in_rendering() {
        let (_directory, library) = library_of(&[("Portrait", "Test Other")]);
        assert_eq!(photo("Camera", &library), ProfileChoices::default());
    }

    #[test]
    fn asking_for_the_built_in_rendering_beats_a_matching_profile() {
        let (_directory, library) = library_of(&[("Adobe Standard", "Test Camera")]);
        let photo = PhotoColor::new(&raw_photo("Camera", PathBuf::new(), None));
        assert!(photo.profile(None, &library).rendering().is_some());
        assert!(photo.profile(Some(BUILT_IN_PROFILE), &library).rendering().is_none());
    }

    #[test]
    fn a_dngs_own_profiles_come_first_and_can_be_named() {
        let extra = Builder::new(b"IIRC").text(50936, "Camera Vivid").floats(50940, &[0.0, 0.0, 1.0, 1.0]).build();
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("photo.dng");
        let bytes = Builder::new(b"II*\0")
            .text(50936, "Main")
            .floats(50940, &[0.0, 0.0, 1.0, 1.0])
            .longs(50933, &[0xfeed_beef])
            .build();
        let start = (bytes.len() as u32).to_le_bytes();
        let slot = bytes.windows(4).position(|field| field == 0xfeed_beef_u32.to_le_bytes()).unwrap();
        let mut bytes = bytes;
        bytes[slot..slot + 4].copy_from_slice(&start);
        bytes.extend(extra);
        fs::write(&file, bytes).unwrap();
        let (_installed, library) = library_of(&[("Adobe Standard", "Test Camera")]);
        let photo = PhotoColor::new(&raw_photo("Camera", file, Some(dng_color())));
        let choices = photo.choices(&library);
        assert_eq!(choices.available, ["Main", "Camera Vivid", "Adobe Standard"]);
        assert_eq!(choices.automatic.as_deref(), Some("Main"));
        let named = photo.profile(Some("Camera Vivid"), &library);
        assert_eq!(named.rendering().unwrap().name, "Camera Vivid");
    }

    #[test]
    fn an_installed_profile_applies_to_its_camera_only() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("camera.dcp"), complete(b"IIRC")).unwrap();
        let library = ProfileLibrary::scan(directory.path());
        let photo = |model: &str| PhotoColor::new(&raw_photo(model, PathBuf::new(), None));
        assert!(photo("Camera").profile(None, &library).rendering().is_some());
        assert!(photo("Other").profile(None, &library).rendering().is_none());
    }

    #[test]
    fn a_recipe_can_name_a_profile_for_another_camera() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("camera.dcp"), complete(b"IIRC")).unwrap();
        let library = ProfileLibrary::scan(directory.path());
        let photo = PhotoColor::new(&raw_photo("Other", PathBuf::new(), None));
        assert!(photo.profile(Some("Test Profile"), &library).rendering().is_some());
    }

    #[test]
    fn a_recipe_naming_a_missing_profile_still_opens() {
        let photo = PhotoColor::new(&raw_photo("Camera", PathBuf::new(), None));
        let profile = photo.profile(Some("From another machine"), &ProfileLibrary::default());
        assert!(profile.rendering().is_none());
    }

    #[test]
    fn a_dng_uses_its_embedded_profile_without_being_asked() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("photo.dng");
        fs::write(&file, complete(b"II*\0")).unwrap();
        let photo = PhotoColor::new(&raw_photo("Camera", file, Some(dng_color())));
        let profile = photo.profile(None, &ProfileLibrary::default());
        assert_eq!(profile.rendering().unwrap().name, "Test Profile");
    }

    #[test]
    fn a_broken_embedded_profile_falls_back_to_the_file_calibration() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("photo.dng");
        fs::write(&file, b"II*\0\x10\0\0\0").unwrap();
        let photo = PhotoColor::new(&raw_photo("Camera", file, Some(dng_color())));
        assert!(photo.profile(None, &ProfileLibrary::default()).rendering().is_none());
    }
}
