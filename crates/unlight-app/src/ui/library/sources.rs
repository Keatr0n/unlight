//! The left side of the Library: where photos come from. Folders and collections each narrow
//! the view, and the catalog section gets back to everything.

use crate::{
    action::{Action, tip},
    state::{AppState, Dialog, Effect, rules::SmartEditor, write::Write},
    ui::{
        left_panel::{collapsible, list_row},
        theme,
        widgets::{
            controls::{icon_button, note},
            icons::Icon,
        },
    },
};
use eframe::egui::{Frame, Panel, ScrollArea, Ui, vec2};
use std::collections::HashMap;
use unlight_catalog::{Collection, CollectionId, Filter, Folder, FolderFilter, FolderId, PhotoStatus};

const INDENT: usize = 2;

fn folder_depths(folders: &[Folder]) -> HashMap<FolderId, usize> {
    let parents: HashMap<FolderId, Option<FolderId>> =
        folders.iter().map(|folder| (folder.id, folder.parent)).collect();
    folders
        .iter()
        .map(|folder| {
            let mut depth = 0;
            let mut parent = folder.parent;
            while let Some(id) = parent {
                depth += 1;
                parent = parents.get(&id).copied().flatten();
            }
            (folder.id, depth)
        })
        .collect()
}

fn folder_label(folder: &Folder, depth: usize) -> String {
    let name = folder
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let name = name.unwrap_or_else(|| folder.path.display().to_string());
    let indent = " ".repeat(depth * INDENT);
    if folder.photo_count > 0 {
        format!("{indent}{name}   {}", folder.photo_count)
    } else {
        format!("{indent}{name}")
    }
}

fn catalog_section(ui: &mut Ui, state: &mut AppState) {
    let filter = state.library.filter().clone();
    let everything = filter.folder.is_none() && filter.collection.is_none() && filter.status.is_none();
    let total = if everything {
        state.library.count().map(|count| format!("   {count}"))
    } else {
        None
    };
    let all = format!("All Photographs{}", total.unwrap_or_default());
    if list_row(ui, &all, everything, false).clicked() {
        state.library.set_filter(Filter {
            folder: None,
            collection: None,
            status: None,
            ..filter.clone()
        });
    }
    let missing = filter.status == Some(PhotoStatus::Missing);
    if list_row(ui, "Missing Photos", missing, false).clicked() {
        let status = (!missing).then_some(PhotoStatus::Missing);
        state.library.set_filter(Filter { status, ..filter });
    }
}

#[derive(Default)]
struct FolderClicks {
    open: Option<FolderId>,
    rescan: Option<usize>,
}

fn folders_section(ui: &mut Ui, state: &mut AppState) {
    let depths = folder_depths(&state.lists.folders);
    let selected = state.library.filter().folder.map(|filter| filter.folder);
    let mut clicks = FolderClicks::default();
    for (index, folder) in state.lists.folders.iter().enumerate() {
        ui.push_id(folder.id.0, |ui| {
            let label = folder_label(folder, depths.get(&folder.id).copied().unwrap_or(0));
            let row = list_row(ui, &label, selected == Some(folder.id), false);
            if row.clicked() {
                clicks.open = Some(folder.id);
            }
            row.context_menu(|ui| {
                if ui.button("Rescan").clicked() {
                    clicks.rescan = Some(index);
                    ui.close();
                }
            });
        });
    }
    if state.lists.folders.is_empty() {
        note(ui, "Imported folders appear here.");
    }
    ui.add_space(theme::GRID);
    if icon_button(ui, Icon::Plus, &tip("Import a folder", Action::OpenImport), false).clicked() {
        state.apply(Action::OpenImport);
    }
    if let Some(folder) = clicks.open {
        let chosen = FolderFilter {
            folder,
            include_subfolders: true,
        };
        let filter = Filter {
            folder: Some(chosen),
            collection: None,
            ..state.library.filter().clone()
        };
        state.library.set_filter(filter);
    }
    if let Some(root) = clicks
        .rescan
        .and_then(|index| state.lists.folders.get(index))
        .map(|folder| folder.path.clone())
    {
        state.import.phase = crate::state::transfer::ImportPhase::Running(None);
        state.effects.push(Effect::StartImport { root, focus: None });
    }
}

#[derive(Default)]
struct CollectionClicks {
    open: Option<CollectionId>,
    add_selection: Option<CollectionId>,
    remove_selection: Option<CollectionId>,
    delete: Option<CollectionId>,
    edit: Option<Collection>,
}

fn collection_row(ui: &mut Ui, collection: &Collection, selected: bool, clicks: &mut CollectionClicks) {
    ui.push_id(collection.id.0, |ui| {
        let row = list_row(ui, &collection.name, selected, false);
        if row.clicked() {
            clicks.open = Some(collection.id);
        }
        row.context_menu(|ui| {
            if collection.rules.is_some() {
                if ui.button("Edit Rules…").clicked() {
                    clicks.edit = Some(collection.clone());
                    ui.close();
                }
            } else {
                if ui.button("Add Selection").clicked() {
                    clicks.add_selection = Some(collection.id);
                    ui.close();
                }
                if ui.button("Remove Selection").clicked() {
                    clicks.remove_selection = Some(collection.id);
                    ui.close();
                }
            }
            if ui.button("Delete").clicked() {
                clicks.delete = Some(collection.id);
                ui.close();
            }
        });
    });
}

fn collections_section(ui: &mut Ui, state: &mut AppState) {
    let selected = state.library.filter().collection;
    let mut clicks = CollectionClicks::default();
    for collection in &state.lists.collections {
        collection_row(ui, collection, selected == Some(collection.id), &mut clicks);
    }
    if state.lists.collections.is_empty() {
        note(
            ui,
            "Group photos by hand, or by rules that keep themselves up to date.",
        );
    }
    ui.add_space(theme::GRID);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::GRID;
        if icon_button(ui, Icon::Plus, "New collection from the selection", false).clicked() {
            state.dialog = Some(Dialog::NewCollection { name: String::new() });
        }
        if icon_button(ui, Icon::Survey, "New smart collection", false).clicked() {
            state.smart_editor = Some(SmartEditor::default());
            state.dialog = Some(Dialog::SmartCollection);
        }
    });
    apply_collection_clicks(state, clicks);
}

fn apply_collection_clicks(state: &mut AppState, clicks: CollectionClicks) {
    let variants = state.library.selection();
    if let Some(collection) = clicks.open {
        let filter = Filter {
            collection: Some(collection),
            folder: None,
            ..state.library.filter().clone()
        };
        state.library.set_filter(filter);
    }
    if let Some(collection) = clicks.add_selection {
        state.writes.push(Write::AddToCollection {
            collection,
            variants: variants.clone(),
        });
    }
    if let Some(collection) = clicks.remove_selection {
        state
            .writes
            .push(Write::RemoveFromCollection { collection, variants });
    }
    if let Some(id) = clicks.delete {
        state.writes.push(Write::DeleteCollection(id));
        if state.library.filter().collection == Some(id) {
            state.library.set_filter(Filter::default());
        }
    }
    if let Some(collection) = clicks.edit
        && let Some(rules) = &collection.rules
    {
        state.smart_editor = Some(SmartEditor::editing(collection.id, &collection.name, rules));
        state.dialog = Some(Dialog::SmartCollection);
    }
}

pub fn show(ui: &mut Ui, state: &mut AppState) {
    let panel = Panel::left("library sources")
        .exact_size(theme::LEFT_PANEL_WIDTH)
        .show_separator_line(false)
        .frame(Frame::new().fill(theme::PANEL));
    let mut open = state.panels.left;
    panel.show_collapsible(ui, &mut open, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ScrollArea::vertical()
            .id_salt("sources scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                collapsible(ui, "Catalog", true, |ui| catalog_section(ui, state));
                collapsible(ui, "Folders", true, |ui| folders_section(ui, state));
                collapsible(ui, "Collections", true, |ui| collections_section(ui, state));
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: i64, parent: Option<i64>, path: &str) -> Folder {
        Folder {
            id: FolderId(id),
            parent: parent.map(FolderId),
            path: path.into(),
            photo_count: 2,
        }
    }

    #[test]
    fn folders_indent_by_their_depth() {
        let folders = [
            folder(1, None, "/photos"),
            folder(2, Some(1), "/photos/2024"),
            folder(3, Some(2), "/photos/2024/may"),
        ];
        let depths = folder_depths(&folders);
        assert_eq!(depths[&FolderId(3)], 2);
        assert_eq!(folder_label(&folders[2], 2), "    may   2");
    }
}
