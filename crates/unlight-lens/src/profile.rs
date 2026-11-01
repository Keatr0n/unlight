//! The one form every correction takes. Radii are in the engine's image-centred coordinates,
//! where the image height is 1, so `reach` and the centres need no further conversion.

/// Entries in a profile's curve. At this density a 61 MP image's corner stays within a
/// hundredth of a pixel of the exact curve, which beats refitting a polynomial: ptlens has odd
/// powers of the radius and camera tables have no closed form at all.
pub const CURVE_SIZE: usize = 256;

/// How far past the image corner the curve runs, for zoomed-out frames and off-centre axes.
const REACH_MARGIN: f64 = 1.4;
/// Fixed-point iterations to carry an output radius over to a source radius.
const RADIUS_ITERATIONS: usize = 8;

const DISTORTION: usize = 0;
const CHROMATIC_RED: usize = 1;
const CHROMATIC_BLUE: usize = 2;
const VIGNETTE: usize = 3;

/// A photo's dimensions, as the corrections need to see them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub width: f64,
    pub height: f64,
}

impl Frame {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width: f64::from(width), height: f64::from(height) }
    }

    pub fn aspect(&self) -> f64 {
        self.width / self.height
    }

    /// Centre-to-corner distance in image heights.
    pub fn half_diagonal(&self) -> f64 {
        0.5 * self.aspect().hypot(1.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    DngOpcodes,
    Sony,
    Fujifilm,
    Panasonic,
    Nikon,
    Lensfun,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::DngOpcodes => "DNG opcodes",
            Self::Sony => "Sony makernote",
            Self::Fujifilm => "Fujifilm makernote",
            Self::Panasonic => "RW2 distortion tag",
            Self::Nikon => "Nikon makernote",
            Self::Lensfun => "lensfun",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LensProfile {
    curve: Vec<[f32; 4]>,
    /// The radius the last curve entry stands for.
    pub reach: f32,
    /// The optical axis, relative to the image centre.
    pub center: [f32; 2],
    pub vignette_center: [f32; 2],
    /// Decentering terms at full strength, in image heights.
    pub tangential: [f32; 2],
    pub distortion: Option<Source>,
    pub chromatic: Option<Source>,
    pub vignette: Option<Source>,
    /// The database entry this came from, if any.
    pub lens: Option<String>,
}

impl LensProfile {
    pub fn empty(frame: Frame) -> Self {
        Self {
            curve: vec![[0.0; 4]; CURVE_SIZE],
            reach: (frame.half_diagonal() * REACH_MARGIN) as f32,
            center: [0.0; 2],
            vignette_center: [0.0; 2],
            tangential: [0.0; 2],
            distortion: None,
            chromatic: None,
            vignette: None,
            lens: None,
        }
    }

    fn radius(&self, index: usize) -> f64 {
        f64::from(self.reach) * index as f64 / (CURVE_SIZE - 1) as f64
    }

    fn fill(&mut self, column: usize, value: impl Fn(f64) -> f64) {
        for index in 0..CURVE_SIZE {
            self.curve[index][column] = value(self.radius(index)) as f32;
        }
    }

    /// `scale` gives source radius over output radius at an output radius.
    pub fn set_distortion(&mut self, source: Source, scale: impl Fn(f64) -> f64) {
        self.fill(DISTORTION, |radius| scale(radius) - 1.0);
        self.distortion = Some(source);
    }

    /// `scale` gives the red and blue source radii over green's, at a source radius, which is
    /// what the shader has once green is resolved.
    pub fn set_chromatic(&mut self, source: Source, scale: impl Fn(f64) -> [f64; 2]) {
        for index in 0..CURVE_SIZE {
            let [red, blue] = scale(self.radius(index));
            self.curve[index][CHROMATIC_RED] = (red - 1.0) as f32;
            self.curve[index][CHROMATIC_BLUE] = (blue - 1.0) as f32;
        }
        self.chromatic = Some(source);
    }

    /// `set_chromatic`, for data keyed by output radius. Needs the distortion to be set first.
    pub(crate) fn set_chromatic_by_output(&mut self, source: Source, scale: impl Fn(f64) -> [f64; 2]) {
        let table: Vec<[f64; 2]> =
            (0..CURVE_SIZE).map(|index| scale(self.output_radius(self.radius(index)))).collect();
        let step = f64::from(self.reach) / (CURVE_SIZE - 1) as f64;
        self.set_chromatic(source, |radius| table[(radius / step).round() as usize]);
    }

    /// `stops` gives the correction's gain in EV at a source radius.
    pub fn set_vignette(&mut self, source: Source, stops: impl Fn(f64) -> f64) {
        self.fill(VIGNETTE, stops);
        self.vignette = Some(source);
    }

    fn output_radius(&self, source_radius: f64) -> f64 {
        (0..RADIUS_ITERATIONS).fold(source_radius, |radius, _| {
            source_radius / (1.0 + f64::from(self.sample(radius as f32)[DISTORTION]))
        })
    }

    /// Takes every correction this profile lacks from `other`.
    pub(crate) fn merge(&mut self, other: Self) {
        let mut take = |column: usize| {
            self.curve.iter_mut().zip(&other.curve).for_each(|(own, theirs)| own[column] = theirs[column]);
        };
        if self.chromatic.is_none() {
            take(CHROMATIC_RED);
            take(CHROMATIC_BLUE);
            self.chromatic = other.chromatic;
        }
        if self.vignette.is_none() {
            take(VIGNETTE);
            self.vignette = other.vignette;
            self.vignette_center = other.vignette_center;
        }
        if self.distortion.is_none() {
            take(DISTORTION);
            self.distortion = other.distortion;
            self.center = other.center;
            self.tangential = other.tangential;
        }
        self.lens = self.lens.take().or(other.lens);
    }

    pub fn is_complete(&self) -> bool {
        self.distortion.is_some() && self.chromatic.is_some() && self.vignette.is_some()
    }

    /// One `[distortion, red, blue, vignette]` entry per even step of `0..=reach`.
    pub fn curve(&self) -> &[[f32; 4]] {
        &self.curve
    }

    /// Interpolates the curve the way the shader's texture lookup does.
    pub fn sample(&self, radius: f32) -> [f32; 4] {
        let position = (radius / self.reach).clamp(0.0, 1.0) * (CURVE_SIZE - 1) as f32;
        let lower = (position as usize).min(CURVE_SIZE - 2);
        let blend = position - lower as f32;
        let (low, high) = (self.curve[lower], self.curve[lower + 1]);
        std::array::from_fn(|column| low[column] + (high[column] - low[column]) * blend)
    }

    /// Source radius over output radius at full strength.
    pub fn distortion_scale(&self, radius: f32) -> f32 {
        1.0 + self.sample(radius)[DISTORTION]
    }

    /// Red and blue source radii over green's, at a source radius.
    pub fn chromatic_scale(&self, radius: f32) -> [f32; 2] {
        let entry = self.sample(radius);
        [1.0 + entry[CHROMATIC_RED], 1.0 + entry[CHROMATIC_BLUE]]
    }

    /// Correction gain in EV at a source radius.
    pub fn vignette_stops(&self, radius: f32) -> f32 {
        self.sample(radius)[VIGNETTE]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Frame = Frame { width: 6000.0, height: 4000.0 };

    #[test]
    fn half_diagonal_is_in_image_heights() {
        assert!((FRAME.half_diagonal() - 0.5 * 3.25f64.sqrt()).abs() < 1e-12);
    }

    #[test]
    fn empty_profile_changes_nothing() {
        let profile = LensProfile::empty(FRAME);
        assert_eq!(profile.distortion_scale(0.5), 1.0);
        assert_eq!(profile.chromatic_scale(0.5), [1.0, 1.0]);
        assert_eq!(profile.vignette_stops(0.5), 0.0);
    }

    #[test]
    fn sampling_follows_a_polynomial_closely_at_the_corner() {
        let mut profile = LensProfile::empty(FRAME);
        let exact = |radius: f64| 1.0 - 0.08 * radius.powi(2) + 0.03 * radius.powi(4);
        profile.set_distortion(Source::Lensfun, exact);
        let corner = FRAME.half_diagonal();
        let error = (f64::from(profile.distortion_scale(corner as f32)) - exact(corner)).abs();
        assert!(error * corner * 6376.0 < 0.01, "{error}");
    }

    #[test]
    fn chromatic_scale_is_keyed_by_source_radius() {
        let mut profile = LensProfile::empty(FRAME);
        profile.set_distortion(Source::Sony, |_| 0.9);
        profile.set_chromatic_by_output(Source::Sony, |radius| [1.0 + radius, 1.0 - radius]);
        let [red, blue] = profile.chromatic_scale(0.45);
        assert!((red - 1.5).abs() < 1e-3 && (blue - 0.5).abs() < 1e-3, "{red} {blue}");
    }

    #[test]
    fn merging_only_fills_what_is_missing() {
        let mut own = LensProfile::empty(FRAME);
        own.set_distortion(Source::Panasonic, |_| 0.99);
        let mut other = LensProfile::empty(FRAME);
        other.set_distortion(Source::Lensfun, |_| 0.5);
        other.set_vignette(Source::Lensfun, |_| 1.0);
        other.lens = Some("Lens".into());
        own.merge(other);
        assert_eq!(own.distortion, Some(Source::Panasonic));
        assert!((own.distortion_scale(0.3) - 0.99).abs() < 1e-6);
        assert_eq!(own.vignette, Some(Source::Lensfun));
        assert_eq!(own.vignette_stops(0.3), 1.0);
        assert_eq!(own.lens.as_deref(), Some("Lens"));
        assert!(!own.is_complete());
    }
}
