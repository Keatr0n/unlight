//! Photo Merge: choose how to merge, then watch it go.

use super::{button_row, title};
use crate::{
    action::Action,
    state::{
        AppState, Effect,
        merge::{MergeForm, MergeRun},
        transfer::count_of,
    },
    ui::{
        theme,
        widgets::{
            controls::{check_row, dropdown, note, progress_bar},
            slider::{Slider, Spec},
        },
    },
};
use eframe::egui::{RichText, Ui};
use unlight_merge::{Deghost, Projection};
use unlight_raw::MergeKind;

const WIDTH: f32 = 420.0;
const DEGHOST: [(Deghost, &str); 4] = [
    (Deghost::None, "None"),
    (Deghost::Low, "Low"),
    (Deghost::Medium, "Medium"),
    (Deghost::High, "High"),
];
const PROJECTIONS: [(Projection, &str); 3] = [
    (Projection::Spherical, "Spherical"),
    (Projection::Cylindrical, "Cylindrical"),
    (Projection::Perspective, "Perspective"),
];
const BOUNDARY_WARP: Spec = Spec::amount("Boundary Warp", 100.0);

fn hdr_options(ui: &mut Ui, form: &mut MergeForm) {
    check_row(ui, &mut form.auto_align, "Auto Align");
    ui.add_space(theme::GRID);
    dropdown(ui, "Deghost", &mut form.deghost, &DEGHOST, WIDTH);
    note(ui, "Picks one exposure for anything that moved between frames.");
}

fn panorama_options(ui: &mut Ui, form: &mut MergeForm) {
    dropdown(ui, "Projection", &mut form.projection, &PROJECTIONS, WIDTH);
    Slider::new(&BOUNDARY_WARP, &mut form.boundary_warp, 0.0).show(ui);
    ui.add_space(theme::GRID);
    check_row(ui, &mut form.auto_crop, "Auto Crop");
    ui.add_space(theme::GRID);
    note(ui, "Lens corrections go into the panorama, using what each file knows about its lens.");
}

fn form_view(ui: &mut Ui, state: &mut AppState) {
    let Some(form) = &mut state.merge_form else {
        return;
    };
    let heading = match form.kind {
        MergeKind::Hdr => "HDR Merge",
        MergeKind::Panorama => "Panorama Merge",
    };
    title(ui, WIDTH, heading);
    ui.label(RichText::new(count_of(form.sources.len(), "photo", "photos")).color(theme::TEXT_DIM));
    ui.add_space(theme::GRID * 2.0);
    match form.kind {
        MergeKind::Hdr => hdr_options(ui, form),
        MergeKind::Panorama => panorama_options(ui, form),
    }
    ui.add_space(theme::GRID * 3.0);
    match button_row(ui, WIDTH, &[("Cancel", false), ("Merge", true)]) {
        Some(0) => state.apply(Action::Cancel),
        Some(1) => {
            state.merge_run = Some(MergeRun::default());
            state.effects.push(Effect::StartMerge);
        }
        _ => {}
    }
}

fn run_view(ui: &mut Ui, state: &mut AppState, run: &MergeRun) {
    if let Some(failure) = &run.failure {
        title(ui, WIDTH, "Merge Failed");
        ui.label(RichText::new(failure).color(theme::ERROR));
        ui.add_space(theme::GRID * 3.0);
        if button_row(ui, WIDTH, &[("Done", true)]).is_some() {
            state.merge_run = None;
            state.dialog = None;
        }
        return;
    }
    title(ui, WIDTH, "Merging");
    let stage = if run.stage.is_empty() { "Starting…" } else { &run.stage };
    ui.label(RichText::new(stage).color(theme::TEXT_DIM));
    ui.add_space(theme::GRID * 2.0);
    progress_bar(ui, Some(run.fraction), WIDTH);
    ui.add_space(theme::GRID * 3.0);
    match button_row(ui, WIDTH, &[("Run in Background", false), ("Cancel Merge", false)]) {
        Some(0) => state.dialog = None,
        Some(1) => state.effects.push(Effect::CancelMerge),
        _ => {}
    }
}

pub(super) fn show(ui: &mut Ui, state: &mut AppState) {
    ui.set_width(WIDTH);
    match state.merge_run.clone() {
        Some(run) => run_view(ui, state, &run),
        None => form_view(ui, state),
    }
}
