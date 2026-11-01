//! The smart collection editor: a name, how rules combine, and a row per rule.

use super::{button_row, labelled_field, title};
use crate::{
    action::Action,
    state::{
        AppState,
        rules::{COMPARISONS, RuleDraft, RuleField, SmartEditor, TEXT_OPS, ValueKind},
        write::Write,
    },
    ui::{
        theme,
        widgets::{
            controls::{bare_dropdown, icon_button, note},
            icons::Icon,
        },
    },
};
use eframe::egui::{TextEdit, Ui};
use unlight_catalog::MatchMode;

const WIDTH: f32 = 580.0;
const MODES: [(MatchMode, &str); 3] = [
    (MatchMode::All, "Match all rules"),
    (MatchMode::Any, "Match any rule"),
    (MatchMode::None, "Match no rules"),
];

fn field_options() -> Vec<(RuleField, &'static str)> {
    RuleField::ALL
        .iter()
        .map(|field| (*field, field.name()))
        .collect()
}

fn value_control(ui: &mut Ui, draft: &mut RuleDraft, index: usize) {
    match draft.field.value_kind() {
        ValueKind::Choice(options) => {
            let options: Vec<(String, &str)> =
                options.iter().map(|name| ((*name).to_owned(), *name)).collect();
            bare_dropdown(ui, ("rule value", index), &mut draft.value, &options, 150.0);
        }
        kind => {
            let hint = match kind {
                ValueKind::Date => "2024-03-15",
                ValueKind::Path => "/path/to/folder",
                ValueKind::Number => "0",
                _ => "text",
            };
            ui.add(
                TextEdit::singleline(&mut draft.value)
                    .desired_width(150.0)
                    .hint_text(hint),
            );
        }
    }
}

fn rule_row(ui: &mut Ui, draft: &mut RuleDraft, index: usize) -> bool {
    let mut remove = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::GRID;
        let mut field = draft.field;
        bare_dropdown(ui, ("rule field", index), &mut field, &field_options(), 140.0);
        if field != draft.field {
            draft.set_field(field);
        }
        match draft.field.value_kind() {
            ValueKind::Number | ValueKind::Date => {
                bare_dropdown(ui, ("rule op", index), &mut draft.comparison, &COMPARISONS, 110.0);
            }
            ValueKind::Text => {
                bare_dropdown(ui, ("rule op", index), &mut draft.text_op, &TEXT_OPS, 110.0);
            }
            ValueKind::Choice(_) | ValueKind::Path => {
                ui.add_space(114.0);
            }
        }
        value_control(ui, draft, index);
        let remove_rule = format!("Remove rule {}", index + 1);
        remove = icon_button(ui, Icon::Close, &remove_rule, false).clicked();
    });
    ui.add_space(theme::GRID);
    remove
}

fn body(ui: &mut Ui, editor: &mut SmartEditor) -> bool {
    labelled_field(ui, "Name", &mut editor.name, WIDTH);
    bare_dropdown(ui, "match mode", &mut editor.mode, &MODES, 200.0);
    ui.add_space(theme::GRID * 2.0);
    let mut removed = None;
    for (index, draft) in editor.drafts.iter_mut().enumerate() {
        if rule_row(ui, draft, index) {
            removed = Some(index);
        }
    }
    if let Some(index) = removed {
        editor.drafts.remove(index);
    }
    if icon_button(ui, Icon::Plus, "Add a rule", false).clicked() {
        editor.drafts.push(RuleDraft::default());
    }
    ui.add_space(theme::GRID * 2.0);
    let ready = editor.rules().is_some();
    if !ready {
        note(ui, "Give it a name and finish every rule to save.");
    }
    ready
}

pub(super) fn show(ui: &mut Ui, state: &mut AppState) {
    ui.set_width(WIDTH);
    let Some(mut editor) = state.smart_editor.take() else {
        state.dialog = None;
        return;
    };
    title(
        ui,
        WIDTH,
        if editor.existing.is_some() {
            "Edit Smart Collection"
        } else {
            "New Smart Collection"
        },
    );
    let ready = body(ui, &mut editor);
    ui.add_space(theme::GRID * 2.0);
    match button_row(ui, WIDTH, &[("Cancel", false), ("Save", ready)]) {
        Some(0) => {
            state.apply(Action::Cancel);
            return;
        }
        Some(1) => {
            if let Some(rules) = editor.rules() {
                state.writes.push(Write::SaveSmartCollection {
                    existing: editor.existing,
                    name: editor.name.trim().to_owned(),
                    rules,
                });
                state.dialog = None;
                return;
            }
        }
        _ => {}
    }
    state.smart_editor = Some(editor);
}
