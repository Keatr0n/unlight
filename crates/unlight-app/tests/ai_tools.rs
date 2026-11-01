//! Select Subject and Select Sky in the Masks panel, driven like a user would.
#![cfg(feature = "ai")]

mod support;

use egui_kittest::kittest::Queryable;
use std::{env, fs, time::Instant};
use support::{editing_app, let_settle, recipe, save, settle};
use tempfile::TempDir;
use unlight_ai::{Manifest, MaskStore, ModelStatus, ModelStore, Task};
use unlight_app::{action::Action, ai_tools::AiTools, state::Tool};
use unlight_recipe::MaskShape;

type Harness = egui_kittest::Harness<'static, unlight_app::app::App>;

fn open_masks(harness: &mut Harness) {
    harness.state_mut().state.apply(Action::ToggleTool(Tool::Masks));
    harness.step();
    harness.step();
}

fn click(harness: &mut Harness, label: &str) {
    harness.get_by_label(label).click();
    harness.step();
    harness.step();
}

fn subject_prompt() -> String {
    Manifest::bundled().unwrap().for_task(Task::Subject).unwrap().consent_prompt()
}

#[test]
fn a_tool_without_its_model_asks_first_and_fetches_nothing_on_its_own() {
    let directory = TempDir::new().unwrap();
    let models = directory.path().join("models");
    let mut harness = editing_app();
    harness.state_mut().state.ai =
        AiTools::with_stores(ModelStore::new(&models), MaskStore::new(directory.path().join("masks"))).unwrap();
    open_masks(&mut harness);
    let prompt = subject_prompt();

    assert!(harness.query_by_label(&prompt).is_none(), "nothing is asked until the tool is used");
    click(&mut harness, "Select Subject");

    assert!(harness.query_by_label(&prompt).is_some(), "the size and licence are shown first");
    assert!(prompt.contains("MB model"));
    assert!(!harness.state().state.ai.is_busy(), "asking must not start a download");
    save(&mut harness, "ai-first-run");

    click(&mut harness, "Not now");

    assert!(harness.query_by_label(&prompt).is_none());
    assert!(!models.exists(), "declining must leave nothing behind");
}

#[test]
fn selecting_a_subject_adds_a_mask_from_the_real_model() {
    let model = Manifest::bundled().unwrap().for_task(Task::Subject).unwrap().clone();
    let (Ok(models), Some(_)) = (ModelStore::open_default(), env::var_os("ORT_DYLIB_PATH")) else {
        return;
    };
    if models.status(&model) != ModelStatus::Ready {
        eprintln!("skipping: {} isn't installed", model.name);
        return;
    }
    let directory = TempDir::new().unwrap();
    let masks = MaskStore::new(directory.path());
    let mut harness = editing_app();
    harness.state_mut().state.ai = AiTools::with_stores(models, masks.clone()).unwrap();
    open_masks(&mut harness);
    let_settle(&mut harness);

    let started = Instant::now();
    click(&mut harness, "Select Subject");
    settle(&mut harness, |app| !app.state.ai.is_busy());
    eprintln!("selected in {:?}", started.elapsed());

    let found = &recipe(&harness).masks;
    assert_eq!(found.len(), 1, "{:?}", harness.state().state.ai.message);
    assert_eq!(found[0].name, "Subject");
    let MaskShape::Raster { id, inverted } = &found[0].components[0].shape else {
        panic!("expected a raster, got {:?}", found[0].components[0].shape);
    };
    assert!(!inverted);
    assert!(masks.load(id).is_ok(), "the id must name a stored mask");
    assert!(fs::read_dir(directory.path()).unwrap().count() == 1);
}
