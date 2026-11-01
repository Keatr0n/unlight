//! The Develop panels as data: which recipe fields each one owns, for per-panel reset and for
//! the enable switch in its header.

use unlight_recipe::{Panel, Recipe};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Basic,
    ToneCurve,
    ColorMixer,
    ColorGrading,
    Detail,
    LensCorrections,
    Transform,
    Effects,
    Calibration,
}

impl Section {
    pub const ALL: [Self; 9] = [
        Self::Basic,
        Self::ToneCurve,
        Self::ColorMixer,
        Self::ColorGrading,
        Self::Detail,
        Self::LensCorrections,
        Self::Transform,
        Self::Effects,
        Self::Calibration,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Basic => "Basic",
            Self::ToneCurve => "Tone Curve",
            Self::ColorMixer => "Color Mixer",
            Self::ColorGrading => "Color Grading",
            Self::Detail => "Detail",
            Self::LensCorrections => "Lens Corrections",
            Self::Transform => "Transform",
            Self::Effects => "Effects",
            Self::Calibration => "Calibration",
        }
    }

    /// The recipe's name for this panel's switch. Basic has none: switching it off would
    /// mean nothing, and Lightroom doesn't have one either.
    pub fn panel(self) -> Option<Panel> {
        match self {
            Self::Basic => None,
            Self::ToneCurve => Some(Panel::ToneCurve),
            Self::ColorMixer => Some(Panel::ColorMixer),
            Self::ColorGrading => Some(Panel::ColorGrading),
            Self::Detail => Some(Panel::Detail),
            Self::LensCorrections => Some(Panel::LensCorrections),
            Self::Transform => Some(Panel::Transform),
            Self::Effects => Some(Panel::Effects),
            Self::Calibration => Some(Panel::Calibration),
        }
    }

    pub fn can_bypass(self) -> bool {
        self.panel().is_some()
    }

    pub fn reset(self, recipe: &mut Recipe) {
        self.copy(&Recipe::default(), recipe);
    }

    pub fn is_default(self, recipe: &Recipe) -> bool {
        let mut reset = recipe.clone();
        self.reset(&mut reset);
        reset == *recipe
    }

    fn copy(self, from: &Recipe, to: &mut Recipe) {
        match self {
            Self::Basic => {
                to.white_balance = from.white_balance;
                to.basic = from.basic;
                to.black_and_white.enabled = from.black_and_white.enabled;
                to.camera_profile = from.camera_profile.clone();
            }
            Self::ToneCurve => to.tone_curve = from.tone_curve.clone(),
            Self::ColorMixer => {
                to.color_mixer = from.color_mixer;
                to.black_and_white.mix = from.black_and_white.mix;
            }
            Self::ColorGrading => to.color_grading = from.color_grading,
            Self::Detail => to.detail = from.detail,
            Self::LensCorrections => to.lens = from.lens,
            Self::Transform => to.transform = from.transform.clone(),
            Self::Effects => to.effects = from.effects,
            Self::Calibration => to.calibration = from.calibration,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_recipe::{Upright, WhiteBalance};

    #[test]
    fn a_reset_only_touches_its_own_panel() {
        let mut recipe = Recipe::default();
        recipe.basic.exposure = 1.0;
        recipe.detail.sharpening.amount = 90.0;
        Section::Detail.reset(&mut recipe);
        assert_eq!(recipe.detail, Default::default());
        assert_eq!(recipe.basic.exposure, 1.0);
    }

    #[test]
    fn basic_reset_clears_white_balance_and_treatment() {
        let mut recipe = Recipe {
            white_balance: WhiteBalance::Custom {
                temperature: 4000.0,
                tint: 3.0,
            },
            ..Recipe::default()
        };
        recipe.black_and_white.enabled = true;
        Section::Basic.reset(&mut recipe);
        assert_eq!(recipe, Recipe::default());
    }

    #[test]
    fn the_profile_belongs_to_the_basic_panel() {
        let mut recipe = Recipe { camera_profile: Some("Vivid".to_owned()), ..Recipe::default() };
        assert!(!Section::Basic.is_default(&recipe));
        Section::Detail.reset(&mut recipe);
        assert_eq!(recipe.camera_profile.as_deref(), Some("Vivid"));
        Section::Basic.reset(&mut recipe);
        assert_eq!(recipe.camera_profile, None);
    }

    #[test]
    fn every_panel_detects_its_own_edits() {
        let mut recipe = Recipe::default();
        assert!(Section::ALL.iter().all(|section| section.is_default(&recipe)));
        recipe.transform.upright = Upright::Full;
        assert!(!Section::Transform.is_default(&recipe));
        assert!(Section::Effects.is_default(&recipe));
    }
}
