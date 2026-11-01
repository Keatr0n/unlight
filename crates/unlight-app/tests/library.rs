//! The catalog side of the app, end to end: edits that persist, the library's views, import,
//! presets and export, on synthetic DNGs in a temp directory.

mod support;

use eframe::egui::{Event, Key, Modifiers, Pos2, Role, vec2};
use egui_kittest::kittest::Queryable;
use std::{fs, path::PathBuf, sync::Arc, thread, time::Duration};
use support::{
    Fixture, TestApp, WINDOW, drag, edit, let_settle, library_app, open_app, recipe, row, save, settle,
    slider_rect, steps,
};
use unlight_app::{
    action::Action,
    state::{
        Dialog, LibraryView, Module, Tool,
        clipboard::SettingGroup,
        guides::source_to_view,
        library::ClickKind,
        rules::{RuleDraft, RuleField, SmartEditor},
        transfer::ImportPhase,
        write::Write,
    },
    store::{Store, StoreConfig},
};
use unlight_catalog::{Comparison, Filter, Flag, Label, Page, PhotoStatus, RatingFilter};
use unlight_engine::Geometry;
use unlight_recipe::{Crop, Orientation, Upright};

fn store_of(app: &TestApp) -> Arc<Store> {
    app.state().store.clone()
}

/// Closes the app and opens it again on the same catalog.
fn reopen(app: TestApp) -> TestApp {
    let TestApp { harness, fixture } = app;
    drop(harness);
    let store = fixture.store();
    let mut reopened = open_app(fixture, store, WINDOW, 1.0, |_| {});
    settle(&mut reopened, |app| app.state.edit().is_some());
    reopened
}

fn select(app: &mut TestApp, index: usize) {
    app.state_mut().state.library.select(index);
    settle(app, |app| app.state.edit().is_some());
}

#[test]
fn a_drag_reaches_the_catalog_as_one_step() {
    let mut app = library_app(2);
    let track = slider_rect(&app, "Exposure");
    drag(&mut app, track.center(), track.center() + vec2(40.0, 0.0));
    let store = store_of(&app);
    store.flush();
    let variant = row(&app, 0).variant;
    assert_eq!(
        store.catalog.variant(variant).unwrap().recipe.basic.exposure,
        recipe(&app).basic.exposure
    );
    assert_eq!(store.catalog.history(variant).unwrap().steps.len(), 2);
}

#[test]
fn reopening_restores_the_recipe_history_and_snapshots() {
    let mut app = library_app(2);
    let track = slider_rect(&app, "Exposure");
    drag(&mut app, track.center(), track.center() + vec2(40.0, 0.0));
    let exposure = recipe(&app).basic.exposure;
    assert!(exposure > 0.5);
    app.state_mut()
        .state
        .edit_mut()
        .unwrap()
        .take_snapshot("Bright".into());
    let contrast = slider_rect(&app, "Contrast");
    drag(&mut app, contrast.center(), contrast.center() + vec2(30.0, 0.0));
    assert_eq!(steps(&app), 3);

    let mut app = reopen(app);
    assert_eq!(recipe(&app).basic.exposure, exposure);
    assert_eq!(steps(&app), 3);
    assert_eq!(edit(&app).snapshots.len(), 1);
    assert_eq!(edit(&app).snapshots[0].name, "Bright");

    app.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    app.step();
    app.step();
    assert_eq!(recipe(&app).basic.contrast, 0.0);
    assert_eq!(recipe(&app).basic.exposure, exposure);
    let store = store_of(&app);
    store.flush();
    assert_eq!(
        store
            .catalog
            .variant(row(&app, 0).variant)
            .unwrap()
            .recipe
            .basic
            .contrast,
        0.0
    );
}

#[test]
fn a_panel_switch_survives_a_reopen() {
    let mut app = library_app(1);
    app.get_by_label("Enable Tone Curve").click();
    app.step();
    app.step();
    let app = reopen(app);
    assert!(edit(&app).is_bypassed(unlight_app::state::sections::Section::ToneCurve));
}

#[test]
fn closing_mid_burst_still_saves_it() {
    let mut app = library_app(1);
    let edit = app.state_mut().state.edit_mut().unwrap();
    edit.recipe.basic.contrast = 12.0;
    edit.note("Contrast", "+12", unlight_app::state::edit::Step::Burst);
    app.step();
    let app = reopen(app);
    assert_eq!(recipe(&app).basic.contrast, 12.0);
}

#[test]
fn ratings_flags_and_labels_persist() {
    let mut app = library_app(3);
    app.key_press(Key::Num4);
    app.step();
    app.key_press(Key::P);
    app.step();
    app.key_press(Key::Num8);
    app.step();
    let first = row(&app, 0);
    assert_eq!(
        (first.rating, first.flag, first.label),
        (4, Flag::Pick, Label::Green)
    );
    let app = reopen(app);
    let first = row(&app, 0);
    assert_eq!(
        (first.rating, first.flag, first.label),
        (4, Flag::Pick, Label::Green)
    );
}

#[test]
fn the_session_comes_back_where_it_was() {
    let mut app = library_app(3);
    select(&mut app, 2);
    app.state_mut().state.apply(Action::ShowGrid);
    app.step();
    let variant = row(&app, 2).variant;
    let TestApp { harness, fixture } = app;
    drop(harness);
    let store = fixture.store();
    let session = unlight_app::state::session::Session::load(store.session_file());
    assert_eq!(session.variant, Some(variant.0));
    assert!(!session.develop);
}

#[test]
fn a_rating_filter_narrows_the_grid_and_a_rating_drops_out_of_it() {
    let mut app = library_app(4);
    app.key_press(Key::Num3);
    app.step();
    let store = store_of(&app);
    store.flush();
    let filter = Filter {
        rating: Some(RatingFilter {
            comparison: Comparison::GreaterOrEqual,
            value: 3,
        }),
        ..Filter::default()
    };
    app.state_mut().state.library.set_filter(filter);
    settle(&mut app, |app| app.state.library.count() == Some(1));

    app.key_press(Key::Num1);
    app.step();
    settle(&mut app, |app| app.state.library.count() == Some(0));
}

#[test]
fn text_search_finds_by_file_name() {
    let mut app = library_app(3);
    let filter = Filter {
        text: Some("photo-001".into()),
        ..Filter::default()
    };
    app.state_mut().state.library.set_filter(filter);
    settle(&mut app, |app| {
        app.state.library.count() == Some(1) && app.state.focused().is_some()
    });
    assert!(row(&app, 0).path.ends_with("photo-001.dng"));
}

#[test]
fn clicking_in_the_grid_selects_and_a_double_click_opens_the_loupe() {
    let mut app = library_app(3);
    app.state_mut().state.apply(Action::ShowGrid);
    app.state_mut().state.panels.filmstrip = false;
    app.step();
    app.step();
    let card = app.get_by_label("photo-001.dng").rect();
    app.hover_at(card.center());
    app.step();
    app.get_by_label("photo-001.dng").click();
    app.step();
    app.step();
    assert_eq!(app.state().state.library.focus(), Some(1));
    assert_eq!(app.state().state.module, Module::Library);

    for _ in 0..2 {
        app.get_by_label("photo-001.dng").click();
        app.step();
    }
    app.step();
    assert_eq!(app.state().state.library_view, LibraryView::Loupe);
}

#[test]
fn a_range_rates_together() {
    let mut app = library_app(4);
    app.state_mut().state.library.click(2, ClickKind::Extend);
    app.state_mut().state.apply(Action::SetRating(2));
    app.step();
    let store = store_of(&app);
    store.flush();
    for index in 0..3 {
        assert_eq!(store.catalog.variant(row(&app, index).variant).unwrap().rating, 2);
    }
    assert_eq!(store.catalog.variant(row(&app, 3).variant).unwrap().rating, 0);
}

#[test]
fn select_all_takes_every_row_including_unloaded_ones() {
    let mut app = library_app(3);
    app.key_press_modifiers(Modifiers::COMMAND, Key::A);
    app.step();
    settle(&mut app, |app| app.state.library.selection_len() == 3);
}

#[test]
fn a_huge_library_only_touches_what_is_on_screen() {
    let mut app = library_app(2);
    app.state_mut().state.apply(Action::ShowGrid);
    app.step();
    let library = &mut app.state_mut().state.library;
    library.apply_count(library.generation(), 100_000);
    library.take_page_requests();
    app.step();
    let requests = app.state_mut().state.library.take_page_requests();
    assert!(
        requests.len() <= 2,
        "{} pages for one screen of a huge library",
        requests.len()
    );
}

#[test]
fn a_virtual_copy_is_its_own_edit() {
    let mut app = library_app(1);
    app.state_mut().state.edit_mut().unwrap().recipe.basic.exposure = 1.0;
    app.step();
    app.step();
    app.state_mut().state.apply(Action::VirtualCopy);
    settle(&mut app, |app| {
        app.state.library.count() == Some(2)
            && app.state.focused().is_some_and(|row| row.copy_name.is_some())
            && app.state.edit().is_some()
    });
    assert_eq!(recipe(&app).basic.exposure, 1.0);
    app.state_mut().state.edit_mut().unwrap().recipe.basic.exposure = -1.0;
    app.step();
    app.step();
    let store = store_of(&app);
    store.flush();
    let copy = app.state().state.focused().unwrap().clone();
    let master = store.catalog.variants_of(copy.photo).unwrap();
    assert_eq!(master[0].recipe.basic.exposure, 1.0);
    assert_eq!(master[1].recipe.basic.exposure, -1.0);
}

/// Says yes in the dialog that's open and waits for the catalog to be `left` photos big.
fn confirm_removal(app: &mut TestApp, left: u64) {
    app.get_by_label("Remove").click();
    settle(app, |app| app.state.dialog.is_none() && app.state.library.count() == Some(left));
}

#[test]
fn delete_asks_first_and_cancelling_changes_nothing() {
    let mut app = library_app(3);
    app.key_press(Key::Delete);
    app.step();
    assert!(matches!(app.state().state.dialog, Some(Dialog::RemovePhotos { .. })));
    app.get_by_label("Cancel").click();
    app.step();
    app.step();
    assert_eq!(app.state().state.dialog, None);
    let store = store_of(&app);
    store.flush();
    assert_eq!(store.catalog.count(&Filter::default()).unwrap(), 3);
}

#[test]
fn removing_a_photo_leaves_its_file_and_moves_on_to_the_next() {
    let mut app = library_app(3);
    app.state_mut().state.library.select(1);
    app.step();
    let (gone, next) = (row(&app, 1), row(&app, 2));
    app.key_press(Key::Delete);
    app.step();
    confirm_removal(&mut app, 2);
    settle(&mut app, |app| app.state.edit().is_some());
    let focused = app.state().state.focused().unwrap().clone();
    assert_eq!(focused.variant, next.variant);
    assert_eq!(app.state().state.library.selection(), vec![next.variant]);
    assert!(gone.path.is_file(), "the file has to stay on disk");
    assert_eq!(app.state().state.status(), None);
}

#[test]
fn removing_the_last_photo_in_the_list_steps_back_onto_the_one_before() {
    let mut app = library_app(3);
    app.state_mut().state.library.select(2);
    app.step();
    let before = row(&app, 1);
    app.key_press(Key::Delete);
    app.step();
    confirm_removal(&mut app, 2);
    settle(&mut app, |app| app.state.focused().is_some());
    assert_eq!(app.state().state.focused().unwrap().variant, before.variant);
}

#[test]
fn a_selection_goes_together() {
    let mut app = library_app(4);
    app.state_mut().state.library.select(0);
    app.state_mut().state.library.click(2, ClickKind::Extend);
    app.step();
    let last = row(&app, 3);
    app.key_press(Key::Delete);
    app.step();
    confirm_removal(&mut app, 1);
    settle(&mut app, |app| app.state.focused().is_some());
    assert_eq!(app.state().state.focused().unwrap().variant, last.variant);
}

#[test]
fn the_whole_catalog_can_be_emptied() {
    let mut app = library_app(2);
    app.state_mut().state.apply(Action::SelectAll);
    settle(&mut app, |app| app.state.library.selection_len() == 2);
    app.key_press(Key::Delete);
    app.step();
    confirm_removal(&mut app, 0);
    assert_eq!(app.state().state.focused(), None);
    assert_eq!(app.state().state.library.selection(), Vec::new());
    for _ in 0..6 {
        app.step();
    }
    assert_eq!(app.state().state.status(), None);
}

#[test]
fn an_unsaved_edit_on_a_removed_photo_goes_quietly() {
    let mut app = library_app(2);
    app.state_mut().state.edit_mut().unwrap().recipe.basic.exposure = 1.0;
    let gone = row(&app, 0).variant;
    app.key_press(Key::Delete);
    app.step();
    confirm_removal(&mut app, 1);
    for _ in 0..6 {
        app.step();
    }
    assert!(!app.state().state.edits.contains_key(&gone));
    assert_eq!(app.state().state.status(), None);
}

#[test]
fn removing_a_virtual_copy_keeps_the_original() {
    let mut app = library_app(1);
    app.state_mut().state.apply(Action::VirtualCopy);
    settle(&mut app, |app| {
        app.state.library.count() == Some(2)
            && app.state.focused().is_some_and(|row| row.copy_name.is_some())
    });
    let copy = app.state().state.focused().unwrap().clone();
    app.key_press(Key::Delete);
    app.step();
    confirm_removal(&mut app, 1);
    let store = store_of(&app);
    store.flush();
    let left = store.catalog.variants_of(copy.photo).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].copy_name, None);
}

#[test]
fn removing_an_original_takes_its_virtual_copies_too() {
    let mut app = library_app(2);
    app.state_mut().state.apply(Action::VirtualCopy);
    settle(&mut app, |app| {
        app.state.library.count() == Some(3)
            && app.state.focused().is_some_and(|row| row.copy_name.is_some())
    });
    let copy = app.state().state.focused().unwrap().clone();
    let original = store_of(&app).catalog.variants_of(copy.photo).unwrap()[0].id;
    let index = (0..3).find(|index| row(&app, *index).variant == original).unwrap();
    app.state_mut().state.library.select(index);
    app.step();
    app.key_press(Key::Delete);
    app.step();
    confirm_removal(&mut app, 1);
    let store = store_of(&app);
    store.flush();
    assert!(store.catalog.variants_of(copy.photo).unwrap().is_empty());
    let edits = &app.state().state.edits;
    assert!(!edits.contains_key(&original) && !edits.contains_key(&copy.variant));
}

#[test]
fn delete_in_develop_only_removes_a_photo_with_no_tool_in_hand() {
    let mut app = library_app(1);
    app.state_mut().state.apply(Action::ShowDevelop);
    app.state_mut().state.apply(Action::ToggleTool(Tool::Masks));
    app.step();
    app.key_press(Key::Delete);
    app.step();
    assert_eq!(app.state().state.dialog, None);
    app.state_mut().state.apply(Action::ToggleTool(Tool::Masks));
    app.step();
    app.key_press(Key::Delete);
    app.step();
    assert!(matches!(app.state().state.dialog, Some(Dialog::RemovePhotos { .. })));
}

#[test]
fn right_clicking_a_photo_selects_it_before_the_menu_acts() {
    let mut app = library_app(3);
    let target = row(&app, 2).variant;
    let title = unlight_app::state::library::title(&row(&app, 2));
    app.step();
    app.get_by_label(&title).click_secondary();
    app.step();
    app.step();
    assert_eq!(app.state().state.library.selection(), vec![target]);
}

#[test]
fn the_menus_lead_to_the_same_question() {
    let mut app = library_app(3);
    let title = unlight_app::state::library::title(&row(&app, 1));
    let target = row(&app, 1).variant;
    let open_menu = "Remove from Catalog…  (Delete)";

    app.get_by_label(&title).click_secondary();
    app.step();
    app.step();
    app.step();
    app.get_by_label(open_menu).click();
    app.step();
    app.step();
    let filmstrip = Dialog::RemovePhotos { variants: vec![target] };
    assert_eq!(app.state().state.dialog, Some(filmstrip.clone()));
    app.state_mut().state.dialog = None;

    app.state_mut().state.apply(Action::ShowGrid);
    app.state_mut().state.apply(Action::ToggleFilmstrip);
    app.state_mut().state.library.select(0);
    app.step();
    app.step();
    app.get_by_label(&title).click_secondary();
    app.step();
    app.step();
    app.step();
    app.get_by_label(open_menu).click();
    app.step();
    app.step();
    assert_eq!(app.state().state.dialog, Some(filmstrip));
}

#[test]
fn a_folder_can_be_removed_from_its_menu() {
    let mut app = library_app(2);
    settle(&mut app, |app| !app.state.lists.folders.is_empty());
    let folder = app.state().state.lists.folders[0].id;
    app.state_mut().state.apply(Action::ShowGrid);
    app.step();
    app.step();
    app.get_by_label("photos   2").click_secondary();
    app.step();
    app.step();
    app.step();
    app.get_by_label("Remove from Catalog…").click();
    app.step();
    app.step();
    assert_eq!(app.state().state.dialog, Some(Dialog::RemoveFolder { folder, name: "photos".into() }));
    confirm_removal(&mut app, 0);
    assert!(app.fixture.photos.join("photo-000.dng").is_file());
}

#[test]
fn a_removed_folder_clears_a_filter_on_it() {
    let mut app = library_app(2);
    settle(&mut app, |app| !app.state.lists.folders.is_empty());
    let folder = app.state().state.lists.folders[0].clone();
    let chosen = unlight_catalog::FolderFilter { folder: folder.id, include_subfolders: true };
    let rating = Some(RatingFilter { comparison: Comparison::GreaterOrEqual, value: 0 });
    app.state_mut().state.library.set_filter(Filter { folder: Some(chosen), rating, ..Filter::default() });
    app.state_mut().state.dialog = Some(Dialog::RemoveFolder { folder: folder.id, name: "photos".into() });
    app.step();
    app.step();
    confirm_removal(&mut app, 0);
    settle(&mut app, |app| app.state.lists.folders.is_empty());
    let filter = app.state().state.library.filter().clone();
    assert_eq!(filter.folder, None);
    assert!(filter.rating.is_some(), "only the folder should go");
    let store = store_of(&app);
    store.flush();
    assert_eq!(store.catalog.count(&Filter::default()).unwrap(), 0);
    assert!(app.fixture.photos.join("photo-000.dng").is_file());
}

fn turned_in_the_catalog(app: &TestApp, variant: unlight_catalog::VariantId) -> Orientation {
    let store = store_of(app);
    store.flush();
    store.catalog.variant(variant).unwrap().recipe.orientation
}

#[test]
fn rotating_is_an_edit_that_can_be_undone() {
    let mut app = library_app(1);
    let variant = row(&app, 0).variant;
    app.key_press_modifiers(Modifiers::COMMAND, Key::CloseBracket);
    settle(&mut app, |app| {
        app.state.edit().is_some_and(|edit| edit.recipe.orientation == Orientation::RotateCw)
    });
    let_settle(&mut app);
    assert_eq!(turned_in_the_catalog(&app, variant), Orientation::RotateCw);
    assert_eq!(row(&app, 0).turned, Orientation::RotateCw, "the thumbnail has to turn too");
    assert_eq!(edit(&app).history().entries().last().unwrap().label, "Rotate Right");

    app.state_mut().state.apply(Action::Undo);
    let_settle(&mut app);
    assert_eq!(recipe(&app).orientation, Orientation::Normal);
    assert_eq!(turned_in_the_catalog(&app, variant), Orientation::Normal);
    assert_eq!(row(&app, 0).turned, Orientation::Normal);
}

#[test]
fn a_rotation_and_a_flip_stack_as_seen_on_screen() {
    let mut app = library_app(1);
    app.state_mut().state.apply(Action::Turn(Orientation::RotateCw));
    app.state_mut().state.apply(Action::Turn(Orientation::MirrorX));
    app.step();
    assert_eq!(recipe(&app).orientation, Orientation::RotateCw.then(Orientation::MirrorX));
}

#[test]
fn the_whole_selection_turns_together() {
    let mut app = library_app(3);
    app.state_mut().state.apply(Action::ShowGrid);
    app.state_mut().state.library.select(0);
    app.state_mut().state.library.click(2, ClickKind::Extend);
    app.step();
    let variants = app.state().state.library.selection();
    assert_eq!(variants.len(), 3);
    app.state_mut().state.apply(Action::Turn(Orientation::MirrorY));
    let_settle(&mut app);
    settle(&mut app, |app| {
        (0..3).all(|index| {
            app.state.library.row(index).is_some_and(|row| row.turned == Orientation::MirrorY)
        })
    });
    for variant in variants {
        assert_eq!(turned_in_the_catalog(&app, variant), Orientation::MirrorY);
    }
}

#[test]
fn develop_only_turns_the_open_photo() {
    let mut app = library_app(3);
    app.state_mut().state.apply(Action::ShowDevelop);
    app.state_mut().state.library.select(0);
    app.state_mut().state.library.click(2, ClickKind::Extend);
    app.step();
    let open = app.state().state.focused().unwrap().variant;
    let others: Vec<_> = app.state().state.library.selection().into_iter().filter(|id| *id != open).collect();
    assert_eq!(others.len(), 2);
    app.state_mut().state.apply(Action::Turn(Orientation::RotateCw));
    let_settle(&mut app);
    assert_eq!(turned_in_the_catalog(&app, open), Orientation::RotateCw);
    for variant in others {
        assert_eq!(turned_in_the_catalog(&app, variant), Orientation::Normal);
    }
}

#[test]
fn the_crop_panel_rotates_and_flips() {
    let mut app = library_app(1);
    app.state_mut().state.apply(Action::ShowDevelop);
    settle(&mut app, support::live);
    app.state_mut().state.apply(Action::ToggleTool(Tool::Crop));
    app.step();
    app.step();
    app.get_by_label("Rotate Right  (Ctrl+])").click();
    app.step();
    app.step();
    assert_eq!(recipe(&app).orientation, Orientation::RotateCw);
    app.get_by_label("Rotate Left  (Ctrl+[)").click();
    app.step();
    app.step();
    assert_eq!(recipe(&app).orientation, Orientation::Normal);
    app.get_by_label("Flip Horizontal").click();
    app.step();
    app.step();
    assert_eq!(recipe(&app).orientation, Orientation::MirrorX);
    app.get_by_label("Flip Vertical").click();
    app.step();
    app.step();
    assert_eq!(recipe(&app).orientation, Orientation::Rotate180);
    assert_eq!(app.state().state.tool, Tool::Crop, "the tool stays open");
}

#[test]
fn a_turn_comes_back_after_a_restart() {
    let mut app = library_app(1);
    let variant = row(&app, 0).variant;
    app.state_mut().state.apply(Action::Turn(Orientation::RotateCcw));
    let_settle(&mut app);
    let reopened = reopen(app);
    assert_eq!(recipe(&reopened).orientation, Orientation::RotateCcw);
    assert_eq!(row(&reopened, 0).turned, Orientation::RotateCcw);
    assert_eq!(turned_in_the_catalog(&reopened, variant), Orientation::RotateCcw);
}

#[test]
fn reset_all_leaves_the_photo_the_way_up_it_was() {
    let mut app = library_app(1);
    app.state_mut().state.edit_mut().unwrap().recipe.basic.exposure = 1.0;
    app.state_mut().state.apply(Action::Turn(Orientation::RotateCw));
    app.state_mut().state.apply(Action::ResetAll);
    app.step();
    assert_eq!(recipe(&app).basic.exposure, 0.0);
    assert_eq!(recipe(&app).orientation, Orientation::RotateCw);
}

#[test]
fn pasting_settings_leaves_a_photo_the_way_up_it_was() {
    let mut app = library_app(2);
    app.state_mut().state.apply(Action::Turn(Orientation::RotateCw));
    app.state_mut().state.edit_mut().unwrap().recipe.basic.exposure = 1.0;
    app.state_mut().state.apply(Action::CopySettings);
    app.step();
    app.state_mut().state.apply(Action::Confirm);
    app.state_mut().state.apply(Action::NextPhoto);
    settle(&mut app, |app| {
        app.state.library.focus() == Some(1) && app.state.edit().is_some()
    });
    app.state_mut().state.apply(Action::PasteSettings);
    app.step();
    assert_eq!(recipe(&app).basic.exposure, 1.0);
    assert_eq!(recipe(&app).orientation, Orientation::Normal);
}

#[test]
fn a_virtual_copy_turns_on_its_own() {
    let mut app = library_app(1);
    app.state_mut().state.apply(Action::VirtualCopy);
    settle(&mut app, |app| {
        app.state.library.count() == Some(2)
            && app.state.focused().is_some_and(|row| row.copy_name.is_some())
            && app.state.edit().is_some()
    });
    let copy = app.state().state.focused().unwrap().clone();
    app.state_mut().state.apply(Action::Turn(Orientation::MirrorX));
    let_settle(&mut app);
    let original = store_of(&app).catalog.variants_of(copy.photo).unwrap()[0].id;
    assert_eq!(turned_in_the_catalog(&app, copy.variant), Orientation::MirrorX);
    assert_eq!(turned_in_the_catalog(&app, original), Orientation::Normal);
}

#[test]
fn the_menus_offer_rotate_and_flip() {
    let mut app = library_app(2);
    let title = unlight_app::state::library::title(&row(&app, 1));
    let target = row(&app, 1).variant;
    app.get_by_label(&title).click_secondary();
    app.step();
    app.step();
    app.step();
    app.get_by_label("Flip Horizontal").click();
    let_settle(&mut app);
    assert_eq!(turned_in_the_catalog(&app, target), Orientation::MirrorX);
    assert_eq!(recipe(&app).orientation, Orientation::MirrorX);
}

#[test]
fn keywords_land_on_the_selection_and_can_be_filtered_by() {
    let mut app = library_app(2);
    let ids = app.state().state.library.selection();
    app.state_mut().state.writes.push(Write::AddKeywords {
        variants: ids,
        names: vec!["cats".into()],
    });
    settle(&mut app, |app| {
        app.state.keywords.iter().any(|keyword| keyword.name == "cats")
    });
    let keyword = app.state().state.lists.keywords[0].id;
    app.state_mut().state.library.set_filter(Filter {
        keyword: Some(keyword),
        ..Filter::default()
    });
    settle(&mut app, |app| app.state.library.count() == Some(1));
}

#[test]
fn a_smart_collection_saved_from_the_editor_gathers_its_photos() {
    let mut app = library_app(3);
    app.key_press(Key::Num5);
    app.step();
    let mut draft = RuleDraft::default();
    draft.set_field(RuleField::Rating);
    draft.value = "5".into();
    let mut editor = SmartEditor::default();
    editor.name = "Best".into();
    editor.drafts = vec![draft];
    app.state_mut().state.smart_editor = Some(editor);
    app.state_mut().state.dialog = Some(Dialog::SmartCollection);
    app.step();
    app.step();
    app.get_by_label("Save").click();
    app.step();
    settle(&mut app, |app| !app.state.lists.collections.is_empty());
    let collection = app.state().state.lists.collections[0].id;
    app.state_mut().state.library.set_filter(Filter {
        collection: Some(collection),
        ..Filter::default()
    });
    settle(&mut app, |app| app.state.library.count() == Some(1));
}

#[test]
fn a_saved_preset_previews_on_hover_and_applies_on_click() {
    let mut app = library_app(1);
    app.state_mut().state.edit_mut().unwrap().recipe.basic.vibrance = 40.0;
    app.step();
    app.step();
    app.state_mut().state.copy_selection.set_all(false);
    app.state_mut()
        .state
        .copy_selection
        .set(SettingGroup::Presence, true);
    app.state_mut().state.save_preset("Mine", "Pop");
    app.step();
    settle(&mut app, |app| !app.state.lists.presets.is_empty());
    app.state_mut().state.edit_mut().unwrap().recipe.basic.vibrance = 0.0;
    app.step();
    app.step();
    let before = steps(&app);

    let preset = app.get_by_label("Pop").rect();
    app.hover_at(preset.center());
    app.step();
    app.step();
    let previewed = app
        .state()
        .state
        .preview_recipe
        .as_ref()
        .map(|recipe| recipe.basic.vibrance);
    assert_eq!(previewed, Some(40.0));
    assert_eq!(recipe(&app).basic.vibrance, 0.0);
    assert_eq!(steps(&app), before);

    app.get_by_label("Pop").click();
    app.step();
    app.step();
    assert_eq!(recipe(&app).basic.vibrance, 40.0);
    assert_eq!(edit(&app).history().current_label(), "Preset: Pop");

    app.remove_cursor();
    app.step();
    app.step();
    assert_eq!(app.state().state.preview_recipe, None);
}

#[test]
fn the_import_dialog_scans_a_folder_on_a_worker_and_reports() {
    let fixture = Fixture::with_photos(3);
    fs::write(fixture.photos.join("broken.nef"), b"not a raw file").unwrap();
    let store = fixture.store();
    let root = fixture.photos.display().to_string();
    let mut app = open_app(fixture, store, WINDOW, 1.0, |_| {});
    app.step();
    app.state_mut().state.apply(Action::OpenImport);
    app.state_mut().state.import.folder = root;
    app.step();
    app.step();
    save(&mut app, "import_choosing");
    app.get_by_label("Import").click();
    app.step();
    settle(&mut app, |app| {
        matches!(app.state.import.phase, ImportPhase::Finished(_))
    });
    settle(&mut app, |app| app.state.library.count() == Some(4));
    let ImportPhase::Finished(report) = app.state().state.import.phase.clone() else {
        panic!("the scan should have finished");
    };
    assert_eq!((report.discovered, report.unsupported), (4, 1));
    save(&mut app, "import_finished");
    app.get_by_label("Done").click();
    app.step();
    app.step();
    assert_eq!(app.state().state.dialog, None);
}

#[test]
fn an_unsupported_file_shows_why() {
    let fixture = Fixture::empty();
    fs::write(fixture.photos.join("broken.nef"), b"not a raw file").unwrap();
    let store = fixture.store();
    fixture.scan(&store);
    let mut app = open_app(fixture, store, WINDOW, 1.0, |_| {});
    settle(&mut app, |app| {
        app.state.focused().is_some() && app.state.photo.is_some()
    });
    assert_eq!(row(&app, 0).status, PhotoStatus::Unsupported);
    assert!(matches!(
        app.state().state.decode_state(),
        unlight_app::state::DecodeState::Failed(_)
    ));
    app.run_steps(4);
    save(&mut app, "unsupported");
}

#[test]
fn a_missing_file_is_marked_on_a_rescan_and_keeps_its_edit() {
    let mut app = library_app(2);
    app.state_mut().state.edit_mut().unwrap().recipe.basic.exposure = 1.0;
    app.step();
    app.step();
    let gone = row(&app, 0).path.clone();
    fs::remove_file(&gone).unwrap();
    let root = gone.parent().unwrap().to_path_buf();
    app.state_mut().state.open_paths(std::slice::from_ref(&root));
    app.step();
    settle(&mut app, |app| {
        app.state
            .library
            .row(0)
            .is_some_and(|row| row.status == PhotoStatus::Missing)
    });
    assert_eq!(recipe(&app).basic.exposure, 1.0);
    assert!(matches!(
        app.state().state.decode_state(),
        unlight_app::state::DecodeState::Failed(_)
    ));
}

#[test]
fn an_opened_file_lands_selected_in_its_folder() {
    let fixture = Fixture::with_photos(3);
    let target = fixture.photos.join("photo-002.dng");
    let store = fixture.store();
    let mut app = open_app(fixture, store, WINDOW, 1.0, move |app| {
        app.state.open_paths(&[target])
    });
    settle(&mut app, |app| {
        app.state
            .focused()
            .is_some_and(|row| row.path.ends_with("photo-002.dng"))
    });
}

fn exports_in(directory: &std::path::Path) -> Vec<PathBuf> {
    fs::read_dir(directory).map_or(Vec::new(), |entries| {
        entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect()
    })
}

fn start_export(app: &mut TestApp, directory: &std::path::Path) {
    app.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    app.step();
    app.step();
    assert_eq!(app.state().state.dialog, Some(Dialog::Export));
    let form = app.state_mut().state.export_form.as_mut().expect("export form");
    form.directory = directory.display().to_string();
    app.step();
    app.step();
    save(app, "export_form");
    app.get_by_label("Export").click();
    app.step();
}

/// Width and height from a JPEG's frame header.
fn jpeg_size(path: &std::path::Path) -> (u32, u32) {
    let bytes = fs::read(path).unwrap();
    let field = |at: usize| u32::from(u16::from_be_bytes([bytes[at], bytes[at + 1]]));
    let mut at = 2;
    while !(0xC0..=0xC2).contains(&bytes[at + 1]) {
        at += 2 + field(at + 2) as usize;
    }
    (field(at + 7), field(at + 5))
}

#[test]
fn a_turned_photo_exports_turned() {
    let mut app = library_app(1);
    let plain = app.fixture.dir.path().join("plain");
    start_export(&mut app, &plain);
    settle(&mut app, |app| app.state.export_run.as_ref().is_some_and(|run| run.finished));
    app.get_by_label("Done").click();
    app.step();
    app.step();
    let (width, height) = jpeg_size(&exports_in(&plain)[0]);
    assert_ne!(width, height);

    app.state_mut().state.apply(Action::Turn(Orientation::RotateCw));
    let_settle(&mut app);
    let turned = app.fixture.dir.path().join("turned");
    start_export(&mut app, &turned);
    let written = turned.clone();
    settle(&mut app, move |app| {
        app.state.export_run.as_ref().is_some_and(|run| run.finished) && !exports_in(&written).is_empty()
    });
    assert_eq!(jpeg_size(&exports_in(&turned)[0]), (height, width));
}

#[test]
fn a_batch_exports_on_a_worker_and_lists_what_failed() {
    let mut app = library_app(3);
    app.state_mut().state.library.click(2, ClickKind::Extend);
    let gone = row(&app, 1).path.clone();
    fs::remove_file(&gone).unwrap();
    let output = app.fixture.dir.path().join("out");
    start_export(&mut app, &output);
    settle(&mut app, |app| {
        app.state.export_run.as_ref().is_some_and(|run| run.finished)
    });
    let run = app.state().state.export_run.clone().unwrap();
    assert_eq!(run.total, 3);
    assert_eq!(run.failures.len(), 1, "{:?}", run.failures);
    assert!(run.failures[0].name.contains("photo-001"));
    assert_eq!(exports_in(&output).len(), 2);
    save(&mut app, "export_finished");
    app.get_by_label("Done").click();
    app.step();
    app.step();
    assert_eq!(app.state().state.dialog, None);
}

#[test]
fn cancelling_stops_after_the_file_in_progress() {
    let mut app = library_app(8);
    app.key_press_modifiers(Modifiers::COMMAND, Key::A);
    settle(&mut app, |app| app.state.library.selection_len() == 8);
    let output = app.fixture.dir.path().join("out");
    start_export(&mut app, &output);
    settle(&mut app, |app| {
        app.state.export_run.as_ref().is_some_and(|run| run.done >= 1)
    });
    app.get_by_label("Cancel Export").click();
    app.step();
    settle(&mut app, |app| {
        app.state.export_run.as_ref().is_some_and(|run| run.finished)
    });
    let run = app.state().state.export_run.clone().unwrap();
    assert!(run.cancelled);
    assert!(run.done < 8, "cancelling did nothing, {} files done", run.done);
}

#[test]
fn sidecars_are_written_on_a_worker_once_things_go_quiet() {
    let fixture = Fixture::with_photos(1);
    let config = StoreConfig {
        sidecar_delay: Some(Duration::from_millis(50)),
        ..fixture.config()
    };
    let store = Arc::new(Store::open(&config).unwrap());
    fixture.scan(&store);
    let sidecar = fixture.photos.join("photo-000.xmp");
    let mut app = open_app(fixture, store, WINDOW, 1.0, |_| {});
    settle(&mut app, |app| app.state.edit().is_some());
    app.state_mut().state.apply(Action::SetRating(5));
    app.step();
    for _ in 0..200 {
        if sidecar.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(sidecar.exists(), "no sidecar appeared");
    assert!(fs::read_to_string(&sidecar).unwrap().contains("Rating"));
}

/// Where the first guide's end shows now. One guide straightens the photo, so it's moved from
/// where it was drawn, and only the geometry on screen can say where to.
fn guide_end_on_screen(app: &TestApp) -> Pos2 {
    let state = app.state();
    let row = state.state.focused().expect("a focused photo");
    let live = state.darkroom.live(&row.path).expect("a live session");
    let recipe = recipe(app).effective();
    let aspect = live.sensor_aspect();
    let optics = live.session.optics();
    let geometry = Geometry::new(&Crop::default(), &recipe.transform, &recipe.lens, aspect, optics);
    let [x, y] = source_to_view(
        &geometry,
        recipe.transform.guides[0].end,
        live.orientation,
        aspect,
    );
    let photo = state.viewport.photo_rect.expect("a photo on screen");
    photo.left_top() + photo.size() * vec2(x, y)
}

#[test]
fn guides_are_drawn_on_the_photo_and_saved() {
    let mut app = support::editing_app();
    app.state_mut().state.apply(Action::ToggleTool(Tool::Guides));
    app.step();
    app.step();
    assert_eq!(recipe(&app).transform.upright, Upright::Guided);
    let photo = app.state().viewport.photo_rect.expect("a photo on screen");
    let start = photo.left_top() + photo.size() * vec2(0.2, 0.2);
    let end = photo.left_top() + photo.size() * vec2(0.25, 0.8);
    drag(&mut app, start, end);
    let guides = recipe(&app).transform.guides.clone();
    assert_eq!(guides.len(), 1, "{guides:?}");
    assert!(guides[0].start[0] < guides[0].end[0]);
    save(&mut app, "guides");

    let step = steps(&app);
    let handle = guide_end_on_screen(&app);
    drag(&mut app, handle, handle + vec2(30.0, 0.0));
    let moved = recipe(&app).transform.guides.clone();
    assert_eq!(moved.len(), 1);
    assert_ne!(moved[0].end, guides[0].end);
    assert_eq!(steps(&app), step + 1);

    let slip = photo.left_top() + photo.size() * vec2(0.7, 0.5);
    drag(&mut app, slip, slip + vec2(2.0, 0.0));
    assert_eq!(recipe(&app).transform.guides.len(), 1);
}

#[test]
fn compare_and_survey_show_the_selection() {
    let mut app = library_app(4);
    settle(&mut app, |app| app.state.library.len() == 4);
    app.state_mut().state.library.click(2, ClickKind::Extend);
    app.key_press(Key::C);
    app.step();
    assert_eq!(app.state().state.library_view, LibraryView::Compare);
    settle(&mut app, |app| {
        let rows = app.state.library.selected_rows();
        rows.iter().all(|row| app.darkroom.has_full_preview(&row.path))
    });
    save(&mut app, "compare");
    app.key_press(Key::N);
    app.step();
    assert_eq!(app.state().state.library_view, LibraryView::Survey);
    app.step();
    let name = row(&app, 1)
        .path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    save(&mut app, "survey");
    app.get_by_label(&format!("Remove {name} from the survey"))
        .click();
    app.step();
    app.step();
    assert_eq!(app.state().state.library.selection_len(), 2);
}

#[test]
fn the_rest_of_the_library_keys_work() {
    let mut app = library_app(4);
    app.key_press(Key::G);
    app.step();
    app.state_mut().state.grid_columns = 2;
    app.key_press(Key::ArrowDown);
    app.step();
    assert_eq!(app.state().state.library.focus(), Some(2));
    app.key_press(Key::E);
    app.step();
    assert_eq!(app.state().state.library_view, LibraryView::Loupe);
}

#[test]
fn part_of_a_preset_can_be_applied() {
    let mut app = library_app(1);
    let edit = app.state_mut().state.edit_mut().unwrap();
    edit.recipe.basic.exposure = 1.0;
    edit.recipe.basic.vibrance = 30.0;
    app.step();
    app.step();
    app.state_mut().state.copy_selection.set_all(true);
    app.state_mut().state.save_preset("Mine", "Both");
    app.step();
    settle(&mut app, |app| !app.state.lists.presets.is_empty());
    app.state_mut().state.edit_mut().unwrap().reset_all();
    app.step();
    app.step();

    let preset = app.state().state.lists.presets[0].id;
    app.state_mut().state.copy_selection.set_all(false);
    app.state_mut()
        .state
        .copy_selection
        .set(SettingGroup::Presence, true);
    app.state_mut().state.dialog = Some(Dialog::ApplyPresetPart(preset));
    app.step();
    app.step();
    save(&mut app, "preset_part");
    app.get_by_label("Apply").click();
    app.step();
    app.step();
    assert_eq!(recipe(&app).basic.vibrance, 30.0);
    assert_eq!(recipe(&app).basic.exposure, 0.0);
}

#[test]
fn keywords_can_be_typed_into_the_panel() {
    let mut app = library_app(1);
    app.state_mut().state.apply(Action::ShowGrid);
    app.step();
    app.step();
    let in_right_panel = |app: &TestApp| {
        let fields = app.get_all_by_role(Role::TextInput);
        fields
            .into_iter()
            .find(|field| field.rect().left() > 1200.0)
            .expect("the keyword field")
            .rect()
    };
    let field = in_right_panel(&app);
    app.hover_at(field.center());
    app.step();
    app.drag_at(field.center());
    app.step();
    app.drop_at(field.center());
    app.step();
    app.step();
    app.event(Event::Text("cats, dogs".into()));
    app.step();
    app.key_press(Key::Enter);
    app.step();
    settle(&mut app, |app| app.state.keywords.len() == 2);
    app.step();
    let names: Vec<String> = app
        .state()
        .state
        .keywords
        .iter()
        .map(|keyword| keyword.name.clone())
        .collect();
    assert_eq!(names, ["cats", "dogs"]);
    assert!(app.state().state.keyword_entry.is_empty());
}

#[test]
fn the_smart_collection_editor_edits_existing_rules() {
    let mut app = library_app(2);
    let mut draft = RuleDraft::default();
    draft.set_field(RuleField::Camera);
    draft.value = "Acme".into();
    let mut editor = SmartEditor::default();
    editor.name = "Acme".into();
    editor.drafts = vec![draft];
    app.state_mut().state.smart_editor = Some(editor);
    app.state_mut().state.dialog = Some(Dialog::SmartCollection);
    app.step();
    app.step();
    save(&mut app, "smart_collection");
    app.get_by_label("Save").click();
    app.step();
    settle(&mut app, |app| !app.state.lists.collections.is_empty());

    let collection = app.state().state.lists.collections[0].clone();
    let rules = collection.rules.clone().expect("a smart collection");
    let mut editing = SmartEditor::editing(collection.id, &collection.name, &rules);
    editing.drafts[0].value = "Nothing".into();
    app.state_mut().state.smart_editor = Some(editing);
    app.state_mut().state.dialog = Some(Dialog::SmartCollection);
    app.step();
    app.step();
    app.get_by_label("Save").click();
    app.step();
    let filter = Filter {
        collection: Some(collection.id),
        ..Filter::default()
    };
    app.state().store.flush();
    app.state_mut().state.library.set_filter(filter);
    settle(&mut app, |app| app.state.library.count() == Some(0));
}

#[test]
fn a_rescan_finds_new_files_without_touching_the_rest() {
    let mut app = library_app(2);
    app.fixture.add_photo(9, "photo-009.dng");
    let root = app.fixture.photos.clone();
    app.state_mut().state.open_paths(&[root]);
    app.step();
    settle(&mut app, |app| {
        matches!(app.state.import.phase, ImportPhase::Finished(_)) && app.state.library.count() == Some(3)
    });
    let ImportPhase::Finished(report) = app.state().state.import.phase.clone() else {
        panic!("the scan should have finished");
    };
    assert_eq!((report.added, report.unchanged), (1, 2));
}

#[test]
fn constrain_crop_removes_empty_corners() {
    let mut app = support::editing_app();
    let edit = app.state_mut().state.edit_mut().unwrap();
    edit.recipe.transform.vertical = 60.0;
    app.step();
    app.step();
    app.state_mut().state.apply(Action::ToggleTool(Tool::Crop));
    app.step();
    app.step();
    app.get_by_label("Constrain").click();
    app.step();
    app.step();
    app.step();
    let crop = recipe(&app).crop;
    assert!(crop.right - crop.left < 1.0, "{crop:?}");
    assert_eq!(edit_label(&app), "Constrain Crop");
}

fn edit_label(app: &TestApp) -> String {
    edit(app).history().current_label().to_owned()
}

#[test]
fn a_sidecar_edited_elsewhere_is_never_overwritten_behind_your_back() {
    let fixture = Fixture::with_photos(1);
    let sidecar = fixture.photos.join("photo-000.xmp");
    {
        let store = fixture.store();
        fixture.scan(&store);
        let variant = store
            .catalog
            .query(
                &Filter::default(),
                Default::default(),
                Page { offset: 0, limit: 1 },
            )
            .unwrap()[0]
            .variant;
        let photo = store.catalog.variant(variant).unwrap().photo;
        store.catalog.write_sidecar(photo).unwrap();
        store.catalog.set_rating(&[variant], 2).unwrap();
        let foreign = fs::read_to_string(&sidecar)
            .unwrap()
            .replace("xmp:Rating=\"0\"", "xmp:Rating=\"5\"");
        fs::write(&sidecar, &foreign).unwrap();
        fixture.scan(&store);
        assert_eq!(store.catalog.sidecar_conflicts().unwrap(), [photo]);
    }
    let before = fs::read_to_string(&sidecar).unwrap();
    let config = StoreConfig {
        sidecar_delay: Some(Duration::from_millis(20)),
        ..fixture.config()
    };
    let store = Arc::new(Store::open(&config).unwrap());
    let mut app = open_app(fixture, store, WINDOW, 1.0, |_| {});
    settle(&mut app, |app| {
        !app.state.lists.sidecar_conflicts.is_empty() && app.state.photo.is_some()
    });
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        fs::read_to_string(&sidecar).unwrap(),
        before,
        "the worker overwrote a conflicted sidecar"
    );

    app.state_mut().state.apply(Action::ShowGrid);
    app.step();
    app.step();
    app.get_by_label("Use Sidecar").click();
    app.step();
    settle(&mut app, |app| app.state.lists.sidecar_conflicts.is_empty());
    let store = store_of(&app);
    let variant = row(&app, 0).variant;
    assert_eq!(store.catalog.variant(variant).unwrap().rating, 5);
}
