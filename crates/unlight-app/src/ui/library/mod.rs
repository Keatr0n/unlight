//! The Library module: folders and collections on the left, the photos in the middle, details
//! on the right.

mod details;
mod filter_bar;
mod grid;
mod sources;
mod views;

use crate::{
    action::{Action, tip},
    app::App,
    state::{AppState, LibraryView},
    ui::{
        theme,
        widgets::{
            controls::icon_button,
            icons::Icon,
            slider::{Slider, Spec},
        },
    },
};
use eframe::egui::{self, CentralPanel, Frame, Layout, Panel, Ui, vec2};

const CELL_SIZE: Spec = Spec::range("Thumbnails", 120.0, 360.0).decimals(0, 8.0);
const CELL_SIZE_WIDTH: f32 = 260.0;
const DEFAULT_CELL: f32 = 196.0;

fn view_button(
    ui: &mut Ui,
    state: &mut AppState,
    view: LibraryView,
    icon: Icon,
    label: &str,
    action: Action,
) {
    let selected = state.library_view == view;
    if icon_button(ui, icon, &tip(label, action), selected).clicked() {
        state.apply(action);
    }
}

fn toolbar(ui: &mut Ui, state: &mut AppState) {
    let frame = Frame::new()
        .fill(theme::PANEL)
        .inner_margin(egui::Margin::symmetric(8, 4));
    Panel::bottom("library toolbar")
        .exact_size(theme::TOOLBAR_HEIGHT)
        .show_separator_line(false)
        .frame(frame)
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = theme::GRID;
                view_button(ui, state, LibraryView::Grid, Icon::Grid, "Grid", Action::ShowGrid);
                view_button(
                    ui,
                    state,
                    LibraryView::Loupe,
                    Icon::Loupe,
                    "Loupe",
                    Action::ShowLoupe,
                );
                view_button(
                    ui,
                    state,
                    LibraryView::Compare,
                    Icon::Compare,
                    "Compare",
                    Action::ShowCompare,
                );
                view_button(
                    ui,
                    state,
                    LibraryView::Survey,
                    Icon::Survey,
                    "Survey",
                    Action::ShowSurvey,
                );
                ui.add_space(theme::GRID * 3.0);
                filter_bar::sort_controls(ui, state);
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    if state.library_view == LibraryView::Grid {
                        ui.allocate_ui(vec2(CELL_SIZE_WIDTH, theme::ROW_HEIGHT), |ui| {
                            Slider::new(&CELL_SIZE, &mut state.grid_cell, DEFAULT_CELL).show(ui);
                        });
                    }
                });
            });
        });
}

pub fn show(ui: &mut Ui, app: &mut App) {
    sources::show(ui, &mut app.state);
    details::show(ui, &mut app.state);
    CentralPanel::no_frame().show(ui, |ui| {
        filter_bar::show(ui, &mut app.state);
        toolbar(ui, &mut app.state);
        let frame = Frame::new().fill(theme::PANEL_DEEP);
        CentralPanel::default()
            .frame(frame)
            .show(ui, |ui| match app.state.library_view {
                LibraryView::Grid => grid::show(ui, app),
                LibraryView::Loupe => views::loupe(ui, app),
                LibraryView::Compare => views::compare(ui, app),
                LibraryView::Survey => views::survey(ui, app),
            });
    });
}
