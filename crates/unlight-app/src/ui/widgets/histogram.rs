//! The histogram: additive colour silhouettes, clip triangles in the corners and a readout
//! under the pointer.

use super::paint;
use crate::{renderer::Histogram, ui::theme};
use eframe::egui::{
    Align2, Color32, Mesh, Pos2, Rect, Response, Role, Sense, Shape, Ui, WidgetInfo, pos2, vec2,
};

const HEIGHT: f32 = 104.0;
const TRIANGLE: f32 = 9.0;
/// Fraction of pixels that counts as clipped. Below this a few stray pixels would light it.
const CLIP_THRESHOLD: f32 = 2e-5;
/// Shapes the height so shadow detail shows next to a tall peak.
const HEIGHT_GAMMA: f32 = 0.6;

const ZONES: [(f32, &str); 5] = [
    (0.1, "Blacks"),
    (0.3, "Shadows"),
    (0.7, "Exposure"),
    (0.9, "Highlights"),
    (1.01, "Whites"),
];

fn zone(level: f32) -> &'static str {
    ZONES
        .iter()
        .find(|(limit, _)| level < *limit)
        .map_or("Whites", |(_, name)| name)
}

/// Tallest bin away from the ends, which hold the clipped pixels and would dwarf the rest.
fn peak(histogram: &Histogram) -> f32 {
    (0..3)
        .flat_map(|channel| histogram.channel(channel)[1..255].iter())
        .copied()
        .max()
        .unwrap_or(1)
        .max(1) as f32
}

fn silhouette(rect: Rect, bins: &[u32], peak: f32, color: Color32) -> Mesh {
    let mut mesh = Mesh::default();
    let columns = rect.width().ceil() as usize;
    let last = bins.len() - 1;
    for column in 0..=columns {
        let t = column as f32 / columns.max(1) as f32;
        let position = t * last as f32;
        let low = position.floor() as usize;
        let high = (low + 1).min(last);
        let blend = position - low as f32;
        let count = bins[low] as f32 * (1.0 - blend) + bins[high] as f32 * blend;
        let level = (count / peak).min(1.0).powf(HEIGHT_GAMMA);
        let x = rect.left() + t * rect.width();
        mesh.colored_vertex(pos2(x, rect.bottom()), color);
        mesh.colored_vertex(pos2(x, rect.bottom() - level * rect.height()), color);
    }
    for column in 0..columns as u32 {
        let first = column * 2;
        mesh.add_triangle(first, first + 1, first + 2);
        mesh.add_triangle(first + 1, first + 2, first + 3);
    }
    mesh
}

fn clip_color(fractions: [f32; 3]) -> Option<Color32> {
    let [red, green, blue] = fractions.map(|fraction| fraction > CLIP_THRESHOLD);
    (red || green || blue).then(|| {
        let level = |on: bool| if on { 240 } else { 70 };
        Color32::from_rgb(level(red), level(green), level(blue))
    })
}

fn clip_triangle(
    ui: &mut Ui,
    corner: Rect,
    left: bool,
    fractions: Option<[f32; 3]>,
    active: bool,
    tip: &str,
) -> Response {
    let id = ui.auto_id_with(tip);
    let response = ui.interact(corner, id, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, ui.is_enabled(), active, tip));
    let color = fractions.and_then(clip_color);
    let fill = match (color, active) {
        (Some(color), _) => color,
        (None, true) => theme::TEXT,
        (None, false) if response.hovered() => theme::TEXT_DIM,
        (None, false) => theme::TEXT_FAINT,
    };
    let (edge, inward) = if left {
        (corner.left() + 6.0, 1.0)
    } else {
        (corner.right() - 6.0, -1.0)
    };
    let top = corner.top() + 6.0;
    let points = [
        pos2(edge, top),
        pos2(edge + inward * TRIANGLE, top),
        pos2(edge, top + TRIANGLE),
    ];
    paint::triangle(ui.painter(), points, fill);
    response.on_hover_text(tip)
}

/// Draws the histogram. Returns true when a clip triangle was clicked.
pub fn show(ui: &mut Ui, histogram: Option<&Histogram>, clipping: bool, clipping_tip: &str) -> bool {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, HEIGHT), Sense::hover());
    ui.painter().rect_filled(rect, 3.0, theme::PANEL_DEEP);
    let plot = rect.shrink2(vec2(6.0, 6.0));
    for quarter in 1..4 {
        let x = plot.left() + plot.width() * quarter as f32 / 4.0;
        ui.painter().vline(x, plot.y_range(), (1.0, theme::DIVIDER));
    }

    if let Some(histogram) = histogram {
        let scale = peak(histogram);
        for (channel, color) in [(150, 44, 44), (44, 128, 54), (48, 62, 152)]
            .into_iter()
            .enumerate()
        {
            let additive = Color32::from_rgba_premultiplied(color.0, color.1, color.2, 0);
            let mesh = silhouette(plot, histogram.channel(channel), scale, additive);
            ui.painter().add(Shape::mesh(mesh));
        }
    }

    let shadows = histogram.map(Histogram::shadow_clipping);
    let highlights = histogram.map(Histogram::highlight_clipping);
    let corner = vec2(24.0, 24.0);
    let left = Rect::from_min_size(rect.min, corner);
    let right = Rect::from_min_size(pos2(rect.max.x - corner.x, rect.min.y), corner);
    let mut clicked = clip_triangle(
        ui,
        left,
        true,
        shadows,
        clipping,
        &format!("Shadow {clipping_tip}"),
    )
    .clicked();
    clicked |= clip_triangle(
        ui,
        right,
        false,
        highlights,
        clipping,
        &format!("Highlight {clipping_tip}"),
    )
    .clicked();

    if let (Some(histogram), Some(pointer)) = (histogram, response.hover_pos()) {
        readout(ui, plot, histogram, pointer);
    }
    clicked
}

fn readout(ui: &Ui, plot: Rect, histogram: &Histogram, pointer: Pos2) {
    let level = ((pointer.x - plot.left()) / plot.width()).clamp(0.0, 1.0);
    let bin = (level * 255.0).round() as usize;
    let painter = ui.painter();
    painter.vline(
        pointer.x.clamp(plot.left(), plot.right()),
        plot.y_range(),
        (1.0, Color32::from_white_alpha(70)),
    );
    let share =
        |channel: usize| histogram.channel(channel)[bin] as f32 / histogram.total.max(1) as f32 * 100.0;
    let text = format!(
        "{}  {}%   R {:.1}  G {:.1}  B {:.1}",
        zone(level),
        (level * 100.0).round(),
        share(0),
        share(1),
        share(2)
    );
    let anchor = pos2(plot.center().x, plot.bottom() - 2.0);
    let galley = painter.layout_no_wrap(text, theme::small(), theme::TEXT);
    let box_rect = Align2::CENTER_BOTTOM
        .anchor_size(anchor, galley.size())
        .expand2(vec2(4.0, 1.0));
    painter.rect_filled(box_rect, 2.0, Color32::from_black_alpha(160));
    painter.galley(box_rect.min + vec2(4.0, 1.0), galley, theme::TEXT);
}

/// The camera settings line under the histogram.
pub fn exposure_line(ui: &mut Ui, parts: &[String]) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let count = parts.len().max(1) as f32;
    let cell = rect.width() / count;
    for (index, part) in parts.iter().enumerate() {
        let center = pos2(rect.left() + cell * (index as f32 + 0.5), rect.center().y);
        ui.painter().text(
            center,
            Align2::CENTER_CENTER,
            part,
            theme::small(),
            theme::TEXT_DIM,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_map_to_lightroom_zones() {
        assert_eq!(zone(0.05), "Blacks");
        assert_eq!(zone(0.5), "Exposure");
        assert_eq!(zone(1.0), "Whites");
    }

    #[test]
    fn clip_colour_shows_which_channels_clipped() {
        assert_eq!(clip_color([0.0, 0.0, 0.0]), None);
        assert_eq!(clip_color([1.0, 0.0, 0.0]), Some(Color32::from_rgb(240, 70, 70)));
        assert_eq!(
            clip_color([1.0, 1.0, 1.0]),
            Some(Color32::from_rgb(240, 240, 240))
        );
    }

    #[test]
    fn peak_ignores_the_clipped_ends() {
        let mut bins = vec![0u32; 768];
        bins[0] = 1_000_000;
        bins[100] = 50;
        assert_eq!(peak(&Histogram::new(bins)), 50.0);
    }

    #[test]
    fn scale_floor_avoids_dividing_by_zero() {
        assert_eq!(peak(&Histogram::new(vec![0; 768])), 1.0);
    }
}
