//! The right side of the Library: keywords and what the file says about itself.

use crate::{
    action::Action,
    state::{AppState, dates::format_capture, write::Write},
    ui::{
        filmstrip::label_color,
        left_panel::collapsible,
        theme,
        widgets::{
            controls::{note, text_button},
            histogram::exposure_line,
            icons,
        },
    },
};
use eframe::egui::{
    self, Align2, Frame, Key, Panel, Rect, Role, ScrollArea, Sense, TextEdit, Ui, WidgetInfo, pos2,
    text::{LayoutJob, TextWrapping},
    vec2,
};
use unlight_catalog::{Label, PhotoId, PhotoStatus};

const LABEL_COLUMN: f32 = 84.0;
const STAR_SPACING: f32 = 16.0;

/// Splits what was typed into keywords.
fn parse_keywords(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

fn keyword_entry(ui: &mut Ui, state: &mut AppState) {
    let width = ui.available_width();
    let field = TextEdit::singleline(&mut state.keyword_entry)
        .desired_width(width)
        .hint_text("Add keywords");
    let response = ui.add(field);
    if response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
        let names = parse_keywords(&state.keyword_entry);
        if !names.is_empty() {
            state.writes.push(Write::AddKeywords {
                variants: state.library.selection(),
                names,
            });
            state.keyword_entry.clear();
        }
    }
}

fn keywords_section(ui: &mut Ui, state: &mut AppState) {
    keyword_entry(ui, state);
    ui.add_space(theme::GRID);
    let mut remove = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(theme::GRID, theme::GRID);
        for keyword in &state.keywords {
            let chip = egui::Button::new(egui::RichText::new(&keyword.name).font(theme::small()));
            if ui.add(chip).on_hover_text("Click to remove").clicked() {
                remove = Some(keyword.id);
            }
        }
    });
    if state.keywords.is_empty() {
        note(ui, "No keywords on this photo.");
    }
    if let Some(keyword) = remove {
        state.writes.push(Write::RemoveKeyword {
            variants: state.library.selection(),
            keyword,
        });
        state.keywords.retain(|existing| existing.id != keyword);
    }
}

fn detail_row(ui: &mut Ui, label: &str, value: &str) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        label,
        theme::small(),
        theme::TEXT_FAINT,
    );
    let mut job = LayoutJob::simple_singleline(value.to_owned(), theme::small(), theme::TEXT);
    job.wrap = TextWrapping::truncate_at_width(rect.width() - LABEL_COLUMN);
    let galley = painter.layout_job(job);
    let elided = galley.elided;
    let top = rect.center().y - galley.size().y * 0.5;
    painter.galley(pos2(rect.left() + LABEL_COLUMN, top), galley, theme::TEXT);
    if elided {
        response.on_hover_text(value);
    }
}

fn rating_row(ui: &mut Ui, state: &mut AppState) {
    let rating = state.focused().map_or(0, |row| row.rating);
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    ui.painter().text(
        row.left_center(),
        Align2::LEFT_CENTER,
        "Rating",
        theme::small(),
        theme::TEXT_FAINT,
    );
    let stars = Rect::from_min_size(
        row.min + vec2(LABEL_COLUMN, 0.0),
        vec2(STAR_SPACING * 5.0, row.height()),
    );
    let response = ui.interact(stars, ui.scope_id().with("rating stars"), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Slider, true, "Rating"));
    let hovered = response
        .hover_pos()
        .map(|pos| ((pos.x - stars.left()) / STAR_SPACING).ceil() as u8);
    if response.clicked()
        && let Some(stars) = hovered
    {
        state.apply(Action::SetRating(if stars == rating { 0 } else { stars.min(5) }));
    }
    for star in 1..=5u8 {
        let center = pos2(
            stars.left() + (f32::from(star) - 0.5) * STAR_SPACING,
            stars.center().y,
        );
        let lit = star <= hovered.unwrap_or(rating);
        icons::star(
            ui.painter(),
            center,
            5.0,
            if lit { theme::TEXT } else { theme::TRACK },
        );
    }
}

fn label_row(ui: &mut Ui, state: &mut AppState) {
    let current = state.focused().map_or(Label::None, |row| row.label);
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    ui.painter().text(
        row.left_center(),
        Align2::LEFT_CENTER,
        "Label",
        theme::small(),
        theme::TEXT_FAINT,
    );
    for (index, label) in [
        Label::Red,
        Label::Yellow,
        Label::Green,
        Label::Blue,
        Label::Purple,
    ]
    .into_iter()
    .enumerate()
    {
        let at = pos2(
            row.left() + LABEL_COLUMN + index as f32 * 18.0 + 8.0,
            row.center().y,
        );
        let rect = Rect::from_center_size(at, vec2(16.0, 16.0));
        let name = crate::state::library::label_name(label);
        let id = ui.scope_id().with(("label swatch", index));
        let response = ui.interact(rect, id, Sense::click());
        response.widget_info(|| {
            WidgetInfo::selected(Role::Button, true, current == label, format!("{name} label"))
        });
        if response.clicked() {
            state.apply(Action::ToggleLabel(label));
        }
        let swatch = Rect::from_center_size(at, vec2(12.0, 12.0));
        ui.painter()
            .rect_filled(swatch, 2.0, label_color(label).unwrap_or(theme::TRACK));
        if current == label {
            ui.painter().rect_stroke(
                swatch.expand(1.5),
                3.0,
                (1.0, theme::TEXT),
                egui::StrokeKind::Inside,
            );
        }
    }
}

fn metadata_section(ui: &mut Ui, state: &mut AppState) {
    let Some(photo) = state.photo.clone() else {
        note(ui, "Select a photo to see its details.");
        return;
    };
    rating_row(ui, state);
    label_row(ui, state);
    ui.add_space(theme::GRID);
    let name = photo
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    detail_row(ui, "File", &name);
    if let Some(parent) = photo.path.parent() {
        detail_row(ui, "Folder", &parent.display().to_string());
    }
    detail_row(ui, "Size", &format!("{:.1} MB", photo.size as f64 / 1_000_000.0));
    detail_row(ui, "Dimensions", &format!("{} × {}", photo.width, photo.height));
    if let Some(time) = photo.capture_time {
        detail_row(ui, "Captured", &format_capture(time));
    }
    detail_row(ui, "Camera", &photo.camera);
    detail_row(ui, "Lens", &photo.lens);
    match photo.status {
        PhotoStatus::Available => {}
        PhotoStatus::Missing => detail_row(ui, "Status", "File is missing"),
        PhotoStatus::Unsupported => {
            let reason = photo.status_detail.as_deref().unwrap_or("Not supported");
            detail_row(ui, "Status", reason);
        }
    }
    ui.add_space(theme::GRID);
    exposure_line(ui, &crate::ui::right_panel::exposure_summary(&photo));
    if state.lists.sidecar_conflicts.contains(&photo.id) {
        sidecar_conflict(ui, state, photo.id);
    }
}

fn sidecar_conflict(ui: &mut Ui, state: &mut AppState, photo: PhotoId) {
    ui.add_space(theme::GRID * 2.0);
    let warning = egui::RichText::new("The sidecar changed outside Unlight").font(theme::small());
    ui.label(warning.color(theme::ERROR));
    ui.add_space(theme::GRID);
    let gap = theme::GRID;
    let width = (ui.available_width() - gap) * 0.5;
    let mut choice = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        if text_button(ui, "Keep Catalog", false, width).clicked() {
            choice = Some(true);
        }
        if text_button(ui, "Use Sidecar", false, width).clicked() {
            choice = Some(false);
        }
    });
    if let Some(keep_catalog) = choice {
        state.writes.push(Write::ResolveSidecar { photo, keep_catalog });
    }
}

pub fn show(ui: &mut Ui, state: &mut AppState) {
    let panel = Panel::right("library details")
        .exact_size(theme::RIGHT_PANEL_WIDTH)
        .show_separator_line(false)
        .frame(Frame::new().fill(theme::PANEL));
    let mut open = state.panels.right;
    panel.show_collapsible(ui, &mut open, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ScrollArea::vertical()
            .id_salt("details scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                collapsible(ui, "Metadata", true, |ui| metadata_section(ui, state));
                collapsible(ui, "Keywording", true, |ui| keywords_section(ui, state));
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_split_on_commas_and_ignore_blanks() {
        assert_eq!(parse_keywords(" cats, ,dogs ,"), ["cats", "dogs"]);
        assert!(parse_keywords("  ").is_empty());
    }
}
