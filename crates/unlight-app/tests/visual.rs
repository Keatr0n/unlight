//! Frames of the real app on real raws, for looking at. They only run where samples exist.

mod support;

use eframe::egui::{Key, pos2, vec2};
use egui_kittest::kittest::Queryable;
use std::fs;
use support::{app, live, sample, save, settle};
use unlight_app::{
    action::Action,
    state::{Compare, Effect, Lights, Tool, local::MaskKind, sections::Section, view::Zoom},
};
use unlight_catalog::{Flag, Label};
use unlight_recipe::MaskShape;

#[test]
fn develop_on_a_real_photo() {
    let Some(path) = sample("sony-a7rm4.ARW") else {
        return;
    };
    let mut harness = app(&[path], vec2(1600.0, 1000.0), 1.0);
    settle(&mut harness, live);
    save(&mut harness, "develop");

    harness.state_mut().state.compare = Compare::Split;
    if let Some(edit) = harness.state_mut().state.edit_mut() {
        edit.recipe.basic.exposure = 0.8;
        edit.recipe.basic.contrast = 30.0;
        edit.recipe.basic.vibrance = 25.0;
    }
    harness.run_steps(3);
    save(&mut harness, "split");

    harness.state_mut().state.compare = Compare::SideBySide;
    harness.run_steps(3);
    save(&mut harness, "side_by_side");

    harness.state_mut().state.compare = Compare::Off;
    harness.state_mut().state.apply(Action::ToggleTool(Tool::Crop));
    if let Some(edit) = harness.state_mut().state.edit_mut() {
        edit.recipe.crop.left = 0.1;
        edit.recipe.crop.top = 0.1;
        edit.recipe.crop.right = 0.85;
        edit.recipe.crop.bottom = 0.8;
    }
    harness.run_steps(3);
    save(&mut harness, "crop");

    harness.state_mut().state.apply(Action::Confirm);
    harness.state_mut().state.apply(Action::CopySettings);
    harness.run_steps(3);
    save(&mut harness, "copy_dialog");
    harness.state_mut().state.dialog = None;

    harness.state_mut().state.apply(Action::ShowGrid);
    settle(&mut harness, |app| {
        (0..app.state.library.len()).all(|index| {
            app.state
                .library
                .row(index)
                .is_some_and(|row| app.darkroom.thumbnail(&row.path).is_some())
        })
    });
    harness.run_steps(3);
    save(&mut harness, "library");

    harness.state_mut().state.apply(Action::SetFlag(Flag::Pick));
    harness.state_mut().state.apply(Action::SetRating(3));
    harness.key_press(Key::ArrowLeft);
    harness.step();
    harness.state_mut().state.apply(Action::ToggleLabel(Label::Green));
    harness.key_press(Key::E);
    settle(&mut harness, |app| {
        app.state
            .focused()
            .is_some_and(|row| app.darkroom.has_full_preview(&row.path))
    });
    harness.run_steps(3);
    save(&mut harness, "library_loupe");
}

#[test]
fn local_tools_on_a_real_photo() {
    let Some(path) = sample("sony-a7rm4.ARW") else {
        return;
    };
    let mut harness = app(&[path], vec2(1600.0, 1000.0), 1.0);
    settle(&mut harness, live);

    harness.state_mut().state.apply(Action::ToggleTool(Tool::Masks));
    harness.state_mut().state.add_mask(MaskKind::Radial);
    if let Some(edit) = harness.state_mut().state.edit_mut() {
        edit.recipe.masks[0].components[0].shape = MaskShape::Radial {
            center: [0.5, 0.86],
            radii: [0.4, 0.1],
            angle: 4.0,
            feather: 0.7,
            inverted: false,
        };
        edit.recipe.masks[0].adjustments.exposure = 1.2;
        edit.recipe.masks[0].adjustments.shadows = 35.0;
    }
    harness.run_steps(3);
    save(&mut harness, "masks_radial");

    harness.state_mut().state.apply(Action::ToggleMaskOverlay);
    harness.run_steps(3);
    save(&mut harness, "masks_overlay");

    harness.state_mut().state.apply(Action::ToggleMaskOverlay);
    harness.state_mut().state.add_mask(MaskKind::Linear);
    harness.run_steps(3);
    save(&mut harness, "masks_linear");

    harness.state_mut().state.add_mask(MaskKind::Brush);
    harness.run_steps(2);
    let photo = harness.state().viewport.photo_rect.expect("photo on screen");
    let from = photo.left_top() + vec2(photo.width() * 0.3, photo.height() * 0.3);
    support::drag(&mut harness, from, from + vec2(300.0, 60.0));
    harness.state_mut().state.apply(Action::ToggleMaskOverlay);
    harness.hover_at(from + vec2(150.0, 180.0));
    harness.run_steps(3);
    save(&mut harness, "masks_brush");

    harness.state_mut().state.apply(Action::ToggleMaskOverlay);
    harness.state_mut().state.apply(Action::ToggleTool(Tool::Heal));
    harness.run_steps(2);
    let spot = photo.left_top() + vec2(photo.width() * 0.60, photo.height() * 0.80);
    harness.hover_at(spot);
    harness.step();
    for pressed in [true, false] {
        harness.event(eframe::egui::Event::PointerButton {
            pos: spot,
            button: eframe::egui::PointerButton::Primary,
            pressed,
            modifiers: eframe::egui::Modifiers::NONE,
        });
        harness.step();
    }
    harness.run_steps(3);
    save(&mut harness, "heal_circles");
}

#[test]
fn zoomed_to_full_resolution() {
    let Some(path) = sample("sony-a7rm4.ARW") else {
        return;
    };
    let mut harness = app(&[path], vec2(2560.0, 1440.0), 1.0);
    settle(&mut harness, live);
    harness.state_mut().state.view.zoom = Zoom::Ratio(1.0);
    harness.state_mut().state.view.center = [0.42, 0.72];
    harness.step();
    save(&mut harness, "zoom_coarse");
    harness.run();
    save(&mut harness, "zoom_final");
    harness.state_mut().state.clipping = true;
    harness.state_mut().state.notify("Clipping shown");
    harness.step();
    harness.step();
    save(&mut harness, "clipping_toast");
}

#[test]
fn panels_in_their_other_modes() {
    let Some(path) = sample("canon-eosr.CR3") else {
        return;
    };
    let mut harness = app(&[path], vec2(1600.0, 1000.0), 1.0);
    harness.state_mut().state.set_section_open(Section::Basic, false);
    harness
        .state_mut()
        .state
        .set_section_open(Section::ToneCurve, false);
    harness
        .state_mut()
        .state
        .set_section_open(Section::ColorMixer, true);
    harness
        .state_mut()
        .state
        .set_section_open(Section::ColorGrading, true);
    settle(&mut harness, live);
    if let Some(edit) = harness.state_mut().state.edit_mut() {
        edit.recipe.color_grading.shadows.saturation = 40.0;
        edit.recipe.color_grading.shadows.hue = 210.0;
        edit.recipe.color_grading.highlights.saturation = 25.0;
        edit.recipe.color_grading.highlights.hue = 40.0;
    }
    harness.step();
    harness.step();
    save(&mut harness, "mixer_grading");

    harness.state_mut().state.apply(Action::ToggleBlackAndWhite);
    harness
        .state_mut()
        .state
        .set_section_open(Section::ToneCurve, true);
    harness
        .state_mut()
        .state
        .set_section_open(Section::ColorGrading, false);
    harness.step();
    harness.get_by_label("Parametric").click();
    harness.step();
    harness.step();
    harness.step();
    save(&mut harness, "bw_parametric");

    harness.state_mut().state.lights = Lights::Dim;
    harness.state_mut().state.panels.left = false;
    harness.step();
    harness.step();
    harness.step();
    save(&mut harness, "lights_dim");
}

#[test]
fn hover_states_and_popups() {
    let Some(path) = sample("gh5.RW2") else {
        return;
    };
    let mut harness = app(&[path], vec2(1600.0, 1000.0), 1.0);
    settle(&mut harness, live);
    harness.hover_at(pos2(1400.0, 100.0));
    harness.state_mut().state.apply(Action::SetFlag(Flag::Pick));
    harness.state_mut().state.apply(Action::SetRating(4));
    harness.state_mut().state.apply(Action::ToggleLabel(Label::Green));
    harness.state_mut().state.library.step(1);
    harness.state_mut().state.apply(Action::SetFlag(Flag::Reject));
    harness.state_mut().state.apply(Action::ToggleLabel(Label::Red));
    harness.state_mut().state.library.step(-1);
    harness.step();
    harness.step();
    save(&mut harness, "histogram_hover");

    harness.get_by_label("White Balance").click();
    harness.step();
    harness.step();
    harness.step();
    save(&mut harness, "white_balance_menu");
    harness.key_press(Key::Escape);
    harness.step();
    harness.step();
    assert!(
        harness
            .state()
            .state
            .edit()
            .is_some_and(|edit| edit.history().entries().len() == 1)
    );

    let slider = harness.get_by_label("Vibrance").rect();
    harness.hover_at(slider.center());
    harness.step();
    harness.step();
    harness.get_by_label("Contrast value").click();
    harness.step();
    harness.step();
    harness.step();
    save(&mut harness, "typing");
}

#[test]
fn flipping_quickly_lands_on_the_right_photo() {
    let Some(path) = sample("sony-a7rm4.ARW") else {
        return;
    };
    let mut harness = app(&[path], vec2(1200.0, 800.0), 1.0);
    settle(&mut harness, live);
    for _ in 0..6 {
        harness.key_press(Key::ArrowLeft);
        harness.step();
    }
    let index = harness.state().state.library.focus();
    settle(&mut harness, |app| {
        let Some(row) = app.state.focused() else {
            return false;
        };
        app.darkroom.has_full_preview(&row.path) && app.darkroom.live(&row.path).is_some()
    });
    assert_eq!(harness.state().state.library.focus(), index);
}

#[test]
fn nothing_open_yet() {
    let mut harness = app(&[], vec2(1600.0, 1000.0), 1.0);
    harness.step();
    harness.step();
    save(&mut harness, "empty");
}

#[test]
fn all_panels_open() {
    let Some(path) = sample("sony-a7rm4.ARW") else {
        return;
    };
    let mut harness = app(&[path], vec2(1600.0, 3400.0), 1.0);
    for section in Section::ALL {
        harness.state_mut().state.set_section_open(section, true);
    }
    settle(&mut harness, live);
    save(&mut harness, "panels");
}

#[test]
fn laptop_at_fractional_scale() {
    let Some(path) = sample("fuji-xt4.RAF") else {
        return;
    };
    let mut harness = app(&[path], vec2(1504.0, 1002.67), 1.5);
    settle(&mut harness, live);
    save(&mut harness, "laptop");
}

#[test]
fn monitor_at_a_quarter_scale() {
    let Some(path) = sample("pixel4xl.dng") else {
        return;
    };
    let mut harness = app(&[path], vec2(2048.0, 1152.0), 1.25);
    settle(&mut harness, live);
    save(&mut harness, "monitor_125");
}

#[test]
fn undecodable_file_shows_its_preview_and_a_message() {
    let Some(path) = sample("nikon-z8-compressed.NEF") else {
        return;
    };
    let mut harness = app(&[path], vec2(1600.0, 1000.0), 1.0);
    settle(&mut harness, |app| {
        let done = !matches!(app.state.decode_state(), unlight_app::state::DecodeState::Pending);
        let path = app.state.focused().map(|row| row.path.clone());
        done && path.is_some_and(|path| {
            app.darkroom.has_full_preview(&path) && app.darkroom.thumbnail(&path).is_some()
        })
    });
    save(&mut harness, "failed");
}

#[test]
fn a_real_photo_exports_and_lands_on_disk() {
    let Some(path) = sample("gh5.RW2") else {
        return;
    };
    let mut harness = app(&[path], vec2(1600.0, 1000.0), 1.0);
    settle(&mut harness, live);
    let output = harness.fixture.dir.path().join("exported");
    harness.state_mut().state.apply(Action::OpenExport);
    harness.step();
    let form = harness
        .state_mut()
        .state
        .export_form
        .as_mut()
        .expect("export form");
    form.choose_preset(1);
    form.directory = output.display().to_string();
    harness.step();
    harness.step();
    save(&mut harness, "export_real_form");
    harness.state_mut().state.effects.push(Effect::StartExport);
    harness.step();
    settle(&mut harness, |app| {
        app.state.export_run.as_ref().is_some_and(|run| run.finished)
    });
    let run = harness.state().state.export_run.clone().expect("a finished run");
    assert!(run.failures.is_empty(), "{:?}", run.failures);
    let files: Vec<_> = fs::read_dir(&output).expect("output folder").flatten().collect();
    assert_eq!(files.len(), 1);
    assert!(files[0].metadata().expect("metadata").len() > 100_000);
}
