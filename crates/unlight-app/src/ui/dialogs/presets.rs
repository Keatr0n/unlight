//! Saving a preset, applying part of one, and naming a collection.

use super::{
    button_row, labelled_field,
    settings::{WIDTH, group_checklist},
    title,
};
use crate::{
    action::Action,
    state::{AppState, Dialog, write::Write},
    ui::{theme, widgets::controls::note},
};
use eframe::egui::Ui;
use unlight_catalog::PresetId;

pub(super) fn save_preset(ui: &mut Ui, state: &mut AppState, mut name: String, mut group: String) {
    ui.set_width(WIDTH);
    title(ui, WIDTH, "New Preset");
    let mut changed = labelled_field(ui, "Name", &mut name, WIDTH);
    changed |= labelled_field(ui, "Group", &mut group, WIDTH);
    ui.add_space(theme::GRID * 2.0);
    group_checklist(ui, state);
    ui.add_space(theme::GRID * 3.0);
    match button_row(ui, WIDTH, &[("Cancel", false), ("Save", true)]) {
        Some(0) => state.apply(Action::Cancel),
        Some(1) if !name.trim().is_empty() => {
            state.save_preset(group.trim(), name.trim());
            return;
        }
        _ => {}
    }
    if changed {
        state.dialog = Some(Dialog::SavePreset { name, group });
    }
}

pub(super) fn apply_part(ui: &mut Ui, state: &mut AppState, id: PresetId) {
    ui.set_width(WIDTH);
    let Some(preset) = state.preset_named(id).cloned() else {
        state.dialog = None;
        return;
    };
    title(ui, WIDTH, &format!("Apply Part of {}", preset.name));
    note(
        ui,
        "Only the ticked settings are applied. A preset only sets what it was saved with.",
    );
    ui.add_space(theme::GRID * 2.0);
    group_checklist(ui, state);
    ui.add_space(theme::GRID * 3.0);
    match button_row(ui, WIDTH, &[("Cancel", false), ("Apply", true)]) {
        Some(0) => state.apply(Action::Cancel),
        Some(1) => {
            state.apply_preset(&preset, true);
            state.dialog = None;
        }
        _ => {}
    }
}

pub(super) fn new_collection(ui: &mut Ui, state: &mut AppState, mut name: String) {
    ui.set_width(WIDTH);
    title(ui, WIDTH, "New Collection");
    let changed = labelled_field(ui, "Name", &mut name, WIDTH);
    let selected = state.library.selection();
    note(
        ui,
        &format!("Starts with the {} selected photos.", selected.len()),
    );
    ui.add_space(theme::GRID * 3.0);
    match button_row(ui, WIDTH, &[("Cancel", false), ("Create", true)]) {
        Some(0) => state.apply(Action::Cancel),
        Some(1) if !name.trim().is_empty() => {
            state.writes.push(Write::CreateCollection {
                name: name.trim().to_owned(),
                variants: selected,
            });
            state.dialog = None;
            return;
        }
        _ => {}
    }
    if changed {
        state.dialog = Some(Dialog::NewCollection { name });
    }
}
