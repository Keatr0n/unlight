//! The Masks, Healing and Red Eye tools: what's selected, what the brush is set to, and the
//! edits they make to a recipe. Positions are all in source coordinates, as the recipe stores
//! them, so a tool's work follows the crop and every warp.

use super::{AppState, Module, Tool, edit::Step};
use unlight_recipe::{BrushDab, HealMode, HealSpot, Mask, MaskComponent, MaskShape, Recipe, RedEye};

const MIN_BRUSH: f32 = 0.0005;
const MAX_BRUSH: f32 = 0.2;
/// How much `[` and `]` change a size by, as Lightroom's steps do.
const SIZE_STEP: f32 = 1.2;
/// Gap between dabs as a share of the radius. Any wider and a soft stroke goes lumpy.
const DAB_SPACING: f32 = 0.2;
const MIN_SPOT: f32 = 0.002;
const MAX_SPOT: f32 = 0.2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskKind {
    Brush,
    Linear,
    Radial,
    Luminance,
    Colour,
}

impl MaskKind {
    pub const ALL: [Self; 5] = [
        Self::Brush,
        Self::Linear,
        Self::Radial,
        Self::Luminance,
        Self::Colour,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Brush => "Brush",
            Self::Linear => "Linear",
            Self::Radial => "Radial",
            Self::Luminance => "Luma",
            Self::Colour => "Colour",
        }
    }

    fn shape(self, center: [f32; 2]) -> MaskShape {
        match self {
            Self::Brush => MaskShape::Brush { dabs: Vec::new() },
            Self::Linear => MaskShape::Linear {
                start: [center[0], center[1] - 0.2],
                end: [center[0], center[1] + 0.2],
            },
            Self::Radial => MaskShape::Radial {
                center,
                radii: [0.2, 0.15],
                angle: 0.0,
                feather: 0.5,
                inverted: false,
            },
            Self::Luminance => MaskShape::LuminanceRange {
                low: 0.5,
                high: 1.0,
                smoothness: 0.3,
            },
            Self::Colour => MaskShape::ColorRange {
                samples: Vec::new(),
                tolerance: 0.5,
            },
        }
    }
}

/// What choosing a kind of mask does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MaskAction {
    #[default]
    New,
    Add,
    Subtract,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrushSettings {
    /// Share of the image's long edge.
    pub radius: f32,
    pub feather: f32,
    pub flow: f32,
    pub auto_mask: bool,
    pub erase: bool,
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self {
            radius: 0.03,
            feather: 0.6,
            flow: 0.7,
            auto_mask: false,
            erase: false,
        }
    }
}

impl BrushSettings {
    pub fn dab(&self, position: [f32; 2], erase: bool) -> BrushDab {
        BrushDab {
            position,
            radius: self.radius,
            feather: self.feather,
            flow: self.flow,
            erase,
            auto_mask: self.auto_mask,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HealSettings {
    pub mode: HealMode,
    pub radius: f32,
    pub feather: f32,
    pub opacity: f32,
}

impl Default for HealSettings {
    fn default() -> Self {
        Self {
            mode: HealMode::Heal,
            radius: 0.012,
            feather: 0.5,
            opacity: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EyeSettings {
    pub radius: f32,
    pub pupil_size: f32,
    pub darken: f32,
}

impl Default for EyeSettings {
    fn default() -> Self {
        Self {
            radius: 0.012,
            pupil_size: 0.8,
            darken: 0.5,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LocalTools {
    pub mask: Option<usize>,
    pub mask_action: MaskAction,
    pub spot: Option<usize>,
    pub eye: Option<usize>,
    pub brush: BrushSettings,
    pub heal: HealSettings,
    pub eyes: EyeSettings,
    /// Shows the selected mask in red.
    pub overlay: bool,
    /// The next click on the photo samples a colour for the selected mask.
    pub picking_colour: bool,
}

fn scaled(value: f32, steps: i32, range: (f32, f32)) -> f32 {
    (value * SIZE_STEP.powi(steps)).clamp(range.0, range.1)
}

/// Selects the whole frame: an ellipse of no size, inverted.
fn whole_frame() -> MaskComponent {
    let shape = MaskShape::Radial {
        center: [0.5, 0.5],
        radii: [0.0; 2],
        angle: 0.0,
        feather: 0.0,
        inverted: true,
    };
    MaskComponent {
        shape,
        subtract: false,
    }
}

/// Flips what a mask selects. A single shape with a flag of its own uses that. Anything else
/// goes the long way round: the whole frame, less everything it selected.
pub fn invert_mask(mask: &mut Mask) {
    if let [MaskComponent { shape, .. }] = mask.components.as_mut_slice() {
        match shape {
            MaskShape::Radial { inverted, .. } | MaskShape::Raster { inverted, .. } => {
                *inverted = !*inverted;
                return;
            }
            MaskShape::Linear { start, end } => {
                std::mem::swap(start, end);
                return;
            }
            _ => {}
        }
    }
    if mask.components.first() == Some(&whole_frame()) {
        mask.components.remove(0);
    } else {
        mask.components.insert(0, whole_frame());
        mask.components[1..]
            .iter_mut()
            .for_each(|component| component.subtract = !component.subtract);
        return;
    }
    mask.components
        .iter_mut()
        .for_each(|component| component.subtract = !component.subtract);
}

/// Dabs from just after `from` up to `to`, evenly spaced. Distances are in shares of the long
/// edge, so spacing stays even whichever way the stroke runs on a wide frame. `axes` converts a
/// normalised difference to those units.
pub fn dabs_between(from: [f32; 2], to: [f32; 2], axes: [f32; 2], radius: f32) -> Vec<[f32; 2]> {
    let length = ((to[0] - from[0]) * axes[0]).hypot((to[1] - from[1]) * axes[1]);
    let spacing = (radius * DAB_SPACING).max(1e-5);
    let count = (length / spacing).floor() as usize;
    (1..=count)
        .map(|step| {
            let along = step as f32 * spacing / length;
            [
                from[0] + (to[0] - from[0]) * along,
                from[1] + (to[1] - from[1]) * along,
            ]
        })
        .collect()
}

fn crop_center(recipe: &Recipe) -> [f32; 2] {
    let crop = &recipe.crop;
    [(crop.left + crop.right) * 0.5, (crop.top + crop.bottom) * 0.5]
}

impl AppState {
    fn selected_mask(&self) -> Option<usize> {
        let count = self.edit()?.recipe.masks.len();
        self.local.mask.filter(|index| *index < count)
    }

    /// The mask the tools are working on, if it's still there after an undo.
    pub fn current_mask(&self) -> Option<&Mask> {
        self.edit()?.recipe.masks.get(self.selected_mask()?)
    }

    pub fn current_mask_index(&self) -> Option<usize> {
        self.selected_mask()
    }

    /// The mask to show in red, when that's switched on.
    pub fn overlay_mask(&self) -> Option<usize> {
        self.selected_mask()
            .filter(|_| self.tool == Tool::Masks && self.local.overlay)
    }

    pub fn add_mask(&mut self, kind: MaskKind) {
        let Some(edit) = self.edit_mut() else {
            return;
        };
        let number = edit.recipe.masks.len() + 1;
        let shape = kind.shape(crop_center(&edit.recipe));
        edit.recipe.masks.push(Mask {
            name: format!("Mask {number}"),
            components: vec![MaskComponent {
                shape,
                subtract: false,
            }],
            ..Mask::default()
        });
        let index = edit.recipe.masks.len() - 1;
        edit.note("Add Mask", kind.name(), Step::New);
        self.local.mask = Some(index);
        self.local.picking_colour = kind == MaskKind::Colour;
    }

    /// Does whatever the panel is set to with a kind of mask.
    pub fn choose_mask_kind(&mut self, kind: MaskKind) {
        match self.local.mask_action {
            MaskAction::Add if self.selected_mask().is_some() => self.add_component(kind, false),
            MaskAction::Subtract if self.selected_mask().is_some() => self.add_component(kind, true),
            _ => self.add_mask(kind),
        }
    }

    /// Adds a shape to the selected mask, or takes one away from it.
    pub fn add_component(&mut self, kind: MaskKind, subtract: bool) {
        let Some(index) = self.selected_mask() else {
            return;
        };
        let Some(edit) = self.edit_mut() else {
            return;
        };
        let shape = kind.shape(crop_center(&edit.recipe));
        edit.recipe.masks[index]
            .components
            .push(MaskComponent { shape, subtract });
        edit.note(
            if subtract {
                "Subtract from Mask"
            } else {
                "Add to Mask"
            },
            kind.name(),
            Step::New,
        );
        self.local.picking_colour = kind == MaskKind::Colour;
    }

    pub fn delete_mask(&mut self) {
        let Some(index) = self.selected_mask() else {
            return;
        };
        let Some(edit) = self.edit_mut() else {
            return;
        };
        edit.recipe.masks.remove(index);
        let left = edit.recipe.masks.len();
        edit.note("Delete Mask", "", Step::New);
        self.local.mask = (left > 0).then(|| index.min(left - 1));
    }

    pub fn duplicate_mask(&mut self) {
        let Some(index) = self.selected_mask() else {
            return;
        };
        let Some(edit) = self.edit_mut() else {
            return;
        };
        let mut copy = edit.recipe.masks[index].clone();
        copy.name = format!("{} Copy", copy.name);
        edit.recipe.masks.insert(index + 1, copy);
        edit.note("Duplicate Mask", "", Step::New);
        self.local.mask = Some(index + 1);
    }

    pub fn invert_current_mask(&mut self) {
        let Some(index) = self.selected_mask() else {
            return;
        };
        if let Some(edit) = self.edit_mut() {
            invert_mask(&mut edit.recipe.masks[index]);
            edit.note("Invert Mask", "", Step::New);
        }
    }

    /// Adds a sampled colour to the selected mask's colour range.
    pub fn add_colour_sample(&mut self, colour: [f32; 3]) {
        let Some(index) = self.selected_mask() else {
            return;
        };
        if let Some(edit) = self.edit_mut() {
            let mut components = edit.recipe.masks[index].components.iter_mut().rev();
            let samples = components.find_map(|component| match &mut component.shape {
                MaskShape::ColorRange { samples, .. } => Some(samples),
                _ => None,
            });
            if let Some(samples) = samples {
                samples.push(colour);
                edit.note("Sample Colour", "", Step::New);
            }
        }
        self.local.picking_colour = false;
    }

    /// A new spot at `target`, patched from `source`, which becomes the selection.
    pub fn add_heal_spot(&mut self, target: [f32; 2], source: [f32; 2]) {
        let settings = self.local.heal;
        let Some(edit) = self.edit_mut() else {
            return;
        };
        edit.recipe.healing.push(HealSpot {
            mode: settings.mode,
            target,
            source,
            radius: settings.radius,
            feather: settings.feather,
            opacity: settings.opacity,
        });
        let index = edit.recipe.healing.len() - 1;
        edit.note("Heal", "", Step::New);
        self.local.spot = Some(index);
    }

    pub fn add_red_eye(&mut self, center: [f32; 2]) {
        let settings = self.local.eyes;
        let Some(edit) = self.edit_mut() else {
            return;
        };
        edit.recipe.red_eye.push(RedEye {
            center,
            radius: settings.radius,
            pupil_size: settings.pupil_size,
            darken: settings.darken,
        });
        let index = edit.recipe.red_eye.len() - 1;
        edit.note("Red Eye", "", Step::New);
        self.local.eye = Some(index);
    }

    /// `[` and `]`: the brush, or whichever spot the tool has selected.
    pub fn resize_tool(&mut self, steps: i32) {
        let (spot, eye) = (self.local.spot, self.local.eye);
        match self.tool {
            Tool::Masks => {
                self.local.brush.radius = scaled(self.local.brush.radius, steps, (MIN_BRUSH, MAX_BRUSH));
            }
            Tool::Heal => {
                let radius = scaled(self.local.heal.radius, steps, (MIN_SPOT, MAX_SPOT));
                self.local.heal.radius = radius;
                if let Some(edit) = self.edit_mut()
                    && let Some(spot) = spot.and_then(|index| edit.recipe.healing.get_mut(index))
                {
                    spot.radius = scaled(spot.radius, steps, (MIN_SPOT, MAX_SPOT));
                    edit.note("Spot Size", "", Step::Burst);
                }
            }
            Tool::RedEye => {
                let radius = scaled(self.local.eyes.radius, steps, (MIN_SPOT, MAX_SPOT));
                self.local.eyes.radius = radius;
                if let Some(edit) = self.edit_mut()
                    && let Some(eye) = eye.and_then(|index| edit.recipe.red_eye.get_mut(index))
                {
                    eye.radius = scaled(eye.radius, steps, (MIN_SPOT, MAX_SPOT));
                    edit.note("Red Eye Size", "", Step::Burst);
                }
            }
            _ => {}
        }
    }

    /// The Delete key: removes whatever the current tool has selected, or with no tool in
    /// hand, the selected photos.
    pub fn delete_selected(&mut self) {
        let (spot, eye) = (self.local.spot, self.local.eye);
        if self.module == Module::Library {
            self.ask_to_remove_selection();
            return;
        }
        match self.tool {
            Tool::None => self.ask_to_remove_selection(),
            Tool::Masks => self.delete_mask(),
            Tool::Heal => {
                if let Some(index) = spot {
                    self.delete_spot(index);
                }
            }
            Tool::RedEye => {
                if let Some(index) = eye {
                    self.delete_eye(index);
                }
            }
            _ => {}
        }
    }

    pub fn delete_spot(&mut self, index: usize) {
        if let Some(edit) = self.edit_mut()
            && index < edit.recipe.healing.len()
        {
            edit.recipe.healing.remove(index);
            edit.note("Delete Spot", "", Step::New);
            self.local.spot = None;
        }
    }

    pub fn delete_eye(&mut self, index: usize) {
        if let Some(edit) = self.edit_mut()
            && index < edit.recipe.red_eye.len()
        {
            edit.recipe.red_eye.remove(index);
            edit.note("Delete Red Eye", "", Step::New);
            self.local.eye = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brushed() -> Mask {
        let shape = MaskShape::Brush { dabs: Vec::new() };
        Mask {
            components: vec![MaskComponent {
                shape,
                subtract: false,
            }],
            ..Mask::default()
        }
    }

    #[test]
    fn inverting_a_gradient_swaps_its_ends() {
        let mut mask = Mask::default();
        let shape = MaskShape::Linear {
            start: [0.0; 2],
            end: [1.0; 2],
        };
        mask.components.push(MaskComponent {
            shape,
            subtract: false,
        });
        invert_mask(&mut mask);
        assert_eq!(
            mask.components[0].shape,
            MaskShape::Linear {
                start: [1.0; 2],
                end: [0.0; 2]
            }
        );
    }

    #[test]
    fn inverting_a_radial_flips_its_flag() {
        let mut mask = Mask::default();
        mask.components.push(MaskComponent {
            shape: MaskKind::Radial.shape([0.5; 2]),
            subtract: false,
        });
        invert_mask(&mut mask);
        assert!(matches!(
            mask.components[0].shape,
            MaskShape::Radial { inverted: true, .. }
        ));
    }

    #[test]
    fn inverting_a_brush_selects_everything_but_the_stroke_and_back() {
        let original = brushed();
        let mut mask = original.clone();
        invert_mask(&mut mask);
        assert_eq!(mask.components.len(), 2);
        assert!(mask.components[1].subtract);
        invert_mask(&mut mask);
        assert_eq!(mask, original);
    }

    #[test]
    fn inverting_keeps_a_subtraction_as_an_addition() {
        let mut mask = brushed();
        let shape = MaskKind::Radial.shape([0.5; 2]);
        mask.components.push(MaskComponent {
            shape,
            subtract: true,
        });
        invert_mask(&mut mask);
        assert!(mask.components[1].subtract);
        assert!(!mask.components[2].subtract);
    }

    #[test]
    fn dabs_land_evenly_along_a_stroke() {
        let dabs = dabs_between([0.0, 0.5], [1.0, 0.5], [1.0, 1.0], 0.5);
        assert_eq!(dabs.len(), 10);
        assert!((dabs[0][0] - 0.1).abs() < 1e-5);
        assert!((dabs[9][0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_short_move_lays_nothing_down_yet() {
        assert!(dabs_between([0.5; 2], [0.5001, 0.5], [1.0, 1.0], 0.1).is_empty());
    }

    #[test]
    fn dab_spacing_measures_the_long_edge_on_a_tall_stretch() {
        let across = dabs_between([0.0, 0.0], [0.0, 1.0], [1.0, 0.5], 0.5);
        assert_eq!(across.len(), 5, "half as far in long-edge units");
    }

    #[test]
    fn sizes_stay_in_range_however_far_they_are_pushed() {
        assert_eq!(scaled(0.1, 100, (MIN_BRUSH, MAX_BRUSH)), MAX_BRUSH);
        assert_eq!(scaled(0.1, -100, (MIN_BRUSH, MAX_BRUSH)), MIN_BRUSH);
    }
}
