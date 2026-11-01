//! Confirming that photos should leave the catalog, since their edits can't be brought back.

use super::{button_row, title};
use crate::{
    action::Action,
    state::{AppState, transfer::count_of},
    ui::{theme, widgets::controls::note},
};
use eframe::egui::Ui;
use unlight_catalog::{FolderId, VariantId};

const WIDTH: f32 = 400.0;

/// Cancel or Remove, returning true for Remove.
fn confirmed(ui: &mut Ui, state: &mut AppState) -> bool {
    ui.add_space(theme::GRID * 3.0);
    match button_row(ui, WIDTH, &[("Cancel", false), ("Remove", true)]) {
        Some(0) => {
            state.apply(Action::Cancel);
            false
        }
        Some(1) => true,
        _ => false,
    }
}

pub(super) fn photos(ui: &mut Ui, state: &mut AppState, variants: Vec<VariantId>) {
    ui.set_width(WIDTH);
    let rows = state.library.selected_rows();
    let copies = rows.iter().filter(|row| row.copy_name.is_some()).count();
    let (heading, explanation) = if copies == variants.len() {
        (
            format!("Remove {}?", count_of(copies, "Virtual Copy", "Virtual Copies")),
            "Their edits are lost, but the original photos stay in the catalog.",
        )
    } else {
        let name = rows.first().filter(|_| variants.len() == 1).and_then(|row| row.path.file_name());
        let heading = name.map_or_else(
            || format!("Remove {}?", count_of(variants.len(), "Photo", "Photos")),
            |name| format!("Remove {}?", name.to_string_lossy()),
        );
        let explanation = if variants.len() == 1 {
            "This takes it out of the catalog with its edits and any virtual copies, and can't be \
             undone. The file stays on disk."
        } else {
            "This takes them out of the catalog with their edits and any virtual copies, and \
             can't be undone. The files stay on disk."
        };
        (heading, explanation)
    };
    title(ui, WIDTH, &heading);
    note(ui, explanation);
    if confirmed(ui, state) {
        state.remove_variants(variants);
    }
}

pub(super) fn folder(ui: &mut Ui, state: &mut AppState, folder: FolderId, name: &str) {
    ui.set_width(WIDTH);
    title(ui, WIDTH, &format!("Remove {name}?"));
    note(
        ui,
        "This takes the folder and its subfolders out of the catalog with all their photos and \
         edits, and can't be undone. The files stay on disk.",
    );
    if confirmed(ui, state) {
        state.remove_folder(folder);
    }
}
