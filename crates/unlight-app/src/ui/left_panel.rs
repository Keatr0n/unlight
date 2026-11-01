//! The left side of Develop: Presets, Snapshots and History.

use crate::{
    action::{Action, tip},
    state::{AppState, Dialog, edit::PhotoEdit},
    ui::{
        theme,
        widgets::{
            controls::{SectionHeader, icon_button, note, section, subheading, text_button},
            icons::Icon,
        },
    },
};
use eframe::egui::{self, Align2, Frame, Id, Panel, Role, ScrollArea, Sense, Ui, WidgetInfo, vec2};
use unlight_catalog::{Preset, PresetId};
use unlight_recipe::Recipe;

/// One clickable line in a list, highlighted when it's the current one.
pub(crate) fn list_row(ui: &mut Ui, text: &str, selected: bool, dim: bool) -> egui::Response {
    let height = 22.0;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::ListItem, true, selected, text));
    if selected {
        ui.painter().rect_filled(rect, 3.0, theme::PANEL_RAISED);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 3.0, theme::PANEL_HOVER);
    }
    let color = match (selected, dim) {
        (true, _) => theme::TEXT,
        (false, true) => theme::TEXT_FAINT,
        (false, false) => theme::TEXT_DIM,
    };
    let painter = ui.painter_at(rect.shrink2(vec2(6.0, 0.0)));
    painter.text(
        rect.left_center() + vec2(6.0, 0.0),
        Align2::LEFT_CENTER,
        text,
        theme::body(),
        color,
    );
    response
}

fn snapshots(ui: &mut Ui, edit: &mut PhotoEdit) {
    let (mut restore, mut delete) = (None, None);
    for (index, snapshot) in edit.snapshots.iter().enumerate() {
        ui.push_id(snapshot.id.0, |ui| {
            let row = list_row(ui, &snapshot.name, false, false).on_hover_text("Right-click to delete");
            if row.clicked() {
                restore = Some(index);
            }
            row.context_menu(|ui| {
                if ui.button("Delete").clicked() {
                    delete = Some(snapshot.id);
                }
            });
        });
    }
    if let Some(index) = restore {
        edit.restore_snapshot(index);
    }
    if let Some(id) = delete {
        edit.delete_snapshot(id);
    }
    if edit.snapshots.is_empty() {
        note(ui, "Save the current edit to come back to it.");
    }
    ui.add_space(theme::GRID);
    if icon_button(ui, Icon::Plus, "Create a snapshot of this edit", false).clicked() {
        let name = format!("Snapshot {}", edit.snapshots.len() + 1);
        edit.take_snapshot(name);
    }
}

#[derive(Default)]
struct PresetClicks {
    apply: Option<PresetId>,
    apply_part: Option<PresetId>,
    delete: Option<PresetId>,
    hovered: Option<PresetId>,
}

fn preset_row(ui: &mut Ui, preset: &Preset, clicks: &mut PresetClicks) {
    ui.push_id(preset.id.0, |ui| {
        let row = list_row(ui, &preset.name, false, false);
        if row.hovered() {
            clicks.hovered = Some(preset.id);
        }
        if row.clicked() {
            clicks.apply = Some(preset.id);
        }
        row.context_menu(|ui| {
            if ui.button("Apply Part…").clicked() {
                clicks.apply_part = Some(preset.id);
            }
            if ui.button("Delete").clicked() {
                clicks.delete = Some(preset.id);
            }
        });
    });
}

/// Shows what a preset under the pointer would do. It renders in about the time of a slider
/// tick, and nothing is applied until the click.
fn preview_hovered(state: &mut AppState, hovered: Option<PresetId>) {
    let recipe: Option<Box<Recipe>> = hovered.and_then(|id| {
        let preset = state.preset_named(id)?;
        let recipe = preset.settings.apply_to(&state.edit()?.recipe).ok()?;
        Some(Box::new(recipe))
    });
    state.preview_recipe = recipe;
}

fn presets(ui: &mut Ui, state: &mut AppState) {
    let mut clicks = PresetClicks::default();
    let mut last_group = None;
    for preset in &state.lists.presets {
        if last_group != Some(&preset.group) {
            last_group = Some(&preset.group);
            if !preset.group.is_empty() {
                subheading(ui, &preset.group);
            }
        }
        preset_row(ui, preset, &mut clicks);
    }
    if state.lists.presets.is_empty() {
        note(ui, "Save settings as a preset to reuse them.");
    }
    ui.add_space(theme::GRID);
    if icon_button(ui, Icon::Plus, "Save these settings as a preset", false).clicked() {
        state.dialog = Some(Dialog::SavePreset {
            name: String::new(),
            group: "User Presets".into(),
        });
    }
    preview_hovered(state, clicks.hovered);
    if let Some(preset) = clicks.apply.and_then(|id| state.preset_named(id).cloned()) {
        state.apply_preset(&preset, false);
    }
    if let Some(id) = clicks.apply_part {
        state.dialog = Some(Dialog::ApplyPresetPart(id));
    }
    if let Some(id) = clicks.delete {
        state.writes.push(crate::state::write::Write::DeletePreset(id));
    }
}

fn history(ui: &mut Ui, edit: &mut PhotoEdit) {
    let cursor = edit.history().cursor();
    let mut jump = None;
    for (index, entry) in edit.history().entries().iter().enumerate().rev() {
        ui.push_id(index, |ui| {
            if list_row(ui, &entry.label, index == cursor, index > cursor).clicked() {
                jump = Some(index);
            }
        });
    }
    if let Some(index) = jump {
        edit.jump_to(index);
    }
}

fn section_open(ui: &Ui, id: Id, default: bool) -> bool {
    ui.data(|data| data.get_temp(id)).unwrap_or(default)
}

pub(crate) fn collapsible(ui: &mut Ui, title: &str, default_open: bool, add_body: impl FnOnce(&mut Ui)) {
    let id = Id::unique(("left section", title));
    let open = section_open(ui, id, default_open);
    let header = SectionHeader {
        title,
        open,
        enabled: None,
        modified: false,
    };
    let events = section(ui, &header, add_body);
    if events.toggled {
        ui.data_mut(|data| data.insert_temp(id, !open));
    }
}

fn footer(ui: &mut Ui, state: &mut AppState) {
    let gap = theme::GRID;
    let width = (ui.available_width() - gap) * 0.5;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        let copy =
            text_button(ui, "Copy…", false, width).on_hover_text(tip("Copy settings", Action::CopySettings));
        if copy.clicked() {
            state.apply(Action::CopySettings);
        }
        let paste = text_button(ui, "Paste", false, width)
            .on_hover_text(tip("Paste settings", Action::PasteSettings));
        if paste.clicked() {
            state.apply(Action::PasteSettings);
        }
    });
}

pub fn show(ui: &mut Ui, state: &mut AppState) {
    state.preview_recipe = None;
    let frame = Frame::new().fill(theme::PANEL);
    let panel = Panel::left("left panel")
        .exact_size(theme::LEFT_PANEL_WIDTH)
        .show_separator_line(false)
        .frame(frame);
    let mut open = state.panels.left;
    panel.show_collapsible(ui, &mut open, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        Panel::bottom("left footer")
            .exact_size(theme::ROW_HEIGHT + 16.0)
            .show_separator_line(false)
            .frame(Frame::new().inner_margin(egui::Margin::symmetric(12, 8)))
            .show(ui, |ui| footer(ui, state));
        ScrollArea::vertical()
            .id_salt("left scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                collapsible(ui, "Presets", true, |ui| presets(ui, state));
                let Some(edit) = state.edit_mut() else {
                    return;
                };
                collapsible(ui, "Snapshots", true, |ui| snapshots(ui, edit));
                collapsible(ui, "History", true, |ui| history(ui, edit));
            });
    });
}
