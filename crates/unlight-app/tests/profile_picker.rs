//! The Profile dropdown, from an empty profile folder to a choice that survives undo and lands
//! in the catalog. One test in its own binary, because the folder comes from the environment
//! and the library is read once for the whole process.

mod support;

use eframe::egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use std::{env, fs, path::Path};
use support::{library_app, live, recipe, row, save, settle, steps};
use unlight_app::app::App;
use unlight_engine::{BUILT_IN_PROFILE, ProfileLibrary, ViewRequest};
use unlight_recipe::Recipe;

const TAG_CAMERA: u16 = 50708;
const TAG_NAME: u16 = 50936;
const TAG_CURVE: u16 = 50940;
const KIND_ASCII: u16 = 2;
const KIND_FLOAT: u16 = 11;

/// A profile with nothing in it but a name, a camera and a tone curve. It's written by hand
/// here because no real profile file may enter the repository.
fn profile_bytes(camera: &str, name: &str, curve: &[f32]) -> Vec<u8> {
    let text = |value: &str| {
        let mut bytes = value.as_bytes().to_vec();
        bytes.resize(bytes.len().max(4) + 1, 0);
        bytes
    };
    let tags = [
        (TAG_CAMERA, KIND_ASCII, text(camera)),
        (TAG_NAME, KIND_ASCII, text(name)),
        (TAG_CURVE, KIND_FLOAT, curve.iter().flat_map(|value| value.to_le_bytes()).collect::<Vec<u8>>()),
    ];
    let mut file = b"IIRC".to_vec();
    file.extend(8u32.to_le_bytes());
    file.extend((tags.len() as u16).to_le_bytes());
    let mut data_at = 8 + 2 + tags.len() * 12 + 4;
    let mut data: Vec<u8> = Vec::new();
    for (tag, kind, payload) in &tags {
        let size = if *kind == KIND_FLOAT { 4 } else { 1 };
        file.extend(tag.to_le_bytes());
        file.extend(kind.to_le_bytes());
        file.extend(((payload.len() / size) as u32).to_le_bytes());
        file.extend((data_at as u32).to_le_bytes());
        data_at += payload.len();
        data.extend(payload);
    }
    file.extend([0; 4]);
    file.extend(data);
    file
}

fn install(folder: &Path, file: &str, camera: &str, name: &str) {
    fs::create_dir_all(folder).expect("the profile folder");
    let bytes = profile_bytes(camera, name, &[0.0, 0.0, 0.5, 0.9, 1.0, 1.0]);
    fs::write(folder.join(file), bytes).expect("a profile");
}

fn rendered(app: &mut App, recipe: &Recipe) -> Vec<u8> {
    let path = app.state.focused().expect("a photo").path.clone();
    let session = &mut app.darkroom.live_mut(&path).expect("a live photo").session;
    let target = session.view_target(64, 64);
    session.render(recipe, &ViewRequest::default(), &target, None).expect("render");
    session.read_view(&target).expect("readback")
}

fn open_menu(harness: &mut support::TestApp) {
    harness.get_by_label("Profile").click();
    for _ in 0..4 {
        harness.step();
    }
}

fn pick(harness: &mut support::TestApp, label: &str) {
    harness.get_by_label(label).click();
    for _ in 0..4 {
        harness.step();
    }
}

#[test]
fn a_profile_can_be_installed_rescanned_chosen_and_undone() {
    let home = tempfile::tempdir().expect("a data folder");
    // SAFETY: this binary's only test sets this before any other thread exists.
    unsafe { env::set_var("XDG_DATA_HOME", home.path()) };
    let folder = home.path().join("unlight/profiles");

    let mut harness = library_app(1);
    settle(&mut harness, live);
    let plain = Recipe::default();
    let default_look = rendered(harness.state_mut(), &plain);
    open_menu(&mut harness);
    save(&mut harness, "profile_menu_empty");
    assert!(harness.query_by_label("Bright Look").is_none());

    install(&folder, "bright.dcp", "ACME Ranger", "Bright Look");
    install(&folder, "other.dcp", "Acme Scout", "Not For This Camera");
    pick(&mut harness, "Rescan Profiles");
    settle(&mut harness, |app| {
        app.state.status().is_some_and(|status| status.text.contains("2 camera profiles"))
    });
    assert_eq!(ProfileLibrary::installed().generation, 1);

    let automatic = rendered(harness.state_mut(), &plain);
    assert_ne!(automatic, default_look, "a matching profile applies by itself");
    assert_eq!(recipe(&harness), &plain, "and that's not an edit");

    open_menu(&mut harness);
    save(&mut harness, "profile_menu_installed");
    assert!(harness.query_by_label("Bright Look").is_some());
    assert!(harness.query_by_label("Not For This Camera").is_none());
    pick(&mut harness, "Unlight Default");
    assert_eq!(recipe(&harness).camera_profile.as_deref(), Some(BUILT_IN_PROFILE));
    assert_eq!(steps(&harness), 2);
    let chosen = recipe(&harness).clone();
    assert_eq!(rendered(harness.state_mut(), &chosen), default_look);

    support::let_settle(&mut harness);
    let variant = row(&harness, 0).variant;
    let stored = harness.state().store.catalog.variant(variant).expect("the variant").recipe;
    assert_eq!(stored.camera_profile.as_deref(), Some(BUILT_IN_PROFILE), "saved like any edit");

    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.step();
    assert_eq!(recipe(&harness).camera_profile, None);
    let undone = recipe(&harness).clone();
    assert_eq!(rendered(harness.state_mut(), &undone), automatic);

    fs::remove_file(folder.join("bright.dcp")).expect("removing it");
    open_menu(&mut harness);
    pick(&mut harness, "Rescan Profiles");
    settle(&mut harness, |app| app.state.status().is_some_and(|status| status.text.contains("1 camera profile found")));
    assert_eq!(rendered(harness.state_mut(), &plain), default_look, "a removed profile stops applying");
}
