//! Maps a mask's adjustments onto the channels of the parameter fields. The shaders add what
//! they read straight onto each global setting, so these are on the scales `tools` gives the
//! global sliders.

use unlight_recipe::{LocalAdjustments, Mask, NoiseReduction};

pub(crate) const FIELD_LAYERS: usize = 4;

/// A mask's hue slider at full travel, in radians of Oklab hue, as far as the colour mixer's.
const HUE_RANGE: f32 = 0.52;
/// Full travel of the temperature and tint sliders, as a log2 ratio between colour channels.
const BALANCE_RANGE: f32 = 0.6;
/// The noise reduction a mask at +100 reaches. A mask can ask for more than the global sliders
/// are set to, so there has to be a stronger image to blend towards.
const CLEANER_LUMINANCE: f32 = 70.0;
const CLEANER_COLOR: f32 = 60.0;

/// One mask's offsets, laid out as the four field layers are read by local_edits.wgsl.
pub(crate) fn field_offsets(adjustments: &LocalAdjustments) -> [[f32; 4]; FIELD_LAYERS] {
    let LocalAdjustments {
        temperature,
        tint,
        exposure,
        contrast,
        highlights,
        shadows,
        whites,
        blacks,
        texture,
        clarity,
        dehaze,
        hue,
        saturation,
        sharpness,
        noise,
    } = *adjustments;
    let percent = |value: f32| value / 100.0;
    [
        [exposure, percent(contrast), percent(highlights), percent(shadows)],
        [percent(whites), percent(blacks), percent(clarity), percent(texture)],
        [percent(dehaze), percent(hue) * HUE_RANGE, percent(saturation), percent(temperature) * BALANCE_RANGE],
        [percent(tint) * BALANCE_RANGE, percent(sharpness), percent(noise), 0.0],
    ]
}

/// Which other images local noise reduction needs beside the global result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LocalNoise {
    /// Some mask asks for more than the global setting.
    pub cleaner: bool,
    /// Some mask asks for less, which blends back towards the undenoised image.
    pub rawer: bool,
}

fn applies(mask: &Mask) -> bool {
    mask.enabled && !mask.components.is_empty()
}

pub(crate) fn local_noise(masks: &[Mask]) -> LocalNoise {
    let noise = || masks.iter().filter(|mask| applies(mask)).map(|mask| mask.adjustments.noise);
    LocalNoise { cleaner: noise().any(|value| value > 0.0), rawer: noise().any(|value| value < 0.0) }
}

/// Whether anything in a recipe's masks reaches the sharpening stage.
pub(crate) fn local_sharpening(masks: &[Mask]) -> bool {
    masks.iter().any(|mask| applies(mask) && mask.adjustments.sharpness != 0.0)
}

pub(crate) fn cleaner_settings(global: &NoiseReduction) -> NoiseReduction {
    NoiseReduction {
        luminance: global.luminance.max(CLEANER_LUMINANCE),
        color: global.color.max(CLEANER_COLOR),
        ..*global
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(noise: f32, enabled: bool) -> Mask {
        Mask {
            enabled,
            components: vec![unlight_recipe::MaskComponent {
                shape: unlight_recipe::MaskShape::Linear { start: [0.0; 2], end: [1.0; 2] },
                subtract: false,
            }],
            adjustments: LocalAdjustments { noise, ..LocalAdjustments::default() },
            ..Mask::default()
        }
    }

    #[test]
    fn offsets_land_on_their_channels() {
        let adjustments = LocalAdjustments {
            exposure: 1.5,
            shadows: 50.0,
            texture: -20.0,
            noise: 100.0,
            ..LocalAdjustments::default()
        };
        let layers = field_offsets(&adjustments);
        assert_eq!(layers[0][0], 1.5);
        assert_eq!(layers[0][3], 0.5);
        assert_eq!(layers[1][3], -0.2);
        assert_eq!(layers[3][2], 1.0);
    }

    #[test]
    fn neutral_adjustments_offset_nothing() {
        let layers = field_offsets(&LocalAdjustments::default());
        assert!(layers.iter().flatten().all(|value| *value == 0.0));
    }

    #[test]
    fn local_noise_ignores_disabled_masks() {
        assert_eq!(local_noise(&[with(50.0, false)]), LocalNoise::default());
        assert_eq!(local_noise(&[with(50.0, true)]), LocalNoise { cleaner: true, rawer: false });
        assert_eq!(local_noise(&[with(-50.0, true)]), LocalNoise { cleaner: false, rawer: true });
    }

    #[test]
    fn the_cleaner_image_never_undoes_a_stronger_global_setting() {
        let global = NoiseReduction { luminance: 90.0, color: 10.0, ..NoiseReduction::default() };
        let cleaner = cleaner_settings(&global);
        assert_eq!(cleaner.luminance, 90.0);
        assert_eq!(cleaner.color, CLEANER_COLOR);
    }
}
