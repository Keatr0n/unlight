//! The Masks, Healing and Red Eye tools, driven like a user would on the real engine.

mod support;

use eframe::egui::{Key, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::kittest::Queryable;
use support::{drag, edit, editing_app, recipe, steps};
use unlight_app::{
    action::Action,
    state::{Tool, local::MaskKind},
};
use unlight_recipe::{HealMode, MaskShape};

type Harness = egui_kittest::Harness<'static, unlight_app::app::App>;

fn open_tool(harness: &mut Harness, action: Action) {
    harness.state_mut().state.apply(action);
    harness.step();
    harness.step();
}

/// A point on the photo, as a share of how far across and down it is.
fn on_photo(harness: &Harness, x: f32, y: f32) -> Pos2 {
    let rect = harness.state().viewport.photo_rect.expect("photo on screen");
    pos2(rect.left() + rect.width() * x, rect.top() + rect.height() * y)
}

fn start_mask(harness: &mut Harness, kind: MaskKind) {
    open_tool(harness, Action::ToggleTool(Tool::Masks));
    harness.state_mut().state.add_mask(kind);
    harness.step();
    harness.step();
}

fn dab_count(harness: &Harness) -> usize {
    match &recipe(harness).masks[0].components[0].shape {
        MaskShape::Brush { dabs } => dabs.len(),
        _ => 0,
    }
}

fn click(harness: &mut Harness, at: Pos2, button: PointerButton) {
    harness.hover_at(at);
    harness.step();
    for pressed in [true, false] {
        harness.event(eframe::egui::Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE });
        harness.step();
    }
    harness.step();
}

#[test]
fn the_panel_starts_a_mask_of_the_kind_chosen() {
    let mut harness = editing_app();
    open_tool(&mut harness, Action::ToggleTool(Tool::Masks));
    harness.get_by_label("Linear").click();
    harness.step();
    harness.step();
    let masks = &recipe(&harness).masks;
    assert_eq!(masks.len(), 1);
    assert!(matches!(masks[0].components[0].shape, MaskShape::Linear { .. }));
    assert_eq!(edit(&harness).history().current_label(), "Add Mask  Linear");
}

#[test]
fn add_and_subtract_extend_the_selected_mask() {
    let mut harness = editing_app();
    start_mask(&mut harness, MaskKind::Linear);
    harness.get_by_label("Subtract").click();
    harness.step();
    harness.step();
    harness.get_by_label("Radial").click();
    harness.step();
    harness.step();
    let masks = &recipe(&harness).masks;
    assert_eq!(masks.len(), 1);
    assert_eq!(masks[0].components.len(), 2);
    assert!(masks[0].components[1].subtract);
}

#[test]
fn a_brush_stroke_paints_as_one_undo_step() {
    let mut harness = editing_app();
    start_mask(&mut harness, MaskKind::Brush);
    let before = steps(&harness);
    let (from, to) = (on_photo(&harness, 0.2, 0.5), on_photo(&harness, 0.8, 0.5));
    drag(&mut harness, from, to);
    assert!(dab_count(&harness) > 10, "{}", dab_count(&harness));
    assert_eq!(steps(&harness), before + 1);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.step();
    assert_eq!(dab_count(&harness), 0);
}

#[test]
fn a_brush_stroke_lands_where_the_pointer_went() {
    let mut harness = editing_app();
    start_mask(&mut harness, MaskKind::Brush);
    let (from, to) = (on_photo(&harness, 0.25, 0.3), on_photo(&harness, 0.75, 0.3));
    drag(&mut harness, from, to);
    let MaskShape::Brush { dabs } = &recipe(&harness).masks[0].components[0].shape else {
        panic!("not a brush");
    };
    let first = dabs.first().expect("a dab").position;
    assert!((first[0] - 0.25).abs() < 0.02 && (first[1] - 0.3).abs() < 0.02, "{first:?}");
    assert!(dabs.iter().all(|dab| (dab.position[1] - 0.3).abs() < 0.02));
}

#[test]
fn shift_paints_a_straight_line() {
    let mut harness = editing_app();
    start_mask(&mut harness, MaskKind::Brush);
    let (from, middle, to) =
        (on_photo(&harness, 0.2, 0.5), on_photo(&harness, 0.5, 0.2), on_photo(&harness, 0.8, 0.5));
    let press = |pos: Pos2, pressed: bool| eframe::egui::Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::SHIFT,
    };
    harness.hover_at(from);
    harness.step();
    harness.event_modifiers(press(from, true), Modifiers::SHIFT);
    harness.step();
    for point in [middle, to] {
        harness.event_modifiers(eframe::egui::Event::PointerMoved(point), Modifiers::SHIFT);
        harness.step();
    }
    harness.event_modifiers(press(to, false), Modifiers::SHIFT);
    harness.step();
    harness.step();
    let MaskShape::Brush { dabs } = &recipe(&harness).masks[0].components[0].shape else {
        panic!("not a brush");
    };
    assert!(dabs.len() > 10);
    assert!(dabs.iter().all(|dab| (dab.position[1] - 0.5).abs() < 0.02), "the swerve went into the stroke");
}

#[test]
fn brackets_resize_the_brush() {
    let mut harness = editing_app();
    start_mask(&mut harness, MaskKind::Brush);
    let start = harness.state().state.local.brush.radius;
    harness.key_press(Key::CloseBracket);
    harness.step();
    let larger = harness.state().state.local.brush.radius;
    assert!(larger > start);
    harness.key_press(Key::OpenBracket);
    harness.step();
    assert!((harness.state().state.local.brush.radius - start).abs() < 1e-5);
}

#[test]
fn o_shows_the_selected_mask() {
    let mut harness = editing_app();
    start_mask(&mut harness, MaskKind::Radial);
    assert_eq!(harness.state().state.overlay_mask(), None);
    harness.key_press(Key::O);
    harness.step();
    assert_eq!(harness.state().state.overlay_mask(), Some(0));
    open_tool(&mut harness, Action::ToggleTool(Tool::Masks));
    assert_eq!(harness.state().state.overlay_mask(), None, "only while the tool is open");
}

#[test]
fn dragging_a_gradient_end_moves_only_that_end() {
    let mut harness = editing_app();
    start_mask(&mut harness, MaskKind::Linear);
    let MaskShape::Linear { start, end } = recipe(&harness).masks[0].components[0].shape else {
        panic!("not a gradient");
    };
    let grab = on_photo(&harness, start[0], start[1]);
    drag(&mut harness, grab, grab + vec2(60.0, 0.0));
    let MaskShape::Linear { start: moved, end: kept } = recipe(&harness).masks[0].components[0].shape else {
        panic!("not a gradient");
    };
    assert!(moved[0] > start[0] + 0.02, "{start:?} -> {moved:?}");
    assert_eq!(kept, end);
}

#[test]
fn a_radial_mask_turns_by_its_handle() {
    let mut harness = editing_app();
    start_mask(&mut harness, MaskKind::Radial);
    let MaskShape::Radial { center, radii, .. } = recipe(&harness).masks[0].components[0].shape else {
        panic!("not a radial");
    };
    let image = harness.state().viewport.photo_rect.expect("photo");
    let handle = on_photo(&harness, center[0] + radii[0] * 1.25, center[1]);
    drag(&mut harness, handle, handle + vec2(0.0, image.height() * 0.15));
    let MaskShape::Radial { angle, .. } = recipe(&harness).masks[0].components[0].shape else {
        panic!("not a radial");
    };
    assert!(angle > 10.0, "{angle}");
}

#[test]
fn delete_removes_the_selected_mask() {
    let mut harness = editing_app();
    start_mask(&mut harness, MaskKind::Radial);
    harness.key_press(Key::Delete);
    harness.step();
    assert!(recipe(&harness).masks.is_empty());
    assert_eq!(harness.state().state.current_mask_index(), None);
}

#[test]
fn duplicate_and_invert_work_from_the_panel() {
    let mut harness = editing_app();
    start_mask(&mut harness, MaskKind::Radial);
    harness.get_by_label("Copy").click();
    harness.step();
    harness.step();
    assert_eq!(recipe(&harness).masks.len(), 2);
    assert_eq!(harness.state().state.current_mask_index(), Some(1));
    harness.get_by_label("Invert").click();
    harness.step();
    harness.step();
    let inverted = |index: usize, harness: &Harness| {
        matches!(recipe(harness).masks[index].components[0].shape, MaskShape::Radial { inverted: true, .. })
    };
    assert!(inverted(1, &harness));
    assert!(!inverted(0, &harness));
}

#[test]
fn undoing_past_a_mask_leaves_no_stale_selection() {
    let mut harness = editing_app();
    start_mask(&mut harness, MaskKind::Radial);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.step();
    harness.step();
    assert_eq!(harness.state().state.current_mask_index(), None);
}

#[test]
fn a_click_heals_with_a_patch_found_nearby() {
    let mut harness = editing_app();
    open_tool(&mut harness, Action::ToggleTool(Tool::Heal));
    let at = on_photo(&harness, 0.4, 0.5);
    click(&mut harness, at, PointerButton::Primary);
    let spots = &recipe(&harness).healing;
    assert_eq!(spots.len(), 1);
    assert!((spots[0].target[0] - 0.4).abs() < 0.01);
    assert_ne!(spots[0].source, spots[0].target);
    assert_eq!(spots[0].mode, HealMode::Heal);
    assert_eq!(edit(&harness).history().current_label(), "Heal");
}

#[test]
fn a_spot_moves_by_either_circle_and_right_click_removes_it() {
    let mut harness = editing_app();
    open_tool(&mut harness, Action::ToggleTool(Tool::Heal));
    let at = on_photo(&harness, 0.4, 0.5);
    click(&mut harness, at, PointerButton::Primary);
    let spot = recipe(&harness).healing[0];

    drag(&mut harness, at, at + vec2(0.0, 40.0));
    let moved = recipe(&harness).healing[0];
    assert!(moved.target[1] > spot.target[1] + 0.02);
    assert_eq!(moved.source, spot.source);

    let source = on_photo(&harness, moved.source[0], moved.source[1]);
    drag(&mut harness, source, source + vec2(30.0, 0.0));
    let moved_again = recipe(&harness).healing[0];
    assert!(moved_again.source[0] > moved.source[0] + 0.01);
    assert_eq!(moved_again.target, moved.target);

    let target = on_photo(&harness, moved_again.target[0], moved_again.target[1]);
    click(&mut harness, target, PointerButton::Secondary);
    assert!(recipe(&harness).healing.is_empty());
}

#[test]
fn clone_mode_applies_to_the_next_spot() {
    let mut harness = editing_app();
    open_tool(&mut harness, Action::ToggleTool(Tool::Heal));
    harness.get_by_label("Clone").click();
    harness.step();
    harness.step();
    let at = on_photo(&harness, 0.6, 0.4);
    click(&mut harness, at, PointerButton::Primary);
    assert_eq!(recipe(&harness).healing[0].mode, HealMode::Clone);
}

#[test]
fn brackets_resize_the_selected_spot() {
    let mut harness = editing_app();
    open_tool(&mut harness, Action::ToggleTool(Tool::Heal));
    let at = on_photo(&harness, 0.4, 0.5);
    click(&mut harness, at, PointerButton::Primary);
    let before = recipe(&harness).healing[0].radius;
    harness.key_press(Key::CloseBracket);
    harness.step();
    assert!(recipe(&harness).healing[0].radius > before);
}

#[test]
fn a_click_adds_a_red_eye_and_delete_takes_it_away() {
    let mut harness = editing_app();
    open_tool(&mut harness, Action::ToggleTool(Tool::RedEye));
    let at = on_photo(&harness, 0.5, 0.5);
    click(&mut harness, at, PointerButton::Primary);
    assert_eq!(recipe(&harness).red_eye.len(), 1);
    harness.key_press(Key::Delete);
    harness.step();
    assert!(recipe(&harness).red_eye.is_empty());
}

#[test]
fn escape_leaves_the_tool_and_keeps_the_work() {
    let mut harness = editing_app();
    open_tool(&mut harness, Action::ToggleTool(Tool::Heal));
    let at = on_photo(&harness, 0.4, 0.5);
    click(&mut harness, at, PointerButton::Primary);
    harness.key_press(Key::Escape);
    harness.step();
    assert_eq!(harness.state().state.tool, Tool::None);
    assert_eq!(recipe(&harness).healing.len(), 1);
}

#[test]
fn delete_does_nothing_outside_the_tools() {
    let mut harness = editing_app();
    start_mask(&mut harness, MaskKind::Radial);
    open_tool(&mut harness, Action::ToggleTool(Tool::Masks));
    harness.key_press(Key::Delete);
    harness.step();
    assert_eq!(recipe(&harness).masks.len(), 1);
}
