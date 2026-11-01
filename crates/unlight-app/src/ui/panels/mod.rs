//! The Develop panels on the right, in Lightroom's order. Each panel only binds recipe fields
//! to widgets; `param` does the bookkeeping every slider shares.

mod basic;
mod color_grading;
mod color_mixer;
mod detail;
mod effects;
mod lens;
mod tone_curve;
mod transform;

pub(crate) use basic::labelled_row;

use crate::{
    state::{edit::PhotoEdit, sections::Section},
    ui::widgets::slider::{Slider, Spec},
};
use eframe::egui::{Color32, Ui};
use unlight_engine::ProfileChoices;
use unlight_recipe::Recipe;

/// Things a panel asks for that it can't do with the edit alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelRequest {
    AutoTone,
    RescanProfiles,
    OpenProfileFolder,
}

/// What a panel can read besides the edit.
pub struct PanelContext<'a> {
    pub histogram: Option<&'a crate::renderer::Histogram>,
    /// The file's own white balance, for showing As Shot in kelvin.
    pub as_shot: Option<(f32, f32)>,
    pub picking_white_balance: &'a mut bool,
    /// What the lens panel should say about the profile in use.
    pub lens_status: Option<String>,
    /// Set when the user asks to draw Upright guides.
    pub draw_guides: bool,
    /// The profiles the open photo can be given, once it's loaded.
    pub profiles: ProfileChoices,
    pub request: Option<PanelRequest>,
}

/// A slider bound to one recipe field. The default comes from `Recipe::default`, so a reset
/// can never disagree with what a new photo starts at.
pub fn param(
    ui: &mut Ui,
    edit: &mut PhotoEdit,
    spec: &Spec,
    field: impl Fn(&mut Recipe) -> &mut f32,
) -> bool {
    slider_row(ui, edit, spec, None, field)
}

pub fn param_gradient(
    ui: &mut Ui,
    edit: &mut PhotoEdit,
    spec: &Spec,
    gradient: &[Color32],
    field: impl Fn(&mut Recipe) -> &mut f32,
) {
    slider_row(ui, edit, spec, Some(gradient), field);
}

/// Returns whether the pointer is on the row.
fn slider_row(
    ui: &mut Ui,
    edit: &mut PhotoEdit,
    spec: &Spec,
    gradient: Option<&[Color32]>,
    field: impl Fn(&mut Recipe) -> &mut f32,
) -> bool {
    let default = *field(&mut Recipe::default());
    let value = field(&mut edit.recipe);
    let mut slider = Slider::new(spec, value, default);
    if let Some(stops) = gradient {
        slider = slider.gradient(stops);
    }
    let result = slider.show(ui);
    if let Some(step) = result.change {
        let text = spec.format(*field(&mut edit.recipe));
        edit.note(spec.name, text, step);
    }
    result.response.contains_pointer()
}

/// Runs a panel's body.
pub fn show_section(ui: &mut Ui, section: Section, edit: &mut PhotoEdit, context: &mut PanelContext<'_>) {
    match section {
        Section::Basic => basic::show(ui, edit, context),
        Section::ToneCurve => tone_curve::show(ui, edit, context),
        Section::ColorMixer => color_mixer::show(ui, edit),
        Section::ColorGrading => color_grading::show(ui, edit),
        Section::Detail => detail::show(ui, edit),
        Section::LensCorrections => lens::show(ui, edit, context),
        Section::Transform => transform::show(ui, edit, context),
        Section::Effects => effects::show(ui, edit),
        Section::Calibration => effects::show_calibration(ui, edit),
    }
}
