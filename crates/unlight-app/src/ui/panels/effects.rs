use super::{param, param_gradient};
use crate::{
    state::edit::PhotoEdit,
    ui::widgets::{controls::subheading, slider::Spec},
};
use eframe::egui::{Color32, Ui};

const VIGNETTE_AMOUNT: Spec = Spec::bipolar("Amount", 100.0).named("Vignette Amount");
const MIDPOINT: Spec = Spec::amount("Midpoint", 100.0).named("Vignette Midpoint");
const ROUNDNESS: Spec = Spec::bipolar("Roundness", 100.0).named("Vignette Roundness");
const FEATHER: Spec = Spec::amount("Feather", 100.0).named("Vignette Feather");
const HIGHLIGHTS: Spec = Spec::amount("Highlights", 100.0).named("Vignette Highlights");
const GRAIN_AMOUNT: Spec = Spec::amount("Amount", 100.0).named("Grain Amount");
const GRAIN_SIZE: Spec = Spec::amount("Size", 100.0).named("Grain Size");
const ROUGHNESS: Spec = Spec::amount("Roughness", 100.0).named("Grain Roughness");

const SHADOW_TINT: Spec = Spec::bipolar("Tint", 100.0).named("Shadow Tint");
const RED_HUE: Spec = Spec::bipolar("Hue", 100.0).named("Red Primary Hue");
const RED_SATURATION: Spec = Spec::bipolar("Saturation", 100.0).named("Red Primary Saturation");
const GREEN_HUE: Spec = Spec::bipolar("Hue", 100.0).named("Green Primary Hue");
const GREEN_SATURATION: Spec = Spec::bipolar("Saturation", 100.0).named("Green Primary Saturation");
const BLUE_HUE: Spec = Spec::bipolar("Hue", 100.0).named("Blue Primary Hue");
const BLUE_SATURATION: Spec = Spec::bipolar("Saturation", 100.0).named("Blue Primary Saturation");

const SHADOW_TINT_TRACK: [Color32; 3] = [
    Color32::from_rgb(0x4f, 0xa3, 0x5a),
    Color32::from_rgb(0x90, 0x90, 0x90),
    Color32::from_rgb(0xc2, 0x55, 0xb5),
];

pub fn show(ui: &mut Ui, edit: &mut PhotoEdit) {
    subheading(ui, "Post-Crop Vignetting");
    param(ui, edit, &VIGNETTE_AMOUNT, |recipe| {
        &mut recipe.effects.vignette.amount
    });
    param(ui, edit, &MIDPOINT, |recipe| {
        &mut recipe.effects.vignette.midpoint
    });
    param(ui, edit, &ROUNDNESS, |recipe| {
        &mut recipe.effects.vignette.roundness
    });
    param(ui, edit, &FEATHER, |recipe| &mut recipe.effects.vignette.feather);
    param(ui, edit, &HIGHLIGHTS, |recipe| {
        &mut recipe.effects.vignette.highlights
    });
    subheading(ui, "Grain");
    param(ui, edit, &GRAIN_AMOUNT, |recipe| &mut recipe.effects.grain.amount);
    param(ui, edit, &GRAIN_SIZE, |recipe| &mut recipe.effects.grain.size);
    param(ui, edit, &ROUGHNESS, |recipe| &mut recipe.effects.grain.roughness);
}

pub fn show_calibration(ui: &mut Ui, edit: &mut PhotoEdit) {
    subheading(ui, "Shadows");
    param_gradient(ui, edit, &SHADOW_TINT, &SHADOW_TINT_TRACK, |recipe| {
        &mut recipe.calibration.shadow_tint
    });
    subheading(ui, "Red Primary");
    param(ui, edit, &RED_HUE, |recipe| &mut recipe.calibration.red_hue);
    param(ui, edit, &RED_SATURATION, |recipe| {
        &mut recipe.calibration.red_saturation
    });
    subheading(ui, "Green Primary");
    param(ui, edit, &GREEN_HUE, |recipe| &mut recipe.calibration.green_hue);
    param(ui, edit, &GREEN_SATURATION, |recipe| {
        &mut recipe.calibration.green_saturation
    });
    subheading(ui, "Blue Primary");
    param(ui, edit, &BLUE_HUE, |recipe| &mut recipe.calibration.blue_hue);
    param(ui, edit, &BLUE_SATURATION, |recipe| {
        &mut recipe.calibration.blue_saturation
    });
}
