//! The right side of Develop: histogram, tool strip, then the panels in Lightroom's order.

use crate::{
    action::{Action, tip},
    darkroom::LivePhoto,
    renderer::Histogram,
    state::{
        AppState, DecodeState, Effect, MAX_GUIDES, Tool,
        crop::{AspectLock, Framing, MAX_ANGLE, fit_inside},
        edit::{PhotoEdit, Step},
        sections::Section,
    },
    ui::{
        local_panels,
        panels::{PanelContext, PanelRequest, show_section},
        proof_panel,
        theme,
        widgets::{
            controls::{SectionHeader, check_row, divider, icon_button, note, section, text_button},
            histogram,
            icons::Icon,
            slider::{Slider, Spec},
        },
    },
};
use eframe::egui::{self, Frame, Panel, Role, ScrollArea, Ui, WidgetInfo, vec2};
use unlight_catalog::{Photo, VariantId};
use unlight_recipe::{Crop, Recipe};

const ANGLE: Spec = Spec::bipolar("Angle", MAX_ANGLE).decimals(2, 0.1).suffix("°");

fn format_shutter(seconds: f64) -> String {
    if seconds <= 0.0 {
        String::new()
    } else if seconds >= 1.0 {
        format!("{seconds:.1} s")
    } else {
        format!("1/{:.0}", 1.0 / seconds)
    }
}

/// ISO, focal length, aperture and shutter speed, as Lightroom lists them under the histogram.
pub fn exposure_summary(photo: &Photo) -> Vec<String> {
    let mut parts = Vec::new();
    parts.extend(photo.iso.map(|iso| format!("ISO {iso:.0}")));
    parts.extend(photo.focal_length.map(|focal| format!("{focal:.0} mm")));
    parts.extend(photo.aperture.map(|aperture| format!("ƒ/{aperture:.1}")));
    parts.extend(
        photo
            .shutter
            .map(format_shutter)
            .filter(|shutter| !shutter.is_empty()),
    );
    parts
}

fn tool_strip(ui: &mut Ui, state: &mut AppState) {
    let tools = [
        (
            Tool::Crop,
            Icon::Crop,
            "Crop & Straighten",
            Some(Action::ToggleTool(Tool::Crop)),
        ),
        (
            Tool::Heal,
            Icon::Heal,
            "Healing",
            Some(Action::ToggleTool(Tool::Heal)),
        ),
        (
            Tool::Masks,
            Icon::Masks,
            "Masking",
            Some(Action::ToggleTool(Tool::Masks)),
        ),
        (Tool::RedEye, Icon::RedEye, "Red Eye Correction", None),
    ];
    let width = ui.available_width();
    let button = 28.0;
    let gap = (width - button * tools.len() as f32) / (tools.len() as f32 + 1.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        ui.add_space(gap);
        for (tool, icon, label, action) in tools {
            let text = action.map_or(label.to_owned(), |action| tip(label, action));
            if icon_button(ui, icon, &text, state.tool == tool).clicked() {
                state.apply(Action::ToggleTool(tool));
            }
        }
    });
}

fn crop_panel(ui: &mut Ui, state: &mut AppState, live: Option<&LivePhoto>) {
    let (orientation, aspect) = match live {
        Some(photo) => {
            let aspect = photo.sensor_aspect();
            let turned = state.edit().map_or(Default::default(), |edit| edit.recipe.orientation);
            let orientation = photo.orientation.then(turned);
            let display = if orientation.swaps_axes() { 1.0 / aspect } else { aspect };
            (orientation, display)
        }
        None => (Default::default(), 1.5),
    };

    let mut tool = state.crop_tool;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::GRID;
        let locked = tool.lock != AspectLock::Free;
        let icon = if locked { Icon::Lock } else { Icon::Unlock };
        if icon_button(
            ui,
            icon,
            &tip("Lock aspect ratio", Action::ToggleAspectLock),
            locked,
        )
        .clicked()
        {
            tool.lock = if locked {
                AspectLock::Free
            } else {
                AspectLock::Original
            };
        }
        let combo = egui::ComboBox::from_id_salt("crop aspect")
            .selected_text(tool.lock.label())
            .width(ui.available_width() - 32.0)
            .show_ui(ui, |ui| {
                for choice in AspectLock::CHOICES {
                    ui.selectable_value(&mut tool.lock, choice, choice.label());
                }
            });
        combo
            .response
            .widget_info(|| WidgetInfo::labeled(Role::ComboBox, true, "Aspect Ratio"));
        let flip = icon_button(
            ui,
            Icon::Flip,
            "Swap the ratio's long and short sides  (X)",
            tool.portrait,
        );
        if flip.clicked() {
            tool.portrait = !tool.portrait;
        }
    });
    let lock_changed = tool != state.crop_tool;
    state.crop_tool = tool;
    if lock_changed {
        state.apply_aspect_lock(orientation, aspect);
    }

    ui.add_space(theme::GRID);
    if let Some(edit) = state.edit_mut() {
        // Dragging right should turn the photo clockwise on screen, flipped or not.
        let on_screen = if orientation.mirrors() { -1.0 } else { 1.0 };
        let mut angle = edit.recipe.crop.angle * on_screen;
        let result = Slider::new(&ANGLE, &mut angle, 0.0).show(ui);
        if let (Some(step), Some(photo)) = (result.change, live) {
            let framing = Framing {
                transform: &edit.recipe.transform,
                lens: &edit.recipe.lens,
                optics: photo.session.optics(),
                aspect: photo.sensor_aspect(),
            };
            let turned = Crop {
                angle: angle * on_screen,
                ..edit.recipe.crop
            };
            edit.recipe.crop = fit_inside(&turned, &framing);
            edit.note("Straighten", ANGLE.format(angle), step);
        }
    }

    check_row(ui, &mut state.crop_tool.show_grid, "Show Grid");
    ui.add_space(theme::GRID);
    let gap = theme::GRID;
    let width = (ui.available_width() - gap * 2.0) / 3.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        let constrain = text_button(ui, "Constrain", false, width)
            .on_hover_text("The largest crop with no empty corners");
        if constrain.clicked() {
            state.effects.push(Effect::ConstrainCrop);
        }
        if text_button(ui, "Reset", false, width).clicked()
            && let Some(edit) = state.edit_mut()
        {
            edit.recipe.crop = Default::default();
            edit.note("Reset Crop", "", Step::New);
        }
        if text_button(ui, "Done", false, width)
            .on_hover_text("Enter")
            .clicked()
        {
            state.apply(Action::Confirm);
        }
    });
}

fn guides_panel(ui: &mut Ui, state: &mut AppState) {
    let count = state.edit().map_or(0, |edit| edit.recipe.transform.guides.len());
    note(
        ui,
        "Drag along lines that should be straight. Two verticals or two horizontals correct perspective.",
    );
    ui.add_space(theme::GRID);
    note(
        ui,
        &format!("{count} of {MAX_GUIDES} guides. Right-click one to remove it."),
    );
    ui.add_space(theme::GRID * 2.0);
    let gap = theme::GRID;
    let width = (ui.available_width() - gap) * 0.5;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        if text_button(ui, "Clear", false, width).clicked()
            && let Some(edit) = state.edit_mut()
        {
            edit.recipe.transform.guides.clear();
            edit.note("Clear Guides", "", Step::New);
        }
        if text_button(ui, "Done", false, width)
            .on_hover_text("Enter")
            .clicked()
        {
            state.apply(Action::Confirm);
        }
    });
}

fn tool_panel(ui: &mut Ui, state: &mut AppState, live: Option<&LivePhoto>) {
    if state.tool == Tool::None {
        return;
    }
    Frame::new()
        .inner_margin(egui::Margin {
            left: 12,
            right: 12,
            top: 8,
            bottom: 12,
        })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            match state.tool {
                Tool::Crop => crop_panel(ui, state, live),
                Tool::Guides => guides_panel(ui, state),
                Tool::Heal => local_panels::heal(ui, state),
                Tool::Masks => local_panels::masks(ui, state),
                Tool::RedEye => local_panels::red_eye(ui, state),
                Tool::None => {}
            }
        });
    divider(ui);
}

fn lens_status(live: Option<&LivePhoto>) -> Option<String> {
    let session = &live?.session;
    if let Some(error) = session.lens_error() {
        return Some(error.to_string());
    }
    session.lens_profile().and_then(|profile| profile.lens.clone())
}

fn sections(
    ui: &mut Ui,
    state: &mut AppState,
    histogram: Option<&Histogram>,
    live: Option<&LivePhoto>,
    picking: &mut bool,
) {
    let mut context = PanelContext {
        histogram,
        as_shot: live.map(|photo| photo.as_shot),
        picking_white_balance: picking,
        lens_status: lens_status(live),
        draw_guides: false,
        profiles: live.map(|photo| photo.session.profile_choices()).unwrap_or_default(),
        request: None,
    };
    let mut nothing_open = PhotoEdit::new(VariantId(0), Recipe::default());
    for panel in Section::ALL {
        let open = state.is_section_open(panel);
        let edit = state.edit_mut().unwrap_or(&mut nothing_open);
        let header = SectionHeader {
            title: panel.title(),
            open,
            enabled: panel.can_bypass().then(|| !edit.is_bypassed(panel)),
            modified: !panel.is_default(&edit.recipe),
        };
        let events = section(ui, &header, |ui| show_section(ui, panel, edit, &mut context));
        if events.reset {
            edit.reset_section(panel);
        }
        if events.switched {
            let bypassed = !edit.is_bypassed(panel);
            edit.set_bypassed(panel, bypassed);
        }
        if events.toggled {
            state.set_section_open(panel, !open);
        }
    }
    if context.draw_guides && state.tool != Tool::Guides {
        state.apply(Action::ToggleTool(Tool::Guides));
    }
    match context.request {
        Some(PanelRequest::AutoTone) => state.apply(Action::AutoTone),
        Some(PanelRequest::RescanProfiles) => state.effects.push(Effect::RescanProfiles),
        Some(PanelRequest::OpenProfileFolder) => state.effects.push(Effect::OpenProfileFolder),
        None => {}
    }
}

pub struct RightPanelInput<'a> {
    pub live: Option<&'a LivePhoto>,
    pub histogram: Option<&'a Histogram>,
    pub picking_white_balance: &'a mut bool,
}

pub fn show(ui: &mut Ui, state: &mut AppState, input: RightPanelInput<'_>) {
    let RightPanelInput {
        live,
        histogram,
        picking_white_balance,
    } = input;
    let frame = Frame::new().fill(theme::PANEL);
    let panel = Panel::right("right panel")
        .exact_size(theme::RIGHT_PANEL_WIDTH)
        .show_separator_line(false)
        .frame(frame);
    let mut open = state.panels.right;
    panel.show_collapsible(ui, &mut open, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        Frame::new()
            .inner_margin(egui::Margin {
                left: 12,
                right: 12,
                top: 12,
                bottom: 4,
            })
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                let clipping = tip("clipping", Action::ToggleClipping);
                if histogram::show(ui, histogram, state.clipping, &clipping) {
                    state.apply(Action::ToggleClipping);
                }
                let parts = state.photo.as_ref().map(exposure_summary).unwrap_or_default();
                histogram::exposure_line(ui, &parts);
            });
        Frame::new()
            .inner_margin(egui::Margin {
                left: 12,
                right: 12,
                top: 0,
                bottom: 4,
            })
            .show(ui, |ui| tool_strip(ui, state));
        divider(ui);
        tool_panel(ui, state, live);
        proof_panel::show(ui, state);

        Panel::bottom("right footer")
            .exact_size(theme::ROW_HEIGHT + 16.0)
            .show_separator_line(false)
            .frame(Frame::new().inner_margin(egui::Margin::symmetric(12, 8)))
            .show(ui, |ui| {
                let width = ui.available_width();
                if text_button(ui, "Reset All", false, width)
                    .on_hover_text(tip("Reset every setting", Action::ResetAll))
                    .clicked()
                {
                    state.apply(Action::ResetAll);
                }
            });

        let inactive = state.edit().is_none() || matches!(state.decode_state(), DecodeState::Failed(_));
        ScrollArea::vertical()
            .id_salt("develop scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if inactive {
                    ui.disable();
                }
                sections(ui, state, histogram, live, picking_white_balance);
            });
    });
}
