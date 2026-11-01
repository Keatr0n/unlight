use super::param;
use crate::{
    state::edit::{PhotoEdit, Step},
    ui::{
        theme,
        widgets::{
            controls::{segmented, subheading},
            slider::Spec,
            wheel,
        },
    },
};
use eframe::egui::{self, Align2, Id, Sense, Ui, UiBuilder, vec2};
use unlight_recipe::{ColorGrading, GradeWheel, Recipe};

const SMALL_WHEEL: f32 = 124.0;
const LARGE_WHEEL: f32 = 200.0;

const BLENDING: Spec = Spec::amount("Blending", 100.0);
const BALANCE: Spec = Spec::bipolar("Balance", 100.0);
const MIDTONES_LUMINANCE: Spec = Spec::bipolar("Midtones", 100.0).named("Midtones Luminance");
const SHADOWS_LUMINANCE: Spec = Spec::bipolar("Shadows", 100.0).named("Shadows Luminance");
const HIGHLIGHTS_LUMINANCE: Spec = Spec::bipolar("Highlights", 100.0).named("Highlights Luminance");

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum View {
    #[default]
    ThreeWay,
    Shadows,
    Midtones,
    Highlights,
    Global,
}

impl View {
    fn wheel(self) -> Option<Wheel> {
        match self {
            Self::ThreeWay => None,
            Self::Shadows => Some(Wheel::SHADOWS),
            Self::Midtones => Some(Wheel::MIDTONES),
            Self::Highlights => Some(Wheel::HIGHLIGHTS),
            Self::Global => Some(Wheel::GLOBAL),
        }
    }
}

#[derive(Clone, Copy)]
struct Wheel {
    name: &'static str,
    select: fn(&mut ColorGrading) -> &mut GradeWheel,
    hue: Spec,
    saturation: Spec,
    luminance: Spec,
}

const fn wheel_specs(
    hue: &'static str,
    saturation: &'static str,
    luminance: &'static str,
) -> (Spec, Spec, Spec) {
    (
        Spec::amount("Hue", 360.0).named(hue),
        Spec::amount("Saturation", 100.0).named(saturation),
        Spec::bipolar("Luminance", 100.0).named(luminance),
    )
}

impl Wheel {
    const fn new(
        name: &'static str,
        select: fn(&mut ColorGrading) -> &mut GradeWheel,
        specs: (Spec, Spec, Spec),
    ) -> Self {
        Self {
            name,
            select,
            hue: specs.0,
            saturation: specs.1,
            luminance: specs.2,
        }
    }

    const SHADOWS: Self = Self::new(
        "Shadows",
        |grading| &mut grading.shadows,
        wheel_specs("Shadows Hue", "Shadows Saturation", "Shadows Luminance"),
    );
    const MIDTONES: Self = Self::new(
        "Midtones",
        |grading| &mut grading.midtones,
        wheel_specs("Midtones Hue", "Midtones Saturation", "Midtones Luminance"),
    );
    const HIGHLIGHTS: Self = Self::new(
        "Highlights",
        |grading| &mut grading.highlights,
        wheel_specs("Highlights Hue", "Highlights Saturation", "Highlights Luminance"),
    );
    const GLOBAL: Self = Self::new(
        "Global",
        |grading| &mut grading.global,
        wheel_specs("Global Hue", "Global Saturation", "Global Luminance"),
    );

    fn grade<'a>(&self, recipe: &'a mut Recipe) -> &'a mut GradeWheel {
        (self.select)(&mut recipe.color_grading)
    }
}

fn wheel_control(ui: &mut Ui, edit: &mut PhotoEdit, wheel_def: Wheel, diameter: f32) {
    let label = wheel_def.name;
    let grade = wheel_def.grade(&mut edit.recipe);
    let response = wheel::show(ui, grade, label, diameter);
    if response.changed() {
        let text = format!("{:.0}° {:.0}%", grade.hue, grade.saturation);
        edit.note(label, text, Step::New);
    }
}

fn large_wheel(ui: &mut Ui, edit: &mut PhotoEdit, wheel_def: Wheel) {
    ui.vertical_centered(|ui| wheel_control(ui, edit, wheel_def, LARGE_WHEEL));
    ui.add_space(theme::GRID * 2.0);
    param(ui, edit, &wheel_def.hue, |recipe| {
        &mut wheel_def.grade(recipe).hue
    });
    param(ui, edit, &wheel_def.saturation, |recipe| {
        &mut wheel_def.grade(recipe).saturation
    });
    param(ui, edit, &wheel_def.luminance, |recipe| {
        &mut wheel_def.grade(recipe).luminance
    });
}

/// A small wheel with its name above and a luminance slider under it.
fn compact_wheel(ui: &mut Ui, edit: &mut PhotoEdit, wheel_def: Wheel, column: egui::Rect) {
    let ui = &mut ui.new_child(UiBuilder::new().max_rect(column));
    ui.spacing_mut().item_spacing.y = 0.0;
    let (title, _) = ui.allocate_exact_size(vec2(column.width(), 18.0), Sense::hover());
    ui.painter().text(
        title.center(),
        Align2::CENTER_CENTER,
        wheel_def.name,
        theme::small(),
        theme::TEXT_DIM,
    );
    ui.vertical_centered(|ui| wheel_control(ui, edit, wheel_def, column.width().min(SMALL_WHEEL)));
}

fn three_way(ui: &mut Ui, edit: &mut PhotoEdit) {
    let width = ui.available_width();
    let height = 18.0 + SMALL_WHEEL;
    let (area, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let top = egui::Rect::from_center_size(
        egui::pos2(area.center().x, area.top() + height * 0.5),
        vec2(SMALL_WHEEL, height),
    );
    compact_wheel(ui, edit, Wheel::MIDTONES, top);
    ui.add_space(theme::GRID);
    let (area, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let column = (width - theme::GRID * 2.0) * 0.5;
    let left = egui::Rect::from_min_size(area.min, vec2(column, height));
    let right = egui::Rect::from_min_size(
        area.min + vec2(column + theme::GRID * 2.0, 0.0),
        vec2(column, height),
    );
    compact_wheel(ui, edit, Wheel::SHADOWS, left);
    compact_wheel(ui, edit, Wheel::HIGHLIGHTS, right);
    subheading(ui, "Luminance");
    param(ui, edit, &MIDTONES_LUMINANCE, |recipe| {
        &mut recipe.color_grading.midtones.luminance
    });
    param(ui, edit, &SHADOWS_LUMINANCE, |recipe| {
        &mut recipe.color_grading.shadows.luminance
    });
    param(ui, edit, &HIGHLIGHTS_LUMINANCE, |recipe| {
        &mut recipe.color_grading.highlights.luminance
    });
}

pub fn show(ui: &mut Ui, edit: &mut PhotoEdit) {
    let id = Id::unique("color grading view");
    let mut view = ui.data(|data| data.get_temp::<View>(id)).unwrap_or_default();
    let options = [
        (View::ThreeWay, "3-Way"),
        (View::Shadows, "Shad"),
        (View::Midtones, "Mid"),
        (View::Highlights, "High"),
        (View::Global, "Global"),
    ];
    if segmented(ui, &mut view, &options) {
        ui.data_mut(|data| data.insert_temp(id, view));
    }
    ui.add_space(theme::GRID * 2.0);
    match view.wheel() {
        Some(single) => large_wheel(ui, edit, single),
        None => three_way(ui, edit),
    }
    ui.add_space(theme::GRID);
    param(ui, edit, &BLENDING, |recipe| &mut recipe.color_grading.blending);
    param(ui, edit, &BALANCE, |recipe| &mut recipe.color_grading.balance);
}
