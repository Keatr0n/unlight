//! The thumbnail grid. Only the rows on screen exist, whatever the size of the library.

use crate::{
    action::{Action, tip},
    app::App,
    state::{Dialog, library::title},
    ui::{
        filmstrip::{click_kind, paint_badges, paint_thumbnail},
        theme,
        top_bar::turn_items,
        widgets::controls::text_button,
    },
};
use eframe::egui::{Align2, Rect, Role, ScrollArea, Sense, StrokeKind, Ui, WidgetInfo, pos2, vec2};
use unlight_catalog::{Filter, LibraryRow};
use unlight_raw::MergeKind;
use unlight_recipe::Orientation;

const GAP: f32 = 8.0;
const MARGIN: f32 = 12.0;
const CAPTION: f32 = 20.0;
const PICTURE_RATIO: f32 = 0.72;

fn cell_height(cell: f32) -> f32 {
    cell * PICTURE_RATIO + CAPTION
}

fn empty_state(ui: &mut Ui, app: &mut App) {
    let area = ui.available_rect_before_wrap();
    let center = area.center();
    let filtered = *app.state.library.filter() != Filter::default();
    let (heading, hint) = if filtered {
        ("No photos match", "Try loosening the filter.")
    } else {
        ("Nothing here yet", "Import a folder, or drop one on the window.")
    };
    let painter = ui.painter();
    painter.text(
        center - vec2(0.0, 24.0),
        Align2::CENTER_CENTER,
        heading,
        theme::medium(15.0),
        theme::TEXT_DIM,
    );
    painter.text(
        center,
        Align2::CENTER_CENTER,
        hint,
        theme::body(),
        theme::TEXT_FAINT,
    );
    let button = Rect::from_center_size(center + vec2(0.0, 32.0), vec2(120.0, theme::ROW_HEIGHT));
    let mut child = ui.new_child(eframe::egui::UiBuilder::new().max_rect(button));
    let label = if filtered { "Clear Filter" } else { "Import…" };
    if text_button(&mut child, label, true, 120.0).clicked() {
        if filtered {
            app.state.library.set_filter(Filter::default());
            app.state.date_form = Default::default();
        } else {
            app.state.apply(Action::OpenImport);
        }
    }
}

struct Cell {
    clicked: bool,
    double_clicked: bool,
    right_clicked: bool,
}

fn card(
    ui: &mut Ui,
    rect: Rect,
    row: &LibraryRow,
    app: &App,
    focused: bool,
    clicks: &mut GridClicks,
) -> Cell {
    let GridClicks {
        wants_collection,
        wants_copy,
        wants_merge,
        wants_remove,
        wants_turn,
        ..
    } = clicks;
    let can_merge = app.state.can_merge();
    let selected = app.state.library.is_selected(row.variant);
    let id = ui.scope_id().with(("grid card", row.variant.0));
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::ListItem, true, selected, title(row)));
    let fill = if selected {
        theme::PANEL_RAISED
    } else if response.hovered() {
        theme::PANEL_HOVER
    } else {
        theme::PANEL
    };
    ui.painter().rect_filled(rect, 4.0, fill);
    let picture = Rect::from_min_max(
        rect.min + vec2(6.0, 6.0),
        pos2(rect.right() - 6.0, rect.bottom() - CAPTION),
    );
    paint_thumbnail(ui, picture, row, &app.darkroom);
    paint_badges(ui, picture, row);
    let caption = pos2(rect.left() + 8.0, rect.bottom() - CAPTION * 0.5);
    let painter = ui.painter_at(rect.shrink2(vec2(6.0, 0.0)));
    painter.text(
        caption,
        Align2::LEFT_CENTER,
        title(row),
        theme::small(),
        theme::TEXT_DIM,
    );
    if focused {
        ui.painter()
            .rect_stroke(rect, 4.0, (1.5, theme::HANDLE), StrokeKind::Inside);
    }
    let (clicked, double_clicked) = (response.clicked(), response.double_clicked());
    let right_clicked = response.secondary_clicked() && !selected;
    response.context_menu(|ui| {
        if ui.button("New Collection from Selection…").clicked() {
            *wants_collection = true;
            ui.close();
        }
        if ui.button("Create Virtual Copy").clicked() {
            *wants_copy = true;
            ui.close();
        }
        ui.add_enabled_ui(can_merge, |ui| {
            ui.menu_button("Photo Merge", |ui| {
                for (label, kind) in [("HDR…", MergeKind::Hdr), ("Panorama…", MergeKind::Panorama)] {
                    if ui.button(label).clicked() {
                        *wants_merge = Some(kind);
                        ui.close();
                    }
                }
            });
        });
        *wants_turn = turn_items(ui).or(*wants_turn);
        if ui.button(tip("Remove from Catalog…", Action::DeleteSelected)).clicked() {
            *wants_remove = true;
            ui.close();
        }
    });
    Cell {
        clicked,
        double_clicked,
        right_clicked,
    }
}

#[derive(Default)]
struct GridClicks {
    click: Option<usize>,
    open: Option<usize>,
    wants_collection: bool,
    wants_copy: bool,
    wants_merge: Option<MergeKind>,
    wants_remove: bool,
    wants_turn: Option<Orientation>,
}

fn visible_rows(
    ui: &mut Ui,
    app: &mut App,
    origin: eframe::egui::Pos2,
    first_row: usize,
    last_row: usize,
    clicks: &mut GridClicks,
) {
    let cell = app.state.grid_cell;
    let (pitch_x, pitch_y) = (cell + GAP, cell_height(cell) + GAP);
    let columns = app.state.grid_columns;
    let focus = app.state.library.focus();
    let count = app.state.library.len();
    for index in (first_row * columns)..((last_row + 1) * columns).min(count) {
        app.state.library.need(index);
        let (row, column) = (index / columns, index % columns);
        let rect = Rect::from_min_size(
            origin + vec2(column as f32 * pitch_x, row as f32 * pitch_y),
            vec2(cell, cell_height(cell)),
        );
        let Some(data) = app.state.library.row(index).cloned() else {
            ui.painter().rect_filled(rect, 4.0, theme::PANEL);
            continue;
        };
        app.darkroom.show_row(&data);
        let result = card(ui, rect, &data, app, focus == Some(index), clicks);
        // The menu acts on the selection, so a photo that's right-clicked has to be part of it.
        if result.clicked || result.right_clicked {
            clicks.click = Some(index);
        }
        if result.double_clicked {
            clicks.open = Some(index);
        }
    }
}

pub fn show(ui: &mut Ui, app: &mut App) {
    let count = app.state.library.len();
    if app.state.library.count() == Some(0) {
        empty_state(ui, app);
        return;
    }
    let cell = app.state.grid_cell;
    let width = ui.available_width() - MARGIN * 2.0;
    let columns = (((width + GAP) / (cell + GAP)).floor() as usize).max(1);
    app.state.grid_columns = columns;
    let rows = count.div_ceil(columns);
    let pitch_y = cell_height(cell) + GAP;

    let focus = app.state.library.focus();
    let scroll_id = eframe::egui::Id::unique("grid scroll target");
    let last_scrolled: Option<usize> = ui.data(|data| data.get_temp(scroll_id));
    let modifiers = ui.input(|input| input.modifiers);
    let mut clicks = GridClicks::default();
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_viewport(ui, |ui, viewport| {
            ui.set_min_size(vec2(ui.available_width(), rows as f32 * pitch_y + MARGIN * 2.0));
            let origin = ui.min_rect().min + vec2(MARGIN, MARGIN);
            let first = ((viewport.top() - MARGIN) / pitch_y).floor().max(0.0) as usize;
            let last = ((viewport.bottom() / pitch_y).ceil() as usize).min(rows.saturating_sub(1));
            visible_rows(ui, app, origin, first, last, &mut clicks);
            if focus != last_scrolled {
                if let Some(index) = focus {
                    let top = origin.y + (index / columns) as f32 * pitch_y;
                    let rect = Rect::from_min_size(pos2(origin.x, top), vec2(cell, pitch_y));
                    ui.scroll_to_rect(rect, None);
                }
                ui.data_mut(|data| data.insert_temp(scroll_id, focus));
            }
        });
    if let Some(index) = clicks.click {
        app.state.library.click(index, click_kind(modifiers));
    }
    if let Some(index) = clicks.open {
        app.state.library.select(index);
        app.state.apply(Action::ShowLoupe);
    }
    if clicks.wants_collection {
        app.state.dialog = Some(Dialog::NewCollection { name: String::new() });
    }
    if clicks.wants_copy {
        app.state.apply(Action::VirtualCopy);
    }
    if let Some(kind) = clicks.wants_merge {
        app.state.apply(Action::PhotoMerge(kind));
    }
    if clicks.wants_remove {
        app.state.ask_to_remove_selection();
    }
    if let Some(turn) = clicks.wants_turn {
        app.state.apply(Action::Turn(turn));
    }
}
