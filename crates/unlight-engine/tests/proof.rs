//! Soft proofing through the real view path. A proof has to show what the target can hold,
//! flag what it can't, and leave every other pixel of the system alone.

mod support;

use std::sync::Arc;
use support::{flat, render};
use unlight_engine::{
    Engine, GpuContext, OutputSpace, Precision, Proof, ProofIntent, ProofLut, ProofTarget, Session, ViewRequest,
};
use unlight_output::{bake_proof, icc_profile};
use unlight_raw::RawImage;
use unlight_recipe::Recipe;

const WARNING: [u8; 3] = [255, 0, 255];
const IN_GAMUT: [f32; 3] = [0.3, 0.2, 0.15];
const SIZE: u32 = 48;

/// Saturation that takes a dark red past sRGB. It's dark enough to stay inside the working
/// space, which a brighter one wouldn't: past 1.0 even ProPhoto clips.
fn past_srgb() -> (RawImage, Recipe) {
    let mut recipe = Recipe::default();
    recipe.basic.saturation = 60.0;
    (flat([0.3, 0.01, 0.01]), recipe)
}

fn open(raw: &RawImage) -> Session {
    let engine = Engine::new(GpuContext::new().expect("a device"), Precision::F16).expect("engine");
    Session::open(engine, raw, &Recipe::default()).expect("session")
}

fn view(session: &mut Session, recipe: &Recipe, proof: Option<Proof>) -> Vec<u8> {
    let target = session.view_target(SIZE, SIZE);
    let request = ViewRequest { proof, ..ViewRequest::default() };
    session.render(recipe, &request, &target, None).expect("render");
    session.read_view(&target).expect("readback")
}

fn space(space: OutputSpace, gamut_warning: bool) -> Proof {
    Proof { target: ProofTarget::Space(space), gamut_warning }
}

fn table(of: OutputSpace, intent: ProofIntent) -> Arc<ProofLut> {
    Arc::new(bake_proof(&icc_profile(of).unwrap(), intent).unwrap())
}

fn center(pixels: &[u8]) -> [u8; 3] {
    let at = (SIZE as usize / 2 * SIZE as usize + SIZE as usize / 2) * 4;
    [pixels[at], pixels[at + 1], pixels[at + 2]]
}

fn assert_close(a: [u8; 3], b: [u8; 3], tolerance: i32) {
    for channel in 0..3 {
        assert!((i32::from(a[channel]) - i32::from(b[channel])).abs() <= tolerance, "{a:?} vs {b:?}");
    }
}

#[test]
fn an_in_gamut_colour_passes_through_every_built_in_target_unchanged() {
    let raw = flat(IN_GAMUT);
    let mut session = open(&raw);
    let recipe = Recipe::default();
    let plain = view(&mut session, &recipe, None);
    for target in [OutputSpace::Srgb, OutputSpace::DisplayP3, OutputSpace::AdobeRgb] {
        let proofed = view(&mut session, &recipe, Some(space(target, true)));
        assert_eq!(proofed, plain, "{target:?} changed an in-gamut pixel");
    }
}

#[test]
fn an_in_gamut_colour_survives_a_baked_profile() {
    let raw = flat(IN_GAMUT);
    let mut session = open(&raw);
    let recipe = Recipe::default();
    let plain = center(&view(&mut session, &recipe, None));
    let proof = Proof { target: ProofTarget::Table(table(OutputSpace::AdobeRgb, ProofIntent::Relative)), gamut_warning: true };
    assert_close(center(&view(&mut session, &recipe, Some(proof))), plain, 2);
}

#[test]
fn an_out_of_gamut_colour_is_flagged_only_by_targets_that_cannot_hold_it() {
    let (raw, recipe) = past_srgb();
    let mut session = open(&raw);
    assert_eq!(center(&view(&mut session, &recipe, Some(space(OutputSpace::Srgb, true)))), WARNING);
    let held = center(&view(&mut session, &recipe, Some(space(OutputSpace::ProPhoto, true))));
    assert_ne!(held, WARNING, "this colour fits in the working space");
}

#[test]
fn a_baked_profile_flags_the_same_colour_a_matrix_does() {
    let (raw, recipe) = past_srgb();
    let mut session = open(&raw);
    let proof = Proof { target: ProofTarget::Table(table(OutputSpace::Srgb, ProofIntent::Relative)), gamut_warning: true };
    assert_eq!(center(&view(&mut session, &recipe, Some(proof))), WARNING);
}

#[test]
fn with_the_warning_off_the_colour_shows_clipped_to_the_target() {
    let (raw, recipe) = past_srgb();
    let mut session = open(&raw);
    let clipped = center(&view(&mut session, &recipe, Some(space(OutputSpace::Srgb, false))));
    assert_ne!(clipped, WARNING);
    assert_close(clipped, center(&view(&mut session, &recipe, None)), 1);
}

#[test]
fn a_narrower_target_changes_what_a_wide_colour_looks_like() {
    let (raw, recipe) = past_srgb();
    let mut session = open(&raw);
    let srgb = center(&view(&mut session, &recipe, Some(space(OutputSpace::Srgb, false))));
    let narrow_table = table(OutputSpace::AdobeRgb, ProofIntent::Relative);
    assert_ne!(narrow_table.id, 0);
    let through = center(&view(
        &mut session,
        &recipe,
        Some(Proof { target: ProofTarget::Table(narrow_table), gamut_warning: false }),
    ));
    assert_close(through, srgb, 40);
}

#[test]
fn a_view_with_proofing_off_is_untouched_by_an_earlier_proof() {
    let (raw, recipe) = past_srgb();
    let mut session = open(&raw);
    let before = view(&mut session, &recipe, None);
    view(&mut session, &recipe, Some(space(OutputSpace::Srgb, true)));
    view(&mut session, &recipe, Some(Proof { target: ProofTarget::Table(table(OutputSpace::Srgb, ProofIntent::Perceptual)), gamut_warning: true }));
    assert_eq!(view(&mut session, &recipe, None), before);
}

fn flagged_share(pixels: &[u8]) -> f32 {
    let pixels = pixels.as_chunks::<4>().0;
    pixels.iter().filter(|pixel| pixel[..3] == WARNING).count() as f32 / pixels.len() as f32
}

/// The warning is only useful if it means something, so an unedited photo can't be covered in
/// it, and a wider target has to flag less than a narrower one.
#[test]
fn real_photos_are_not_covered_in_warnings() {
    let Some(samples) = std::env::var_os("UNLIGHT_SAMPLES").map(std::path::PathBuf::from).filter(|path| path.is_dir())
    else {
        return;
    };
    // Ordinary photos should be nearly clear, the Sony especially: its big pale sky is the case
    // a warning most easily gets wrong. The Hasselblad frame is a colour chart and gems on vivid
    // blue cloth, which really is outside sRGB.
    let photos = [
        ("gh5.RW2", 0.01),
        ("fuji-xt4.RAF", 0.01),
        ("sony-a7rm4.ARW", 0.01),
        ("canon-5d4.CR2", 0.03),
        ("hasselblad-h6d.fff", 0.1),
    ];
    let recipe = Recipe::default();
    for (name, most_flagged) in photos {
        let raw = unlight_raw::load(&samples.join(name)).expect("decode");
        let mut session = open(&raw);
        let mut share = |target| flagged_share(&view(&mut session, &recipe, Some(space(target, true))));
        let (srgb, p3, adobe) = (share(OutputSpace::Srgb), share(OutputSpace::DisplayP3), share(OutputSpace::AdobeRgb));
        println!("{name:<22} flagged sRGB {:.2}%  P3 {:.2}%  Adobe RGB {:.2}%", srgb * 100.0, p3 * 100.0, adobe * 100.0);
        assert!(srgb < most_flagged, "{name}: {srgb}");
        assert!(p3 <= srgb && adobe <= srgb, "{name}: {srgb} {p3} {adobe}");
    }
}

#[test]
fn the_export_is_byte_identical_with_proofing_on_and_off() {
    let (raw, recipe) = past_srgb();
    let bytes = |pixels: &[f32]| pixels.iter().flat_map(|value| value.to_bits().to_le_bytes()).collect::<Vec<u8>>();
    let off = bytes(&render(&raw, &recipe).pixels);

    let mut session = open(&raw);
    view(&mut session, &recipe, Some(space(OutputSpace::Srgb, true)));
    let on = bytes(&render(&raw, &recipe).pixels);
    drop(session);
    assert_eq!(off, on);
}

#[test]
fn a_proof_never_needs_a_mutable_recipe() {
    let (raw, recipe) = past_srgb();
    let copy = recipe.clone();
    let mut session = open(&raw);
    view(&mut session, &recipe, Some(space(OutputSpace::AdobeRgb, true)));
    assert_eq!(recipe, copy);
}
