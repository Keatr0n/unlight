//! The soft proofing controls, shown while a proof is on. They live apart from the Develop
//! panels because they change how the photo is viewed, not the photo.

use crate::{
    action::{Action, tip},
    state::{
        AppState, Effect,
        proof::{BUILT_IN_TARGETS, ProofChoice},
    },
    ui::{
        theme,
        widgets::controls::{check_row, divider, note, segmented, subheading},
    },
};
use eframe::egui::{self, Frame, Role, Ui, WidgetInfo};
use unlight_engine::ProofIntent;

const INTENTS: [(ProofIntent, &str); 2] = [(ProofIntent::Perceptual, "Perceptual"), (ProofIntent::Relative, "Relative")];

fn target_row(ui: &mut Ui, state: &mut AppState) {
    let proof = &mut state.proof;
    let mut chosen = None;
    let mut add_profile = false;
    let current = proof.chosen_name();
    let combo = egui::ComboBox::from_id_salt("proof target")
        .selected_text(current)
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for space in BUILT_IN_TARGETS {
                let choice = ProofChoice::Space(space);
                if ui.selectable_label(proof.choice == choice, space.name()).clicked() {
                    chosen = Some(choice);
                }
            }
            if !proof.profiles.is_empty() {
                ui.separator();
            }
            for profile in &proof.profiles {
                let choice = ProofChoice::Icc(profile.path.clone());
                if ui.selectable_label(proof.choice == choice, &profile.name).clicked() {
                    chosen = Some(choice);
                }
            }
            ui.separator();
            add_profile = ui.selectable_label(false, "Load ICC Profile").clicked();
        });
    combo
        .response
        .widget_info(|| WidgetInfo::labeled(Role::ComboBox, true, "Proof Target"));
    if let Some(choice) = chosen {
        proof.choice = choice;
    }
    if add_profile {
        state.effects.push(Effect::ChooseProofProfile);
    }
}

/// Lists what a proof is against and how it gets there, while one is on.
pub fn show(ui: &mut Ui, state: &mut AppState) {
    if !state.proof.enabled {
        return;
    }
    Frame::new()
        .inner_margin(egui::Margin { left: 12, right: 12, top: 4, bottom: 12 })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            subheading(ui, "Soft Proofing");
            ui.add_space(theme::GRID);
            target_row(ui, state);
            ui.add_space(theme::GRID);
            segmented(ui, &mut state.proof.intent, &INTENTS);
            ui.add_space(theme::GRID);
            let label = tip("Gamut Warning", Action::ToggleGamutWarning);
            check_row(ui, &mut state.proof.gamut_warning, &label);
            if let Some(message) = state.proof.refusal() {
                ui.add_space(theme::GRID);
                note(ui, &format!("This profile can't be used: {message}"));
            }
        });
    divider(ui);
}
