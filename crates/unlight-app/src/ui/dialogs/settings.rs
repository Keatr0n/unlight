//! The setting group checklist, shared by copying settings and by presets.

use super::{button_row, title};
use crate::{
    action::Action,
    state::{AppState, clipboard::SettingGroup},
    ui::{theme, widgets::controls::check_row},
};
use eframe::egui::Ui;

pub(super) const WIDTH: f32 = 400.0;

pub(super) fn group_checklist(ui: &mut Ui, state: &mut AppState) {
    let half = SettingGroup::ALL.len().div_ceil(2);
    ui.columns_const(|[left, right]| {
        for (column, groups) in [
            (left, &SettingGroup::ALL[..half]),
            (right, &SettingGroup::ALL[half..]),
        ] {
            column.spacing_mut().item_spacing.y = 0.0;
            for group in groups {
                let mut checked = state.copy_selection.contains(*group);
                if check_row(column, &mut checked, group.label()).clicked() {
                    state.copy_selection.set(*group, checked);
                }
            }
        }
    });
}

pub(super) fn copy_settings(ui: &mut Ui, state: &mut AppState) {
    ui.set_width(WIDTH);
    title(ui, WIDTH, "Copy Settings");
    group_checklist(ui, state);
    ui.add_space(theme::GRID * 3.0);
    let buttons = [("All", false), ("None", false), ("Cancel", false), ("Copy", true)];
    match button_row(ui, WIDTH, &buttons) {
        Some(0) => state.copy_selection.set_all(true),
        Some(1) => state.copy_selection.set_all(false),
        Some(2) => state.apply(Action::Cancel),
        Some(3) => state.confirm_copy(),
        _ => {}
    }
}
