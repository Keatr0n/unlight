//! The bar across the top: name, what's open, and the module switch.

use crate::{
    action::{Action, tip},
    state::{
        AppState, Dialog, Module,
        library::title,
        transfer::{ImportPhase, describe_progress},
    },
    ui::{
        theme,
        widgets::{controls::icon_button, icons::Icon},
    },
};
use eframe::egui::{self, Align2, Frame, Panel, Response, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use unlight_raw::MergeKind;

const TAB_WIDTH: f32 = 72.0;

fn module_tab(ui: &mut Ui, label: &str, selected: bool, hint: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(TAB_WIDTH, theme::TOP_BAR_HEIGHT), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Tab, true, selected, label));
    let color = match (selected, response.hovered()) {
        (true, _) | (false, true) => theme::TEXT,
        (false, false) => theme::TEXT_FAINT,
    };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::medium(13.0),
        color,
    );
    if selected {
        let underline = egui::Rect::from_min_max(
            pos2(rect.left() + 12.0, rect.bottom() - 2.0),
            pos2(rect.right() - 12.0, rect.bottom()),
        );
        ui.painter().rect_filled(underline, 0.0, theme::ACCENT);
    }
    response.on_hover_text(hint)
}

fn photo_summary(state: &AppState) -> Option<String> {
    let row = state.focused()?;
    let position = state.library.focus().unwrap_or(0) + 1;
    let total = state.library.len();
    Some(format!("{}   {position} of {total}", title(row)))
}

/// A quiet note while work runs in the background. Clicking it brings its dialog back.
fn background_work(ui: &mut Ui, state: &mut AppState) {
    if state.import.is_running() {
        let (text, _) = describe_progress(match state.import.phase {
            ImportPhase::Running(progress) => progress,
            _ => None,
        });
        if work_chip(ui, &text).clicked() {
            state.dialog = Some(Dialog::Import);
        }
    }
    if let Some(run) = state.export_run.as_ref().filter(|run| !run.finished) {
        let text = format!("Exporting {} of {}", run.done + 1, run.total);
        if work_chip(ui, &text).clicked() {
            state.dialog = Some(Dialog::Export);
        }
    }
    if let Some(run) = state.merge_run.as_ref().filter(|run| run.is_running()) {
        let text = run.summary();
        if work_chip(ui, &text).clicked() {
            state.dialog = Some(Dialog::PhotoMerge);
        }
    }
}

/// The Photo menu, which is where merging lives, as it is in Lightroom.
fn photo_menu(ui: &mut Ui, state: &mut AppState) {
    let response = ui.menu_button("Photo", |ui| {
        let enabled = state.can_merge();
        ui.add_enabled_ui(enabled, |ui| {
            ui.menu_button("Photo Merge", |ui| merge_items(ui, state));
        });
    });
    response.response.on_hover_text("Select two or more photos to merge them");
}

fn merge_items(ui: &mut Ui, state: &mut AppState) {
    for (label, kind) in [("HDR…", MergeKind::Hdr), ("Panorama…", MergeKind::Panorama)] {
        if ui.button(tip(label, Action::PhotoMerge(kind))).clicked() {
            state.apply(Action::PhotoMerge(kind));
            ui.close();
        }
    }
}

fn work_chip(ui: &mut Ui, text: &str) -> Response {
    let label = egui::Label::new(
        egui::RichText::new(text)
            .font(theme::small())
            .color(theme::TEXT_DIM),
    )
    .sense(Sense::click());
    ui.add_space(theme::GRID * 3.0);
    ui.add(label).on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn show(ui: &mut Ui, state: &mut AppState) {
    let frame = Frame::new()
        .fill(theme::PANEL_DEEP)
        .inner_margin(egui::Margin::symmetric(12, 0));
    Panel::top("top bar")
        .exact_size(theme::TOP_BAR_HEIGHT)
        .show_separator_line(false)
        .frame(frame)
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let (wordmark, _) = ui.allocate_exact_size(vec2(64.0, 24.0), Sense::hover());
                ui.painter().text(
                    wordmark.left_center(),
                    Align2::LEFT_CENTER,
                    "Unlight",
                    theme::medium(14.0),
                    theme::TEXT,
                );
                let import = icon_button(ui, Icon::Open, &tip("Import photos", Action::OpenImport), false);
                if import.clicked() {
                    state.apply(Action::OpenImport);
                }
                ui.add_space(theme::GRID * 2.0);
                photo_menu(ui, state);
                ui.add_space(theme::GRID * 3.0);
                if let Some(summary) = photo_summary(state) {
                    ui.label(egui::RichText::new(summary).color(theme::TEXT_DIM));
                }
                background_work(ui, state);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let develop = module_tab(
                        ui,
                        "Develop",
                        state.module == Module::Develop,
                        &tip("Develop", Action::ShowDevelop),
                    );
                    if develop.clicked() {
                        state.apply(Action::ShowDevelop);
                    }
                    let library = module_tab(
                        ui,
                        "Library",
                        state.module == Module::Library,
                        &tip("Library", Action::ShowGrid),
                    );
                    if library.clicked() {
                        state.apply(Action::ShowGrid);
                    }
                    ui.add_space(theme::GRID * 2.0);
                    let export = icon_button(ui, Icon::Export, &tip("Export", Action::OpenExport), false);
                    if export.clicked() {
                        state.apply(Action::OpenExport);
                    }
                });
            });
        });
}
