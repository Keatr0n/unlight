use super::{PanelContext, PanelRequest, param};
use crate::{
    action::{Action, tip},
    state::{
        edit::{PhotoEdit, Step},
        profiles::{ProfileEntry, profile_menu},
    },
    ui::{
        theme,
        widgets::{
            controls::{divider, icon_button, segmented_with_hint, subheading, text_button},
            icons::Icon,
            slider::{Scale, Slider, Spec},
        },
    },
};
use eframe::egui::{self, Align2, Color32, Role, Sense, Ui, WidgetInfo, vec2};
use unlight_recipe::{Recipe, WhiteBalance};

const TEMPERATURE: Spec = Spec::range("Temp", 2000.0, 50000.0)
    .scale(Scale::Reciprocal)
    .decimals(0, 50.0);
const TINT: Spec = Spec::bipolar("Tint", 150.0);
const EXPOSURE: Spec = Spec::bipolar("Exposure", 5.0).decimals(2, 0.05);
const CONTRAST: Spec = Spec::bipolar("Contrast", 100.0);
const HIGHLIGHTS: Spec = Spec::bipolar("Highlights", 100.0);
const SHADOWS: Spec = Spec::bipolar("Shadows", 100.0);
const WHITES: Spec = Spec::bipolar("Whites", 100.0);
const BLACKS: Spec = Spec::bipolar("Blacks", 100.0);
const TEXTURE: Spec = Spec::bipolar("Texture", 100.0);
const CLARITY: Spec = Spec::bipolar("Clarity", 100.0);
const DEHAZE: Spec = Spec::bipolar("Dehaze", 100.0);
const VIBRANCE: Spec = Spec::bipolar("Vibrance", 100.0);
const SATURATION: Spec = Spec::bipolar("Saturation", 100.0);

const TEMPERATURE_TRACK: [Color32; 3] = [
    Color32::from_rgb(0x4d, 0x7c, 0xc9),
    Color32::from_rgb(0xd8, 0xd8, 0xd0),
    Color32::from_rgb(0xde, 0xb5, 0x3e),
];
const TINT_TRACK: [Color32; 3] = [
    Color32::from_rgb(0x4f, 0xa3, 0x5a),
    Color32::from_rgb(0xd0, 0xd0, 0xd0),
    Color32::from_rgb(0xc2, 0x55, 0xb5),
];
const SATURATION_TRACK: [Color32; 4] = [
    Color32::from_gray(0x70),
    Color32::from_rgb(0x8a, 0x6c, 0x5a),
    Color32::from_rgb(0xb0, 0x7f, 0x3f),
    Color32::from_rgb(0xc8, 0x4c, 0x3c),
];

/// Lightroom's raw white balance presets.
const PRESETS: [(&str, f32, f32); 6] = [
    ("Daylight", 5500.0, 10.0),
    ("Cloudy", 6500.0, 10.0),
    ("Shade", 7500.0, 10.0),
    ("Tungsten", 2850.0, 0.0),
    ("Fluorescent", 3800.0, 21.0),
    ("Flash", 5500.0, 0.0),
];

const FALLBACK_AS_SHOT: (f32, f32) = (5500.0, 0.0);
const PROFILE_LABEL_WIDTH: f32 = 56.0;

fn preset_label(balance: &WhiteBalance) -> &'static str {
    match balance {
        WhiteBalance::AsShot => "As Shot",
        WhiteBalance::Custom { temperature, tint } => PRESETS
            .iter()
            .find(|(_, t, g)| t == temperature && g == tint)
            .map_or("Custom", |(name, _, _)| name),
    }
}

fn white_balance_row(ui: &mut Ui, edit: &mut PhotoEdit, context: &mut PanelContext<'_>) {
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::ROW_HEIGHT + 4.0),
        Sense::hover(),
    );
    let mut row = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let picker = icon_button(
        &mut row,
        Icon::Picker,
        "White Balance Selector: click a neutral area",
        *context.picking_white_balance,
    );
    if picker.clicked() {
        *context.picking_white_balance = !*context.picking_white_balance;
    }
    row.add_space(theme::GRID * 2.0);
    let current = preset_label(&edit.recipe.white_balance);
    let combo = egui::ComboBox::from_id_salt("white balance")
        .selected_text(current)
        .width(row.available_width())
        .show_ui(&mut row, |ui| {
            let mut choice = None;
            if ui.selectable_label(current == "As Shot", "As Shot").clicked() {
                choice = Some(("As Shot", WhiteBalance::AsShot));
            }
            for (name, temperature, tint) in PRESETS {
                if ui.selectable_label(current == name, name).clicked() {
                    choice = Some((name, WhiteBalance::Custom { temperature, tint }));
                }
            }
            if let Some((name, balance)) = choice {
                edit.recipe.white_balance = balance;
                edit.note("White Balance", name, Step::New);
            }
        });
    combo
        .response
        .widget_info(|| WidgetInfo::labeled(Role::ComboBox, true, "White Balance"));
}

/// A row with a label on the left and the rest of the width for `add`, as Lightroom's Profile.
fn labelled_row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::ROW_HEIGHT + 4.0),
        Sense::hover(),
    );
    let mut row = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let (name, _) = row.allocate_exact_size(vec2(PROFILE_LABEL_WIDTH, theme::ROW_HEIGHT), Sense::hover());
    row.painter().text(
        name.left_center(),
        Align2::LEFT_CENTER,
        label,
        theme::body(),
        theme::TEXT_DIM,
    );
    add(&mut row)
}

fn choose_profile(edit: &mut PhotoEdit, entry: &ProfileEntry) {
    edit.recipe.camera_profile.clone_from(&entry.stored);
    edit.note("Profile", entry.label.as_str(), Step::New);
}

fn profile_row(ui: &mut Ui, edit: &mut PhotoEdit, context: &mut PanelContext<'_>) {
    let menu = profile_menu(edit.recipe.camera_profile.as_deref(), &context.profiles);
    let mut request = None;
    let combo = labelled_row(ui, "Profile", |row| {
        egui::ComboBox::from_id_salt("camera profile")
            .selected_text(&menu.current)
            .width(row.available_width())
            .show_ui(row, |ui| {
                for entry in &menu.entries {
                    let chosen = entry.stored == edit.recipe.camera_profile;
                    if ui.selectable_label(chosen, &entry.label).clicked() && !chosen {
                        choose_profile(edit, entry);
                    }
                }
                ui.separator();
                if ui.selectable_label(false, "Rescan Profiles").clicked() {
                    request = Some(PanelRequest::RescanProfiles);
                }
                if ui.selectable_label(false, "Open Profile Folder").clicked() {
                    request = Some(PanelRequest::OpenProfileFolder);
                }
            })
    });
    context.request = context.request.or(request);
    let hint = if menu.missing {
        "This profile isn't installed here, so the photo is using its automatic profile"
    } else {
        "Camera profiles are .dcp files in the profile folder"
    };
    combo
        .response
        .on_hover_text(hint)
        .widget_info(|| WidgetInfo::labeled(Role::ComboBox, true, "Profile"));
}

fn tone_heading(ui: &mut Ui, context: &mut PanelContext<'_>) {
    subheading(ui, "Tone");
    ui.add_space(theme::GRID);
    let width = ui.available_width();
    if text_button(ui, "Auto", false, width)
        .on_hover_text(tip("Set the tone from the photo", Action::AutoTone))
        .clicked()
    {
        context.request = Some(PanelRequest::AutoTone);
    }
    ui.add_space(theme::GRID);
}

fn temperature_tint(ui: &mut Ui, edit: &mut PhotoEdit, context: &PanelContext<'_>) {
    let as_shot = context.as_shot.unwrap_or(FALLBACK_AS_SHOT);
    let (mut temperature, mut tint) = match edit.recipe.white_balance {
        WhiteBalance::AsShot => as_shot,
        WhiteBalance::Custom { temperature, tint } => (temperature, tint),
    };
    let temperature_change = Slider::new(&TEMPERATURE, &mut temperature, as_shot.0)
        .gradient(&TEMPERATURE_TRACK)
        .show(ui)
        .change;
    let tint_change = Slider::new(&TINT, &mut tint, as_shot.1)
        .gradient(&TINT_TRACK)
        .show(ui)
        .change;
    let (spec, value, step) = match (temperature_change, tint_change) {
        (Some(step), _) => (&TEMPERATURE, temperature, step),
        (None, Some(step)) => (&TINT, tint, step),
        (None, None) => return,
    };
    edit.recipe.white_balance = if (temperature, tint) == as_shot {
        WhiteBalance::AsShot
    } else {
        WhiteBalance::Custom { temperature, tint }
    };
    edit.note(spec.name, spec.format(value), step);
}

pub fn show(ui: &mut Ui, edit: &mut PhotoEdit, context: &mut PanelContext<'_>) {
    let mut black_and_white = edit.recipe.black_and_white.enabled;
    let options = [(false, "Color"), (true, "Black & White")];
    let hint = tip("Treatment", Action::ToggleBlackAndWhite);
    if segmented_with_hint(ui, &mut black_and_white, &options, &hint) {
        edit.recipe.black_and_white.enabled = black_and_white;
        edit.note(
            "Treatment",
            if black_and_white { "Black & White" } else { "Color" },
            Step::New,
        );
    }
    ui.add_space(theme::GRID * 2.0);
    profile_row(ui, edit, context);
    ui.add_space(theme::GRID);

    white_balance_row(ui, edit, context);
    ui.add_space(theme::GRID);
    temperature_tint(ui, edit, context);

    gap(ui);
    tone_heading(ui, context);
    param(ui, edit, &EXPOSURE, |recipe| &mut recipe.basic.exposure);
    param(ui, edit, &CONTRAST, |recipe| &mut recipe.basic.contrast);
    gap(ui);
    param(ui, edit, &HIGHLIGHTS, |recipe| &mut recipe.basic.highlights);
    param(ui, edit, &SHADOWS, |recipe| &mut recipe.basic.shadows);
    param(ui, edit, &WHITES, |recipe| &mut recipe.basic.whites);
    param(ui, edit, &BLACKS, |recipe| &mut recipe.basic.blacks);
    gap(ui);
    param(ui, edit, &TEXTURE, |recipe| &mut recipe.basic.texture);
    param(ui, edit, &CLARITY, |recipe| &mut recipe.basic.clarity);
    param(ui, edit, &DEHAZE, |recipe| &mut recipe.basic.dehaze);
    gap(ui);
    param(ui, edit, &VIBRANCE, |recipe: &mut Recipe| {
        &mut recipe.basic.vibrance
    });
    super::param_gradient(ui, edit, &SATURATION, &SATURATION_TRACK, |recipe| {
        &mut recipe.basic.saturation
    });
}

fn gap(ui: &mut Ui) {
    ui.add_space(theme::GRID);
    divider(ui);
    ui.add_space(theme::GRID);
}
