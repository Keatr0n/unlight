//! The panels of the Masks, Healing and Red Eye tools.

use crate::{
    action::{Action, tip},
    state::{
        AppState,
        edit::{PhotoEdit, Step},
        local::{BrushSettings, MaskAction, MaskKind},
    },
    ui::{
        theme,
        widgets::{
            controls::{check_row, note, segmented, subheading, text_button},
            slider::{Slider, Spec},
        },
    },
};
use eframe::egui::{Align2, Role, Sense, Ui, WidgetInfo, vec2};
use unlight_recipe::{HealMode, LocalAdjustments, MaskComponent, MaskShape, Recipe};

const EXPOSURE: Spec = Spec::bipolar("Exposure", 4.0)
    .decimals(2, 0.05)
    .named("Mask Exposure");
const TEMPERATURE: Spec = Spec::bipolar("Temp", 100.0).named("Mask Temperature");
const TINT: Spec = Spec::bipolar("Tint", 100.0).named("Mask Tint");
const CONTRAST: Spec = Spec::bipolar("Contrast", 100.0).named("Mask Contrast");
const HIGHLIGHTS: Spec = Spec::bipolar("Highlights", 100.0).named("Mask Highlights");
const SHADOWS: Spec = Spec::bipolar("Shadows", 100.0).named("Mask Shadows");
const WHITES: Spec = Spec::bipolar("Whites", 100.0).named("Mask Whites");
const BLACKS: Spec = Spec::bipolar("Blacks", 100.0).named("Mask Blacks");
const TEXTURE: Spec = Spec::bipolar("Texture", 100.0).named("Mask Texture");
const CLARITY: Spec = Spec::bipolar("Clarity", 100.0).named("Mask Clarity");
const DEHAZE: Spec = Spec::bipolar("Dehaze", 100.0).named("Mask Dehaze");
const HUE: Spec = Spec::bipolar("Hue", 100.0).named("Mask Hue");
const SATURATION: Spec = Spec::bipolar("Saturation", 100.0).named("Mask Saturation");
const SHARPNESS: Spec = Spec::bipolar("Sharpness", 100.0).named("Mask Sharpness");
const NOISE: Spec = Spec::bipolar("Noise", 100.0).named("Mask Noise");

const SIZE: Spec = Spec::amount("Size", 100.0).decimals(1, 0.5).named("Brush Size");
const FEATHER: Spec = Spec::amount("Feather", 100.0).named("Brush Feather");
const FLOW: Spec = Spec::amount("Flow", 100.0).named("Brush Flow");
const RADIAL_FEATHER: Spec = Spec::amount("Feather", 100.0).named("Mask Feather");
const LOW: Spec = Spec::amount("Low", 100.0).named("Range Low");
const HIGH: Spec = Spec::amount("High", 100.0).named("Range High");
const SMOOTHNESS: Spec = Spec::amount("Smoothness", 100.0).named("Range Smoothness");
const TOLERANCE: Spec = Spec::amount("Tolerance", 100.0).named("Colour Tolerance");

const SPOT_SIZE: Spec = Spec::amount("Size", 100.0).decimals(1, 0.2).named("Spot Size");
const SPOT_FEATHER: Spec = Spec::amount("Feather", 100.0).named("Spot Feather");
const SPOT_OPACITY: Spec = Spec::amount("Opacity", 100.0).named("Spot Opacity");
const PUPIL: Spec = Spec::amount("Pupil Size", 100.0).named("Pupil Size");
const DARKEN: Spec = Spec::amount("Darken", 100.0).named("Red Eye Darken");

/// Size sliders reach 100 at the largest radius, a fifth of the long edge.
const SIZE_SCALE: f32 = 500.0;

type Adjustment = fn(&mut LocalAdjustments) -> &mut f32;

const ADJUSTMENTS: [(&Spec, Adjustment); 15] = [
    (&TEMPERATURE, |a| &mut a.temperature),
    (&TINT, |a| &mut a.tint),
    (&EXPOSURE, |a| &mut a.exposure),
    (&CONTRAST, |a| &mut a.contrast),
    (&HIGHLIGHTS, |a| &mut a.highlights),
    (&SHADOWS, |a| &mut a.shadows),
    (&WHITES, |a| &mut a.whites),
    (&BLACKS, |a| &mut a.blacks),
    (&TEXTURE, |a| &mut a.texture),
    (&CLARITY, |a| &mut a.clarity),
    (&DEHAZE, |a| &mut a.dehaze),
    (&HUE, |a| &mut a.hue),
    (&SATURATION, |a| &mut a.saturation),
    (&SHARPNESS, |a| &mut a.sharpness),
    (&NOISE, |a| &mut a.noise),
];

/// A 0..=1 setting on a 0..=100 slider. Returns whether it moved, and how.
fn share(ui: &mut Ui, spec: &Spec, value: &mut f32, default: f32) -> Option<Step> {
    let mut percent = *value * 100.0;
    let change = Slider::new(spec, &mut percent, default * 100.0).show(ui).change;
    if change.is_some() {
        *value = percent / 100.0;
    }
    change
}

/// The same, but bound to the recipe, so that moving it is an edit with a history step.
fn recipe_share(
    ui: &mut Ui,
    edit: &mut PhotoEdit,
    spec: &Spec,
    default: f32,
    field: impl Fn(&mut Recipe) -> Option<&mut f32>,
) {
    let Some(value) = field(&mut edit.recipe) else {
        return;
    };
    if let Some(step) = share(ui, spec, value, default) {
        let text = spec.format(*value * 100.0);
        edit.note(spec.name, text, step);
    }
}

fn four_buttons(ui: &mut Ui, labels: [&str; 4]) -> [bool; 4] {
    let gap = theme::GRID;
    let width = (ui.available_width() - gap * 3.0) / 4.0;
    let mut clicked = [false; 4];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (label, hit) in labels.iter().zip(&mut clicked) {
            *hit = text_button(ui, label, false, width).clicked();
        }
    });
    clicked
}

fn kind_buttons(ui: &mut Ui) -> Option<MaskKind> {
    let gap = theme::GRID;
    let count = MaskKind::ALL.len() as f32;
    let width = (ui.available_width() - gap * (count - 1.0)) / count;
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for kind in MaskKind::ALL {
            if text_button(ui, kind.name(), false, width).clicked() {
                chosen = Some(kind);
            }
        }
    });
    chosen
}

fn mask_row(ui: &mut Ui, name: &str, selected: bool, enabled: &mut bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::ListItem, true, selected, name));
    if selected {
        ui.painter().rect_filled(rect, 3.0, theme::PANEL_RAISED);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 3.0, theme::PANEL_HOVER);
    }
    let color = if *enabled { theme::TEXT } else { theme::TEXT_FAINT };
    ui.painter().text(
        rect.left_center() + vec2(6.0, 0.0),
        Align2::LEFT_CENTER,
        name,
        theme::body(),
        color,
    );
    let state = if *enabled { "On" } else { "Off" };
    let toggle = eframe::egui::Rect::from_min_max(rect.right_top() - vec2(36.0, 0.0), rect.right_bottom());
    let id = ui.auto_id_with(("mask on", name));
    let switch = ui.interact(toggle, id, Sense::click());
    switch.widget_info(|| WidgetInfo::labeled(Role::Button, true, format!("{name} {state}")));
    ui.painter().text(
        toggle.center(),
        Align2::CENTER_CENTER,
        state,
        theme::small(),
        theme::TEXT_DIM,
    );
    if switch.clicked() {
        *enabled = !*enabled;
    }
    response.clicked()
}

fn mask_list(ui: &mut Ui, state: &mut AppState) {
    let current = state.current_mask_index();
    let Some(edit) = state.edit_mut() else {
        return;
    };
    let mut picked = None;
    let mut switched = None;
    for (index, mask) in edit.recipe.masks.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            let was = mask.enabled;
            if mask_row(ui, &mask.name, current == Some(index), &mut mask.enabled) {
                picked = Some(index);
            }
            if was != mask.enabled {
                switched = Some(mask.enabled);
            }
        });
    }
    if let Some(enabled) = switched {
        edit.note("Mask", if enabled { "On" } else { "Off" }, Step::New);
    }
    if picked.is_some() {
        state.local.mask = picked;
        state.local.picking_colour = false;
    }
}

fn brush_controls(ui: &mut Ui, brush: &mut BrushSettings) {
    subheading(ui, "Brush");
    let mut size = brush.radius * SIZE_SCALE;
    if Slider::new(&SIZE, &mut size, BrushSettings::default().radius * SIZE_SCALE)
        .tooltip("[ and ] change this")
        .show(ui)
        .change
        .is_some()
    {
        brush.radius = (size / SIZE_SCALE).max(1e-4);
    }
    share(ui, &FEATHER, &mut brush.feather, BrushSettings::default().feather);
    share(ui, &FLOW, &mut brush.flow, BrushSettings::default().flow);
    check_row(ui, &mut brush.auto_mask, "Auto Mask");
    check_row(ui, &mut brush.erase, "Erase  (or hold Alt)");
}

/// The controls of one component of a mask, for the shapes that have any.
fn component_controls(ui: &mut Ui, edit: &mut PhotoEdit, mask: usize, index: usize) {
    let part = |recipe: &mut Recipe| {
        let component: Option<&mut MaskComponent> = recipe.masks.get_mut(mask)?.components.get_mut(index);
        component.map(|component| component.shape.clone())
    };
    match part(&mut edit.recipe) {
        Some(MaskShape::Radial { .. }) => {
            subheading(ui, "Radial");
            recipe_share(ui, edit, &RADIAL_FEATHER, 0.5, move |recipe| {
                match &mut recipe.masks.get_mut(mask)?.components.get_mut(index)?.shape {
                    MaskShape::Radial { feather, .. } => Some(feather),
                    _ => None,
                }
            });
        }
        Some(MaskShape::LuminanceRange { .. }) => {
            subheading(ui, "Luminance Range");
            range_slider(ui, edit, (mask, index), &LOW, 0.5, |low, _, _| low);
            range_slider(ui, edit, (mask, index), &HIGH, 1.0, |_, high, _| high);
            range_slider(ui, edit, (mask, index), &SMOOTHNESS, 0.3, |_, _, smoothness| {
                smoothness
            });
        }
        Some(MaskShape::ColorRange { samples, .. }) => {
            subheading(ui, "Colour Range");
            note(ui, &format!("{} colours sampled", samples.len()));
            recipe_share(ui, edit, &TOLERANCE, 0.5, move |recipe| {
                match &mut recipe.masks.get_mut(mask)?.components.get_mut(index)?.shape {
                    MaskShape::ColorRange { tolerance, .. } => Some(tolerance),
                    _ => None,
                }
            });
        }
        _ => {}
    }
}

fn range_slider(
    ui: &mut Ui,
    edit: &mut PhotoEdit,
    (mask, index): (usize, usize),
    spec: &Spec,
    default: f32,
    pick: for<'a> fn(&'a mut f32, &'a mut f32, &'a mut f32) -> &'a mut f32,
) {
    recipe_share(ui, edit, spec, default, move |recipe| {
        match &mut recipe.masks.get_mut(mask)?.components.get_mut(index)?.shape {
            MaskShape::LuminanceRange {
                low,
                high,
                smoothness,
            } => Some(pick(low, high, smoothness)),
            _ => None,
        }
    });
}

fn adjustments(ui: &mut Ui, edit: &mut PhotoEdit, index: usize) {
    subheading(ui, "Adjustments");
    for (spec, field) in ADJUSTMENTS {
        let Some(mask) = edit.recipe.masks.get_mut(index) else {
            return;
        };
        let value = field(&mut mask.adjustments);
        let change = Slider::new(spec, value, 0.0).show(ui).change;
        let text = spec.format(*value);
        if let Some(step) = change {
            edit.note(spec.name, text, step);
        }
    }
}

pub fn masks(ui: &mut Ui, state: &mut AppState) {
    let actions = [
        (MaskAction::New, "New Mask"),
        (MaskAction::Add, "Add"),
        (MaskAction::Subtract, "Subtract"),
    ];
    segmented(ui, &mut state.local.mask_action, &actions);
    ui.add_space(theme::GRID);
    if let Some(kind) = kind_buttons(ui) {
        state.choose_mask_kind(kind);
    }
    ui.add_space(theme::GRID);
    #[cfg(feature = "ai")]
    {
        let variant = state.edit().map(|edit| edit.variant);
        crate::ui::ai_tools::show(ui, &mut state.ai, variant);
    }
    mask_list(ui, state);
    let Some(index) = state.current_mask_index() else {
        note(ui, "Pick a kind of mask to start one.");
        return;
    };
    ui.add_space(theme::GRID);
    let [invert, copy, delete, overlay] = four_buttons(ui, ["Invert", "Copy", "Delete", "Show"]);
    if invert {
        state.invert_current_mask();
    }
    if copy {
        state.duplicate_mask();
    }
    if delete {
        state.delete_mask();
    }
    if overlay {
        state.apply(Action::ToggleMaskOverlay);
    }
    let Some(index) = state.current_mask_index().filter(|current| *current == index) else {
        return;
    };

    let components = state.current_mask().map_or(0, |mask| mask.components.len());
    let paints = state.current_mask().is_some_and(|mask| {
        mask.components
            .iter()
            .any(|component| matches!(component.shape, MaskShape::Brush { .. }))
    });
    if paints {
        brush_controls(ui, &mut state.local.brush);
    }
    if state.local.picking_colour {
        note(ui, "Click the colour to select on the photo.");
    }
    if let Some(edit) = state.edit_mut() {
        for component in 0..components {
            ui.push_id(component, |ui| component_controls(ui, edit, index, component));
        }
        adjustments(ui, edit, index);
    }
    ui.add_space(theme::GRID);
    note(ui, &tip("Show the mask in red", Action::ToggleMaskOverlay));
}

pub fn heal(ui: &mut Ui, state: &mut AppState) {
    let selected = state.local.spot;
    let mut settings = state.local.heal;
    if segmented(
        ui,
        &mut settings.mode,
        &[(HealMode::Heal, "Heal"), (HealMode::Clone, "Clone")],
    ) && let Some(edit) = state.edit_mut()
        && let Some(spot) = selected.and_then(|index| edit.recipe.healing.get_mut(index))
    {
        spot.mode = settings.mode;
        edit.note("Spot Mode", "", Step::New);
    }
    ui.add_space(theme::GRID);
    let mut size = settings.radius * SIZE_SCALE;
    let size_change = Slider::new(&SPOT_SIZE, &mut size, 6.0)
        .tooltip("[ and ] change this")
        .show(ui)
        .change;
    settings.radius = (size / SIZE_SCALE).max(0.002);
    let feather_change = share(ui, &SPOT_FEATHER, &mut settings.feather, 0.5);
    let opacity_change = share(ui, &SPOT_OPACITY, &mut settings.opacity, 1.0);
    state.local.heal = settings;
    if let Some(step) = size_change.or(feather_change).or(opacity_change)
        && let Some(edit) = state.edit_mut()
        && let Some(spot) = selected.and_then(|index| edit.recipe.healing.get_mut(index))
    {
        spot.radius = settings.radius;
        spot.feather = settings.feather;
        spot.opacity = settings.opacity;
        edit.note("Spot", "", step);
    }
    ui.add_space(theme::GRID);
    note(
        ui,
        "Click a spot to heal it. Drag either circle to move it. Right-click removes it.",
    );
}

pub fn red_eye(ui: &mut Ui, state: &mut AppState) {
    let selected = state.local.eye;
    let mut settings = state.local.eyes;
    let pupil_change = share(ui, &PUPIL, &mut settings.pupil_size, 0.8);
    let darken_change = share(ui, &DARKEN, &mut settings.darken, 0.5);
    state.local.eyes = settings;
    if let Some(step) = pupil_change.or(darken_change)
        && let Some(edit) = state.edit_mut()
        && let Some(eye) = selected.and_then(|index| edit.recipe.red_eye.get_mut(index))
    {
        eye.pupil_size = settings.pupil_size;
        eye.darken = settings.darken;
        edit.note("Red Eye", "", step);
    }
    ui.add_space(theme::GRID);
    note(
        ui,
        "Click an eye. Drag to move it, right-click to remove it, [ and ] to resize.",
    );
}
