//! The small controls: switch, segmented picker, icon and text buttons, tick box, and the
//! collapsible section every Develop panel sits in.

use super::icons::{self, Icon};
use crate::ui::theme;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, Id, Pos2, Rect, Response, Role, Sense, StrokeKind, Ui, WidgetInfo,
    collapsing_header::CollapsingState, pos2, vec2,
};

const SWITCH_SIZE: egui::Vec2 = vec2(22.0, 12.0);

/// A small pill switch. Returns its response, `clicked` means it flipped.
pub fn switch(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(SWITCH_SIZE + vec2(4.0, 8.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, ui.is_enabled(), *on, label));
    let lit = ui.ctx().animate_bool_with_time(response.id, *on, 0.1);
    let track = Rect::from_center_size(rect.center(), SWITCH_SIZE);
    let fill = Color32::from_gray(0x44).lerp_to_gamma(Color32::from_gray(0x8a), lit);
    ui.painter().rect_filled(track, 6.0, fill);
    let x = egui::lerp(track.left() + 6.0..=track.right() - 6.0, lit);
    ui.painter()
        .circle_filled(pos2(x, track.center().y), 4.5, Color32::from_gray(0xe6));
    response
}

/// A row of mutually exclusive choices sharing one rounded background.
pub fn segmented<T: Copy + PartialEq>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    segmented_with_hint(ui, value, options, "")
}

/// A segmented control whose options all carry the same tooltip.
pub fn segmented_with_hint<T: Copy + PartialEq>(
    ui: &mut Ui,
    value: &mut T,
    options: &[(T, &str)],
    hint: &str,
) -> bool {
    let height = theme::ROW_HEIGHT;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    ui.painter().rect_filled(rect, 4.0, theme::PANEL_DEEP);
    let width = rect.width() / options.len().max(1) as f32;
    let mut changed = false;
    for (index, (option, label)) in options.iter().enumerate() {
        let cell = Rect::from_min_size(rect.min + vec2(width * index as f32, 0.0), vec2(width, height));
        let id = ui.auto_id_with(*label);
        let response = ui.interact(cell, id, Sense::click());
        let selected = *value == *option;
        response.widget_info(|| WidgetInfo::selected(Role::RadioButton, ui.is_enabled(), selected, *label));
        if response.clicked() && !selected {
            *value = *option;
            changed = true;
        }
        if selected {
            ui.painter()
                .rect_filled(cell.shrink(2.0), 3.0, theme::PANEL_RAISED);
        } else if response.hovered() {
            ui.painter()
                .rect_filled(cell.shrink(2.0), 3.0, theme::PANEL_HOVER);
        }
        let color = if selected { theme::TEXT } else { theme::TEXT_DIM };
        ui.painter()
            .text(cell.center(), Align2::CENTER_CENTER, *label, theme::body(), color);
        if !hint.is_empty() {
            response.on_hover_text(hint);
        }
    }
    changed
}

/// A square button with a painted icon.
pub fn icon_button(ui: &mut Ui, icon: Icon, tip: &str, selected: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(28.0, 24.0), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, ui.is_enabled(), selected, tip));
    let hovered = response.hovered();
    if selected {
        ui.painter().rect_filled(rect, 4.0, theme::PANEL_RAISED);
    } else if hovered {
        ui.painter().rect_filled(rect, 4.0, theme::PANEL_HOVER);
    }
    let color = match (selected, hovered) {
        (true, _) => theme::ACCENT,
        (false, true) => theme::TEXT,
        (false, false) => theme::TEXT_DIM,
    };
    icons::paint(ui.painter(), rect, icon, color);
    response.on_hover_text(tip)
}

/// A flat text button that fills its width, for rows like Upright.
pub fn text_button(ui: &mut Ui, label: &str, selected: bool, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, theme::ROW_HEIGHT), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, ui.is_enabled(), selected, label));
    let fill = if selected {
        theme::PANEL_RAISED
    } else if response.hovered() {
        theme::PANEL_HOVER
    } else {
        theme::PANEL_DEEP
    };
    ui.painter().rect_filled(rect, 4.0, fill);
    if selected {
        let line = Rect::from_min_max(
            pos2(rect.left() + 6.0, rect.bottom() - 2.0),
            pos2(rect.right() - 6.0, rect.bottom() - 1.0),
        );
        ui.painter().rect_filled(line, 0.0, theme::ACCENT);
    }
    let color = if selected { theme::TEXT } else { theme::TEXT_DIM };
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, label, theme::body(), color);
    response
}

/// A tick box and its label on one comfortable row.
pub fn check_row(ui: &mut Ui, checked: &mut bool, label: &str) -> Response {
    let (rect, mut response) =
        ui.allocate_exact_size(vec2(ui.available_width(), theme::ROW_HEIGHT), Sense::click());
    if response.clicked() {
        *checked = !*checked;
        response.mark_changed();
    }
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, ui.is_enabled(), *checked, label));
    let box_rect = Rect::from_center_size(pos2(rect.left() + 7.0, rect.center().y), vec2(14.0, 14.0));
    let hovered = response.hovered();
    let fill = if *checked {
        theme::ACCENT
    } else {
        theme::PANEL_DEEP
    };
    ui.painter().rect_filled(box_rect, 3.0, fill);
    if !*checked {
        let edge = if hovered { theme::TEXT_FAINT } else { theme::TRACK };
        ui.painter()
            .rect_stroke(box_rect, 3.0, (1.0, edge), StrokeKind::Inside);
    }
    if *checked {
        icons::paint(ui.painter(), box_rect, Icon::Tick, theme::PANEL_DEEP);
    }
    let color = if hovered { theme::TEXT } else { theme::TEXT_DIM };
    ui.painter().text(
        pos2(box_rect.right() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        theme::body(),
        color,
    );
    response
}

/// A dropdown with its label on the left, sized to sit with the dialog fields.
pub fn dropdown<T: Clone + PartialEq>(
    ui: &mut Ui,
    label: &str,
    value: &mut T,
    options: &[(T, &str)],
    width: f32,
) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(96.0, theme::ROW_HEIGHT), Sense::hover());
        ui.painter().text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            label,
            theme::body(),
            theme::TEXT_DIM,
        );
        bare_dropdown(ui, label, value, options, width - 96.0);
    });
    ui.add_space(theme::GRID);
}

/// A dropdown with nothing around it.
pub fn bare_dropdown<T: Clone + PartialEq>(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    value: &mut T,
    options: &[(T, &str)],
    width: f32,
) {
    let current = options
        .iter()
        .find(|(option, _)| option == value)
        .map_or("", |(_, name)| *name);
    let combo = egui::ComboBox::from_id_salt(&id)
        .selected_text(current)
        .width(width)
        .show_ui(ui, |ui| {
            for (option, name) in options {
                ui.selectable_value(value, option.clone(), *name);
            }
        });
    combo
        .response
        .widget_info(|| WidgetInfo::labeled(Role::ComboBox, true, format!("{id:?} {current}")));
}

/// A thin progress bar. `None` means the amount isn't known yet.
pub fn progress_bar(ui: &mut Ui, fraction: Option<f32>, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 4.0), Sense::hover());
    ui.painter().rect_filled(rect, 2.0, theme::PANEL_DEEP);
    if let Some(fraction) = fraction {
        let mut filled = rect;
        filled.set_width(rect.width() * fraction.clamp(0.0, 1.0));
        ui.painter().rect_filled(filled, 2.0, theme::ACCENT);
    }
}

/// A dim one-line note, for empty states and hints.
pub fn note(ui: &mut Ui, text: &str) {
    let text = egui::RichText::new(text)
        .color(theme::TEXT_FAINT)
        .font(theme::small());
    ui.add(egui::Label::new(text).wrap());
}

/// A subtle heading inside a panel, like Lightroom's "Luminance" over its sliders.
pub fn subheading(ui: &mut Ui, text: &str) {
    ui.add_space(theme::GRID);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
    ui.painter().text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        text,
        theme::small(),
        theme::TEXT_FAINT,
    );
}

/// A one-pixel rule across the available width.
pub fn divider(ui: &mut Ui) {
    let rule = theme::hairline(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), rule), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::DIVIDER);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SectionEvents {
    pub toggled: bool,
    pub switched: bool,
    pub reset: bool,
}

pub struct SectionHeader<'a> {
    pub title: &'a str,
    pub open: bool,
    /// Whether the panel is on. `None` hides the switch.
    pub enabled: Option<bool>,
    /// Shows the reset button.
    pub modified: bool,
}

fn header_row(ui: &mut Ui, header: &SectionHeader<'_>, id: Id, openness: f32) -> SectionEvents {
    let mut events = SectionEvents::default();
    let width = ui.available_width();
    let height = theme::SECTION_HEADER_HEIGHT;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let row = ui.interact(rect, id, Sense::click());
    row.widget_info(|| WidgetInfo::selected(Role::Button, ui.is_enabled(), header.open, header.title));
    if row.clicked() {
        events.toggled = true;
    }
    if ui.rect_contains_pointer(rect) {
        ui.painter().rect_filled(rect, 0.0, theme::PANEL_HOVER);
    }

    let chevron = Pos2::new(rect.left() + theme::PANEL_MARGIN + 3.0, rect.center().y);
    icons::chevron(ui.painter(), chevron, openness, theme::TEXT_FAINT);
    let title_color = match header.enabled {
        Some(false) => theme::TEXT_FAINT,
        _ => theme::TEXT,
    };
    ui.painter().text(
        pos2(rect.left() + theme::PANEL_MARGIN + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        header.title,
        theme::medium(12.0),
        title_color,
    );

    let mut right = rect.right() - theme::PANEL_MARGIN + 2.0;
    if let Some(enabled) = header.enabled {
        let mut on = enabled;
        let area = Rect::from_center_size(
            pos2(right - (SWITCH_SIZE.x + 4.0) * 0.5, rect.center().y),
            SWITCH_SIZE + vec2(4.0, 8.0),
        );
        let label = format!("Enable {}", header.title);
        let response = switch(
            &mut ui.new_child(egui::UiBuilder::new().max_rect(area)),
            &mut on,
            &label,
        );
        events.switched = response.clicked();
        right = area.left() - 2.0;
    }
    if header.modified {
        let area = Rect::from_center_size(pos2(right - 10.0, rect.center().y), vec2(20.0, 20.0));
        let label = format!("Reset {}", header.title);
        let response = ui.interact(area, id.with("reset"), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), &label));
        if response.clicked() {
            events.reset = true;
        }
        let color = if response.hovered() {
            theme::TEXT
        } else {
            theme::TEXT_FAINT
        };
        icons::paint(ui.painter(), area, Icon::Reset, color);
        let _ = response.on_hover_text(label);
    }
    events
}

/// A panel with a clickable header and an animated body.
pub fn section(ui: &mut Ui, header: &SectionHeader<'_>, add_body: impl FnOnce(&mut Ui)) -> SectionEvents {
    let id = ui.make_persistent_id(header.title);
    let mut state = CollapsingState::load_with_default_open(ui.ctx(), id.with("body"), header.open);
    state.set_open(header.open);
    let openness = state.openness(ui.ctx());
    let events = header_row(ui, header, id, openness);
    let enabled = header.enabled != Some(false);
    state.show_body_unindented(ui, |ui| {
        egui::Frame::NONE
            .inner_margin(egui::Margin {
                left: 12,
                right: 12,
                top: 4,
                bottom: 12,
            })
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if !enabled {
                    ui.multiply_opacity(0.55);
                }
                add_body(ui);
            });
    });
    divider(ui);
    events
}

/// A rounded card for floating messages.
pub fn card(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(Color32::from_black_alpha(190))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, add_contents);
}
