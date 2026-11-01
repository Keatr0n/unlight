//! Modal dialogs, and the bits they share.

mod export;
mod import;
mod merge;
mod presets;
mod remove;
mod settings;
mod smart_collection;

use crate::{
    state::{AppState, Dialog},
    ui::{theme, widgets::controls::text_button},
};
use eframe::egui::{Align2, Frame, Id, Modal, Sense, TextEdit, Ui, vec2};

pub(crate) fn title(ui: &mut Ui, width: f32, text: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
    ui.painter().text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        text,
        theme::medium(14.0),
        theme::TEXT,
    );
    ui.add_space(theme::GRID * 2.0);
}

/// A label and a text field on one row.
pub(crate) fn labelled_field(ui: &mut Ui, label: &str, text: &mut String, width: f32) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(96.0, theme::ROW_HEIGHT), Sense::hover());
        ui.painter().text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            label,
            theme::body(),
            theme::TEXT_DIM,
        );
        let field = TextEdit::singleline(text)
            .desired_width(width - 96.0)
            .hint_text(label);
        changed = ui.add(field).changed();
    });
    ui.add_space(theme::GRID);
    changed
}

/// A folder path with a chooser beside it. Returns true when the chooser was asked for.
pub(crate) fn folder_row(ui: &mut Ui, text: &mut String, width: f32) -> bool {
    let mut browse = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::GRID;
        let label_width = 96.0 - theme::GRID;
        let (rect, _) = ui.allocate_exact_size(vec2(label_width, theme::ROW_HEIGHT), Sense::hover());
        ui.painter().text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            "Folder",
            theme::body(),
            theme::TEXT_DIM,
        );
        ui.add(
            TextEdit::singleline(text)
                .desired_width(width - 184.0)
                .hint_text("Folder"),
        );
        browse = text_button(ui, "Browse…", false, 84.0).clicked();
    });
    ui.add_space(theme::GRID);
    browse
}

/// Evenly spaced buttons along the bottom of a dialog. Returns the index clicked.
pub(crate) fn button_row(ui: &mut Ui, width: f32, buttons: &[(&str, bool)]) -> Option<usize> {
    let gap = theme::GRID;
    let count = buttons.len() as f32;
    let button_width = (width - gap * (count - 1.0)) / count;
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (index, (label, primary)) in buttons.iter().enumerate() {
            if text_button(ui, label, *primary, button_width).clicked() {
                clicked = Some(index);
            }
        }
    });
    clicked
}

pub fn show(ui: &mut Ui, state: &mut AppState) {
    let Some(dialog) = state.dialog.clone() else {
        return;
    };
    let frame = Frame::popup(ui.style()).fill(theme::PANEL).inner_margin(16);
    let id = Id::unique(("dialog", std::mem::discriminant(&dialog)));
    let response = Modal::new(id).frame(frame).show(ui.ctx(), |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        match dialog {
            Dialog::CopySettings => settings::copy_settings(ui, state),
            Dialog::SavePreset { name, group } => presets::save_preset(ui, state, name, group),
            Dialog::ApplyPresetPart(preset) => presets::apply_part(ui, state, preset),
            Dialog::NewCollection { name } => presets::new_collection(ui, state, name),
            Dialog::SmartCollection => smart_collection::show(ui, state),
            Dialog::Import => import::show(ui, state),
            Dialog::Export => export::show(ui, state),
            Dialog::PhotoMerge => merge::show(ui, state),
            Dialog::RemovePhotos { variants } => remove::photos(ui, state, variants),
            Dialog::RemoveFolder { folder, name } => remove::folder(ui, state, folder, &name),
        }
    });
    if response.should_close() && state.dialog.is_some() {
        state.dialog = None;
    }
}
