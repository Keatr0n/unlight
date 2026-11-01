use super::{PanelContext, param};
use crate::{
    state::edit::{PhotoEdit, Step},
    ui::{
        theme,
        widgets::{controls::text_button, slider::Spec},
    },
};
use eframe::egui::{Layout, Ui, vec2};
use unlight_recipe::Upright;

const VERTICAL: Spec = Spec::bipolar("Vertical", 100.0);
const HORIZONTAL: Spec = Spec::bipolar("Horizontal", 100.0);
const ROTATE: Spec = Spec::bipolar("Rotate", 10.0).decimals(1, 0.1).suffix("°");
const ASPECT: Spec = Spec::bipolar("Aspect", 100.0);
const SCALE: Spec = Spec::range("Scale", 50.0, 150.0).decimals(0, 1.0).suffix("%");
const OFFSET_X: Spec = Spec::bipolar("X Offset", 100.0);
const OFFSET_Y: Spec = Spec::bipolar("Y Offset", 100.0);

const UPRIGHT: [(Upright, &str); 6] = [
    (Upright::Off, "Off"),
    (Upright::Auto, "Auto"),
    (Upright::Guided, "Guided"),
    (Upright::Level, "Level"),
    (Upright::Vertical, "Vertical"),
    (Upright::Full, "Full"),
];

fn upright_buttons(ui: &mut Ui, edit: &mut PhotoEdit, context: &mut PanelContext<'_>) {
    let width = ui.available_width();
    let columns = 3;
    let gap = theme::GRID;
    let button = (width - gap * (columns as f32 - 1.0)) / columns as f32;
    for row in UPRIGHT.chunks(columns) {
        let height = theme::ROW_HEIGHT;
        ui.allocate_ui_with_layout(
            vec2(width, height),
            Layout::left_to_right(eframe::egui::Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (mode, label) in row {
                    let selected = edit.recipe.transform.upright == *mode;
                    if text_button(ui, label, selected, button).clicked() {
                        if *mode == Upright::Guided {
                            context.draw_guides = true;
                        } else if !selected {
                            edit.recipe.transform.upright = *mode;
                            edit.note("Upright", *label, Step::New);
                        }
                    }
                }
            },
        );
        ui.add_space(gap);
    }
}

pub fn show(ui: &mut Ui, edit: &mut PhotoEdit, context: &mut PanelContext<'_>) {
    upright_buttons(ui, edit, context);
    ui.add_space(theme::GRID);
    param(ui, edit, &VERTICAL, |recipe| &mut recipe.transform.vertical);
    param(ui, edit, &HORIZONTAL, |recipe| &mut recipe.transform.horizontal);
    param(ui, edit, &ROTATE, |recipe| &mut recipe.transform.rotate);
    param(ui, edit, &ASPECT, |recipe| &mut recipe.transform.aspect);
    param(ui, edit, &SCALE, |recipe| &mut recipe.transform.scale);
    param(ui, edit, &OFFSET_X, |recipe| &mut recipe.transform.offset_x);
    param(ui, edit, &OFFSET_Y, |recipe| &mut recipe.transform.offset_y);
}
