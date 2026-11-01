//! The Lightroom slider: label, thin track, value. Double-click resets, Alt drags finely, a
//! wheel notch steps it and clicking the number lets you type one.

use crate::{
    state::edit::Step,
    ui::{theme, widgets::paint},
};
use eframe::egui::{
    self, Align2, Color32, Event, Id, Key, MouseWheelUnit, Pos2, Rect, Response, Role, Sense, TextEdit, Ui,
    UiBuilder, Vec2, WidgetInfo, text_selection::CCursorRange, vec2,
};

const LABEL_WIDTH: f32 = 88.0;
const VALUE_WIDTH: f32 = 44.0;
const GAP: f32 = 8.0;
const THUMB_RADIUS: f32 = 5.0;
const GRAB_DISTANCE: f32 = 7.0;
const FINE_DRAG: f32 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scale {
    Linear,
    /// Even in reciprocal units, how colour temperature is perceived.
    Reciprocal,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    pub label: &'static str,
    /// What history and assistive tech call it, when the label needs its panel for context.
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub decimals: u8,
    pub suffix: &'static str,
    pub bipolar: bool,
    pub wheel_step: f32,
    pub scale: Scale,
}

impl Spec {
    /// A symmetric slider, zero in the middle, like Contrast.
    pub const fn bipolar(label: &'static str, reach: f32) -> Self {
        Self {
            label,
            name: label,
            min: -reach,
            max: reach,
            decimals: 0,
            suffix: "",
            bipolar: true,
            wheel_step: 1.0,
            scale: Scale::Linear,
        }
    }

    /// A slider that starts from nothing, like Sharpening Amount.
    pub const fn amount(label: &'static str, max: f32) -> Self {
        Self {
            min: 0.0,
            max,
            bipolar: false,
            ..Self::bipolar(label, max)
        }
    }

    pub const fn range(label: &'static str, min: f32, max: f32) -> Self {
        Self {
            min,
            max,
            bipolar: false,
            ..Self::bipolar(label, max)
        }
    }

    pub const fn named(self, name: &'static str) -> Self {
        Self { name, ..self }
    }

    pub const fn decimals(self, decimals: u8, wheel_step: f32) -> Self {
        Self {
            decimals,
            wheel_step,
            ..self
        }
    }

    pub const fn suffix(self, suffix: &'static str) -> Self {
        Self { suffix, ..self }
    }

    pub const fn scale(self, scale: Scale) -> Self {
        Self { scale, ..self }
    }

    fn track_position(&self, value: f32) -> f32 {
        let t = match self.scale {
            Scale::Linear => (value - self.min) / (self.max - self.min),
            Scale::Reciprocal => (1.0 / self.min - 1.0 / value) / (1.0 / self.min - 1.0 / self.max),
        };
        t.clamp(0.0, 1.0)
    }

    fn value_at(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self.scale {
            Scale::Linear => self.min + t * (self.max - self.min),
            Scale::Reciprocal => 1.0 / (1.0 / self.min - t * (1.0 / self.min - 1.0 / self.max)),
        }
    }

    fn round(&self, value: f32) -> f32 {
        let factor = 10f32.powi(i32::from(self.decimals));
        ((value * factor).round() / factor).clamp(self.min, self.max)
    }

    pub fn format(&self, value: f32) -> String {
        let decimals = usize::from(self.decimals);
        let number = if self.bipolar {
            let rounded = self.round(value);
            if rounded == 0.0 {
                format!("{:.decimals$}", 0.0)
            } else {
                format!("{rounded:+.decimals$}")
            }
        } else {
            format!("{value:.decimals$}")
        };
        format!("{number}{}", self.suffix)
    }

    fn parse(&self, text: &str) -> Option<f32> {
        let cleaned = text.trim().trim_end_matches(self.suffix).trim();
        cleaned
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| self.round(value))
    }
}

/// What the row reports back. `change` says how the edit should enter history.
pub struct SliderResult {
    pub response: Response,
    pub change: Option<Step>,
}

pub struct Slider<'a> {
    spec: &'a Spec,
    value: &'a mut f32,
    default: f32,
    gradient: Option<&'a [Color32]>,
    tooltip: Option<&'a str>,
}

/// Remembered while the pointer holds the thumb. `exact` keeps fine drags from being lost
/// to the rounding of the stored value.
#[derive(Clone, Copy, Default)]
struct Grip {
    exact: f32,
    /// Pointer's distance from the thumb, so grabbing it off-centre doesn't make it jump.
    offset: f32,
}

#[derive(Clone, Copy, Default)]
struct WheelState {
    armed: Option<Id>,
    last_pointer: Option<Pos2>,
}

impl<'a> Slider<'a> {
    pub fn new(spec: &'a Spec, value: &'a mut f32, default: f32) -> Self {
        Self {
            spec,
            value,
            default,
            gradient: None,
            tooltip: None,
        }
    }

    pub fn gradient(mut self, stops: &'a [Color32]) -> Self {
        self.gradient = Some(stops);
        self
    }

    pub fn tooltip(mut self, text: &'a str) -> Self {
        self.tooltip = Some(text);
        self
    }

    pub fn show(self, ui: &mut Ui) -> SliderResult {
        let Self {
            spec,
            value,
            default,
            gradient,
            tooltip,
        } = self;
        let id = ui.auto_id_with(spec.name);
        let width = ui.available_width();
        let (row, _) = ui.allocate_exact_size(vec2(width, theme::ROW_HEIGHT), Sense::hover());
        let label_rect = Rect::from_min_size(row.min, vec2(LABEL_WIDTH, row.height()));
        let value_rect = Rect::from_min_size(
            Pos2::new(row.max.x - VALUE_WIDTH, row.min.y),
            vec2(VALUE_WIDTH, row.height()),
        );
        let track_rect = Rect::from_min_max(
            Pos2::new(label_rect.max.x + GAP, row.min.y),
            Pos2::new(value_rect.min.x - GAP, row.max.y),
        );
        let track_hit = track_rect.expand2(vec2(THUMB_RADIUS + 1.0, 0.0));

        let label = ui.interact(label_rect, id.with("label"), Sense::click());
        let track = ui.interact(track_hit, id, Sense::click_and_drag());
        let number = ui.interact(value_rect, id.with("value"), Sense::click_and_drag());
        let track = track.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        let number = number.on_hover_cursor(egui::CursorIcon::Text);

        let mut change = None;
        if label.double_clicked() || track.double_clicked() {
            change = set(value, default, Step::Burst).or(change);
        }

        let track_x = track_rect.x_range();
        if let Some(step) = drag_thumb(ui, &track, spec, value, track_x.min, track_x.span(), id) {
            change = Some(step);
        }
        if let Some(step) = scrub_number(ui, &number, spec, value, track_x.span()) {
            change = Some(step);
        }
        if let Some(step) = wheel(ui, id, row, spec, value) {
            change = Some(step);
        }

        let editing = type_number(ui, &number, id.with("typing"), value_rect, spec, value);
        if let Some(step) = editing.change {
            change = Some(step);
        }

        let hovered = ui.rect_contains_pointer(row) || track.dragged();
        let painter = ui.painter();
        let label_color = if hovered { theme::TEXT } else { theme::TEXT_DIM };
        painter.text(
            label_rect.left_center(),
            Align2::LEFT_CENTER,
            spec.label,
            theme::body(),
            label_color,
        );
        paint_track(ui, track_rect, spec, *value, default, gradient, &track);
        if !editing.active {
            let changed_from_default = (*value - default).abs() > 1e-5;
            let color = if changed_from_default {
                theme::TEXT
            } else {
                theme::TEXT_DIM
            };
            ui.painter().text(
                value_rect.right_center(),
                Align2::RIGHT_CENTER,
                spec.format(*value),
                theme::body(),
                color,
            );
        }

        track.widget_info(|| WidgetInfo::slider(ui.is_enabled(), f64::from(*value), spec.name));
        number.widget_info(|| {
            WidgetInfo::labeled(Role::Button, ui.is_enabled(), format!("{} value", spec.name))
        });
        let mut response = track;
        if let Some(text) = tooltip {
            response = response.on_hover_text(text);
        }
        if change.is_some() {
            response.mark_changed();
        }
        SliderResult { response, change }
    }
}

fn set(value: &mut f32, target: f32, step: Step) -> Option<Step> {
    if *value == target {
        return None;
    }
    *value = target;
    Some(step)
}

fn drag_thumb(
    ui: &Ui,
    track: &Response,
    spec: &Spec,
    value: &mut f32,
    track_left: f32,
    track_width: f32,
    id: Id,
) -> Option<Step> {
    let grip_id = id.with("grip");
    if !track.is_pointer_button_down_on() {
        ui.data_mut(|data| data.remove_temp::<Grip>(grip_id));
        return None;
    }
    let pointer = track.interact_pointer_pos()?.x;
    let fine = ui.input(|input| input.modifiers.alt);
    let thumb = |value: f32| track_left + spec.track_position(value) * track_width;

    let existing = ui.data(|data| data.get_temp::<Grip>(grip_id));
    let mut grip = existing.unwrap_or_else(|| {
        let near_thumb = (pointer - thumb(*value)).abs() <= GRAB_DISTANCE;
        let offset = if fine || near_thumb {
            pointer - thumb(*value)
        } else {
            0.0
        };
        Grip {
            exact: *value,
            offset,
        }
    });

    if fine {
        let moved = track.drag_delta().x / track_width.max(1.0) * FINE_DRAG;
        grip.exact = spec.value_at(spec.track_position(grip.exact) + moved);
        grip.offset = pointer - thumb(grip.exact);
    } else {
        grip.exact = spec.value_at((pointer - grip.offset - track_left) / track_width.max(1.0));
    }
    ui.data_mut(|data| data.insert_temp(grip_id, grip));
    let step = if track.dragged() { Step::New } else { Step::Burst };
    set(value, spec.round(grip.exact), step)
}

fn scrub_number(ui: &Ui, number: &Response, spec: &Spec, value: &mut f32, track_width: f32) -> Option<Step> {
    if !number.dragged() {
        return None;
    }
    let scale = if ui.input(|input| input.modifiers.alt) {
        FINE_DRAG
    } else {
        1.0
    };
    let moved = number.drag_delta().x / track_width.max(1.0) * scale;
    set(
        value,
        spec.round(spec.value_at(spec.track_position(*value) + moved)),
        Step::New,
    )
}

/// Line-based wheels step the value, once the pointer has actually moved onto the row. That
/// way a list scrolling under a resting pointer keeps scrolling, and trackpads, which send
/// pixel deltas, always scroll the panel.
fn wheel(ui: &Ui, id: Id, row: Rect, spec: &Spec, value: &mut f32) -> Option<Step> {
    let store = Id::unique("slider wheel");
    if !ui.rect_contains_pointer(row) {
        return None;
    }
    let pointer = ui.input(|input| input.pointer.latest_pos());
    let mut state = ui
        .data(|data| data.get_temp::<WheelState>(store))
        .unwrap_or_default();
    if pointer != state.last_pointer {
        state = WheelState {
            armed: Some(id),
            last_pointer: pointer,
        };
        ui.data_mut(|data| data.insert_temp(store, state));
    }
    if state.armed != Some(id) {
        return None;
    }
    let (ticks, fine) = ui.input_mut(|input| {
        input.smooth_scroll_delta = Vec2::ZERO;
        let ticks: f32 = input
            .events
            .iter()
            .filter_map(|event| match event {
                Event::MouseWheel {
                    unit: MouseWheelUnit::Line,
                    delta,
                    ..
                } => Some(delta.y),
                _ => None,
            })
            .sum();
        (ticks, input.modifiers.alt)
    });
    if ticks == 0.0 {
        return None;
    }
    let step = if fine {
        spec.wheel_step * FINE_DRAG
    } else {
        spec.wheel_step
    };
    let notches = ticks.signum() * ticks.abs().ceil();
    set(value, spec.round(*value + notches * step), Step::Burst)
}

struct Typing {
    active: bool,
    change: Option<Step>,
}

fn type_number(ui: &mut Ui, number: &Response, id: Id, rect: Rect, spec: &Spec, value: &mut f32) -> Typing {
    let mut text: Option<String> = ui.data(|data| data.get_temp(id));
    let starting = number.clicked() && text.is_none();
    if starting {
        text = Some(spec.format(*value));
    }
    let Some(mut text) = text else {
        return Typing {
            active: false,
            change: None,
        };
    };

    let edit_id = id.with("field");
    let field = TextEdit::singleline(&mut text)
        .id(edit_id)
        .frame(egui::Frame::NONE.fill(theme::PANEL_DEEP).corner_radius(2))
        .align(Align2::RIGHT_CENTER)
        .font(theme::body())
        .desired_width(rect.width());
    let builder = UiBuilder::new().max_rect(rect.shrink2(vec2(0.0, 3.0)));
    let output = field.show(&mut ui.new_child(builder));
    if starting {
        let mut state = output.state.clone();
        state
            .cursor
            .set_char_range(Some(CCursorRange::select_all_str(&text)));
        state.store(ui.ctx(), edit_id);
        ui.memory_mut(|memory| memory.request_focus(edit_id));
    }

    if output.response.lost_focus() {
        let cancelled = ui.input(|input| input.key_pressed(Key::Escape));
        ui.data_mut(|data| data.remove_temp::<String>(id));
        let change = if cancelled {
            None
        } else {
            spec.parse(&text).and_then(|parsed| set(value, parsed, Step::New))
        };
        return Typing {
            active: false,
            change,
        };
    }
    ui.data_mut(|data| data.insert_temp(id, text));
    Typing {
        active: true,
        change: None,
    }
}

fn paint_track(
    ui: &Ui,
    rect: Rect,
    spec: &Spec,
    value: f32,
    default: f32,
    gradient: Option<&[Color32]>,
    response: &Response,
) {
    let painter = ui.painter();
    let y = rect.center().y;
    let x_at = |value: f32| rect.left() + spec.track_position(value) * rect.width();
    let thumb_x = x_at(value);

    match gradient {
        Some(stops) => {
            let band = Rect::from_min_max(Pos2::new(rect.left(), y - 2.0), Pos2::new(rect.right(), y + 2.0));
            paint::horizontal_gradient(painter, band, stops);
        }
        None => {
            let band = Rect::from_center_size(rect.center(), vec2(rect.width(), 2.0));
            painter.rect_filled(band, 1.0, theme::TRACK);
            let origin = if spec.bipolar { x_at(default) } else { rect.left() };
            let (low, high) = (origin.min(thumb_x), origin.max(thumb_x));
            let fill = Rect::from_min_max(Pos2::new(low, y - 1.0), Pos2::new(high, y + 1.0));
            painter.rect_filled(fill, 1.0, theme::TRACK_FILL);
            if spec.bipolar {
                let tick = Rect::from_center_size(Pos2::new(x_at(default), y), vec2(1.0, 6.0));
                painter.rect_filled(tick, 0.0, theme::TEXT_FAINT);
            }
        }
    }

    let held = response.dragged() || response.is_pointer_button_down_on();
    let lit = ui
        .ctx()
        .animate_bool_with_time(response.id.with("lit"), response.hovered() || held, 0.08);
    let radius = THUMB_RADIUS - 1.0 + lit;
    let color = theme::HANDLE.lerp_to_gamma(theme::HANDLE_HOVER, lit);
    let center = Pos2::new(thumb_x, y);
    painter.circle_filled(center, radius + 1.5, theme::PANEL);
    painter.circle_filled(center, radius, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPOSURE: Spec = Spec::bipolar("Exposure", 5.0).decimals(2, 0.05);
    const TEMPERATURE: Spec = Spec::range("Temp", 2000.0, 50000.0)
        .scale(Scale::Reciprocal)
        .suffix(" K");

    #[test]
    fn values_format_like_lightroom() {
        assert_eq!(EXPOSURE.format(0.5), "+0.50");
        assert_eq!(EXPOSURE.format(-1.25), "-1.25");
        assert_eq!(EXPOSURE.format(0.0), "0.00");
        assert_eq!(EXPOSURE.format(-0.001), "0.00");
        assert_eq!(TEMPERATURE.format(5500.0), "5500 K");
    }

    #[test]
    fn typed_numbers_are_clamped_and_rounded() {
        assert_eq!(EXPOSURE.parse("9"), Some(5.0));
        assert_eq!(EXPOSURE.parse(" +0.126 "), Some(0.13));
        assert_eq!(EXPOSURE.parse("nope"), None);
        assert_eq!(TEMPERATURE.parse("5500 K"), Some(5500.0));
    }

    #[test]
    fn the_track_maps_back_to_the_same_value() {
        for spec in [EXPOSURE, TEMPERATURE] {
            for t in [0.0, 0.25, 0.5, 1.0] {
                assert!((spec.track_position(spec.value_at(t)) - t).abs() < 1e-3);
            }
        }
    }

    #[test]
    fn reciprocal_tracks_give_low_kelvin_more_room() {
        assert!(TEMPERATURE.value_at(0.5) < 4000.0);
    }
}
