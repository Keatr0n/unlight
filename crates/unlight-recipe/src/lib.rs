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
    /// How the photo has been turned or flipped, on top of how the camera was held. The engine
    /// renders sensor-up whatever this says, so it never costs a render.
    #[serde(skip_serializing_if = "Orientation::is_normal")]
    pub orientation: Orientation,
}

/// One of the eight ways to lay a rectangle down, as what happens to a sensor-up image to show
/// it. Coordinates run 0..1 across the image.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    #[default]
    Normal,
    Rotate180,
    RotateCw,
    RotateCcw,
    /// Left and right swapped.
    MirrorX,
    /// Top and bottom swapped.
    MirrorY,
    /// Flipped about the diagonal through the top left corner.
    Transpose,
    /// Flipped about the diagonal through the top right corner.
    Transverse,
}

impl Orientation {
    pub const ALL: [Self; 8] = [
        Self::Normal,
        Self::Rotate180,
        Self::RotateCw,
        Self::RotateCcw,
        Self::MirrorX,
        Self::MirrorY,
        Self::Transpose,
        Self::Transverse,
    ];

    /// LibRaw's flip code: bit 0 mirrors columns, bit 1 mirrors rows, bit 2 swaps the axes.
    pub fn from_libraw(flip: i32) -> Self {
        match flip {
            1 => Self::MirrorX,
            2 => Self::MirrorY,
            3 => Self::Rotate180,
            4 => Self::Transpose,
            5 => Self::RotateCcw,
            6 => Self::RotateCw,
            7 => Self::Transverse,
            _ => Self::Normal,
        }
    }

    pub fn is_normal(&self) -> bool {
        *self == Self::Normal
    }

    pub fn swaps_axes(self) -> bool {
        matches!(self, Self::RotateCw | Self::RotateCcw | Self::Transpose | Self::Transverse)
    }

    /// Whether this reverses handedness, so a clockwise turn on screen is anticlockwise in the
    /// sensor image.
    pub fn mirrors(self) -> bool {
        matches!(self, Self::MirrorX | Self::MirrorY | Self::Transpose | Self::Transverse)
    }

    pub fn display_size(self, sensor: [u32; 2]) -> [u32; 2] {
        if self.swaps_axes() { [sensor[1], sensor[0]] } else { sensor }
    }

    pub fn to_display(self, [x, y]: [f32; 2]) -> [f32; 2] {
        match self {
            Self::Normal => [x, y],
            Self::Rotate180 => [1.0 - x, 1.0 - y],
            Self::RotateCw => [1.0 - y, x],
            Self::RotateCcw => [y, 1.0 - x],
            Self::MirrorX => [1.0 - x, y],
            Self::MirrorY => [x, 1.0 - y],
            Self::Transpose => [y, x],
            Self::Transverse => [1.0 - y, 1.0 - x],
        }
    }

    pub fn inverse(self) -> Self {
        match self {
            Self::RotateCw => Self::RotateCcw,
            Self::RotateCcw => Self::RotateCw,
            other => other,
        }
    }

    pub fn to_sensor(self, point: [f32; 2]) -> [f32; 2] {
        self.inverse().to_display(point)
    }

    /// Converts an x, y, width, height rectangle from display to sensor coordinates.
    pub fn sensor_region(self, [x, y, width, height]: [f32; 4]) -> [f32; 4] {
        let [ax, ay] = self.to_sensor([x, y]);
        let [bx, by] = self.to_sensor([x + width, y + height]);
        [ax.min(bx), ay.min(by), (ax - bx).abs(), (ay - by).abs()]
    }

    /// This orientation with `next` done after it, so `next` happens as seen on screen.
    pub fn then(self, next: Self) -> Self {
        const CORNERS: [[f32; 2]; 3] = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let same = |candidate: &Self| {
            CORNERS.iter().all(|corner| candidate.to_display(*corner) == next.to_display(self.to_display(*corner)))
        };
        Self::ALL.into_iter().find(same).unwrap_or_default()
    }

    /// Where pixel `x`, `y` of a `width` by `height` image lands.
    fn display_pixel(self, x: usize, y: usize, width: usize, height: usize) -> (usize, usize) {
        let (right, bottom) = (width - 1 - x, height - 1 - y);
        match self {
            Self::Normal => (x, y),
            Self::Rotate180 => (right, bottom),
            Self::RotateCw => (bottom, x),
            Self::RotateCcw => (y, right),
            Self::MirrorX => (right, y),
            Self::MirrorY => (x, bottom),
            Self::Transpose => (y, x),
            Self::Transverse => (bottom, right),
        }
    }

    /// Lays out interleaved RGB this way. Returns the pixels and their new size.
    pub fn apply(self, pixels: Vec<f32>, width: u32, height: u32) -> (Vec<f32>, u32, u32) {
        if self.is_normal() {
            return (pixels, width, height);
        }
        let [out_width, out_height] = self.display_size([width, height]);
        let (w, h) = (width as usize, height as usize);
        let mut turned = vec![0.0; pixels.len()];
        for y in 0..h {
            for x in 0..w {
                let (to_x, to_y) = self.display_pixel(x, y, w, h);
                let from = (y * w + x) * 3;
                let to = (to_y * out_width as usize + to_x) * 3;
                turned[to..to + 3].copy_from_slice(&pixels[from..from + 3]);
            }
        }
        (turned, out_width, out_height)
    }
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
            orientation: Orientation::Normal,
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
    fn four_turns_the_same_way_come_back_around() {
        for start in Orientation::ALL {
            for turn in [Orientation::RotateCw, Orientation::RotateCcw] {
                let turned = (0..4).fold(start, |orientation, _| orientation.then(turn));
                assert_eq!(turned, start);
            }
        }
    }

    #[test]
    fn a_flip_undoes_itself_and_left_undoes_right() {
        for start in Orientation::ALL {
            assert_eq!(start.then(Orientation::MirrorX).then(Orientation::MirrorX), start);
            assert_eq!(start.then(Orientation::MirrorY).then(Orientation::MirrorY), start);
            assert_eq!(start.then(Orientation::RotateCw).then(Orientation::RotateCcw), start);
        }
    }

    #[test]
    fn orientations_combine_like_doing_one_then_the_other() {
        let points = [[0.25, 0.125], [0.875, 0.375], [0.5, 0.75]];
        for first in Orientation::ALL {
            for second in Orientation::ALL {
                let both = first.then(second);
                for point in points {
                    assert_eq!(both.to_display(point), second.to_display(first.to_display(point)));
                }
            }
        }
    }

    #[test]
    fn to_sensor_undoes_to_display() {
        for orientation in Orientation::ALL {
            let point = [0.25, 0.125];
            assert_eq!(orientation.to_sensor(orientation.to_display(point)), point);
        }
    }

    #[test]
    fn a_flip_goes_the_way_it_looks_on_a_photo_the_camera_turned() {
        let shown = Orientation::RotateCw.then(Orientation::MirrorX);
        let [x, y] = Orientation::RotateCw.to_display([0.25, 0.125]);
        assert_eq!(shown.to_display([0.25, 0.125]), [1.0 - x, y]);
    }

    #[test]
    fn pixels_land_where_coordinates_say() {
        let (width, height) = (3usize, 2usize);
        let pixels: Vec<f32> = (0..width * height).flat_map(|value| [value as f32; 3]).collect();
        for orientation in Orientation::ALL {
            let (turned, out_width, out_height) = orientation.apply(pixels.clone(), 3, 2);
            assert_eq!([out_width, out_height], orientation.display_size([3, 2]));
            for y in 0..height {
                for x in 0..width {
                    let center = [(x as f32 + 0.5) / 3.0, (y as f32 + 0.5) / 2.0];
                    let [u, v] = orientation.to_display(center);
                    let (to_x, to_y) = ((u * out_width as f32) as usize, (v * out_height as f32) as usize);
                    let found = turned[(to_y * out_width as usize + to_x) * 3];
                    assert_eq!(found, (y * width + x) as f32, "{orientation:?} {x},{y}");
                }
            }
        }
    }

    #[test]
    fn flips_are_the_only_ones_that_reverse_handedness() {
        for orientation in Orientation::ALL {
            let [ox, oy] = orientation.to_display([0.0, 0.0]);
            let [ax, ay] = orientation.to_display([1.0, 0.0]);
            let [bx, by] = orientation.to_display([0.0, 1.0]);
            let area = (ax - ox) * (by - oy) - (ay - oy) * (bx - ox);
            assert_eq!(area < 0.0, orientation.mirrors(), "{orientation:?}");
        }
    }

    #[test]
    fn libraw_codes_match_their_bits() {
        for flip in 0..8 {
            let orientation = Orientation::from_libraw(flip);
            assert_eq!(orientation.swaps_axes(), flip & 4 != 0, "{flip}");
            let [x, y] = orientation.to_sensor([0.0, 0.0]);
            assert_eq!((x == 1.0, y == 1.0), (flip & 1 != 0, flip & 2 != 0), "{flip}");
        }
    }

    #[test]
    fn an_unturned_recipe_says_nothing_about_it() {
        let json = serde_json::to_string(&Recipe::default()).unwrap();
        assert!(!json.contains("orientation"), "{json}");
        let recipe: Recipe = serde_json::from_str(r#"{"process_version":1}"#).unwrap();
        assert_eq!(recipe.orientation, Orientation::Normal);
    }

    #[test]
    fn a_turn_survives_being_saved() {
        let recipe = Recipe { orientation: Orientation::Transverse, ..Recipe::default() };
        let json = serde_json::to_string(&recipe).unwrap();
        assert!(json.contains(r#""orientation":"transverse""#), "{json}");
        assert_eq!(serde_json::from_str::<Recipe>(&json).unwrap(), recipe);
    }

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
