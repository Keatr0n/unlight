use super::param_gradient;
use crate::{
    state::edit::PhotoEdit,
    ui::{
        theme,
        widgets::{controls::segmented, slider::Spec},
    },
};
use eframe::egui::{Color32, Id, Ui, ecolor::HsvaGamma};
use unlight_recipe::HSL_BANDS;

/// Band centres in degrees, matching the engine's band order.
const BAND_HUES: [f32; HSL_BANDS] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 280.0, 320.0];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Target {
    #[default]
    Hue,
    Saturation,
    Luminance,
    All,
}

fn band_color(hue: f32, saturation: f32, value: f32) -> Color32 {
    HsvaGamma {
        h: hue.rem_euclid(360.0) / 360.0,
        s: saturation,
        v: value,
        a: 1.0,
    }
    .into()
}

fn hue_track(band: usize) -> [Color32; 3] {
    let hue = BAND_HUES[band];
    [
        band_color(hue - 30.0, 0.62, 0.78),
        band_color(hue, 0.62, 0.78),
        band_color(hue + 30.0, 0.62, 0.78),
    ]
}

fn saturation_track(band: usize) -> [Color32; 2] {
    [Color32::from_gray(0x70), band_color(BAND_HUES[band], 0.72, 0.82)]
}

fn luminance_track(band: usize) -> [Color32; 3] {
    let hue = BAND_HUES[band];
    [
        band_color(hue, 0.6, 0.28),
        band_color(hue, 0.62, 0.62),
        band_color(hue, 0.3, 0.95),
    ]
}

const fn band_spec(label: &'static str, name: &'static str) -> Spec {
    Spec::bipolar(label, 100.0).named(name)
}

const HUE_SPECS: [Spec; HSL_BANDS] = [
    band_spec("Red", "Red Hue"),
    band_spec("Orange", "Orange Hue"),
    band_spec("Yellow", "Yellow Hue"),
    band_spec("Green", "Green Hue"),
    band_spec("Aqua", "Aqua Hue"),
    band_spec("Blue", "Blue Hue"),
    band_spec("Purple", "Purple Hue"),
    band_spec("Magenta", "Magenta Hue"),
];
const SATURATION_SPECS: [Spec; HSL_BANDS] = [
    band_spec("Red", "Red Saturation"),
    band_spec("Orange", "Orange Saturation"),
    band_spec("Yellow", "Yellow Saturation"),
    band_spec("Green", "Green Saturation"),
    band_spec("Aqua", "Aqua Saturation"),
    band_spec("Blue", "Blue Saturation"),
    band_spec("Purple", "Purple Saturation"),
    band_spec("Magenta", "Magenta Saturation"),
];
const LUMINANCE_SPECS: [Spec; HSL_BANDS] = [
    band_spec("Red", "Red Luminance"),
    band_spec("Orange", "Orange Luminance"),
    band_spec("Yellow", "Yellow Luminance"),
    band_spec("Green", "Green Luminance"),
    band_spec("Aqua", "Aqua Luminance"),
    band_spec("Blue", "Blue Luminance"),
    band_spec("Purple", "Purple Luminance"),
    band_spec("Magenta", "Magenta Luminance"),
];
const MIX_SPECS: [Spec; HSL_BANDS] = [
    band_spec("Red", "Red Mix"),
    band_spec("Orange", "Orange Mix"),
    band_spec("Yellow", "Yellow Mix"),
    band_spec("Green", "Green Mix"),
    band_spec("Aqua", "Aqua Mix"),
    band_spec("Blue", "Blue Mix"),
    band_spec("Purple", "Purple Mix"),
    band_spec("Magenta", "Magenta Mix"),
];

fn hue_sliders(ui: &mut Ui, edit: &mut PhotoEdit) {
    for (band, spec) in HUE_SPECS.iter().enumerate() {
        param_gradient(ui, edit, spec, &hue_track(band), |recipe| {
            &mut recipe.color_mixer.hue[band]
        });
    }
}

fn saturation_sliders(ui: &mut Ui, edit: &mut PhotoEdit) {
    for (band, spec) in SATURATION_SPECS.iter().enumerate() {
        param_gradient(ui, edit, spec, &saturation_track(band), |recipe| {
            &mut recipe.color_mixer.saturation[band]
        });
    }
}

fn luminance_sliders(ui: &mut Ui, edit: &mut PhotoEdit) {
    for (band, spec) in LUMINANCE_SPECS.iter().enumerate() {
        param_gradient(ui, edit, spec, &luminance_track(band), |recipe| {
            &mut recipe.color_mixer.luminance[band]
        });
    }
}

fn heading(ui: &mut Ui, text: &str) {
    crate::ui::widgets::controls::subheading(ui, text);
}

fn black_and_white_mix(ui: &mut Ui, edit: &mut PhotoEdit) {
    heading(ui, "B&W Mix");
    for (band, spec) in MIX_SPECS.iter().enumerate() {
        param_gradient(ui, edit, spec, &luminance_track(band), |recipe| {
            &mut recipe.black_and_white.mix[band]
        });
    }
}

pub fn show(ui: &mut Ui, edit: &mut PhotoEdit) {
    if edit.recipe.black_and_white.enabled {
        black_and_white_mix(ui, edit);
        return;
    }
    let id = Id::unique("color mixer target");
    let mut target = ui.data(|data| data.get_temp::<Target>(id)).unwrap_or_default();
    let options = [
        (Target::Hue, "Hue"),
        (Target::Saturation, "Sat"),
        (Target::Luminance, "Lum"),
        (Target::All, "All"),
    ];
    if segmented(ui, &mut target, &options) {
        ui.data_mut(|data| data.insert_temp(id, target));
    }
    ui.add_space(theme::GRID);
    match target {
        Target::Hue => hue_sliders(ui, edit),
        Target::Saturation => saturation_sliders(ui, edit),
        Target::Luminance => luminance_sliders(ui, edit),
        Target::All => {
            heading(ui, "Hue");
            hue_sliders(ui, edit);
            heading(ui, "Saturation");
            saturation_sliders(ui, edit);
            heading(ui, "Luminance");
            luminance_sliders(ui, edit);
        }
    }
}
