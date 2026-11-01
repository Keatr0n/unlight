use super::{PanelContext, param};
use crate::{
    state::edit::{PhotoEdit, Step},
    ui::{
        theme,
        widgets::{
            controls::segmented,
            curve::{PointCurve, parametric_plot},
            slider::Spec,
        },
    },
};
use eframe::egui::{Color32, Id, Ui};
use unlight_engine::build_lut;
use unlight_recipe::{Curve, Recipe, ToneCurve};

const HIGHLIGHTS: Spec = Spec::bipolar("Highlights", 100.0).named("Curve Highlights");
const LIGHTS: Spec = Spec::bipolar("Lights", 100.0).named("Curve Lights");
const DARKS: Spec = Spec::bipolar("Darks", 100.0).named("Curve Darks");
const SHADOWS: Spec = Spec::bipolar("Shadows", 100.0).named("Curve Shadows");

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    Parametric,
    #[default]
    Point,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Channel {
    #[default]
    Rgb,
    Red,
    Green,
    Blue,
}

impl Channel {
    fn column(self) -> usize {
        match self {
            Self::Red => 0,
            Self::Green => 1,
            Self::Blue => 2,
            Self::Rgb => 3,
        }
    }

    fn color(self) -> Color32 {
        match self {
            Self::Rgb => theme::HANDLE,
            Self::Red => Color32::from_rgb(0xe0, 0x6a, 0x60),
            Self::Green => Color32::from_rgb(0x70, 0xc0, 0x74),
            Self::Blue => Color32::from_rgb(0x70, 0x9c, 0xe6),
        }
    }

    fn curve(self, tone: &mut ToneCurve) -> &mut Curve {
        match self {
            Self::Rgb => &mut tone.rgb,
            Self::Red => &mut tone.red,
            Self::Green => &mut tone.green,
            Self::Blue => &mut tone.blue,
        }
    }
}

fn composite_samples(tone: &ToneCurve) -> Vec<f32> {
    build_lut(tone).iter().map(|entry| entry[3]).collect()
}

fn point_mode(ui: &mut Ui, edit: &mut PhotoEdit, context: &PanelContext<'_>) {
    let id = Id::unique("tone curve channel");
    let mut channel = ui.data(|data| data.get_temp::<Channel>(id)).unwrap_or_default();
    let options = [
        (Channel::Rgb, "RGB"),
        (Channel::Red, "Red"),
        (Channel::Green, "Green"),
        (Channel::Blue, "Blue"),
    ];
    if segmented(ui, &mut channel, &options) {
        ui.data_mut(|data| data.insert_temp(id, channel));
    }
    ui.add_space(theme::GRID * 2.0);

    let base = edit.recipe.tone_curve.clone();
    let sampler = move |curve: &Curve| {
        let mut tone = base.clone();
        *channel.curve(&mut tone) = curve.clone();
        let lut = build_lut(&tone);
        let column = channel.column();
        lut.iter().map(|entry| entry[column]).collect::<Vec<f32>>()
    };
    let curve = channel.curve(&mut edit.recipe.tone_curve);
    let label = "Point Curve";
    let change = PointCurve {
        curve,
        sampler: &sampler,
        color: channel.color(),
        histogram: context.histogram,
        label,
    }
    .show(ui);
    if change.is_some() {
        edit.note(label, "", Step::New);
    }
}

/// Shades the part of the curve the hovered slider works on, as Lightroom does.
fn parametric_mode(ui: &mut Ui, edit: &mut PhotoEdit, context: &PanelContext<'_>) {
    let parametric = edit.recipe.tone_curve.parametric;
    let mut splits = [
        parametric.split_shadows,
        parametric.split_midtones,
        parametric.split_highlights,
    ];
    let [low, middle, high] = splits.map(|split| split / 100.0);
    let bands = [(high, 1.0), (middle, high), (low, middle), (0.0, low)];

    let hover_id = Id::unique("tone curve hovered region");
    let hovered: Option<usize> = ui.data(|data| data.get_temp(hover_id));
    let region = hovered.map(|index| bands[index]);
    let samples = composite_samples(&edit.recipe.tone_curve);
    if parametric_plot(ui, &samples, context.histogram, &mut splits, region) {
        let curve = &mut edit.recipe.tone_curve.parametric;
        [curve.split_shadows, curve.split_midtones, curve.split_highlights] = splits;
        edit.note("Curve Splits", "", Step::New);
    }
    ui.add_space(theme::GRID);
    let rows = [
        param(ui, edit, &HIGHLIGHTS, |recipe: &mut Recipe| {
            &mut recipe.tone_curve.parametric.highlights
        }),
        param(ui, edit, &LIGHTS, |recipe| {
            &mut recipe.tone_curve.parametric.lights
        }),
        param(ui, edit, &DARKS, |recipe| &mut recipe.tone_curve.parametric.darks),
        param(ui, edit, &SHADOWS, |recipe| {
            &mut recipe.tone_curve.parametric.shadows
        }),
    ];
    let now = rows.iter().position(|hovered| *hovered);
    if now != hovered {
        ui.data_mut(|data| data.insert_temp(hover_id, now));
    }
}

pub fn show(ui: &mut Ui, edit: &mut PhotoEdit, context: &PanelContext<'_>) {
    let id = Id::unique("tone curve mode");
    let mut mode = ui.data(|data| data.get_temp::<Mode>(id)).unwrap_or_default();
    if segmented(
        ui,
        &mut mode,
        &[(Mode::Parametric, "Parametric"), (Mode::Point, "Point")],
    ) {
        ui.data_mut(|data| data.insert_temp(id, mode));
    }
    ui.add_space(theme::GRID);
    match mode {
        Mode::Parametric => parametric_mode(ui, edit, context),
        Mode::Point => point_mode(ui, edit, context),
    }
}
