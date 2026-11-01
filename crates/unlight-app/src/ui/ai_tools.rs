//! Select Subject and Select Sky in the Masks panel, including the first-run question about
//! downloading a model.

use crate::{
    ai_tools::{AiTools, ToolStatus, tool_name},
    ui::{
        theme,
        widgets::controls::{note, progress_bar, text_button},
    },
};
use eframe::egui::Ui;
use unlight_ai::Task;
use unlight_catalog::VariantId;

const TOOLS: [Task; 2] = [Task::Subject, Task::Sky];

fn cancel_row(ui: &mut Ui, tools: &mut AiTools, width: f32) {
    if text_button(ui, "Cancel", false, width).clicked() {
        tools.cancel();
    }
}

fn tool_row(ui: &mut Ui, tools: &mut AiTools, task: Task, variant: Option<VariantId>, width: f32) {
    let name = tool_name(task);
    match tools.status(task) {
        ToolStatus::Ready => {
            let pressed = ui.add_enabled_ui(variant.is_some(), |ui| text_button(ui, name, false, width).clicked());
            if let (true, Some(variant)) = (pressed.inner, variant) {
                tools.select(task, variant);
            }
        }
        ToolStatus::NeedsModel => {
            if text_button(ui, name, false, width).clicked() {
                tools.ask_to_download(task);
            }
        }
        ToolStatus::Asking { prompt } => {
            note(ui, &prompt);
            ui.horizontal(|ui| {
                if text_button(ui, "Download", false, width / 2.0).clicked() {
                    tools.download(task);
                }
                if text_button(ui, "Not now", false, width / 2.0).clicked() {
                    tools.decline_download();
                }
            });
        }
        ToolStatus::Downloading(progress) => {
            note(ui, &format!("Downloading the model for {name}…"));
            let fraction = (progress.total > 0).then(|| progress.done as f32 / progress.total as f32);
            progress_bar(ui, fraction, width);
            cancel_row(ui, tools, width);
        }
        ToolStatus::Working => {
            note(ui, &format!("{name}…"));
            progress_bar(ui, None, width);
            cancel_row(ui, tools, width);
        }
        ToolStatus::Unavailable => note(ui, &format!("{name} isn't available.")),
    }
}

pub fn show(ui: &mut Ui, tools: &mut AiTools, variant: Option<VariantId>) {
    let width = ui.available_width();
    for task in TOOLS {
        tool_row(ui, tools, task, variant, width);
    }
    if let Some(message) = &tools.message {
        note(ui, message);
    }
    ui.add_space(theme::GRID);
}
