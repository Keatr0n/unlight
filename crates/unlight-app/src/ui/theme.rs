//! The look: neutral greys so nothing biases colour judgement, one quiet accent, one typeface,
//! and everything spaced on a 4 px grid.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, Style,
    TextStyle, Theme,
    epaint::{
        Shadow,
        text::{FontTweak, HintingTarget, SmoothHinting, VariationCoords},
    },
    vec2,
};
use std::sync::Arc;

pub const GRID: f32 = 4.0;
pub const ROW_HEIGHT: f32 = 24.0;
pub const PANEL_MARGIN: f32 = 12.0;
pub const SECTION_HEADER_HEIGHT: f32 = 32.0;
pub const TOP_BAR_HEIGHT: f32 = 40.0;
pub const TOOLBAR_HEIGHT: f32 = 32.0;
pub const LEFT_PANEL_WIDTH: f32 = 244.0;
pub const RIGHT_PANEL_WIDTH: f32 = 320.0;
pub const FILMSTRIP_HEIGHT: f32 = 104.0;

pub const PANEL: Color32 = Color32::from_gray(0x2b);
pub const PANEL_HOVER: Color32 = Color32::from_gray(0x33);
pub const PANEL_RAISED: Color32 = Color32::from_gray(0x38);
pub const PANEL_DEEP: Color32 = Color32::from_gray(0x22);
pub const DIVIDER: Color32 = Color32::from_gray(0x1f);
pub const TRACK: Color32 = Color32::from_gray(0x4c);
pub const TRACK_FILL: Color32 = Color32::from_gray(0x96);
pub const HANDLE: Color32 = Color32::from_gray(0xc4);
pub const HANDLE_HOVER: Color32 = Color32::from_gray(0xee);
pub const TEXT: Color32 = Color32::from_gray(0xd8);
pub const TEXT_DIM: Color32 = Color32::from_gray(0xa2);
pub const TEXT_FAINT: Color32 = Color32::from_gray(0x70);
pub const ACCENT: Color32 = Color32::from_rgb(0x6c, 0xa6, 0xd9);
pub const ERROR: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x5f);
pub const PICK: Color32 = Color32::from_gray(0xf0);
pub const REJECT: Color32 = Color32::from_rgb(0xd0, 0x4f, 0x45);

pub const LABEL_RED: Color32 = Color32::from_rgb(0xc6, 0x4b, 0x45);
pub const LABEL_YELLOW: Color32 = Color32::from_rgb(0xd1, 0xb0, 0x3f);
pub const LABEL_GREEN: Color32 = Color32::from_rgb(0x5a, 0xa8, 0x5a);
pub const LABEL_BLUE: Color32 = Color32::from_rgb(0x4a, 0x84, 0xc4);
pub const LABEL_PURPLE: Color32 = Color32::from_rgb(0x8d, 0x62, 0xb8);

const BODY_SIZE: f32 = 12.0;
const SMALL_SIZE: f32 = 11.0;
const HEADING_SIZE: f32 = 12.0;
const REGULAR: &str = "Inter";
const MEDIUM: &str = "Inter Medium";
const INTER: &[u8] = include_bytes!("../../assets/InterVariable.ttf");

fn medium_family() -> FontFamily {
    FontFamily::Name(MEDIUM.into())
}

/// The medium weight of the interface font, for headings and values that need presence.
pub fn medium(size: f32) -> FontId {
    FontId::new(size, medium_family())
}

pub fn small() -> FontId {
    FontId::proportional(SMALL_SIZE)
}

pub fn body() -> FontId {
    FontId::proportional(BODY_SIZE)
}

fn font(weight: f32) -> Arc<FontData> {
    let tweak = FontTweak {
        coords: VariationCoords::new([(b"wght", weight), (b"opsz", 14.0)]),
        hinting: Some(true),
        hinting_target: HintingTarget::Smooth(SmoothHinting {
            light: true,
            ..Default::default()
        }),
        subpixel_binning: Some(true),
        ..FontTweak::default()
    };
    Arc::new(FontData::from_static(INTER).tweak(tweak))
}

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::empty();
    fonts.font_data.insert(REGULAR.into(), font(420.0));
    fonts.font_data.insert(MEDIUM.into(), font(540.0));
    fonts
        .families
        .insert(FontFamily::Proportional, vec![REGULAR.into()]);
    fonts.families.insert(FontFamily::Monospace, vec![REGULAR.into()]);
    fonts.families.insert(medium_family(), vec![MEDIUM.into()]);
    fonts
}

fn apply_style(style: &mut Style) {
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(SMALL_SIZE)),
        (TextStyle::Body, FontId::proportional(BODY_SIZE)),
        (TextStyle::Button, FontId::proportional(BODY_SIZE)),
        (TextStyle::Heading, FontId::new(HEADING_SIZE, medium_family())),
        (TextStyle::Monospace, FontId::proportional(BODY_SIZE)),
    ]
    .into();

    let spacing = &mut style.spacing;
    spacing.item_spacing = vec2(GRID * 2.0, GRID);
    spacing.button_padding = vec2(GRID * 2.0, GRID);
    spacing.interact_size = vec2(GRID * 6.0, ROW_HEIGHT);
    spacing.window_margin = Margin::same(12);
    spacing.menu_margin = Margin::same(4);
    spacing.scroll.bar_width = 6.0;
    spacing.scroll.floating = true;
    spacing.scroll.floating_width = 2.0;
    spacing.scroll.floating_allocated_width = 0.0;
    spacing.tooltip_width = 280.0;

    style.animation_time = 0.12;
    style.interaction.selectable_labels = false;
    style.interaction.tooltip_delay = 0.4;
    style.interaction.show_tooltips_only_when_still = true;

    let visuals = &mut style.visuals;
    *visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL_RAISED;
    visuals.extreme_bg_color = PANEL_DEEP;
    visuals.faint_bg_color = PANEL_HOVER;
    visuals.override_text_color = Some(TEXT);
    visuals.window_stroke = Stroke::new(1.0, DIVIDER);
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.menu_corner_radius = CornerRadius::same(6);
    visuals.window_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    visuals.popup_shadow = visuals.window_shadow;
    visuals.selection.bg_fill = ACCENT.linear_multiply(0.4);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.text_cursor.stroke = Stroke::new(1.0, ACCENT);

    let shape = CornerRadius::same(4);
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, DIVIDER);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.noninteractive.corner_radius = shape;
    for (state, fill, text) in [
        (&mut widgets.inactive, PANEL_RAISED, TEXT_DIM),
        (&mut widgets.hovered, Color32::from_gray(0x44), TEXT),
        (&mut widgets.active, Color32::from_gray(0x4c), TEXT),
        (&mut widgets.open, PANEL_RAISED, TEXT),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::NONE;
        state.fg_stroke = Stroke::new(1.0, text);
        state.corner_radius = shape;
        state.expansion = 0.0;
    }
}

pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, apply_style);
    ctx.options_mut(|options| {
        options.zoom_with_keyboard = false;
        options.tessellation_options.feathering_size_in_pixels = 1.0;
    });
}

/// Rounds a length in points to whole device pixels, so a hairline stays one pixel at any
/// fractional scale.
pub fn hairline(ctx: &egui::Context) -> f32 {
    1.0 / ctx.pixels_per_point()
}
