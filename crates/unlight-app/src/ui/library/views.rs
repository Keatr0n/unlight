//! The views of whole photos: loupe, compare and survey. They show previews, and fall back on
//! the thumbnail until the preview lands.

use crate::{
    app::App,
    state::{SURVEY_LIMIT, library::title},
    ui::{
        filmstrip::paint_badges,
        theme,
        viewport::paint_preview,
        widgets::{controls::icon_button, icons::Icon},
    },
};
use eframe::egui::{
    self, Align2, Rect, Role, Sense, StrokeKind, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use unlight_catalog::LibraryRow;

const MARGIN: f32 = 16.0;
const GAP: f32 = 8.0;
const CAPTION: f32 = 24.0;

/// Draws a photo in `area` and returns where it ended up.
fn paint_photo(ui: &Ui, area: Rect, row: &LibraryRow, app: &App) -> Rect {
    let Some(texture) = app.darkroom.preview(&row.path) else {
        ui.painter().rect_filled(area, 2.0, theme::PANEL);
        return area;
    };
    paint_preview(ui, area, texture, row.turned)
}

fn caption(ui: &Ui, area: Rect, row: &LibraryRow, role: Option<&str>) {
    let text = match role {
        Some(role) => format!("{role}   {}", title(row)),
        None => title(row),
    };
    let painter = ui.painter_at(area);
    painter.text(
        area.left_center(),
        Align2::LEFT_CENTER,
        text,
        theme::body(),
        theme::TEXT_DIM,
    );
}

fn no_photo(ui: &Ui, area: Rect, hint: &str) {
    ui.painter().text(
        area.center(),
        Align2::CENTER_CENTER,
        hint,
        theme::body(),
        theme::TEXT_FAINT,
    );
}

pub fn loupe(ui: &mut Ui, app: &mut App) {
    let area = ui.available_rect_before_wrap().shrink(MARGIN);
    let Some(row) = app.state.focused().cloned() else {
        no_photo(ui, area, "Select a photo to see it here");
        return;
    };
    app.darkroom.want_preview(&row);
    let picture = Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - CAPTION));
    let rect = paint_photo(ui, picture, &row, app);
    paint_badges(ui, rect, &row);
    let line = Rect::from_min_max(pos2(area.left(), area.bottom() - CAPTION), area.max);
    caption(ui, line, &row, None);
}

/// The two photos to compare: the focus, and whichever else is selected or comes next.
fn compare_pair(app: &App) -> Vec<LibraryRow> {
    let mut rows = app.state.library.selected_rows();
    if rows.len() < 2 {
        let next = app.state.library.neighbours(1).first().map(|row| (*row).clone());
        rows.extend(next);
    }
    rows.truncate(2);
    rows
}

pub fn compare(ui: &mut Ui, app: &mut App) {
    let area = ui.available_rect_before_wrap().shrink(MARGIN);
    let rows = compare_pair(app);
    if rows.is_empty() {
        no_photo(ui, area, "Select two photos to compare them");
        return;
    }
    let width = (area.width() - GAP) * 0.5;
    for (index, row) in rows.iter().enumerate() {
        app.darkroom.want_preview(row);
        let left = area.left() + index as f32 * (width + GAP);
        let pane = Rect::from_min_size(pos2(left, area.top()), vec2(width, area.height()));
        let picture = Rect::from_min_max(pane.min, pos2(pane.right(), pane.bottom() - CAPTION));
        let rect = paint_photo(ui, picture, row, app);
        paint_badges(ui, rect, row);
        let line = Rect::from_min_max(pos2(pane.left(), pane.bottom() - CAPTION), pane.max);
        caption(
            ui,
            line,
            row,
            Some(if index == 0 { "Select" } else { "Candidate" }),
        );
    }
}

/// How many columns give the largest pictures for a count of 3:2 photos.
fn survey_columns(count: usize, area: egui::Vec2) -> usize {
    (1..=count.max(1))
        .max_by_key(|columns| {
            let rows = count.div_ceil(*columns).max(1);
            let cell = vec2(area.x / *columns as f32, area.y / rows as f32);
            (cell.x.min(cell.y * 1.5) * 100.0) as u32
        })
        .unwrap_or(1)
}

pub fn survey(ui: &mut Ui, app: &mut App) {
    let area = ui.available_rect_before_wrap().shrink(MARGIN);
    let mut rows = app.state.library.selected_rows();
    rows.truncate(SURVEY_LIMIT);
    if rows.is_empty() {
        no_photo(ui, area, "Select some photos to survey them");
        return;
    }
    let columns = survey_columns(rows.len(), area.size());
    let lines = rows.len().div_ceil(columns);
    let cell = vec2(
        (area.width() - GAP * (columns as f32 - 1.0)) / columns as f32,
        (area.height() - GAP * (lines as f32 - 1.0)) / lines as f32,
    );
    let focused = app.state.focused().map(|row| row.variant);
    let (mut focus, mut dismiss) = (None, None);
    for (index, row) in rows.iter().enumerate() {
        app.darkroom.want_preview(row);
        let at = vec2(
            (index % columns) as f32 * (cell.x + GAP),
            (index / columns) as f32 * (cell.y + GAP),
        );
        let rect = Rect::from_min_size(area.min + at, cell);
        let id = ui.scope_id().with(("survey", row.variant.0));
        let response = ui.interact(rect, id, Sense::click());
        response.widget_info(|| WidgetInfo::labeled(Role::ListItem, true, title(row)));
        ui.painter().rect_filled(rect, 3.0, theme::PANEL);
        let picture = Rect::from_min_max(rect.min + vec2(4.0, 4.0), rect.max - vec2(4.0, CAPTION));
        let shown = paint_photo(ui, picture, row, app);
        paint_badges(ui, shown, row);
        let line = Rect::from_min_max(pos2(rect.left() + 6.0, rect.bottom() - CAPTION), rect.max);
        caption(ui, line, row, None);
        if focused == Some(row.variant) {
            ui.painter()
                .rect_stroke(rect, 3.0, (1.5, theme::HANDLE), StrokeKind::Inside);
        }
        if response.clicked() {
            focus = Some(row.variant);
        }
        let close = Rect::from_min_size(pos2(rect.right() - 32.0, rect.top() + 4.0), vec2(28.0, 24.0));
        let mut button = ui.new_child(UiBuilder::new().max_rect(close));
        let label = format!("Remove {} from the survey", title(row));
        if icon_button(&mut button, Icon::Close, &label, false).clicked() {
            dismiss = Some(row.variant);
        }
    }
    if let Some(id) = focus {
        app.state.library.focus_variant(id);
    }
    if let Some(id) = dismiss {
        app.state.library.deselect(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_area_lays_photos_out_in_a_row() {
        assert_eq!(survey_columns(3, vec2(1800.0, 300.0)), 3);
    }

    #[test]
    fn six_photos_on_a_screen_go_three_by_two() {
        assert_eq!(survey_columns(6, vec2(1500.0, 800.0)), 3);
    }
}
