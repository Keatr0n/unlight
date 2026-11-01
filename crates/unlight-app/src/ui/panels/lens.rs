use super::{PanelContext, param};
use crate::{
    state::edit::{PhotoEdit, Step},
    ui::widgets::{
        controls::{check_row, note, subheading},
        slider::Spec,
    },
};
use eframe::egui::Ui;

const PROFILE_DISTORTION: Spec = Spec::amount("Distortion", 200.0).named("Profile Distortion");
const PROFILE_VIGNETTE: Spec = Spec::amount("Vignetting", 200.0).named("Profile Vignetting");
const PURPLE_AMOUNT: Spec = Spec::amount("Purple", 20.0)
    .named("Purple Amount")
    .decimals(0, 1.0);
const PURPLE_LOW: Spec = Spec::amount("Hue Low", 100.0).named("Purple Hue Low");
const PURPLE_HIGH: Spec = Spec::amount("Hue High", 100.0).named("Purple Hue High");
const GREEN_AMOUNT: Spec = Spec::amount("Green", 20.0).named("Green Amount").decimals(0, 1.0);
const GREEN_LOW: Spec = Spec::amount("Hue Low", 100.0).named("Green Hue Low");
const GREEN_HIGH: Spec = Spec::amount("Hue High", 100.0).named("Green Hue High");
const MANUAL_DISTORTION: Spec = Spec::bipolar("Distortion", 100.0).named("Manual Distortion");
const MANUAL_VIGNETTE: Spec = Spec::bipolar("Vignetting", 100.0).named("Manual Vignetting");
const MIDPOINT: Spec = Spec::amount("Midpoint", 100.0).named("Vignetting Midpoint");

fn toggle(ui: &mut Ui, edit: &mut PhotoEdit, label: &str, value: bool) -> Option<bool> {
    let mut checked = value;
    let changed = check_row(ui, &mut checked, label).clicked();
    if changed {
        edit.note(label, if checked { "On" } else { "Off" }, Step::New);
    }
    changed.then_some(checked)
}

pub fn show(ui: &mut Ui, edit: &mut PhotoEdit, context: &PanelContext<'_>) {
    subheading(ui, "Profile");
    if let Some(on) = toggle(
        ui,
        edit,
        "Enable Profile Corrections",
        edit.recipe.lens.profile_enabled,
    ) {
        edit.recipe.lens.profile_enabled = on;
    }
    if let Some(status) = &context.lens_status {
        note(ui, status);
    }
    param(ui, edit, &PROFILE_DISTORTION, |recipe| {
        &mut recipe.lens.profile_distortion
    });
    param(ui, edit, &PROFILE_VIGNETTE, |recipe| {
        &mut recipe.lens.profile_vignette
    });

    subheading(ui, "Defringe");
    if let Some(on) = toggle(
        ui,
        edit,
        "Remove Chromatic Aberration",
        edit.recipe.lens.remove_chromatic_aberration,
    ) {
        edit.recipe.lens.remove_chromatic_aberration = on;
    }
    param(ui, edit, &PURPLE_AMOUNT, |recipe| {
        &mut recipe.lens.defringe.purple_amount
    });
    param(ui, edit, &PURPLE_LOW, |recipe| {
        &mut recipe.lens.defringe.purple_hue_low
    });
    param(ui, edit, &PURPLE_HIGH, |recipe| {
        &mut recipe.lens.defringe.purple_hue_high
    });
    param(ui, edit, &GREEN_AMOUNT, |recipe| {
        &mut recipe.lens.defringe.green_amount
    });
    param(ui, edit, &GREEN_LOW, |recipe| {
        &mut recipe.lens.defringe.green_hue_low
    });
    param(ui, edit, &GREEN_HIGH, |recipe| {
        &mut recipe.lens.defringe.green_hue_high
    });

    subheading(ui, "Manual");
    param(ui, edit, &MANUAL_DISTORTION, |recipe| {
        &mut recipe.lens.manual_distortion
    });
    param(ui, edit, &MANUAL_VIGNETTE, |recipe| {
        &mut recipe.lens.manual_vignette
    });
    param(ui, edit, &MIDPOINT, |recipe| {
        &mut recipe.lens.manual_vignette_midpoint
    });
}
