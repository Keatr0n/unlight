//! The strip of thumbnails along the bottom.

use crate::{
    darkroom::Darkroom,
    state::{
        AppState,
        library::{ClickKind, title},
        transfer::count_of,
    },
    ui::{
        theme,
        widgets::{
            controls::divider,
            icons::{self, Icon},
        },
    },
};
use eframe::egui::{
    self, Align2, Color32, Frame, Modifiers, Panel, Rect, Role, ScrollArea, Sense, StrokeKind, Ui,
    WidgetInfo, pos2, scroll_area::ScrollBarVisibility, vec2,
};
use unlight_catalog::{Flag, Label, LibraryRow, PhotoStatus};

const CELL_WIDTH: f32 = 116.0;
const CELL_HEIGHT: f32 = 76.0;
const CELL_GAP: f32 = 4.0;

pub fn label_color(label: Label) -> Option<Color32> {
    match label {
        Label::None => None,
        Label::Red => Some(theme::LABEL_RED),
        Label::Yellow => Some(theme::LABEL_YELLOW),
        Label::Green => Some(theme::LABEL_GREEN),
        Label::Blue => Some(theme::LABEL_BLUE),
        Label::Purple => Some(theme::LABEL_PURPLE),
    }
}

/// Fits an image of `size` inside `area`, centred.
pub fn fitted(area: Rect, size: egui::Vec2) -> Rect {
    let scale = (area.width() / size.x.max(1.0)).min(area.height() / size.y.max(1.0));
    Rect::from_center_size(area.center(), size * scale)
}

pub fn click_kind(modifiers: Modifiers) -> ClickKind {
    if modifiers.shift {
        ClickKind::Extend
    } else if modifiers.command {
        ClickKind::Toggle
    } else {
        ClickKind::Replace
    }
}

/// Draws a thumbnail's flag, stars, label and any problem on top of it.
pub fn paint_badges(ui: &Ui, cell: Rect, row: &LibraryRow) {
    let painter = ui.painter();
    let corner = Rect::from_center_size(cell.min + vec2(10.0, 10.0), vec2(14.0, 14.0));
    match row.flag {
        Flag::Pick => icons::paint(painter, corner, Icon::Flag, theme::PICK),
        Flag::Reject => icons::paint(painter, corner, Icon::Close, theme::REJECT),
        Flag::Unflagged => {}
    }
    if row.status != PhotoStatus::Available {
        let warning = Rect::from_center_size(pos2(cell.right() - 12.0, cell.top() + 12.0), vec2(16.0, 16.0));
        painter.circle_filled(warning.center(), 10.0, Color32::from_black_alpha(150));
        icons::paint(painter, warning, Icon::Warning, theme::ERROR);
    }
    if row.copy_name.is_some() {
        let corner = pos2(cell.right() - 3.0, cell.bottom() - 3.0);
        let points = [corner, corner - vec2(10.0, 0.0), corner - vec2(0.0, 10.0)];
        crate::ui::widgets::paint::triangle(painter, points, theme::TEXT);
    }
    for star in 0..row.rating {
        let center = pos2(cell.left() + 9.0 + f32::from(star) * 10.0, cell.bottom() - 9.0);
        painter.circle_filled(center, 6.0, Color32::from_black_alpha(90));
        icons::star(painter, center, 4.0, theme::TEXT);
    }
    if let Some(color) = label_color(row.label) {
        let bar = Rect::from_min_max(pos2(cell.left(), cell.bottom() - 3.0), cell.right_bottom());
        painter.rect_filled(bar, 0.0, color);
    }
}

/// Fills a thumbnail's area with its picture, or a calm placeholder while it loads.
pub fn paint_thumbnail(ui: &Ui, area: Rect, row: &LibraryRow, darkroom: &Darkroom) {
    if let Some(thumbnail) = darkroom.thumbnail(&row.path) {
        let image = fitted(area, thumbnail.size_vec2());
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        ui.painter().image(thumbnail.id(), image, uv, Color32::WHITE);
    }
}

struct CellResult {
    clicked: bool,
    double_clicked: bool,
}

fn cell(
    ui: &mut Ui,
    rect: Rect,
    row: &LibraryRow,
    darkroom: &Darkroom,
    focused: bool,
    selected: bool,
) -> CellResult {
    let id = ui.scope_id().with(("film cell", row.variant.0));
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::ListItem, true, selected, title(row)));
    let fill = if selected {
        theme::PANEL_RAISED
    } else if response.hovered() {
        theme::PANEL_HOVER
    } else {
        theme::PANEL_DEEP
    };
    ui.painter().rect_filled(rect, 3.0, fill);
    let picture_area = rect.shrink(2.0);
    paint_thumbnail(ui, picture_area, row, darkroom);
    paint_badges(ui, picture_area, row);
    if focused {
        ui.painter()
            .rect_stroke(rect, 3.0, (1.5, theme::HANDLE), StrokeKind::Inside);
    }
    let result = CellResult {
        clicked: response.clicked(),
        double_clicked: response.double_clicked(),
    };
    response.on_hover_text(title(row));
    result
}

pub fn show(ui: &mut Ui, state: &mut AppState, darkroom: &mut Darkroom) {
    if !state.panels.filmstrip {
        return;
    }
    let frame = Frame::new().fill(theme::PANEL).inner_margin(egui::Margin {
        left: 8,
        right: 8,
        top: 0,
        bottom: 6,
    });
    Panel::bottom("filmstrip")
        .exact_size(theme::FILMSTRIP_HEIGHT)
        .show_separator_line(false)
        .frame(frame)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            divider(ui);
            info_row(ui, state);
            strip(ui, state, darkroom);
        });
}

fn info_row(ui: &mut Ui, state: &AppState) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let count = state.library.len();
    if count > 0 {
        let selected = state.library.selection_len();
        let photos = count_of(count, "photo", "photos");
        let text = if selected > 1 {
            format!("{photos}, {selected} selected")
        } else {
            photos
        };
        ui.painter().text(
            rect.left_center() + vec2(4.0, 0.0),
            Align2::LEFT_CENTER,
            text,
            theme::small(),
            theme::TEXT_FAINT,
        );
    }
}

fn strip(ui: &mut Ui, state: &mut AppState, darkroom: &mut Darkroom) {
    let focus = state.library.focus();
    let count = state.library.len();
    let pitch = CELL_WIDTH + CELL_GAP;
    let scroll_id = egui::Id::unique("filmstrip scroll target");
    let last_scrolled: Option<usize> = ui.data(|data| data.get_temp(scroll_id));
    let modifiers = ui.input(|input| input.modifiers);
    let mut clicked = None;
    let mut opened = None;
    ScrollArea::horizontal()
        .auto_shrink([false, false])
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
        .show_viewport(ui, |ui, viewport| {
            ui.set_min_size(vec2(pitch * count as f32, CELL_HEIGHT));
            let origin = ui.min_rect().min;
            let first = (viewport.left() / pitch).floor().max(0.0) as usize;
            let last = ((viewport.right() / pitch).ceil() as usize).min(count);
            for index in first..last {
                state.library.need(index);
                let rect = Rect::from_min_size(
                    origin + vec2(index as f32 * pitch, 0.0),
                    vec2(CELL_WIDTH, CELL_HEIGHT),
                );
                let Some(row) = state.library.row(index) else {
                    ui.painter().rect_filled(rect, 3.0, theme::PANEL_DEEP);
                    continue;
                };
                darkroom.show_row(row);
                let selected = state.library.is_selected(row.variant);
                let result = cell(ui, rect, row, darkroom, focus == Some(index), selected);
                if result.clicked {
                    clicked = Some(index);
                }
                if result.double_clicked {
                    opened = Some(index);
                }
            }
            if focus != last_scrolled {
                if let Some(index) = focus {
                    let rect = Rect::from_min_size(
                        origin + vec2(index as f32 * pitch, 0.0),
                        vec2(CELL_WIDTH, CELL_HEIGHT),
                    );
                    ui.scroll_to_rect(rect, None);
                }
                ui.data_mut(|data| data.insert_temp(scroll_id, focus));
            }
        });
    if let Some(index) = clicked {
        state.library.click(index, click_kind(modifiers));
    }
    if let Some(index) = opened {
        state.library.select(index);
        state.apply(crate::action::Action::ShowDevelop);
    }
}
