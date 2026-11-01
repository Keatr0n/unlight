//! Non-destructive edit recipes. A recipe is plain data: every Develop control lives here,
//! and nothing in it refers to pixels. `process_version` pins the rendering behaviour an
//! edit was made with, so old photos keep their look when the engine improves.
//!
//! Slider conventions follow Lightroom: symmetric controls run -100..=100 with 0 meaning no
//! change, amount-style controls run 0..=100. Every field has a neutral default, so a recipe
//! saved by an older version loads with new tools switched off.

use serde::{Deserialize, Serialize};

pub const PROCESS_VERSION: u32 = 1;

/// Hue bands of the colour mixer, in this order.
pub const HSL_BANDS: usize = 8;
pub const HSL_BAND_NAMES: [&str; HSL_BANDS] =
    ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recipe {
    pub process_version: u32,
    pub white_balance: WhiteBalance,
    pub basic: Basic,
    pub tone_curve: ToneCurve,
    pub color_mixer: ColorMixer,
    pub black_and_white: BlackAndWhite,
    pub color_grading: ColorGrading,
    pub detail: Detail,
    pub lens: LensCorrections,
    pub transform: Transform,
    pub effects: Effects,
    pub calibration: Calibration,
    pub crop: Crop,
    pub masks: Vec<Mask>,
    pub healing: Vec<HealSpot>,
    pub red_eye: Vec<RedEye>,
    /// Panels switched off. They keep their settings, which just don't apply.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disabled_panels: Vec<Panel>,
    /// The camera profile to use, by name: one a DNG carries, one installed, or "Unlight Default"
    /// for the built-in rendering. Without one, a DNG's own profile or a profile matching the
    /// camera applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_profile: Option<String>,
}

/// The Develop panels that have an on/off switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Panel {
    ToneCurve,
    ColorMixer,
    ColorGrading,
    Detail,
    LensCorrections,
    Transform,
    Effects,
    Calibration,
}

impl Default for Recipe {
    fn default() -> Self {
        Self {
            process_version: PROCESS_VERSION,
            white_balance: WhiteBalance::default(),
            basic: Basic::default(),
            tone_curve: ToneCurve::default(),
            color_mixer: ColorMixer::default(),
            black_and_white: BlackAndWhite::default(),
            color_grading: ColorGrading::default(),
            detail: Detail::default(),
            lens: LensCorrections::default(),
            transform: Transform::default(),
            effects: Effects::default(),
            calibration: Calibration::default(),
            crop: Crop::default(),
            masks: Vec::new(),
            healing: Vec::new(),
            red_eye: Vec::new(),
            disabled_panels: Vec::new(),
            camera_profile: None,
        }
    }
}

impl Recipe {
    /// The recipe as it renders, with switched-off panels doing nothing.
    pub fn effective(&self) -> Self {
        let mut recipe = self.clone();
        for panel in std::mem::take(&mut recipe.disabled_panels) {
            match panel {
                Panel::ToneCurve => recipe.tone_curve = ToneCurve::default(),
                Panel::ColorMixer => {
                    recipe.color_mixer = ColorMixer::default();
                    recipe.black_and_white.mix = [0.0; HSL_BANDS];
                }
                Panel::ColorGrading => recipe.color_grading = ColorGrading::default(),
                Panel::Detail => {
                    recipe.detail.sharpening.amount = 0.0;
                    recipe.detail.noise_reduction.luminance = 0.0;
                    recipe.detail.noise_reduction.color = 0.0;
                }
                Panel::LensCorrections => recipe.lens = LensCorrections::default(),
                Panel::Transform => recipe.transform = Transform::default(),
                Panel::Effects => recipe.effects = Effects::default(),
                Panel::Calibration => recipe.calibration = Calibration::default(),
            }
        }
        recipe
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum WhiteBalance {
    #[default]
    AsShot,
    /// Temperature in kelvin and tint on Lightroom's -150..=150 scale.
    Custom { temperature: f32, tint: f32 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Basic {
    /// Stops, -5..=5.
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub texture: f32,
    pub clarity: f32,
    pub dehaze: f32,
    pub vibrance: f32,
    pub saturation: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub x: f32,
    pub y: f32,
}

impl CurvePoint {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A monotonic-input curve through control points on 0..=1. An empty curve is the identity.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Curve(pub Vec<CurvePoint>);

impl Curve {
    pub fn is_identity(&self) -> bool {
        self.0.iter().all(|point| (point.x - point.y).abs() < 1e-6)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParametricCurve {
    pub highlights: f32,
    pub lights: f32,
    pub darks: f32,
    pub shadows: f32,
    /// Region boundaries on 0..=100.
    pub split_shadows: f32,
    pub split_midtones: f32,
    pub split_highlights: f32,
}

impl Default for ParametricCurve {
    fn default() -> Self {
        Self {
            highlights: 0.0,
            lights: 0.0,
            darks: 0.0,
            shadows: 0.0,
            split_shadows: 25.0,
            split_midtones: 50.0,
            split_highlights: 75.0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToneCurve {
    pub parametric: ParametricCurve,
    pub rgb: Curve,
    pub red: Curve,
    pub green: Curve,
    pub blue: Curve,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorMixer {
    pub hue: [f32; HSL_BANDS],
    pub saturation: [f32; HSL_BANDS],
    pub luminance: [f32; HSL_BANDS],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlackAndWhite {
    pub enabled: bool,
    pub mix: [f32; HSL_BANDS],
}

/// One colour wheel. Hue in degrees, saturation 0..=100, luminance -100..=100.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GradeWheel {
    pub hue: f32,
    pub saturation: f32,
    pub luminance: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorGrading {
    pub shadows: GradeWheel,
    pub midtones: GradeWheel,
    pub highlights: GradeWheel,
    pub global: GradeWheel,
    /// 0..=100.
    pub blending: f32,
    /// -100..=100.
    pub balance: f32,
}

impl Default for ColorGrading {
    fn default() -> Self {
        Self {
            shadows: GradeWheel::default(),
            midtones: GradeWheel::default(),
            highlights: GradeWheel::default(),
            global: GradeWheel::default(),
            blending: 50.0,
            balance: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sharpening {
    /// 0..=150.
    pub amount: f32,
    /// Pixels, 0.5..=3.
    pub radius: f32,
    pub detail: f32,
    pub masking: f32,
}

impl Default for Sharpening {
    fn default() -> Self {
        Self { amount: 40.0, radius: 1.0, detail: 25.0, masking: 0.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoiseReduction {
    pub luminance: f32,
    pub luminance_detail: f32,
    pub luminance_contrast: f32,
    pub color: f32,
    pub color_detail: f32,
    pub color_smoothness: f32,
}

impl Default for NoiseReduction {
    fn default() -> Self {
        Self {
            luminance: 0.0,
            luminance_detail: 50.0,
            luminance_contrast: 0.0,
            color: 25.0,
            color_detail: 50.0,
            color_smoothness: 50.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Detail {
    pub sharpening: Sharpening,
    pub noise_reduction: NoiseReduction,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Defringe {
    /// 0..=100.
    pub purple_amount: f32,
    /// Hue range on 0..=100.
    pub purple_hue_low: f32,
    pub purple_hue_high: f32,
    /// 0..=100.
    pub green_amount: f32,
    pub green_hue_low: f32,
    pub green_hue_high: f32,
}

impl Default for Defringe {
    fn default() -> Self {
        Self {
            purple_amount: 0.0,
            purple_hue_low: 30.0,
            purple_hue_high: 70.0,
            green_amount: 0.0,
            green_hue_low: 40.0,
            green_hue_high: 60.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LensCorrections {
    pub profile_enabled: bool,
    /// 0..=200, scales the profile's distortion correction.
    pub profile_distortion: f32,
    /// 0..=200, scales the profile's vignette correction.
    pub profile_vignette: f32,
    pub remove_chromatic_aberration: bool,
    pub defringe: Defringe,
    /// -100..=100.
    pub manual_distortion: f32,
    /// -100..=100.
    pub manual_vignette: f32,
    /// 0..=100.
    pub manual_vignette_midpoint: f32,
}

impl Default for LensCorrections {
    fn default() -> Self {
        Self {
            profile_enabled: false,
            profile_distortion: 100.0,
            profile_vignette: 100.0,
            remove_chromatic_aberration: false,
            defringe: Defringe::default(),
            manual_distortion: 0.0,
            manual_vignette: 0.0,
            manual_vignette_midpoint: 50.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Upright {
    #[default]
    Off,
    Auto,
    Level,
    Vertical,
    Full,
    Guided,
}

/// A straight line in the source image, in normalised coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GuideLine {
    pub start: [f32; 2],
    pub end: [f32; 2],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transform {
    pub upright: Upright,
    pub guides: Vec<GuideLine>,
    /// Leaves a warped photo at its natural size, empty corners and all, rather than scaling
    /// it to fill the frame.
    pub allow_empty_corners: bool,
    pub vertical: f32,
    pub horizontal: f32,
    /// Degrees.
    pub rotate: f32,
    pub aspect: f32,
    /// Percent, 100 is unscaled.
    pub scale: f32,
    pub offset_x: f32,
    pub offset_y: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            upright: Upright::Off,
            guides: Vec::new(),
            allow_empty_corners: false,
            vertical: 0.0,
            horizontal: 0.0,
            rotate: 0.0,
            aspect: 0.0,
            scale: 100.0,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PostCropVignette {
    /// -100..=100.
    pub amount: f32,
    pub midpoint: f32,
    /// -100..=100.
    pub roundness: f32,
    pub feather: f32,
    pub highlights: f32,
}

impl Default for PostCropVignette {
    fn default() -> Self {
        Self { amount: 0.0, midpoint: 50.0, roundness: 0.0, feather: 50.0, highlights: 0.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grain {
    pub amount: f32,
    pub size: f32,
    pub roughness: f32,
}

impl Default for Grain {
    fn default() -> Self {
        Self { amount: 0.0, size: 25.0, roughness: 50.0 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Effects {
    pub vignette: PostCropVignette,
    pub grain: Grain,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Calibration {
    pub shadow_tint: f32,
    pub red_hue: f32,
    pub red_saturation: f32,
    pub green_hue: f32,
    pub green_saturation: f32,
    pub blue_hue: f32,
    pub blue_saturation: f32,
}

/// Crop rectangle in normalised coordinates of the transformed image, plus straightening.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Crop {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    /// Degrees.
    pub angle: f32,
}

impl Default for Crop {
    fn default() -> Self {
        Self { left: 0.0, top: 0.0, right: 1.0, bottom: 1.0, angle: 0.0 }
    }
}

impl Crop {
    pub fn is_full_frame(&self) -> bool {
        *self == Self::default()
    }
}

/// The adjustments a mask can apply, as offsets added to the global values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalAdjustments {
    pub temperature: f32,
    pub tint: f32,
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub texture: f32,
    pub clarity: f32,
    pub dehaze: f32,
    pub hue: f32,
    pub saturation: f32,
    pub sharpness: f32,
    pub noise: f32,
}

impl LocalAdjustments {
    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrushDab {
    /// Normalised image position.
    pub position: [f32; 2],
    /// Fraction of the image's long edge.
    pub radius: f32,
    /// 0..=1.
    pub feather: f32,
    /// 0..=1.
    pub flow: f32,
    pub erase: bool,
    /// Keeps the dab to colours like the one under its centre.
    #[serde(default)]
    pub auto_mask: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MaskShape {
    Brush { dabs: Vec<BrushDab> },
    /// Full effect at `start`, fading to nothing at `end`, in normalised image positions.
    Linear { start: [f32; 2], end: [f32; 2] },
    /// An ellipse. Radii are fractions of the image's long edge, `angle` is in degrees and
    /// `feather` is 0..=1.
    Radial { center: [f32; 2], radii: [f32; 2], angle: f32, feather: f32, inverted: bool },
    /// Selected by brightness. All three are 0..=1.
    LuminanceRange { low: f32, high: f32, smoothness: f32 },
    /// Selected by similarity to sampled ProPhoto colours. Tolerance is 0..=1.
    ColorRange { samples: Vec<[f32; 3]>, tolerance: f32 },
    /// A raster produced by an AI model, stored outside the recipe and named by its hash.
    Raster { id: String, inverted: bool },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaskComponent {
    pub shape: MaskShape,
    /// Subtracts from the mask instead of adding to it.
    pub subtract: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mask {
    pub name: String,
    pub enabled: bool,
    pub components: Vec<MaskComponent>,
    pub adjustments: LocalAdjustments,
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            components: Vec::new(),
            adjustments: LocalAdjustments::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealMode {
    #[default]
    Heal,
    Clone,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HealSpot {
    pub mode: HealMode,
    pub target: [f32; 2],
    pub source: [f32; 2],
    /// Fraction of the image's long edge.
    pub radius: f32,
    /// 0..=1.
    pub feather: f32,
    /// 0..=1.
    pub opacity: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RedEye {
    pub center: [f32; 2],
    /// Fraction of the image's long edge.
    pub radius: f32,
    /// The pupil's share of `radius`, 0..=1.
    pub pupil_size: f32,
    /// 0..=1.
    pub darken: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_json_gets_current_defaults() {
        let recipe: Recipe = serde_json::from_str("{}").unwrap();
        assert_eq!(recipe, Recipe::default());
    }

    #[test]
    fn fully_populated_recipe_round_trips_through_json() {
        let mut recipe = Recipe {
            white_balance: WhiteBalance::Custom { temperature: 4800.0, tint: 12.0 },
            ..Recipe::default()
        };
        recipe.basic.exposure = 1.5;
        recipe.tone_curve.rgb = Curve(vec![CurvePoint::new(0.0, 0.1), CurvePoint::new(1.0, 0.9)]);
        recipe.color_mixer.hue[3] = -20.0;
        recipe.black_and_white.enabled = true;
        recipe.color_grading.shadows.hue = 210.0;
        recipe.transform.guides.push(GuideLine { start: [0.1, 0.2], end: [0.3, 0.4] });
        recipe.masks.push(Mask {
            name: "Sky".into(),
            components: vec![
                MaskComponent {
                    shape: MaskShape::Linear { start: [0.5, 0.0], end: [0.5, 0.4] },
                    subtract: false,
                },
                MaskComponent {
                    shape: MaskShape::Brush {
                        dabs: vec![BrushDab {
                            position: [0.2, 0.2],
                            radius: 0.05,
                            feather: 0.5,
                            flow: 1.0,
                            erase: false,
                            auto_mask: true,
                        }],
                    },
                    subtract: true,
                },
            ],
            ..Mask::default()
        });
        recipe.healing.push(HealSpot {
            mode: HealMode::Clone,
            target: [0.1, 0.1],
            source: [0.2, 0.2],
            radius: 0.01,
            feather: 0.5,
            opacity: 1.0,
        });
        let json = serde_json::to_string(&recipe).unwrap();
        assert_eq!(serde_json::from_str::<Recipe>(&json).unwrap(), recipe);
    }

    #[test]
    fn keeps_the_process_version_it_was_saved_with() {
        let recipe: Recipe = serde_json::from_str(r#"{"process_version": 0}"#).unwrap();
        assert_eq!(recipe.process_version, 0);
    }

    #[test]
    fn partial_sections_keep_defaults_for_missing_fields() {
        let recipe: Recipe =
            serde_json::from_str(r#"{"basic": {"exposure": 2.0}, "detail": {"sharpening": {"amount": 90}}}"#)
                .unwrap();
        assert_eq!(recipe.basic.exposure, 2.0);
        assert_eq!(recipe.basic.contrast, 0.0);
        assert_eq!(recipe.detail.sharpening.amount, 90.0);
        assert_eq!(recipe.detail.sharpening.radius, 1.0);
    }

    #[test]
    fn a_disabled_panel_keeps_its_settings_but_does_not_apply() {
        let mut recipe = Recipe::default();
        recipe.effects.grain.amount = 40.0;
        recipe.disabled_panels.push(Panel::Effects);
        let effective = recipe.effective();
        assert_eq!(effective.effects.grain.amount, 0.0);
        assert!(effective.disabled_panels.is_empty());
        let json = serde_json::to_string(&recipe).unwrap();
        assert_eq!(serde_json::from_str::<Recipe>(&json).unwrap(), recipe);
    }

    #[test]
    fn disabling_detail_turns_sharpening_off() {
        let recipe = Recipe { disabled_panels: vec![Panel::Detail], ..Recipe::default() };
        assert_eq!(recipe.effective().detail.sharpening.amount, 0.0);
    }

    #[test]
    fn identity_curves_are_detected() {
        assert!(Curve::default().is_identity());
        assert!(Curve(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]).is_identity());
        assert!(!Curve(vec![CurvePoint::new(0.5, 0.6)]).is_identity());
    }
}
