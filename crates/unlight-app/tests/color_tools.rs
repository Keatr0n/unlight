//! Auto tone and soft proofing as a user meets them: through the buttons and keys, into the
//! undo stack, and out through a real export.

mod support;

use eframe::egui::{Key, Modifiers, pos2};
use egui_kittest::kittest::Queryable;
use std::{env, fs, path::Path};
use support::{app, editing_raw, library_app, live, recipe, row, sample, save, settle, steps, synthetic_raw};
use unlight_app::{
    action::Action,
    state::{Effect, proof::ProofChoice},
};
use unlight_engine::OutputSpace;
use unlight_output::icc_profile;
use unlight_recipe::{Basic, Recipe};

fn dark_photo() -> support::TestApp {
    editing_raw(&synthetic_raw(600, 400, 0, [0.04; 3]), 2)
}

#[test]
fn the_auto_button_is_one_undo_step() {
    let mut harness = dark_photo();
    harness.get_by_label("Auto").click();
    harness.step();
    harness.step();
    harness.step();
    assert!(recipe(&harness).basic.exposure > 0.5, "{:?}", recipe(&harness).basic);
    assert_eq!(steps(&harness), 2);
    assert_eq!(support::edit(&harness).history().current_label(), "Auto Tone");

    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.step();
    assert_eq!(recipe(&harness).basic, Basic::default());
    assert_eq!(recipe(&harness), &Recipe::default());
}

#[test]
fn auto_again_on_an_auto_photo_adds_no_step() {
    let mut harness = dark_photo();
    harness.state_mut().state.apply(Action::AutoTone);
    harness.step();
    harness.step();
    assert_eq!(steps(&harness), 2);
    harness.state_mut().state.apply(Action::AutoTone);
    harness.step();
    harness.step();
    assert_eq!(steps(&harness), 2, "the sliders didn't move, so there's nothing to undo");
}

#[test]
fn ctrl_u_is_auto() {
    let mut harness = dark_photo();
    harness.key_press_modifiers(Modifiers::COMMAND, Key::U);
    harness.step();
    harness.step();
    harness.step();
    assert!(recipe(&harness).basic.exposure > 0.5);
}

#[test]
fn auto_saves_like_any_other_edit() {
    let mut harness = dark_photo();
    harness.state_mut().state.apply(Action::AutoTone);
    harness.step();
    harness.step();
    support::let_settle(&mut harness);
    let variant = row(&harness, 0).variant;
    let stored = harness.state().store.catalog.variant(variant).expect("the variant").recipe;
    assert_eq!(stored.basic, recipe(&harness).basic);
}

#[test]
fn auto_leaves_the_rest_of_the_panel_alone() {
    let mut harness = dark_photo();
    if let Some(edit) = harness.state_mut().state.edit_mut() {
        edit.recipe.basic.clarity = 35.0;
    }
    harness.step();
    harness.state_mut().state.apply(Action::AutoTone);
    harness.step();
    harness.step();
    assert_eq!(recipe(&harness).basic.clarity, 35.0);
}

#[test]
fn s_turns_a_proof_on_without_touching_the_photo() {
    let mut harness = dark_photo();
    let before = recipe(&harness).clone();
    harness.key_press(Key::S);
    harness.step();
    harness.step();
    assert!(harness.state().state.proof.enabled);
    assert_eq!(recipe(&harness), &before);
    assert_eq!(steps(&harness), 1);
    harness.key_press(Key::S);
    harness.step();
    assert!(!harness.state().state.proof.enabled);
}

fn center_pixel(harness: &mut support::TestApp) -> [u8; 4] {
    let photo = harness.state().viewport.photo_rect.expect("a photo on screen");
    let pixel = pos2(photo.center().x, photo.center().y + photo.height() * 0.25);
    let image = harness.render().expect("a frame");
    image.get_pixel(pixel.x as u32, pixel.y as u32).0
}

#[test]
fn the_gamut_warning_paints_what_the_target_cannot_hold() {
    let raw = synthetic_raw(600, 400, 0, [1.4, 0.05, 0.05]);
    let mut harness = editing_raw(&raw, 1);
    if let Some(edit) = harness.state_mut().state.edit_mut() {
        edit.recipe.basic.saturation = 90.0;
    }
    harness.step();
    harness.step();
    let plain = center_pixel(&mut harness);
    harness.state_mut().state.apply(Action::ToggleSoftProof);
    harness.state_mut().state.proof.choice = ProofChoice::Space(OutputSpace::Srgb);
    harness.step();
    harness.step();
    harness.step();
    let warned = center_pixel(&mut harness);
    assert_eq!(&warned[..3], &[255, 0, 255], "{plain:?} should have been flagged");
    for _ in 0..12 {
        harness.step();
    }
    let histogram = harness.state().renderer.histogram().expect("a histogram");
    assert!(
        histogram.highlight_clipping()[2] < 0.01,
        "a red photo has no white blue channel, so that would be the warning colour being counted"
    );
    harness.state_mut().state.apply(Action::ToggleGamutWarning);
    harness.step();
    harness.step();
    harness.step();
    assert_ne!(&center_pixel(&mut harness)[..3], &[255, 0, 255]);
}

fn export_to(harness: &mut support::TestApp, folder: &Path) -> Vec<u8> {
    harness.state_mut().state.apply(Action::OpenExport);
    harness.step();
    let form = harness.state_mut().state.export_form.as_mut().expect("export form");
    form.choose_preset(0);
    form.directory = folder.display().to_string();
    harness.step();
    harness.state_mut().state.effects.push(Effect::StartExport);
    harness.step();
    settle(harness, |app| app.state.export_run.as_ref().is_some_and(|run| run.finished));
    let run = harness.state().state.export_run.clone().expect("a finished run");
    assert!(run.failures.is_empty(), "{:?}", run.failures);
    harness.state_mut().state.export_run = None;
    harness.state_mut().state.dialog = None;
    let files: Vec<_> = fs::read_dir(folder).expect("the folder").flatten().collect();
    assert_eq!(files.len(), 1);
    fs::read(files[0].path()).expect("the file")
}

#[test]
fn an_export_is_byte_identical_with_a_proof_showing() {
    let mut harness = library_app(1);
    settle(&mut harness, live);
    if let Some(edit) = harness.state_mut().state.edit_mut() {
        edit.recipe.basic.saturation = 60.0;
    }
    harness.step();
    harness.step();
    // The exporter reads recipes from the catalog, so the edit has to have landed there.
    support::let_settle(&mut harness);
    let scratch = tempfile::tempdir().expect("a folder");
    let plain = export_to(&mut harness, &scratch.path().join("off"));

    let icc = scratch.path().join("wide.icc");
    fs::write(&icc, icc_profile(OutputSpace::AdobeRgb).expect("a profile")).expect("writing it");
    harness.state_mut().state.apply(Action::ToggleSoftProof);
    harness.state_mut().state.proof.choice = ProofChoice::Icc(icc);
    settle(&mut harness, |app| app.state.proof.view().is_some());
    harness.step();
    harness.step();
    let before = recipe(&harness).clone();
    let proofed = export_to(&mut harness, &scratch.path().join("on"));
    harness.state_mut().state.apply(Action::ToggleSoftProof);
    harness.step();
    let again = export_to(&mut harness, &scratch.path().join("off again"));
    assert!(again == plain, "exporting the same recipe twice has to match, or this proves nothing");
    assert!(proofed == plain, "a proof must not reach an export");
    assert_eq!(recipe(&harness), &before);
}

#[test]
fn a_file_that_is_not_a_profile_says_so() {
    let mut harness = dark_photo();
    let scratch = tempfile::tempdir().expect("a folder");
    let icc = scratch.path().join("broken.icc");
    fs::write(&icc, b"nonsense").expect("writing it");
    harness.state_mut().state.apply(Action::ToggleSoftProof);
    harness.state_mut().state.proof.choice = ProofChoice::Icc(icc);
    settle(&mut harness, |app| app.state.proof.refusal().is_some());
    assert_eq!(harness.state().state.proof.view(), None);
}

/// Frames on real raws, for looking at. The files are copied first, so that nothing the app
/// does can land beside the samples.
#[test]
fn proof_and_auto_on_real_photos() {
    if env::var_os("UNLIGHT_SNAPSHOTS").is_none() {
        return;
    }
    let Some(path) = sample("hasselblad-h6d.fff") else {
        return;
    };
    let scratch = tempfile::tempdir().expect("a folder");
    let copy = scratch.path().join("hasselblad-h6d.fff");
    fs::copy(&path, &copy).expect("copying the sample");
    let mut harness = app(&[copy], eframe::egui::vec2(1600.0, 1000.0), 1.0);
    settle(&mut harness, live);
    save(&mut harness, "basic_panel");
    harness.state_mut().state.apply(Action::AutoTone);
    harness.step();
    harness.step();
    harness.step();
    save(&mut harness, "auto_applied");

    if let Some(edit) = harness.state_mut().state.edit_mut() {
        edit.recipe.basic.saturation = 30.0;
    }
    harness.state_mut().state.apply(Action::ToggleSoftProof);
    harness.state_mut().state.proof.choice = ProofChoice::Space(OutputSpace::Srgb);
    for _ in 0..6 {
        harness.step();
    }
    save(&mut harness, "proof_srgb");
    harness.state_mut().state.proof.choice = ProofChoice::Space(OutputSpace::AdobeRgb);
    for _ in 0..6 {
        harness.step();
    }
    save(&mut harness, "proof_adobe");
    harness.get_by_label("Proof Target").click();
    for _ in 0..4 {
        harness.step();
    }
    save(&mut harness, "proof_menu");
}

/// An unedited photo with a big bright sky, for seeing where a warning lands.
#[test]
fn proof_of_an_unedited_sky() {
    if env::var_os("UNLIGHT_SNAPSHOTS").is_none() {
        return;
    }
    let Some(path) = sample("sony-a7rm4.ARW") else {
        return;
    };
    let scratch = tempfile::tempdir().expect("a folder");
    let copy = scratch.path().join("sony-a7rm4.ARW");
    fs::copy(&path, &copy).expect("copying the sample");
    let mut harness = app(&[copy], eframe::egui::vec2(1600.0, 1000.0), 1.0);
    settle(&mut harness, live);
    harness.state_mut().state.apply(Action::ToggleSoftProof);
    harness.state_mut().state.proof.choice = ProofChoice::Space(OutputSpace::Srgb);
    for _ in 0..6 {
        harness.step();
    }
    save(&mut harness, "proof_unedited_sky");
}
