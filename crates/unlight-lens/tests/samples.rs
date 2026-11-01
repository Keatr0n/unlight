//! Real camera files. These read the directory named by `UNLIGHT_SAMPLES` and skip when it's
//! unset, because `nix flake check` has no samples.

use std::{env, path::PathBuf};
use unlight_lens::{LensError, LensProfile, PhotoLens, Source};
use unlight_raw::RawImage;

fn load(name: &str) -> Option<RawImage> {
    let directory = PathBuf::from(env::var_os("UNLIGHT_SAMPLES")?);
    Some(unlight_raw::load(&directory.join(name)).expect("sample decodes"))
}

fn resolved(name: &str) -> Option<LensProfile> {
    Some(PhotoLens::new(&load(name)?).resolve().expect("sample resolves"))
}

fn sources(profile: &LensProfile) -> [Option<Source>; 3] {
    [profile.distortion, profile.chromatic, profile.vignette]
}

#[test]
fn sony_uses_its_makernote_but_not_its_vignette_table() {
    let Some(profile) = resolved("sony-a7rm4.ARW") else { return };
    assert_eq!(profile.distortion, Some(Source::Sony));
    assert_eq!(profile.chromatic, Some(Source::Sony));
    assert_ne!(profile.vignette, Some(Source::Sony));
}

#[test]
fn fujifilm_supplies_all_three_corrections() {
    let Some(profile) = resolved("fuji-xt4.RAF") else { return };
    assert_eq!(sources(&profile), [Some(Source::Fujifilm); 3]);
}

#[test]
fn gh5_takes_distortion_from_its_tag_and_the_rest_from_the_database() {
    let Some(raw) = load("gh5.RW2") else { return };
    assert_eq!(raw.metadata.lens, "LEICA DG 12-60/F2.8-4.0");
    let profile = PhotoLens::new(&raw).resolve().unwrap();
    assert_eq!(sources(&profile), [Some(Source::Panasonic), Some(Source::Lensfun), Some(Source::Lensfun)]);
}

#[test]
fn nikon_z_takes_distortion_from_its_makernote() {
    let Some(profile) = resolved("nikon-z6.NEF") else { return };
    assert_eq!(profile.distortion, Some(Source::Nikon));
    assert_eq!(profile.vignette, Some(Source::Lensfun));
}

#[test]
fn canon_comes_from_the_database() {
    let Some(profile) = resolved("canon-m6ii-craw.CR3") else { return };
    assert_eq!(sources(&profile), [Some(Source::Lensfun); 3]);
}

#[test]
fn pixel_warp_applies_with_no_profile_at_all() {
    let Some(raw) = load("pixel4xl.dng") else { return };
    let profile = PhotoLens::new(&raw).file_profile().unwrap();
    assert_eq!(profile.distortion, Some(Source::DngOpcodes));
    assert!(profile.center[0] < -0.04, "{:?}", profile.center);
}

#[test]
fn a_lens_named_only_by_its_focal_range_is_refused() {
    let Some(raw) = load("canon-5d4.CR2") else { return };
    let error = PhotoLens::new(&raw).resolve().unwrap_err();
    assert!(matches!(error, LensError::AmbiguousLens { .. }), "{error}");
}

#[test]
fn a_body_the_database_lacks_is_refused() {
    let Some(raw) = load("hasselblad-h6d.fff") else { return };
    assert!(matches!(PhotoLens::new(&raw).resolve(), Err(LensError::UnknownCamera(_))));
}
