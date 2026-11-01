//! The app driven like a user would, on a synthetic photo through the real engine. These
//! need no sample files, so they run under `nix flake check` on llvmpipe.

mod support;

use eframe::egui::{
    Event, Key, Modifiers, MouseWheelSource, MouseWheelUnit, PointerButton, TouchPhase, vec2,
};
use egui_kittest::kittest::Queryable;
use std::{env, fs};
use support::{WINDOW, drag, edit, editing_app, editing_raw, recipe, slider_rect, steps, synthetic_raw};
use unlight_app::{
    action::Action,
    state::{Compare, DecodeState, Dialog, Lights, Module, Tool, sections::Section, view::Zoom},
};
use unlight_catalog::Flag;
use unlight_engine::{Engine, GpuContext, Precision, Session, ViewRequest};
use unlight_recipe::{Crop, Recipe, WhiteBalance};

#[test]
fn one_drag_is_one_undo_step() {
    let mut harness = editing_app();
    let track = slider_rect(&harness, "Exposure");
    drag(&mut harness, track.center(), track.center() + vec2(40.0, 0.0));
    assert!(
        recipe(&harness).basic.exposure > 0.5,
        "{}",
        recipe(&harness).basic.exposure
    );
    assert_eq!(steps(&harness), 2);
    assert!(edit(&harness).history().current_label().starts_with("Exposure"));

    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.step();
    assert_eq!(recipe(&harness).basic.exposure, 0.0);
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    harness.step();
    assert!(recipe(&harness).basic.exposure > 0.5);
}

#[test]
fn a_press_on_the_track_jumps_the_thumb_there() {
    let mut harness = editing_app();
    let track = slider_rect(&harness, "Contrast");
    harness.hover_at(track.right_center());
    harness.step();
    harness.drag_at(track.right_center());
    harness.step();
    harness.drop_at(track.right_center());
    harness.step();
    harness.step();
    assert!(recipe(&harness).basic.contrast > 90.0);
}

#[test]
fn double_click_resets_a_slider() {
    let mut harness = editing_app();
    harness
        .state_mut()
        .state
        .edit_mut()
        .expect("photo")
        .recipe
        .basic
        .shadows = 42.0;
    harness.step();
    harness.step();
    let label = harness.get_by_label("Shadows").rect();
    let point = label.center() - vec2(label.width() * 0.5 + 40.0, 0.0);
    for _ in 0..2 {
        harness.hover_at(point);
        harness.event(Event::PointerButton {
            pos: point,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.step();
        harness.event(Event::PointerButton {
            pos: point,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.step();
    }
    assert_eq!(recipe(&harness).basic.shadows, 0.0);
}

#[test]
fn alt_drag_moves_a_tenth_as_far() {
    let mut harness = editing_app();
    let track = slider_rect(&harness, "Whites");
    let start = track.center();
    harness.hover_at(start);
    harness.step();
    harness.event(Event::PointerButton {
        pos: start,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::ALT,
    });
    harness.event_modifiers(Event::PointerMoved(start), Modifiers::ALT);
    harness.step();
    for stage in 1..=10 {
        harness.event_modifiers(
            Event::PointerMoved(start + vec2(stage as f32 * 4.0, 0.0)),
            Modifiers::ALT,
        );
        harness.step();
    }
    harness.drop_at(start + vec2(40.0, 0.0));
    harness.step();
    harness.step();
    let fine = recipe(&harness).basic.whites;

    let mut harness = editing_app();
    drag(&mut harness, track.center(), track.center() + vec2(40.0, 0.0));
    let coarse = recipe(&harness).basic.whites;
    assert!(coarse > 40.0, "{coarse}");
    assert!((fine - coarse * 0.1).abs() <= 2.0, "fine {fine}, coarse {coarse}");
}

fn wheel(harness: &egui_kittest::Harness<'static, unlight_app::app::App>, unit: MouseWheelUnit, y: f32) {
    harness.event(Event::MouseWheel {
        unit,
        delta: vec2(0.0, y),
        phase: TouchPhase::Move,
        modifiers: Modifiers::NONE,
        source: MouseWheelSource::Unknown,
    });
}

#[test]
fn wheel_notches_step_a_hovered_slider_and_fold_into_one_undo_step() {
    let mut harness = editing_app();
    let track = slider_rect(&harness, "Exposure");
    harness.hover_at(track.center());
    harness.step();
    for _ in 0..4 {
        wheel(&harness, MouseWheelUnit::Line, 1.0);
        harness.step();
    }
    assert!(
        (recipe(&harness).basic.exposure - 0.2).abs() < 1e-3,
        "{}",
        recipe(&harness).basic.exposure
    );
    assert_eq!(steps(&harness), 1, "a burst waits to see if more follows");
    support::let_settle(&mut harness);
    assert_eq!(steps(&harness), 2);
}

#[test]
fn a_trackpad_scrolls_the_panel_instead_of_changing_values() {
    let mut harness = editing_app();
    let track = slider_rect(&harness, "Exposure");
    harness.hover_at(track.center());
    harness.step();
    wheel(&harness, MouseWheelUnit::Point, 30.0);
    harness.step();
    assert_eq!(recipe(&harness).basic.exposure, 0.0);
}

#[test]
fn clicking_the_value_lets_you_type_one() {
    let mut harness = editing_app();
    let value = harness.get_by_label("Exposure value").rect();
    harness.hover_at(value.center());
    harness.step();
    harness.get_by_label("Exposure value").click();
    harness.step();
    harness.step();
    harness.step();
    harness
        .get_by_role(eframe::egui::accesskit::Role::TextInput)
        .type_text("1.25");
    harness.step();
    harness.key_press(Key::Enter);
    harness.step();
    harness.step();
    assert_eq!(recipe(&harness).basic.exposure, 1.25);
    assert_eq!(steps(&harness), 2);
}

#[test]
fn typing_in_a_value_doesnt_trigger_shortcuts() {
    let mut harness = editing_app();
    harness.get_by_label("Exposure value").click();
    harness.step();
    harness.step();
    harness.step();
    harness.key_press(Key::P);
    harness.step();
    assert_eq!(support::row(&harness, 0).flag, Flag::Unflagged);
}

#[test]
fn lightroom_keys_reach_their_actions() {
    let mut harness = editing_app();
    let press = |harness: &mut egui_kittest::Harness<'static, unlight_app::app::App>, key| {
        harness.key_press(key);
        harness.step();
    };

    press(&mut harness, Key::J);
    assert!(harness.state().state.clipping);
    press(&mut harness, Key::P);
    assert_eq!(support::row(&harness, 0).flag, Flag::Pick);
    press(&mut harness, Key::X);
    assert_eq!(support::row(&harness, 0).flag, Flag::Reject);
    press(&mut harness, Key::U);
    assert_eq!(support::row(&harness, 0).flag, Flag::Unflagged);
    press(&mut harness, Key::Num3);
    assert_eq!(support::row(&harness, 0).rating, 3);
    press(&mut harness, Key::R);
    assert_eq!(harness.state().state.tool, Tool::Crop);
    press(&mut harness, Key::Escape);
    assert_eq!(harness.state().state.tool, Tool::None);
    press(&mut harness, Key::Backslash);
    assert_eq!(harness.state().state.compare, Compare::Split);
    press(&mut harness, Key::Backslash);
    press(&mut harness, Key::Z);
    assert_eq!(harness.state().state.view.zoom, Zoom::Ratio(1.0));
    press(&mut harness, Key::Z);
    press(&mut harness, Key::L);
    assert_eq!(harness.state().state.lights, Lights::Dim);
    press(&mut harness, Key::G);
    assert_eq!(harness.state().state.module, Module::Library);
    press(&mut harness, Key::D);
    assert_eq!(harness.state().state.module, Module::Develop);
    press(&mut harness, Key::ArrowRight);
    assert_eq!(harness.state().state.library.focus(), Some(1));
    press(&mut harness, Key::ArrowLeft);
    assert_eq!(harness.state().state.library.focus(), Some(0));
}

#[test]
fn tab_hides_the_side_panels_and_keys_keep_working_after() {
    let mut harness = editing_app();
    harness.key_press(Key::Tab);
    harness.step();
    harness.step();
    assert!(!harness.state().state.panels.left);
    harness.key_press(Key::Tab);
    harness.step();
    harness.key_press(Key::P);
    harness.step();
    assert!(harness.state().state.panels.left);
    assert_eq!(support::row(&harness, 0).flag, Flag::Pick);
}

#[test]
fn the_copy_dialog_decides_what_gets_pasted() {
    let mut harness = editing_app();
    harness
        .state_mut()
        .state
        .edit_mut()
        .expect("photo")
        .recipe
        .basic
        .exposure = 1.0;
    harness
        .state_mut()
        .state
        .edit_mut()
        .expect("photo")
        .recipe
        .basic
        .vibrance = 20.0;
    harness.step();
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::C);
    harness.step();
    harness.step();
    assert_eq!(harness.state().state.dialog, Some(Dialog::CopySettings));

    harness.get_by_label("Presence").click();
    harness.step();
    harness.step();
    harness.get_by_label("Copy").click();
    harness.step();
    harness.step();
    assert_eq!(harness.state().state.dialog, None);

    harness.state_mut().state.library.select(1);
    support::settle(&mut harness, |app| app.state.edit().is_some());
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::V);
    harness.step();
    harness.step();
    assert_eq!(recipe(&harness).basic.exposure, 1.0);
    assert_eq!(recipe(&harness).basic.vibrance, 0.0);
}

#[test]
fn a_click_on_the_photo_zooms_there_and_a_drag_pans() {
    let mut harness = editing_app();
    let photo = harness.state().viewport.photo_rect.expect("a photo on screen");
    let point = photo.left_top() + photo.size() * 0.25;
    harness.hover_at(point);
    harness.step();
    harness.drag_at(point);
    harness.step();
    harness.drop_at(point);
    harness.step();
    harness.step();
    let view = harness.state().state.view;
    assert_eq!(view.zoom, Zoom::Ratio(1.0));
    assert!(
        (view.center[0] - 0.25).abs() < 0.02 && (view.center[1] - 0.25).abs() < 0.02,
        "{view:?}"
    );
}

#[test]
fn the_crop_tool_drags_a_corner_and_escape_puts_it_back() {
    let mut harness = editing_app();
    harness.key_press(Key::R);
    harness.step();
    harness.step();
    let photo = harness.state().viewport.photo_rect.expect("a photo on screen");
    drag(
        &mut harness,
        photo.left_top(),
        photo.left_top() + photo.size() * 0.25,
    );
    let crop = recipe(&harness).crop;
    assert!(
        (crop.left - 0.25).abs() < 0.02 && (crop.top - 0.25).abs() < 0.02,
        "{crop:?}"
    );
    assert_eq!(steps(&harness), 2);

    harness.key_press(Key::Escape);
    harness.step();
    harness.step();
    assert_eq!(recipe(&harness).crop, Crop::default());
}

#[test]
fn dragging_outside_the_crop_straightens_without_exposing_corners() {
    let mut harness = editing_app();
    harness.key_press(Key::R);
    harness.step();
    harness.step();
    harness.state_mut().state.edit_mut().expect("photo").recipe.crop = Crop {
        left: 0.25,
        top: 0.25,
        right: 0.75,
        bottom: 0.75,
        angle: 0.0,
    };
    harness.step();
    harness.step();
    let photo = harness.state().viewport.photo_rect.expect("a photo on screen");
    let start = photo.center() + vec2(photo.width() * 0.45, 0.0);
    drag(&mut harness, start, start + vec2(0.0, photo.height() * 0.2));
    assert!(
        recipe(&harness).crop.angle.abs() > 1.0,
        "{:?}",
        recipe(&harness).crop
    );
}

#[test]
fn a_portrait_photo_shows_upright() {
    let mut harness = editing_raw(&synthetic_raw(600, 400, 6, [1.0; 3]), 1);
    harness.step();
    let photo = harness.state().viewport.photo_rect.expect("a photo on screen");
    assert!(photo.height() > photo.width(), "{photo:?}");
}

#[test]
fn the_tone_curve_takes_a_point_and_undoes_in_one_step() {
    let mut harness = editing_app();
    let plot = harness.get_by_label("Point Curve").rect();
    harness.state_mut().state.set_section_open(Section::Basic, false);
    harness.step();
    harness.step();
    let plot_after = harness.get_by_label("Point Curve").rect();
    let on_diagonal = plot_after.left_bottom() + vec2(plot_after.width() * 0.5, -plot_after.height() * 0.5);
    drag(
        &mut harness,
        on_diagonal,
        on_diagonal - vec2(0.0, plot_after.height() * 0.2),
    );
    let points = &recipe(&harness).tone_curve.rgb.0;
    assert_eq!(points.len(), 3, "{points:?} from {plot:?}");
    assert!(points[1].y > 0.65, "{points:?}");
    assert_eq!(steps(&harness), 2);
}

#[test]
fn a_panel_resets_on_its_own() {
    let mut harness = editing_app();
    harness
        .state_mut()
        .state
        .edit_mut()
        .expect("photo")
        .recipe
        .basic
        .exposure = 1.0;
    harness
        .state_mut()
        .state
        .edit_mut()
        .expect("photo")
        .recipe
        .effects
        .grain
        .amount = 30.0;
    harness.step();
    harness.step();
    harness.get_by_label("Reset Basic").click();
    harness.step();
    harness.step();
    assert_eq!(recipe(&harness).basic.exposure, 0.0);
    assert_eq!(recipe(&harness).effects.grain.amount, 30.0);
}

#[test]
fn a_panel_switch_bypasses_it_without_losing_the_edit() {
    let mut harness = editing_app();
    harness
        .state_mut()
        .state
        .edit_mut()
        .expect("photo")
        .recipe
        .tone_curve
        .parametric
        .lights = 30.0;
    harness.step();
    harness.step();
    harness.get_by_label("Enable Tone Curve").click();
    harness.step();
    harness.step();
    assert!(edit(&harness).is_bypassed(Section::ToneCurve));
    assert_eq!(edit(&harness).effective().tone_curve.parametric.lights, 0.0);
    assert_eq!(recipe(&harness).tone_curve.parametric.lights, 30.0);
}

#[test]
fn an_idle_app_stays_idle() {
    let mut harness = editing_app();
    harness.run();
    let renders = harness.state().renderer.renders();
    harness.run();
    harness.step();
    harness.step();
    assert_eq!(
        harness.state().renderer.renders(),
        renders,
        "an unchanged frame reached the GPU"
    );
}

#[test]
fn moving_a_slider_changes_what_is_on_screen() {
    let mut harness = editing_app();
    let photo = harness.state().viewport.photo_rect.expect("a photo on screen");
    let before = harness
        .render()
        .expect("frame")
        .get_pixel(photo.center().x as u32, photo.center().y as u32)
        .0;
    harness
        .state_mut()
        .state
        .edit_mut()
        .expect("photo")
        .recipe
        .basic
        .exposure = 1.0;
    harness.step();
    harness.step();
    let after = harness
        .render()
        .expect("frame")
        .get_pixel(photo.center().x as u32, photo.center().y as u32)
        .0;
    assert!(after[1] > before[1] + 10, "{before:?} -> {after:?}");
}

#[test]
fn the_white_balance_picker_neutralises_the_spot_it_is_given() {
    let mut harness = editing_raw(&synthetic_raw(600, 400, 0, [1.0, 0.8, 0.55]), 1);
    harness.step();
    harness.state_mut().viewport.picking_white_balance = true;
    harness.step();
    let photo = harness.state().viewport.photo_rect.expect("a photo on screen");
    harness.hover_at(photo.center());
    harness.step();
    harness.drag_at(photo.center());
    harness.step();
    harness.drop_at(photo.center());
    harness.step();
    harness.step();
    harness.step();
    assert!(
        matches!(recipe(&harness).white_balance, WhiteBalance::Custom { .. }),
        "{:?}",
        recipe(&harness).white_balance
    );
    let [red, green, blue, _] = harness
        .render()
        .expect("frame")
        .get_pixel(photo.center().x as u32, photo.center().y as u32)
        .0;
    let spread = i32::from(red.max(green).max(blue)) - i32::from(red.min(green).min(blue));
    assert!(spread <= 8, "{red} {green} {blue}");
}

#[test]
fn losing_the_gpu_hands_the_edit_over_for_a_new_window() {
    let raw = synthetic_raw(600, 400, 0, [1.0; 3]);
    let mut harness = editing_raw(&raw, 1);
    harness.step();
    harness
        .state_mut()
        .state
        .edit_mut()
        .expect("photo")
        .recipe
        .basic
        .exposure = 1.5;
    harness.state_mut().context().simulate_device_loss();
    harness.step();
    let handed_over = harness.state().handoff().take_state().expect("state kept");
    assert_eq!(
        handed_over.edit().map(|edit| edit.recipe.basic.exposure),
        Some(1.5)
    );
    assert!(!handed_over.device_lost);
    let store = harness.state().store.clone();
    store.flush();
    let variant = handed_over.focused().expect("a focused row").variant;
    assert_eq!(
        store
            .catalog
            .variant(variant)
            .expect("variant")
            .recipe
            .basic
            .exposure,
        1.5
    );
}

#[test]
fn the_panel_order_matches_lightroom() {
    let harness = editing_app();
    let tops: Vec<f32> = Section::ALL
        .iter()
        .map(|section| harness.get_by_label(section.title()).rect().top())
        .collect();
    assert!(tops.windows(2).all(|pair| pair[0] < pair[1]), "{tops:?}");
}

#[test]
fn escape_cancels_the_picker_before_anything_else() {
    let mut harness = editing_app();
    harness.state_mut().state.apply(Action::ToggleTool(Tool::Crop));
    harness.state_mut().state.apply(Action::Confirm);
    harness.state_mut().viewport.picking_white_balance = true;
    harness.key_press(Key::Escape);
    harness.step();
    assert!(!harness.state().viewport.picking_white_balance);
}

#[test]
fn a_zoomed_render_refines_without_blocking_and_ends_up_like_a_full_one() {
    let raw = synthetic_raw(1600, 1200, 0, [1.0; 3]);
    let context = GpuContext::new().expect("no Vulkan device for the app tests");
    let engine = Engine::new(context, Precision::F16).expect("engine");
    let recipe = Recipe::default();
    let mut session = Session::open(engine, &raw, &recipe).expect("session");
    let request = ViewRequest {
        region: [0.25, 0.25, 0.5, 0.5],
        ..ViewRequest::default()
    };
    let target = session.view_target(800, 600);

    assert!(
        !session
            .render_progressive(&recipe, &request, &target, 0)
            .expect("render")
    );
    let mut frames = 0;
    while !session
        .render_progressive(&recipe, &request, &target, 1)
        .expect("render")
    {
        frames += 1;
        assert!(frames < 64, "never finished refining");
    }
    assert!(frames > 0, "one tile should not have been enough");
    let progressive = session.read_view(&target).expect("pixels");

    session.render(&recipe, &request, &target, None).expect("render");
    assert_eq!(progressive, session.read_view(&target).expect("pixels"));
}

#[test]
fn a_file_that_wont_decode_leaves_the_app_running_with_a_message() {
    let folder = env::temp_dir().join(format!("unlight-broken-{}", std::process::id()));
    fs::create_dir_all(&folder).expect("temp folder");
    let path = folder.join("broken.arw");
    fs::write(&path, b"this is not a raw file").expect("broken file");
    let mut harness = support::app(&[path], WINDOW, 1.0);
    support::settle(&mut harness, |app| {
        matches!(app.state.decode_state(), DecodeState::Failed(_))
    });
    harness.get_by_label("Exposure");
    fs::remove_dir_all(&folder).expect("removing temp folder");
}

#[test]
fn the_split_divider_drags() {
    let mut harness = editing_app();
    harness.key_press(Key::Backslash);
    harness.step();
    harness.step();
    let photo = harness.state().viewport.photo_rect.expect("a photo on screen");
    let divider = photo.center();
    drag(&mut harness, divider, divider - vec2(photo.width() * 0.25, 0.0));
    assert!(
        (harness.state().state.split - 0.25).abs() < 0.03,
        "{}",
        harness.state().state.split
    );
}

#[test]
fn the_render_reaches_the_screen_unchanged() {
    let mut harness = editing_app();
    let photo = harness.state().viewport.photo_rect.expect("a photo on screen");
    let (x, y) = (photo.center().x as u32, photo.center().y as u32);
    let on_screen = harness.render().expect("frame").get_pixel(x, y).0;

    let raw = synthetic_raw(600, 400, 0, [1.0; 3]);
    let context = GpuContext::new().expect("no Vulkan device for the app tests");
    let engine = Engine::new(context, Precision::F16).expect("engine");
    let mut session = Session::open(engine, &raw, &Recipe::default()).expect("session");
    let width = photo.width() as u32;
    let height = photo.height() as u32;
    let target = session.view_target(width, height);
    session
        .render(&Recipe::default(), &ViewRequest::default(), &target, None)
        .expect("render");
    let pixels = session.read_view(&target).expect("pixels");
    let index = ((height / 2 * width + width / 2) * 4) as usize;
    for channel in 0..3 {
        let difference = i32::from(on_screen[channel]) - i32::from(pixels[index + channel]);
        assert!(
            difference.abs() <= 3,
            "{on_screen:?} vs {:?}",
            &pixels[index..index + 4]
        );
    }
}
