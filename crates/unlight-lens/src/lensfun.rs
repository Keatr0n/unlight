//! Safe access to the lensfun database. lensfun's own modifier evaluates each profile, which
//! keeps ptlens, poly3, poly5, crop factors and its normalisation (half the shorter side of
//! the calibration sensor) entirely on its side of the fence: only the resulting radial
//! curves cross over into our coordinates.

use crate::{
    LensError,
    matching::{Candidate, Choice, choose},
    profile::{CURVE_SIZE, Frame, LensProfile, Source},
};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    ptr,
    sync::{Mutex, OnceLock},
};
use unlight_lensfun_sys as sys;
use unlight_raw::Metadata;

/// Focus distance when the file doesn't say. lensfun's documentation suggests this, and
/// vignetting barely changes with focus.
const UNKNOWN_DISTANCE: f32 = 1000.0;
/// Slack on a lens's stated focal range, for bodies that round.
const FOCAL_TOLERANCE: f32 = 1.02;

/// Searches write match scores into database entries, so access has to be exclusive.
struct Database(*mut sys::lfDatabase);

// SAFETY: the pointer is owned here for the life of the process, and every use goes through
// the mutex around it.
unsafe impl Send for Database {}

fn load_database() -> Result<Database, LensError> {
    // SAFETY: lf_db_new has no preconditions and a null result is checked.
    let database = unsafe { sys::lf_db_new() };
    if database.is_null() {
        return Err(LensError::NoDatabase(sys::DATABASE_DIR.to_owned()));
    }
    // SAFETY: the handle is valid. This reads the packaged data plus any per-user updates.
    let status = unsafe { sys::lf_db_load(database) };
    // SAFETY: the handle is valid and the result is a null-terminated array it owns.
    let empty = unsafe {
        let lenses = sys::lf_db_get_lenses(database);
        lenses.is_null() || (*lenses).is_null()
    };
    if status != sys::LF_NO_ERROR || empty {
        return Err(LensError::NoDatabase(sys::DATABASE_DIR.to_owned()));
    }
    Ok(Database(database))
}

fn database() -> Result<&'static Mutex<Database>, LensError> {
    static DATABASE: OnceLock<Result<Mutex<Database>, LensError>> = OnceLock::new();
    DATABASE.get_or_init(|| load_database().map(Mutex::new)).as_ref().map_err(Clone::clone)
}

fn c_string(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap_or_default()
}

/// Every name in a multi-language string: the default, then alternating language and text.
///
/// SAFETY: `text` must be null or a valid lfMLstr.
unsafe fn all_names(text: *const c_char) -> Vec<String> {
    let mut names = Vec::new();
    if text.is_null() {
        return names;
    }
    let mut strings = 0;
    let mut cursor = text;
    // SAFETY: an lfMLstr is a run of NUL-terminated strings closed by an empty one.
    unsafe {
        while *cursor != 0 {
            let entry = CStr::from_ptr(cursor);
            let position = strings;
            if position == 0 || position % 2 == 0 {
                names.push(entry.to_string_lossy().into_owned());
            }
            strings += 1;
            cursor = cursor.add(entry.to_bytes_with_nul().len());
        }
    }
    names
}

/// Copies a null-terminated array lensfun allocated, then releases it.
///
/// SAFETY: `list` must be null or a lensfun-allocated, null-terminated array of pointers.
unsafe fn take_list<T>(list: *mut *const T) -> Vec<*const T> {
    let mut items = Vec::new();
    if list.is_null() {
        return items;
    }
    // SAFETY: the array is null-terminated, and it's freed once after the copy.
    unsafe {
        let mut cursor = list;
        while !(*cursor).is_null() {
            items.push(*cursor);
            cursor = cursor.add(1);
        }
        sys::lf_free(list.cast::<c_void>());
    }
    items
}

fn find_camera(database: &Database, metadata: &Metadata) -> Result<*const sys::lfCamera, LensError> {
    let (make, model) = (c_string(&metadata.make), c_string(&metadata.model));
    // SAFETY: all pointers are valid for the call and the result is handled by take_list.
    let cameras = unsafe {
        take_list(sys::lf_db_find_cameras_ext(database.0, make.as_ptr(), model.as_ptr(), 0))
    };
    cameras.first().copied().ok_or_else(|| LensError::UnknownCamera(metadata.camera()))
}

fn find_lens(
    database: &Database,
    camera: *const sys::lfCamera,
    metadata: &Metadata,
) -> Result<*const sys::lfLens, LensError> {
    let name = c_string(&metadata.lens);
    let flags = sys::LF_SEARCH_LOOSE as i32;
    // SAFETY: all pointers are valid for the call and the result is handled by take_list.
    let found = unsafe {
        take_list(sys::lf_db_find_lenses_hd(database.0, camera, ptr::null(), name.as_ptr(), flags))
    };
    // SAFETY: the entries belong to the database, which outlives this call.
    let candidates: Vec<Candidate<*const sys::lfLens>> = found
        .iter()
        .map(|lens| unsafe {
            Candidate {
                names: all_names((**lens).Model),
                maker: all_names((**lens).Maker).into_iter().next().unwrap_or_default(),
                value: lens,
            }
        })
        .collect();
    match choose(&metadata.lens, &candidates) {
        Choice::Found(&lens) => Ok(lens),
        Choice::Ambiguous(names) => Err(LensError::AmbiguousLens {
            lens: metadata.lens.clone(),
            candidates: names.join(", "),
        }),
        Choice::Missing => Err(LensError::UnknownLens {
            camera: metadata.camera(),
            lens: metadata.lens.clone(),
        }),
    }
}

struct Modifier(*mut sys::lfModifier);

impl Drop for Modifier {
    fn drop(&mut self) {
        // SAFETY: the pointer came from lf_modifier_new and is destroyed exactly once.
        unsafe { sys::lf_modifier_destroy(self.0) };
    }
}

/// lensfun requires 16-byte alignment here for its SIMD paths.
#[repr(align(16))]
struct SubpixelCoordinates([f32; 6]);

impl Modifier {
    fn subpixel(&self, x: f32, y: f32) -> Option<[f32; 6]> {
        let mut result = SubpixelCoordinates([0.0; 6]);
        // SAFETY: the buffer holds the 6 floats one pixel needs and has the alignment required.
        let filled = unsafe {
            sys::lf_modifier_apply_subpixel_geometry_distortion(self.0, x, y, 1, 1, result.0.as_mut_ptr())
        };
        (filled != 0).then_some(result.0)
    }

    fn vignette_gain(&self, x: f32, y: f32) -> Option<f32> {
        let mut pixel = 1.0f32;
        let role = sys::LF_CR_INTENSITY as i32;
        let stride = size_of::<f32>() as i32;
        // SAFETY: `pixel` is the one F32 sample a 1×1 block with this role touches.
        let done = unsafe {
            sys::lf_modifier_apply_color_modification(
                self.0,
                (&raw mut pixel).cast::<c_void>(),
                x,
                y,
                1,
                1,
                role,
                stride,
            )
        };
        (done != 0).then_some(pixel)
    }
}

/// Reads a curve off the modifier by probing outward from the optical centre in both
/// directions. Halving the span cancels lensfun's half-pixel view of where the centre is.
struct Probe<'a> {
    modifier: &'a Modifier,
    origin: [f32; 2],
    height: f64,
}

impl Probe<'_> {
    fn source_radii(&self, radius: f64) -> Option<[f64; 3]> {
        let span = (radius * self.height) as f32;
        let [x, y] = self.origin;
        let plus = self.modifier.subpixel(x + span, y)?;
        let minus = self.modifier.subpixel(x - span, y)?;
        let channel = |index: usize| f64::from(plus[index * 2] - minus[index * 2]) * 0.5 / self.height;
        Some([channel(0), channel(1), channel(2)])
    }
}

fn smallest_probe(frame: Frame) -> f64 {
    frame.half_diagonal() / CURVE_SIZE as f64
}

fn sampled_profile(
    modifier: &Modifier,
    flags: i32,
    lens: *const sys::lfLens,
    frame: Frame,
) -> Result<LensProfile, LensError> {
    // SAFETY: the lens belongs to the database, which outlives this call.
    let (shift_x, shift_y, name) = unsafe {
        ((*lens).CenterX, (*lens).CenterY, all_names((*lens).Model).into_iter().next().unwrap_or_default())
    };
    let longest = frame.width.max(frame.height) / frame.height;
    let center = [f64::from(shift_x) * longest, f64::from(shift_y) * longest];
    let origin = [
        ((center[0] / frame.aspect() + 0.5) * frame.width) as f32,
        ((center[1] + 0.5) * frame.height) as f32,
    ];
    let probe = Probe { modifier, origin, height: frame.height };
    let smallest = smallest_probe(frame);

    let mut profile = LensProfile::empty(frame);
    profile.center = center.map(|value| value as f32);
    profile.vignette_center = profile.center;
    profile.lens = Some(name.clone());

    let has = |flag: i32| flags & flag != 0;
    if has(sys::LF_MODIFY_DISTORTION) {
        let scale = |radius: f64| {
            let radius = radius.max(smallest);
            probe.source_radii(radius).map_or(1.0, |[_, green, _]| green / radius)
        };
        profile.set_distortion(Source::Lensfun, scale);
    }
    if has(sys::LF_MODIFY_TCA) {
        let scale = |radius: f64| {
            let radius = radius.max(smallest);
            probe.source_radii(radius).map_or([1.0, 1.0], |[red, green, blue]| [red / green, blue / green])
        };
        profile.set_chromatic_by_output(Source::Lensfun, scale);
    }
    if has(sys::LF_MODIFY_VIGNETTING) {
        let stops = |radius: f64| {
            let x = probe.origin[0] + (radius * probe.height) as f32;
            modifier.vignette_gain(x, probe.origin[1]).map_or(0.0, |gain| f64::from(gain).max(1e-6).log2())
        };
        profile.set_vignette(Source::Lensfun, stops);
    }
    if profile.distortion.is_none() && profile.chromatic.is_none() && profile.vignette.is_none() {
        return Err(LensError::NoCalibration(name));
    }
    Ok(profile)
}

/// Looks a photo's lens up and evaluates its calibration at the shot's settings.
pub(crate) fn profile(metadata: &Metadata, frame: Frame) -> Result<LensProfile, LensError> {
    if metadata.lens.is_empty() {
        return Err(LensError::UnnamedLens(metadata.camera()));
    }
    if metadata.focal_length <= 0.0 {
        return Err(LensError::NoFocalLength);
    }
    let database = database()?.lock().map_err(|_| LensError::NoDatabase(sys::DATABASE_DIR.to_owned()))?;
    let camera = find_camera(&database, metadata)?;
    let lens = find_lens(&database, camera, metadata)?;

    let focal = metadata.focal_length;
    // SAFETY: both entries belong to the database, whose lock is held.
    let (crop, min_focal, max_focal, lens_type) = unsafe {
        ((*camera).CropFactor, (*lens).MinFocal, (*lens).MaxFocal, (*lens).Type)
    };
    if focal * FOCAL_TOLERANCE < min_focal || focal > max_focal * FOCAL_TOLERANCE {
        return Err(LensError::FocalOutOfRange { lens: metadata.lens.clone(), focal });
    }

    let (width, height) = (frame.width as i32, frame.height as i32);
    // SAFETY: the lens is valid and a null result is checked.
    let raw = unsafe { sys::lf_modifier_new(lens, crop, width, height) };
    if raw.is_null() {
        return Err(LensError::NoCalibration(metadata.lens.clone()));
    }
    let modifier = Modifier(raw);
    let wanted = sys::LF_MODIFY_TCA | sys::LF_MODIFY_VIGNETTING | sys::LF_MODIFY_DISTORTION;
    // SAFETY: the modifier and lens are valid. Keeping the lens's own type requests no
    // projection change, and a scale of 1 leaves zoom to the engine.
    let flags = unsafe {
        sys::lf_modifier_initialize(
            modifier.0,
            lens,
            sys::LF_PF_F32,
            focal,
            metadata.aperture,
            UNKNOWN_DISTANCE,
            1.0,
            lens_type,
            wanted,
            0,
        )
    };
    sampled_profile(&modifier, flags, lens, frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(make: &str, model: &str, lens: &str, focal: f32, aperture: f32) -> Metadata {
        Metadata {
            make: make.into(),
            model: model.into(),
            lens: lens.into(),
            focal_length: focal,
            aperture,
            ..Metadata::default()
        }
    }

    const FULL_FRAME: Frame = Frame { width: 6000.0, height: 4000.0 };

    /// Sony FE 24-70mm f/4 ZA OSS at 24 mm, from the database: a=0.02797 b=-0.09138 c=0.04487.
    fn ptlens_scale(radius: f64) -> f64 {
        let (a, b, c) = (0.02797, -0.09138, 0.04487);
        // lensfun normalises to half the shorter side of the sensor.
        let r = radius * 2.0;
        a * r * r * r + b * r * r + c * r + 1.0 - a - b - c
    }

    #[test]
    fn ptlens_evaluates_with_the_short_side_as_two_units() {
        let metadata = shot("Sony", "ILCE-7M3", "FE 24-70mm F4 ZA OSS", 24.0, 4.0);
        let profile = profile(&metadata, FULL_FRAME).unwrap();
        assert_eq!(profile.distortion, Some(Source::Lensfun));
        for radius in [0.2f32, 0.5, 0.8] {
            let expected = ptlens_scale(f64::from(radius));
            let found = f64::from(profile.distortion_scale(radius));
            assert!((found - expected).abs() < 2e-5, "at {radius}: {found} vs {expected}");
        }
    }

    #[test]
    fn crop_factor_rescales_the_same_calibration() {
        let frame = FULL_FRAME;
        let full = profile(&shot("Sony", "ILCE-7M3", "FE 24-70mm F4 ZA OSS", 24.0, 4.0), frame).unwrap();
        let cropped = profile(&shot("Sony", "ILCE-6400", "FE 24-70mm F4 ZA OSS", 24.0, 4.0), frame).unwrap();
        assert_ne!(full.distortion_scale(0.8), cropped.distortion_scale(0.8));
    }

    #[test]
    fn an_ambiguous_lens_gets_no_correction() {
        let metadata = shot("Canon", "EOS 5D Mark IV", "24-70mm", 40.0, 2.8);
        assert!(matches!(profile(&metadata, FULL_FRAME), Err(LensError::AmbiguousLens { .. })));
    }

    #[test]
    fn an_unknown_lens_says_so() {
        let metadata = shot("Canon", "EOS R", "RF999mm F1 DREAM", 999.0, 1.0);
        assert!(matches!(profile(&metadata, FULL_FRAME), Err(LensError::UnknownLens { .. })));
    }

    #[test]
    fn an_unknown_camera_says_so() {
        let metadata = shot("Acme", "Obscura 1", "50mm f/1.8", 50.0, 1.8);
        assert!(matches!(profile(&metadata, FULL_FRAME), Err(LensError::UnknownCamera(_))));
    }

    #[test]
    fn a_focal_length_outside_the_lens_is_refused() {
        let metadata = shot("Sony", "ILCE-7M3", "FE 24-70mm F4 ZA OSS", 200.0, 4.0);
        assert!(matches!(profile(&metadata, FULL_FRAME), Err(LensError::FocalOutOfRange { .. })));
    }

    #[test]
    fn a_missing_lens_name_is_reported() {
        let metadata = shot("Google", "Pixel 4 XL", "", 5.8, 2.4);
        assert!(matches!(profile(&metadata, FULL_FRAME), Err(LensError::UnnamedLens(_))));
    }

    #[test]
    fn all_names_walks_translations() {
        let text = b"Default\0en\0English\0de\0Deutsch\0\0";
        // SAFETY: the literal is laid out like an lfMLstr.
        let names = unsafe { all_names(text.as_ptr().cast()) };
        assert_eq!(names, vec!["Default", "English", "Deutsch"]);
    }
}
