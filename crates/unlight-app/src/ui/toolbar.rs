//! The slim bar under the photo: compare modes, clipping and zoom.

use crate::{
    action::{Action, tip},
    state::{AppState, Compare, view::Zoom},
    ui::{
        theme,
        widgets::{controls::icon_button, icons::Icon},
    },
};
use eframe::egui::{self, Align2, Frame, Panel, Role, Sense, Ui, WidgetInfo, vec2};

fn zoom_button(ui: &mut Ui, label: &str, selected: bool, hint: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(36.0, 24.0), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, label));
    if selected {
        ui.painter().rect_filled(rect, 4.0, theme::PANEL_RAISED);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 4.0, theme::PANEL_HOVER);
    }
    let color = if selected { theme::TEXT } else { theme::TEXT_DIM };
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, label, theme::small(), color);
    response.on_hover_text(hint)
}

pub fn show(ui: &mut Ui, state: &mut AppState, scale: Option<f32>) {
    let frame = Frame::new()
        .fill(theme::PANEL)
        .inner_margin(egui::Margin::symmetric(8, 4));
    Panel::bottom("toolbar")
        .exact_size(theme::TOOLBAR_HEIGHT)
        .show_separator_line(false)
        .frame(frame)
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = theme::GRID;
                let left = tip("Left panel", Action::ToggleSidePanels);
                if icon_button(ui, Icon::PanelLeft, &left, false).clicked() {
                    state.panels.left = !state.panels.left;
                }
                ui.add_space(theme::GRID);
                let split = icon_button(
                    ui,
                    Icon::Split,
                    &tip("Before / After split", Action::CompareSplit),
                    state.compare == Compare::Split,
                );
                if split.clicked() {
                    state.apply(Action::CompareSplit);
                }
                let side = icon_button(
                    ui,
                    Icon::SideBySide,
                    &tip("Before / After side by side", Action::CompareSideBySide),
                    state.compare == Compare::SideBySide,
                );
                if side.clicked() {
                    state.apply(Action::CompareSideBySide);
                }
                let clipping = icon_button(
                    ui,
                    Icon::Clipping,
                    &tip("Show clipping", Action::ToggleClipping),
                    state.clipping,
                );
                if clipping.clicked() {
                    state.apply(Action::ToggleClipping);
                }
                let proof = icon_button(
                    ui,
                    Icon::Proof,
                    &tip("Soft Proofing", Action::ToggleSoftProof),
                    state.proof.enabled,
                );
                if proof.clicked() {
                    state.apply(Action::ToggleSoftProof);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let right = tip("Right panel", Action::ToggleSidePanels);
                    if icon_button(ui, Icon::PanelRight, &right, false).clicked() {
                        state.panels.right = !state.panels.right;
                    }
                    ui.add_space(theme::GRID * 2.0);
                    let view = &mut state.view;
                    let is_ratio =
                        |ratio: f32| matches!(view.zoom, Zoom::Ratio(value) if (value - ratio).abs() < 1e-3);
                    let (two, one) = (is_ratio(2.0), is_ratio(1.0));
                    if zoom_button(ui, "2:1", two, "200%").clicked() {
                        view.zoom = Zoom::Ratio(2.0);
                        view.toggle_ratio = 2.0;
                    }
                    if zoom_button(ui, "1:1", one, &tip("100%", Action::ToggleZoom)).clicked() {
                        view.zoom = Zoom::Ratio(1.0);
                        view.toggle_ratio = 1.0;
                    }
                    if zoom_button(ui, "Fill", view.zoom == Zoom::Fill, "Fill the view").clicked() {
                        view.zoom = Zoom::Fill;
                    }
                    if zoom_button(ui, "Fit", view.zoom == Zoom::Fit, "Fit in view").clicked() {
                        view.zoom = Zoom::Fit;
                    }
                    ui.add_space(theme::GRID * 2.0);
                    if let Some(scale) = scale {
                        let text = format!("{:.0}%", scale * 100.0);
                        ui.label(
                            egui::RichText::new(text)
                                .font(theme::small())
                                .color(theme::TEXT_FAINT),
                        );
                    }
                });
            });
        });
}
