//! Copy and paste of develop settings, with Lightroom's per-group selection.

use serde_json::{Map, Value};
use unlight_catalog::PartialRecipe;
use unlight_recipe::Recipe;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingGroup {
    Treatment,
    Profile,
    WhiteBalance,
    BasicTone,
    Presence,
    ToneCurve,
    ColorMixer,
    BlackAndWhiteMix,
    ColorGrading,
    Sharpening,
    NoiseReduction,
    LensCorrections,
    Transform,
    Effects,
    Calibration,
    Crop,
    Masks,
    Healing,
    RedEye,
}

impl SettingGroup {
    pub const ALL: [Self; 19] = [
        Self::Treatment,
        Self::Profile,
        Self::WhiteBalance,
        Self::BasicTone,
        Self::Presence,
        Self::ToneCurve,
        Self::ColorMixer,
        Self::BlackAndWhiteMix,
        Self::ColorGrading,
        Self::Sharpening,
        Self::NoiseReduction,
        Self::LensCorrections,
        Self::Transform,
        Self::Effects,
        Self::Calibration,
        Self::Crop,
        Self::Masks,
        Self::Healing,
        Self::RedEye,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Treatment => "Treatment",
            Self::Profile => "Profile",
            Self::WhiteBalance => "White Balance",
            Self::BasicTone => "Basic Tone",
            Self::Presence => "Presence",
            Self::ToneCurve => "Tone Curve",
            Self::ColorMixer => "Color Mixer",
            Self::BlackAndWhiteMix => "B&W Mix",
            Self::ColorGrading => "Color Grading",
            Self::Sharpening => "Sharpening",
            Self::NoiseReduction => "Noise Reduction",
            Self::LensCorrections => "Lens Corrections",
            Self::Transform => "Transform",
            Self::Effects => "Effects",
            Self::Calibration => "Calibration",
            Self::Crop => "Crop",
            Self::Masks => "Masks",
            Self::Healing => "Healing",
            Self::RedEye => "Red Eye",
        }
    }

    /// Where this group's settings live in a recipe's JSON, as (section, keys). No keys means
    /// the whole section.
    fn json_paths(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Treatment => ("black_and_white", &["enabled"]),
            Self::Profile => ("camera_profile", &[]),
            Self::WhiteBalance => ("white_balance", &[]),
            Self::BasicTone => (
                "basic",
                &[
                    "exposure",
                    "contrast",
                    "highlights",
                    "shadows",
                    "whites",
                    "blacks",
                ],
            ),
            Self::Presence => (
                "basic",
                &["texture", "clarity", "dehaze", "vibrance", "saturation"],
            ),
            Self::ToneCurve => ("tone_curve", &[]),
            Self::ColorMixer => ("color_mixer", &[]),
            Self::BlackAndWhiteMix => ("black_and_white", &["mix"]),
            Self::ColorGrading => ("color_grading", &[]),
            Self::Sharpening => ("detail", &["sharpening"]),
            Self::NoiseReduction => ("detail", &["noise_reduction"]),
            Self::LensCorrections => ("lens", &[]),
            Self::Transform => ("transform", &[]),
            Self::Effects => ("effects", &[]),
            Self::Calibration => ("calibration", &[]),
            Self::Crop => ("crop", &[]),
            Self::Masks => ("masks", &[]),
            Self::Healing => ("healing", &[]),
            Self::RedEye => ("red_eye", &[]),
        }
    }

    /// Geometry and retouching belong to one photo, so they start unticked.
    fn selected_by_default(self) -> bool {
        !matches!(
            self,
            Self::Crop | Self::Transform | Self::Masks | Self::Healing | Self::RedEye
        )
    }

    fn copy(self, from: &Recipe, to: &mut Recipe) {
        match self {
            Self::Treatment => to.black_and_white.enabled = from.black_and_white.enabled,
            Self::Profile => to.camera_profile = from.camera_profile.clone(),
            Self::WhiteBalance => to.white_balance = from.white_balance,
            Self::BasicTone => {
                let (source, target) = (&from.basic, &mut to.basic);
                target.exposure = source.exposure;
                target.contrast = source.contrast;
                target.highlights = source.highlights;
                target.shadows = source.shadows;
                target.whites = source.whites;
                target.blacks = source.blacks;
            }
            Self::Presence => {
                let (source, target) = (&from.basic, &mut to.basic);
                target.texture = source.texture;
                target.clarity = source.clarity;
                target.dehaze = source.dehaze;
                target.vibrance = source.vibrance;
                target.saturation = source.saturation;
            }
            Self::ToneCurve => to.tone_curve = from.tone_curve.clone(),
            Self::ColorMixer => to.color_mixer = from.color_mixer,
            Self::BlackAndWhiteMix => to.black_and_white.mix = from.black_and_white.mix,
            Self::ColorGrading => to.color_grading = from.color_grading,
            Self::Sharpening => to.detail.sharpening = from.detail.sharpening,
            Self::NoiseReduction => to.detail.noise_reduction = from.detail.noise_reduction,
            Self::LensCorrections => to.lens = from.lens,
            Self::Transform => to.transform = from.transform.clone(),
            Self::Effects => to.effects = from.effects,
            Self::Calibration => to.calibration = from.calibration,
            Self::Crop => to.crop = from.crop,
            Self::Masks => to.masks = from.masks.clone(),
            Self::Healing => to.healing = from.healing.clone(),
            Self::RedEye => to.red_eye = from.red_eye.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingSelection([bool; SettingGroup::ALL.len()]);

impl Default for SettingSelection {
    fn default() -> Self {
        Self(SettingGroup::ALL.map(SettingGroup::selected_by_default))
    }
}

impl SettingSelection {
    fn index(group: SettingGroup) -> usize {
        group as usize
    }

    pub fn contains(&self, group: SettingGroup) -> bool {
        self.0[Self::index(group)]
    }

    pub fn set(&mut self, group: SettingGroup, selected: bool) {
        self.0[Self::index(group)] = selected;
    }

    pub fn set_all(&mut self, selected: bool) {
        self.0 = [selected; SettingGroup::ALL.len()];
    }

    pub fn count(&self) -> usize {
        self.0.iter().filter(|selected| **selected).count()
    }
}

fn merge(target: &mut Map<String, Value>, section: &str, value: Value) {
    match (target.get_mut(section), value) {
        (Some(Value::Object(existing)), Value::Object(new)) => existing.extend(new),
        (_, value) => {
            target.insert(section.to_owned(), value);
        }
    }
}

/// Keeps only the selected groups of a recipe-shaped JSON object.
fn select_settings(settings: &Map<String, Value>, selection: &SettingSelection) -> Map<String, Value> {
    let mut kept = Map::new();
    for group in SettingGroup::ALL
        .into_iter()
        .filter(|group| selection.contains(*group))
    {
        let (section, keys) = group.json_paths();
        let Some(value) = settings.get(section) else {
            continue;
        };
        let picked = match (keys, value) {
            ([], _) => Some(value.clone()),
            (keys, Value::Object(object)) => {
                let fields = keys
                    .iter()
                    .filter_map(|key| object.get(*key).map(|field| ((*key).to_owned(), field.clone())));
                let fields: Map<String, Value> = fields.collect();
                (!fields.is_empty()).then_some(Value::Object(fields))
            }
            _ => None,
        };
        if let Some(picked) = picked {
            merge(&mut kept, section, picked);
        }
    }
    kept
}

fn as_object(value: Result<Value, serde_json::Error>) -> Map<String, Value> {
    match value {
        Ok(Value::Object(object)) => object,
        _ => Map::new(),
    }
}

/// The selected groups of a recipe, for saving as a preset.
pub fn partial_recipe(recipe: &Recipe, selection: &SettingSelection) -> PartialRecipe {
    let settings = as_object(serde_json::to_value(recipe));
    PartialRecipe::from_settings(select_settings(&settings, selection))
}

/// A preset cut down to the selected groups, for applying part of one.
pub fn narrow_preset(preset: &PartialRecipe, selection: &SettingSelection) -> PartialRecipe {
    let settings = as_object(serde_json::to_value(preset));
    PartialRecipe::from_settings(select_settings(&settings, selection))
}

#[derive(Clone, Debug, PartialEq)]
pub struct CopiedSettings {
    recipe: Recipe,
    selection: SettingSelection,
}

impl CopiedSettings {
    pub fn new(recipe: Recipe, selection: SettingSelection) -> Self {
        Self { recipe, selection }
    }

    pub fn paste_onto(&self, target: &mut Recipe) {
        for group in SettingGroup::ALL {
            if self.selection.contains(group) {
                group.copy(&self.recipe, target);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_recipe::{Crop, WhiteBalance};

    fn edited() -> Recipe {
        let mut recipe = Recipe::default();
        recipe.basic.exposure = 1.0;
        recipe.basic.vibrance = 30.0;
        recipe.white_balance = WhiteBalance::Custom {
            temperature: 5000.0,
            tint: 4.0,
        };
        recipe.crop = Crop {
            left: 0.1,
            ..Crop::default()
        };
        recipe.detail.sharpening.amount = 80.0;
        recipe
    }

    #[test]
    fn default_selection_leaves_crop_behind() {
        let mut target = Recipe::default();
        CopiedSettings::new(edited(), SettingSelection::default()).paste_onto(&mut target);
        assert_eq!(target.basic.exposure, 1.0);
        assert_eq!(target.crop, Crop::default());
    }

    #[test]
    fn only_selected_groups_are_pasted() {
        let mut selection = SettingSelection::default();
        selection.set_all(false);
        selection.set(SettingGroup::Presence, true);
        let mut target = Recipe::default();
        CopiedSettings::new(edited(), selection).paste_onto(&mut target);
        assert_eq!(target.basic.vibrance, 30.0);
        assert_eq!(target.basic.exposure, 0.0);
        assert_eq!(target.white_balance, WhiteBalance::AsShot);
        assert_eq!(target.detail, Default::default());
    }

    #[test]
    fn selecting_everything_copies_the_whole_recipe() {
        let mut selection = SettingSelection::default();
        selection.set_all(true);
        let mut target = Recipe::default();
        CopiedSettings::new(edited(), selection).paste_onto(&mut target);
        assert_eq!(target, edited());
    }

    #[test]
    fn the_profile_is_its_own_group() {
        let source = Recipe { camera_profile: Some("Vivid".to_owned()), ..Recipe::default() };
        let mut selection = SettingSelection::default();
        selection.set_all(false);
        let mut target = Recipe::default();
        CopiedSettings::new(source.clone(), selection).paste_onto(&mut target);
        assert_eq!(target.camera_profile, None);
        selection.set(SettingGroup::Profile, true);
        CopiedSettings::new(source.clone(), selection).paste_onto(&mut target);
        assert_eq!(target.camera_profile.as_deref(), Some("Vivid"));
        let applied = partial_recipe(&source, &selection).apply_to(&Recipe::default()).unwrap();
        assert_eq!(applied.camera_profile.as_deref(), Some("Vivid"));
    }

    #[test]
    fn a_partial_recipe_names_only_its_groups() {
        let mut selection = SettingSelection::default();
        selection.set_all(false);
        selection.set(SettingGroup::BasicTone, true);
        let partial = partial_recipe(&edited(), &selection);
        let mut target = Recipe::default();
        target.basic.vibrance = 11.0;
        target.detail.sharpening.amount = 5.0;
        let applied = partial.apply_to(&target).unwrap();
        assert_eq!(applied.basic.exposure, 1.0);
        assert_eq!(applied.basic.vibrance, 11.0);
        assert_eq!(applied.detail.sharpening.amount, 5.0);
    }

    #[test]
    fn a_selected_group_at_its_default_still_overrides() {
        let mut selection = SettingSelection::default();
        selection.set_all(false);
        selection.set(SettingGroup::BasicTone, true);
        let partial = partial_recipe(&Recipe::default(), &selection);
        let mut target = Recipe::default();
        target.basic.exposure = 2.0;
        assert_eq!(partial.apply_to(&target).unwrap().basic.exposure, 0.0);
    }

    #[test]
    fn groups_sharing_a_section_combine() {
        let mut selection = SettingSelection::default();
        selection.set_all(false);
        selection.set(SettingGroup::BasicTone, true);
        selection.set(SettingGroup::Presence, true);
        let partial = partial_recipe(&edited(), &selection);
        let applied = partial.apply_to(&Recipe::default()).unwrap();
        assert_eq!((applied.basic.exposure, applied.basic.vibrance), (1.0, 30.0));
    }

    #[test]
    fn a_preset_can_be_applied_in_part() {
        let mut everything = SettingSelection::default();
        everything.set_all(true);
        let preset = partial_recipe(&edited(), &everything);
        let mut only_presence = SettingSelection::default();
        only_presence.set_all(false);
        only_presence.set(SettingGroup::Presence, true);
        let applied = narrow_preset(&preset, &only_presence)
            .apply_to(&Recipe::default())
            .unwrap();
        assert_eq!(applied.basic.vibrance, 30.0);
        assert_eq!(applied.basic.exposure, 0.0);
    }

    #[test]
    fn pasting_keeps_unselected_values_of_the_target() {
        let mut selection = SettingSelection::default();
        selection.set_all(false);
        selection.set(SettingGroup::WhiteBalance, true);
        let mut target = Recipe::default();
        target.basic.contrast = 12.0;
        CopiedSettings::new(edited(), selection).paste_onto(&mut target);
        assert_eq!(target.basic.contrast, 12.0);
    }
}
