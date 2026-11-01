//! The export dialog: what to write and where, then how it's going.

use super::{button_row, folder_row, labelled_field, title};
use crate::{
    action::Action,
    state::{
        AppState, Dialog, Effect, FolderUse,
        transfer::{ExportFormat, ExportRun, count_of},
    },
    ui::{
        theme,
        widgets::controls::{check_row, dropdown, note, progress_bar},
    },
};
use eframe::egui::{RichText, ScrollArea, Ui};
use unlight_output::{
    CollisionPolicy, ExportSettings, FormatSettings, MetadataPolicy, OutputSharpening, OutputSpace,
    ResizeMode, ResizeSettings, SharpenAmount, SharpenMedia,
};

const WIDTH: f32 = 480.0;
const FAILURE_LIST_HEIGHT: f32 = 160.0;
const DEFAULT_LONG_EDGE: u32 = 2048;
const SPACES: [(OutputSpace, &str); 4] = [
    (OutputSpace::Srgb, "sRGB"),
    (OutputSpace::DisplayP3, "Display P3"),
    (OutputSpace::AdobeRgb, "Adobe RGB"),
    (OutputSpace::ProPhoto, "ProPhoto RGB"),
];
const METADATA: [(MetadataPolicy, &str); 3] = [
    (MetadataPolicy::All, "All"),
    (MetadataPolicy::CopyrightOnly, "Copyright only"),
    (MetadataPolicy::None, "None"),
];
const COLLISIONS: [(CollisionPolicy, &str); 3] = [
    (CollisionPolicy::Unique, "Add a number"),
    (CollisionPolicy::Overwrite, "Overwrite"),
    (CollisionPolicy::Skip, "Skip"),
];
const SHARPENING: [(Option<SharpenMedia>, &str); 4] = [
    (None, "None"),
    (Some(SharpenMedia::Screen), "Screen"),
    (Some(SharpenMedia::Matte), "Matte paper"),
    (Some(SharpenMedia::Glossy), "Glossy paper"),
];

fn format_row(ui: &mut Ui, settings: &mut ExportSettings) {
    let current = ExportFormat::of(&settings.format);
    let mut format = current;
    dropdown(ui, "Format", &mut format, &ExportFormat::ALL, WIDTH);
    if format != current {
        settings.format = format.default_settings();
    }
}

fn quality_of(format: &mut FormatSettings) -> Option<&mut u8> {
    match format {
        FormatSettings::Jpeg(settings) => Some(&mut settings.quality),
        FormatSettings::Avif(settings) => Some(&mut settings.quality),
        _ => None,
    }
}

fn long_edge_row(ui: &mut Ui, settings: &mut ExportSettings) {
    let mut limited = settings.resize.is_some();
    if check_row(ui, &mut limited, "Limit the long edge").clicked() {
        settings.resize = limited.then_some(ResizeSettings {
            mode: ResizeMode::LongEdge(DEFAULT_LONG_EDGE),
            allow_upscale: false,
        });
    }
    if let Some(ResizeSettings {
        mode: ResizeMode::LongEdge(edge),
        ..
    }) = &mut settings.resize
    {
        let mut text = edge.to_string();
        labelled_field(ui, "Pixels", &mut text, WIDTH);
        if let Ok(value) = text.trim().parse::<u32>()
            && value > 0
        {
            *edge = value;
        }
    }
}

fn sharpening_row(ui: &mut Ui, settings: &mut ExportSettings) {
    let current = settings.sharpening.map(|sharpening| sharpening.media);
    let mut media = current;
    dropdown(ui, "Sharpen for", &mut media, &SHARPENING, WIDTH);
    if media != current {
        settings.sharpening = media.map(|media| OutputSharpening {
            media,
            amount: SharpenAmount::Standard,
        });
    }
}

fn quality_row(ui: &mut Ui, settings: &mut ExportSettings) {
    let Some(quality) = quality_of(&mut settings.format) else {
        return;
    };
    let mut text = quality.to_string();
    labelled_field(ui, "Quality", &mut text, WIDTH);
    if let Ok(value) = text.trim().parse::<u8>()
        && value <= 100
    {
        *quality = value;
    }
}

fn form(ui: &mut Ui, state: &mut AppState) {
    let count = state.library.selection().len();
    title(
        ui,
        WIDTH,
        &format!("Export {}", count_of(count, "Photo", "Photos")),
    );
    let mut browse = false;
    let mut start = false;
    let mut close = false;
    if let Some(form) = &mut state.export_form {
        let names: Vec<(usize, &str)> = form
            .presets
            .iter()
            .enumerate()
            .map(|(index, preset)| (index, preset.name.as_str()))
            .collect();
        let mut preset = form.preset;
        dropdown(ui, "Preset", &mut preset, &names, WIDTH);
        if preset != form.preset {
            form.choose_preset(preset);
        }
        browse = folder_row(ui, &mut form.directory, WIDTH);
        ui.add_space(theme::GRID);

        let settings = &mut form.settings;
        format_row(ui, settings);
        quality_row(ui, settings);
        dropdown(ui, "Colour space", &mut settings.color_space, &SPACES, WIDTH);
        long_edge_row(ui, settings);
        sharpening_row(ui, settings);
        dropdown(ui, "Metadata", &mut settings.metadata.policy, &METADATA, WIDTH);
        labelled_field(ui, "File name", &mut settings.filename_template, WIDTH);
        note(ui, "Tokens: {name} {date} {seq} {camera} {rating}");
        ui.add_space(theme::GRID);
        dropdown(ui, "If it exists", &mut settings.collision, &COLLISIONS, WIDTH);
        ui.add_space(theme::GRID * 3.0);

        let ready = !form.directory.trim().is_empty() && form.settings.validate().is_ok();
        match button_row(ui, WIDTH, &[("Cancel", false), ("Export", ready)]) {
            Some(0) => close = true,
            Some(1) if ready => start = true,
            _ => {}
        }
    }
    if browse {
        state.effects.push(Effect::ChooseFolder(FolderUse::Export));
    }
    if start {
        state.effects.push(Effect::StartExport);
        state.export_run = Some(ExportRun {
            total: count,
            ..ExportRun::default()
        });
    }
    if close {
        state.apply(Action::Cancel);
    }
}

fn failure_list(ui: &mut Ui, run: &ExportRun) {
    if run.failures.is_empty() {
        return;
    }
    ui.add_space(theme::GRID * 2.0);
    ScrollArea::vertical()
        .max_height(FAILURE_LIST_HEIGHT)
        .show(ui, |ui| {
            for failure in &run.failures {
                ui.label(RichText::new(&failure.name).color(theme::ERROR));
                ui.label(
                    RichText::new(&failure.message)
                        .font(theme::small())
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(theme::GRID);
            }
        });
}

fn progress(ui: &mut Ui, state: &mut AppState, run: &ExportRun) {
    if run.finished {
        title(ui, WIDTH, "Export Finished");
        ui.label(RichText::new(run.summary()).color(theme::TEXT));
    } else {
        title(ui, WIDTH, "Exporting");
        let text = format!(
            "{} of {}: {}",
            (run.done + 1).min(run.total),
            run.total,
            run.current
        );
        ui.label(RichText::new(text).color(theme::TEXT_DIM));
        ui.add_space(theme::GRID * 2.0);
        progress_bar(ui, Some(run.done as f32 / run.total.max(1) as f32), WIDTH);
    }
    failure_list(ui, run);
    ui.add_space(theme::GRID * 3.0);
    if run.finished {
        if button_row(ui, WIDTH, &[("Done", true)]).is_some() {
            state.export_run = None;
            state.dialog = None;
        }
    } else {
        match button_row(
            ui,
            WIDTH,
            &[("Run in Background", false), ("Cancel Export", false)],
        ) {
            Some(0) => state.dialog = None,
            Some(1) => state.effects.push(Effect::CancelExport),
            _ => {}
        }
    }
}

pub(super) fn show(ui: &mut Ui, state: &mut AppState) {
    ui.set_width(WIDTH);
    match state.export_run.clone() {
        Some(run) => progress(ui, state, &run),
        None if state.dialog == Some(Dialog::Export) => form(ui, state),
        None => {}
    }
}
