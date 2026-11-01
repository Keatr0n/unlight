//! The filter bar above the grid. Every control edits the library's filter directly, so what
//! it shows and what's on screen can't disagree.

use crate::{
    state::{AppState, DateForm, dates::date_span, library::next_flag_filter},
    ui::{
        filmstrip::label_color,
        theme,
        widgets::{
            controls::{bare_dropdown, icon_button},
            icons::{self, Icon},
        },
    },
};
use eframe::egui::{self, Frame, Panel, Rect, Role, Sense, TextEdit, Ui, WidgetInfo, pos2, vec2};
use unlight_catalog::{
    Comparison, DateRange, Filter, Flag, KeywordId, Label, PhotoStatus, RatingFilter, SortKey,
};

const STAR_SPACING: f32 = 15.0;
const SORT_KEYS: [(SortKey, &str); 4] = [
    (SortKey::CaptureTime, "Capture Time"),
    (SortKey::FileName, "File Name"),
    (SortKey::Rating, "Rating"),
    (SortKey::EditTime, "Edit Time"),
];
const STATUSES: [(Option<PhotoStatus>, &str); 4] = [
    (None, "Any Status"),
    (Some(PhotoStatus::Available), "Available"),
    (Some(PhotoStatus::Missing), "Missing"),
    (Some(PhotoStatus::Unsupported), "Unsupported"),
];

fn text_search(ui: &mut Ui, filter: &mut Filter) {
    let mut text = filter.text.clone().unwrap_or_default();
    let field = TextEdit::singleline(&mut text)
        .desired_width(150.0)
        .hint_text("Search");
    if ui.add(field).changed() {
        filter.text = (!text.trim().is_empty()).then_some(text);
    }
}

fn rating_stars(ui: &mut Ui, filter: &mut Filter) {
    let current = filter.rating.map_or(0, |rating| rating.value);
    let (rect, response) =
        ui.allocate_exact_size(vec2(STAR_SPACING * 5.0, theme::ROW_HEIGHT), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Slider, true, "Minimum rating"));
    let hovered = response
        .hover_pos()
        .map(|pos| ((pos.x - rect.left()) / STAR_SPACING).ceil() as u8);
    if response.clicked()
        && let Some(stars) = hovered
    {
        filter.rating = (stars != current).then_some(RatingFilter {
            comparison: Comparison::GreaterOrEqual,
            value: stars.min(5),
        });
    }
    for star in 1..=5u8 {
        let center = pos2(
            rect.left() + (f32::from(star) - 0.5) * STAR_SPACING,
            rect.center().y,
        );
        let lit = star <= hovered.unwrap_or(current);
        let color = if lit { theme::TEXT } else { theme::TRACK };
        icons::star(ui.painter(), center, 5.0, color);
    }
    let _ = response.on_hover_text("Show this rating and above");
}

fn flag_chips(ui: &mut Ui, filter: &mut Filter) {
    let chips = [
        (Flag::Pick, Icon::Flag, "Picks"),
        (Flag::Reject, Icon::Close, "Rejected"),
    ];
    for (flag, icon, label) in chips {
        if icon_button(ui, icon, label, filter.flag == Some(flag)).clicked() {
            filter.flag = next_flag_filter(filter.flag, flag);
        }
    }
}

fn label_swatches(ui: &mut Ui, filter: &mut Filter) {
    for label in [
        Label::Red,
        Label::Yellow,
        Label::Green,
        Label::Blue,
        Label::Purple,
    ] {
        let (rect, response) = ui.allocate_exact_size(vec2(16.0, theme::ROW_HEIGHT), Sense::click());
        let selected = filter.label == Some(label);
        let name = crate::state::library::label_name(label);
        response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, name));
        if response.clicked() {
            filter.label = (!selected).then_some(label);
        }
        let swatch = Rect::from_center_size(rect.center(), vec2(12.0, 12.0));
        let color = label_color(label).unwrap_or(theme::TRACK);
        let alpha = if selected || filter.label.is_none() {
            1.0
        } else {
            0.35
        };
        ui.painter().rect_filled(swatch, 2.0, color.gamma_multiply(alpha));
        if selected {
            ui.painter().rect_stroke(
                swatch.expand(1.5),
                3.0,
                (1.0, theme::TEXT),
                egui::StrokeKind::Inside,
            );
        }
    }
}

fn text_dropdown(
    ui: &mut Ui,
    name: &'static str,
    value: &mut Option<String>,
    choices: &[String],
    width: f32,
) {
    let mut options: Vec<(Option<String>, &str)> = vec![(None, name)];
    options.extend(
        choices
            .iter()
            .map(|choice| (Some(choice.clone()), choice.as_str())),
    );
    bare_dropdown(ui, name, value, &options, width);
}

fn keyword_dropdown(ui: &mut Ui, value: &mut Option<KeywordId>, keywords: &[unlight_catalog::Keyword]) {
    let mut options: Vec<(Option<KeywordId>, &str)> = vec![(None, "Keyword")];
    options.extend(
        keywords
            .iter()
            .map(|keyword| (Some(keyword.id), keyword.name.as_str())),
    );
    bare_dropdown(ui, "Keyword", value, &options, 96.0);
}

/// Dates stay as typed while they don't parse, and only reach the filter once they do.
fn date_fields(ui: &mut Ui, form: &mut DateForm, filter: &mut Filter) {
    let from = ui.add(
        TextEdit::singleline(&mut form.from)
            .desired_width(84.0)
            .hint_text("From"),
    );
    let until = ui.add(
        TextEdit::singleline(&mut form.until)
            .desired_width(84.0)
            .hint_text("To"),
    );
    if from.changed() || until.changed() {
        filter.captured = parse_dates(form);
    }
}

fn parse_dates(form: &DateForm) -> Option<DateRange> {
    let from = date_span(&form.from).map(|span| span.0);
    let until = date_span(&form.until).map(|span| span.1);
    (from.is_some() || until.is_some()).then_some(DateRange { from, until })
}

/// Status and order, which sit with the view controls under the grid.
pub fn sort_controls(ui: &mut Ui, state: &mut AppState) {
    let mut filter = state.library.filter().clone();
    let mut sort = state.library.sort();
    bare_dropdown(ui, "Status", &mut filter.status, &STATUSES, 96.0);
    bare_dropdown(ui, "Sort", &mut sort.key, &SORT_KEYS, 104.0);
    let icon = if sort.descending {
        Icon::SortDown
    } else {
        Icon::SortUp
    };
    if icon_button(ui, icon, "Reverse the order", false).clicked() {
        sort.descending = !sort.descending;
    }
    state.library.set_filter(filter);
    state.library.set_sort(sort);
}

pub fn show(ui: &mut Ui, state: &mut AppState) {
    let frame = Frame::new()
        .fill(theme::PANEL)
        .inner_margin(egui::Margin::symmetric(8, 4));
    Panel::top("library filter")
        .show_separator_line(false)
        .frame(frame)
        .show(ui, |ui| {
            let mut filter = state.library.filter().clone();
            let lists = &state.lists;
            let form = &mut state.date_form;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(theme::GRID * 2.0, theme::GRID);
                text_search(ui, &mut filter);
                rating_stars(ui, &mut filter);
                flag_chips(ui, &mut filter);
                label_swatches(ui, &mut filter);
                text_dropdown(ui, "Camera", &mut filter.camera, &lists.cameras, 104.0);
                text_dropdown(ui, "Lens", &mut filter.lens, &lists.lenses, 104.0);
                keyword_dropdown(ui, &mut filter.keyword, &lists.keywords);
                date_fields(ui, form, &mut filter);
            });
            state.library.set_filter(filter);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_typed_dates_leave_the_filter_alone() {
        let form = DateForm {
            from: "202".into(),
            until: String::new(),
        };
        assert_eq!(
            parse_dates(&form).and_then(|range| range.from),
            date_span("202").map(|span| span.0)
        );
        let form = DateForm {
            from: "soon".into(),
            until: "later".into(),
        };
        assert_eq!(parse_dates(&form), None);
    }

    #[test]
    fn a_range_runs_to_the_end_of_its_last_day() {
        let form = DateForm {
            from: "2024-03-01".into(),
            until: "2024-03-31".into(),
        };
        let range = parse_dates(&form).unwrap();
        assert_eq!(range.until, date_span("2024-04-01").map(|span| span.0));
    }
}
