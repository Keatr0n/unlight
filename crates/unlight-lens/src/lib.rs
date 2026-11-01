//! Per-photo lens corrections. Whatever a correction's origin (DNG opcodes, data the camera
//! embedded, or the lensfun database), it ends up as one `LensProfile`, which is what the
//! engine evaluates. Profiles are derived from the file on demand and are never stored in a
//! recipe; a recipe only holds the sliders that scale them.

mod dng;
mod embedded;
mod lensfun;
mod matching;
mod profile;
mod spline;

pub use profile::{CURVE_SIZE, Frame, LensProfile, Source};

use unlight_raw::{EmbeddedLens, Metadata, Opcodes, RawImage};

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum LensError {
    #[error("the lens database could not be loaded from {0}")]
    NoDatabase(String),
    #[error("{0} isn't in the lens database")]
    UnknownCamera(String),
    #[error("{0} doesn't record which lens took this photo")]
    UnnamedLens(String),
    #[error("no profile for \"{lens}\" on {camera}")]
    UnknownLens { camera: String, lens: String },
    #[error("\"{lens}\" could be any of {candidates}, so no profile was applied")]
    AmbiguousLens { lens: String, candidates: String },
    #[error("{lens} doesn't cover the shot's {focal} mm")]
    FocalOutOfRange { lens: String, focal: f32 },
    #[error("the file doesn't record a focal length, which every lens correction depends on")]
    NoFocalLength,
    #[error("{0} has no usable calibration")]
    NoCalibration(String),
    #[error("lens corrections went into this photo when it was merged")]
    AlreadyApplied,
}

/// Everything about a photo that a correction depends on, detached from its pixels so a
/// session can resolve a profile later without keeping the raw around.
#[derive(Clone, Debug)]
pub struct PhotoLens {
    metadata: Metadata,
    frame: Frame,
    embedded: Option<EmbeddedLens>,
    opcodes: Opcodes,
}

impl PhotoLens {
    pub fn new(raw: &RawImage) -> Self {
        Self::from_parts(raw.metadata.clone(), raw.width, raw.height, raw.embedded_lens.clone(), raw.opcodes.clone())
    }

    /// The same, for a photo whose pixels have already gone.
    pub fn from_parts(
        metadata: Metadata,
        width: u32,
        height: u32,
        embedded: Option<EmbeddedLens>,
        opcodes: Opcodes,
    ) -> Self {
        Self { metadata, frame: Frame::new(width, height), embedded, opcodes }
    }

    fn already_applied(&self) -> bool {
        self.metadata.merge.as_ref().is_some_and(|merge| merge.lens_applied)
    }

    /// The correction that's part of how a file renders, so unlike the other sources it doesn't
    /// wait for a profile to be switched on: a DNG's opcodes, or a Micro Four Thirds body's
    /// distortion data, which its lenses are designed to rely on.
    pub fn file_profile(&self) -> Option<LensProfile> {
        if self.already_applied() {
            return None;
        }
        dng::profile(&self.opcodes, self.frame).or_else(|| match &self.embedded {
            Some(data @ EmbeddedLens::Panasonic(_)) => embedded::profile(data, self.frame),
            _ => None,
        })
    }

    /// Finds the best correction for each of distortion, chromatic aberration and vignetting.
    /// A DNG's opcodes win outright, then the camera's own data, then the lens database for
    /// whatever is still missing. An error means nothing could be found at all.
    pub fn resolve(&self) -> Result<LensProfile, LensError> {
        if self.already_applied() {
            return Err(LensError::AlreadyApplied);
        }
        if let Some(profile) = dng::profile(&self.opcodes, self.frame) {
            return Ok(profile);
        }
        let embedded = self.embedded.as_ref().and_then(|data| embedded::profile(data, self.frame));
        match embedded {
            Some(profile) if profile.is_complete() => Ok(profile),
            Some(mut profile) => {
                if let Ok(database) = lensfun::profile(&self.metadata, self.frame) {
                    profile.merge(database);
                }
                Ok(profile)
            }
            None => lensfun::profile(&self.metadata, self.frame),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_raw::{NikonLens, PanasonicLens};

    fn lens_with(embedded: EmbeddedLens) -> PhotoLens {
        PhotoLens::from_parts(Metadata::default(), 5200, 3900, Some(embedded), Opcodes::default())
    }

    #[test]
    fn micro_four_thirds_distortion_is_part_of_the_file() {
        let data = PanasonicLens { scale: 1.0, a: 0.03, b: -0.01, c: 0.002, radius: 1950.0 };
        let profile = lens_with(EmbeddedLens::Panasonic(data)).file_profile().unwrap();
        assert_eq!(profile.distortion, Some(Source::Panasonic));
    }

    #[test]
    fn other_makers_data_waits_for_the_switch() {
        let data = NikonLens { distortion: [0.01, -0.02, 0.0], vignette: [0.0; 3] };
        assert!(lens_with(EmbeddedLens::Nikon(data)).file_profile().is_none());
    }
}
