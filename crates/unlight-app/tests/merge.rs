//! Photo Merge driven through the real app: selecting photos, the dialog, the worker, and the
//! result landing in the catalog as an editable photo. Runs on synthetic raws, with no samples.

mod support;

use egui_kittest::kittest::Queryable;
use std::{path::PathBuf, sync::Arc, time::Instant};
use support::{Fixture, LOAD_TIMEOUT, TestApp, WINDOW, library_app, let_settle, open_app, raws::SyntheticRaw, row, save, settle};
use unlight_app::{
    action::Action,
    app::App,
    export::{ExportEvent, ExportJob, Exporter},
    state::{Dialog, transfer::ExportForm},
};
use unlight_merge::Deghost;
use unlight_raw::{MergeKind, merged};

fn select_all(harness: &mut TestApp) {
    harness.state_mut().state.apply(Action::SelectAll);
    settle(harness, |app: &App| app.state.library.selected_rows().len() >= 2);
}

/// A modal spends its first frame being measured, unseen.
fn show_dialog(harness: &mut TestApp) {
    (0..3).for_each(|_| {
        harness.step();
    });
}

fn wait_for_merge(harness: &mut TestApp) {
    let start = Instant::now();
    while harness.state().state.merge_run.as_ref().is_some_and(|run| run.is_running()) {
        assert!(start.elapsed() < LOAD_TIMEOUT, "the merge never finished");
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    harness.step();
}

fn merged_files(photos: &PathBuf) -> Vec<PathBuf> {
    std::fs::read_dir(photos)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| merged::is_merged(path))
        .collect()
}

#[test]
fn one_photo_cant_be_merged() {
    let mut harness = library_app(1);
    settle(&mut harness, |app: &App| app.state.library.len() == 1);
    harness.state_mut().state.apply(Action::PhotoMerge(MergeKind::Hdr));
    assert_eq!(harness.state().state.dialog, None);
}

#[test]
fn the_shortcuts_open_the_dialog_for_each_kind() {
    let mut harness = library_app(3);
    select_all(&mut harness);
    harness.state_mut().state.apply(Action::PhotoMerge(MergeKind::Panorama));
    assert_eq!(harness.state().state.dialog, Some(Dialog::PhotoMerge));
    let form = harness.state().state.merge_form.clone().unwrap();
    assert_eq!(form.kind, MergeKind::Panorama);
    assert_eq!(form.sources.len(), 3);
    assert!(form.auto_crop);
    show_dialog(&mut harness);
    harness.get_by_label("Boundary Warp");
    harness.get_by_label("Auto Crop");
    save(&mut harness, "merge-panorama-dialog");

    harness.state_mut().state.apply(Action::Cancel);
    harness.state_mut().state.apply(Action::PhotoMerge(MergeKind::Hdr));
    show_dialog(&mut harness);
    harness.get_by_label("Auto Align");
    save(&mut harness, "merge-hdr-dialog");
}

#[test]
fn an_hdr_merge_lands_in_the_catalog_selected_and_editable() {
    let mut harness = library_app(3);
    select_all(&mut harness);
    harness.state_mut().state.apply(Action::PhotoMerge(MergeKind::Hdr));
    harness.state_mut().state.merge_form.as_mut().unwrap().deghost = Deghost::Low;
    show_dialog(&mut harness);
    harness.get_by_label("Merge").click();
    harness.step();
    harness.step();
    assert!(harness.state().state.merge_run.is_some());
    harness.step();
    save(&mut harness, "merge-running");
    wait_for_merge(&mut harness);

    let state = &harness.state().state;
    assert!(state.merge_run.is_none(), "{:?}", state.merge_run);
    assert_eq!(state.dialog, None);
    let photos = harness.fixture.photos.clone();
    assert_eq!(merged_files(&photos).len(), 1);
    let path = merged_files(&photos).remove(0);
    settle(&mut harness, |app: &App| app.state.focused().is_some_and(|focused| focused.path == path));
    let raw = unlight_raw::load(&path).unwrap();
    let info = raw.metadata.merge.clone().unwrap();
    assert_eq!(info.kind, MergeKind::Hdr);
    assert_eq!(info.sources.len(), 3);
    assert!(!info.lens_applied);

    let focused = row(&harness, harness.state().state.library.focus().unwrap());
    assert_eq!(focused.path, path);
    harness.state_mut().state.apply(Action::ShowDevelop);
    settle(&mut harness, support::live);
    harness.state_mut().state.edit_mut().unwrap().recipe.basic.exposure = -1.0;
    let_settle(&mut harness);
    save(&mut harness, "merge-develop");

    // It exports through the same worker a raw does, edits and all.
    let variant = harness.state().state.focused().unwrap().variant;
    let output = harness.fixture.dir.path().join("exports");
    let job = ExportJob {
        variants: vec![variant],
        settings: ExportForm::new(output.display().to_string()).unwrap().settings,
        directory: output.clone(),
    };
    let catalog = harness.state().store.catalog.clone();
    let exporter = Exporter::start(catalog, job, Arc::new(|| {})).unwrap();
    let start = Instant::now();
    let failures = loop {
        assert!(start.elapsed() < LOAD_TIMEOUT, "the export never finished");
        match exporter.try_next() {
            Some(ExportEvent::Done { cancelled }) => {
                assert!(!cancelled);
                break 0;
            }
            Some(ExportEvent::Failed(failure)) => break failure.message.len().max(1),
            _ => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    };
    assert_eq!(failures, 0);
    let written = std::fs::read_dir(&output).unwrap().flatten().count();
    assert_eq!(written, 1);
}

/// Blocks of different brightness painted around the camera, so that views overlap.
fn scenery(longitude: f64, latitude: f64) -> u16 {
    let cell = |size: f64| {
        let (a, b) = (((longitude + 4.0) * size).floor() as i64, ((latitude + 2.0) * size).floor() as i64);
        (a.wrapping_mul(73_856_093) ^ b.wrapping_mul(19_349_663)) & 0xff
    };
    (300 + cell(4.0) * 5 + cell(11.0) * 3 + cell(29.0) * 2) as u16
}

fn view_of_scenery(yaw: f64) -> (u32, u32, Vec<u16>) {
    let (width, height, focal) = (960u32, 640u32, 960.0f64);
    let (sin, cos) = yaw.sin_cos();
    let mut sites = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let (dx, dy) = ((f64::from(x) + 0.5 - 480.0) / focal, (f64::from(y) + 0.5 - 320.0) / focal);
            let (world_x, world_z) = (dx * cos + sin, cos - dx * sin);
            sites.push(scenery(world_x.atan2(world_z), dy.atan2(world_x.hypot(world_z))));
        }
    }
    (width, height, sites)
}

#[test]
fn a_panorama_of_real_files_stitches_and_lands_in_the_catalog() {
    let fixture = Fixture::empty();
    for (index, yaw) in [-0.5, 0.0, 0.5].into_iter().enumerate() {
        let raw = SyntheticRaw { sensor: Some(view_of_scenery(yaw)), ..SyntheticRaw::seeded(index as u32) };
        raw.write(&fixture.photos.join(format!("view-{index}.dng")));
    }
    let store = fixture.store();
    fixture.scan(&store);
    let mut harness = open_app(fixture, store, WINDOW, 1.0, |_| {});
    settle(&mut harness, |app: &App| app.state.edit().is_some());
    select_all(&mut harness);
    harness.state_mut().state.apply(Action::PhotoMerge(MergeKind::Panorama));
    harness.state_mut().state.merge_form.as_mut().unwrap().boundary_warp = 40.0;
    show_dialog(&mut harness);
    harness.get_by_label("Merge").click();
    harness.step();
    harness.step();
    wait_for_merge(&mut harness);

    let state = &harness.state().state;
    assert!(state.merge_run.is_none(), "{:?}", state.merge_run);
    let photos = harness.fixture.photos.clone();
    let path = merged_files(&photos).remove(0);
    settle(&mut harness, |app: &App| app.state.focused().is_some_and(|focused| focused.path == path));
    let raw = unlight_raw::load(&path).unwrap();
    let info = raw.metadata.merge.clone().unwrap();
    assert_eq!(info.kind, MergeKind::Panorama);
    assert!(info.lens_applied);
    assert!(raw.width > 1200, "{}", raw.width);
    harness.state_mut().state.apply(Action::ShowDevelop);
    settle(&mut harness, support::live);
    let_settle(&mut harness);
    save(&mut harness, "merge-panorama-develop");
}

#[test]
fn cancelling_leaves_nothing_behind() {
    let mut harness = library_app(4);
    select_all(&mut harness);
    harness.state_mut().state.apply(Action::PhotoMerge(MergeKind::Panorama));
    show_dialog(&mut harness);
    harness.get_by_label("Merge").click();
    show_dialog(&mut harness);
    harness.get_by_label("Cancel Merge").click();
    harness.step();
    wait_for_merge(&mut harness);
    let state = &harness.state().state;
    assert!(state.merge_run.is_none());
    assert!(state.merge_form.is_none());
    assert_eq!(state.dialog, None);
    let photos = harness.fixture.photos.clone();
    assert!(merged_files(&photos).is_empty());
    let leftovers: Vec<_> = std::fs::read_dir(&photos)
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn a_failed_merge_says_why_and_keeps_the_app_alive() {
    // These synthetic photos are all different scenes, so they can't be stitched.
    let mut harness = library_app(2);
    select_all(&mut harness);
    harness.state_mut().state.apply(Action::PhotoMerge(MergeKind::Panorama));
    show_dialog(&mut harness);
    harness.get_by_label("Merge").click();
    harness.step();
    harness.step();
    wait_for_merge(&mut harness);
    let state = &harness.state().state;
    let failure = state.merge_run.as_ref().and_then(|run| run.failure.clone()).expect("a failure");
    assert!(failure.contains("overlap"), "{failure}");
    assert_eq!(state.dialog, Some(Dialog::PhotoMerge));
    show_dialog(&mut harness);
    save(&mut harness, "merge-failed");
    harness.get_by_label("Done").click();
    harness.step();
    harness.step();
    assert!(harness.state().state.merge_run.is_none());
    assert_eq!(harness.state().state.dialog, None);
}
