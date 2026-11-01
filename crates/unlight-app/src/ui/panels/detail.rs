use super::param;
use crate::{
    state::edit::PhotoEdit,
    ui::widgets::{controls::subheading, slider::Spec},
};
use eframe::egui::Ui;

const AMOUNT: Spec = Spec::amount("Amount", 150.0).named("Sharpening Amount");
const RADIUS: Spec = Spec::range("Radius", 0.5, 3.0)
    .named("Sharpening Radius")
    .decimals(1, 0.1);
const DETAIL: Spec = Spec::amount("Detail", 100.0).named("Sharpening Detail");
const MASKING: Spec = Spec::amount("Masking", 100.0).named("Sharpening Masking");
const LUMINANCE: Spec = Spec::amount("Luminance", 100.0).named("Luminance Noise");
const LUMINANCE_DETAIL: Spec = Spec::amount("Detail", 100.0).named("Luminance Detail");
const LUMINANCE_CONTRAST: Spec = Spec::amount("Contrast", 100.0).named("Luminance Contrast");
const COLOR: Spec = Spec::amount("Color", 100.0).named("Color Noise");
const COLOR_DETAIL: Spec = Spec::amount("Detail", 100.0).named("Color Detail");
const SMOOTHNESS: Spec = Spec::amount("Smoothness", 100.0).named("Color Smoothness");

pub fn show(ui: &mut Ui, edit: &mut PhotoEdit) {
    subheading(ui, "Sharpening");
    param(ui, edit, &AMOUNT, |recipe| &mut recipe.detail.sharpening.amount);
    param(ui, edit, &RADIUS, |recipe| &mut recipe.detail.sharpening.radius);
    param(ui, edit, &DETAIL, |recipe| &mut recipe.detail.sharpening.detail);
    param(ui, edit, &MASKING, |recipe| &mut recipe.detail.sharpening.masking);
    subheading(ui, "Noise Reduction");
    param(ui, edit, &LUMINANCE, |recipe| {
        &mut recipe.detail.noise_reduction.luminance
    });
    param(ui, edit, &LUMINANCE_DETAIL, |recipe| {
        &mut recipe.detail.noise_reduction.luminance_detail
    });
    param(ui, edit, &LUMINANCE_CONTRAST, |recipe| {
        &mut recipe.detail.noise_reduction.luminance_contrast
    });
    param(ui, edit, &COLOR, |recipe| {
        &mut recipe.detail.noise_reduction.color
    });
    param(ui, edit, &COLOR_DETAIL, |recipe| {
        &mut recipe.detail.noise_reduction.color_detail
    });
    param(ui, edit, &SMOOTHNESS, |recipe| {
        &mut recipe.detail.noise_reduction.color_smoothness
    });
}
