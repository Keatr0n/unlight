//! The import dialog: pick a folder, watch it scan, see what turned up.

use super::{button_row, folder_row, title};
use crate::{
    action::Action,
    state::{
        AppState, Effect, FolderUse,
        transfer::{ImportPhase, describe_progress, describe_report},
    },
    ui::{
        theme,
        widgets::controls::{note, progress_bar},
    },
};
use eframe::egui::{RichText, ScrollArea, Ui};
use std::path::PathBuf;
use unlight_catalog::ScanReport;

const WIDTH: f32 = 480.0;
const FAILURE_LIST_HEIGHT: f32 = 140.0;

fn choosing(ui: &mut Ui, state: &mut AppState) {
    title(ui, WIDTH, "Import Photos");
    if folder_row(ui, &mut state.import.folder, WIDTH) {
        state.effects.push(Effect::ChooseFolder(FolderUse::Import));
    }
    note(
        ui,
        "Subfolders are included. Files already in the catalog are only re-read if they changed.",
    );
    ui.add_space(theme::GRID * 3.0);
    match button_row(ui, WIDTH, &[("Cancel", false), ("Import", true)]) {
        Some(0) => state.apply(Action::Cancel),
        Some(1) if !state.import.folder.trim().is_empty() => start(state),
        _ => {}
    }
}

fn start(state: &mut AppState) {
    let root = PathBuf::from(state.import.folder.trim());
    state.import.phase = ImportPhase::Running(None);
    state.effects.push(Effect::StartImport { root, focus: None });
}

fn running(ui: &mut Ui, state: &mut AppState) {
    title(ui, WIDTH, "Importing");
    let progress = match state.import.phase {
        ImportPhase::Running(progress) => progress,
        _ => None,
    };
    let (text, fraction) = describe_progress(progress);
    ui.label(RichText::new(text).color(theme::TEXT_DIM));
    ui.add_space(theme::GRID * 2.0);
    progress_bar(ui, fraction, WIDTH);
    ui.add_space(theme::GRID * 3.0);
    match button_row(
        ui,
        WIDTH,
        &[("Run in Background", false), ("Cancel Import", false)],
    ) {
        Some(0) => state.dialog = None,
        Some(1) => state.effects.push(Effect::CancelImport),
        _ => {}
    }
}

fn failures(ui: &mut Ui, report: &ScanReport) {
    if report.failures.is_empty() {
        return;
    }
    ui.add_space(theme::GRID * 2.0);
    ui.label(RichText::new(format!("{} files couldn't be read", report.failures.len())).color(theme::ERROR));
    ui.add_space(theme::GRID);
    ScrollArea::vertical()
        .max_height(FAILURE_LIST_HEIGHT)
        .show(ui, |ui| {
            for failure in &report.failures {
                let text = format!("{}: {}", failure.path.display(), failure.message);
                ui.label(RichText::new(text).font(theme::small()).color(theme::TEXT_DIM));
            }
        });
}

fn finished(ui: &mut Ui, state: &mut AppState, report: &ScanReport) {
    title(ui, WIDTH, "Import Finished");
    for line in describe_report(report) {
        ui.label(RichText::new(line).color(theme::TEXT));
    }
    failures(ui, report);
    ui.add_space(theme::GRID * 3.0);
    if button_row(ui, WIDTH, &[("Done", true)]).is_some() {
        state.dialog = None;
        state.import.phase = ImportPhase::Choosing;
    }
}

fn failed(ui: &mut Ui, state: &mut AppState, message: &str) {
    title(ui, WIDTH, "Import Failed");
    ui.label(RichText::new(message).color(theme::ERROR));
    ui.add_space(theme::GRID * 3.0);
    if button_row(ui, WIDTH, &[("Back", true)]).is_some() {
        state.import.phase = ImportPhase::Choosing;
    }
}

pub(super) fn show(ui: &mut Ui, state: &mut AppState) {
    ui.set_width(WIDTH);
    match state.import.phase.clone() {
        ImportPhase::Choosing => choosing(ui, state),
        ImportPhase::Running(_) => running(ui, state),
        ImportPhase::Finished(report) => finished(ui, state, &report),
        ImportPhase::Failed(message) => failed(ui, state, &message),
    }
}
