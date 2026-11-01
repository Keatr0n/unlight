//! Lightroom's Tone → Auto: sets the Basic sliders from statistics of the photo. Everything
//! here is plain arithmetic on a log-luminance histogram, with a CPU copy of the develop
//! shader's tone maths standing in for the render, so the choices can be tested without a GPU.
//!
//! A camera has already metered the photo and the default rendering is fitted to camera JPEGs,
//! so a photo that looks sensible is left close to where it is. Scene brightness from the
//! exposure settings tells a night scene from a mistake, because the histogram alone can't.

use crate::{
    geometry::Geometry,
    guides::GuideImage,
    rendering::{BASE_CURVE_REACH, BASE_CURVE_SIZE},
};
use unlight_raw::Metadata;
use unlight_recipe::Basic;

const LOW_EV: f32 = -17.0;
const HIGH_EV: f32 = 4.0;
const BINS_PER_STOP: usize = 8;
pub const TONE_BINS: usize = (HIGH_EV - LOW_EV) as usize * BINS_PER_STOP;

/// Must stay in step with the tone section of develop.wgsl.
const PIVOT_EV: f32 = -2.47;

/// Mean output luma below which a photo is taken to be underexposed, in sRGB encoding.
const BAND_LOW: f32 = 0.27;
/// Where an underexposed photo is lifted to, well inside the band rather than on its edge.
const LIFT_TARGET: f32 = 0.36;
/// Scene brightness, in EV at ISO 100, between which a photo stops counting as night.
const NIGHT_LIGHT: [f32; 2] = [2.0, 7.0];
/// Where the top of the histogram may sit before exposure comes down to protect it.
const HIGHLIGHT_LIMIT: f32 = 0.965;
/// Where Whites takes a dull top of the histogram, and how low the top has to be to bother.
const WHITE_TARGET: f32 = 0.93;
/// Where Blacks takes a faded bottom of the histogram, and how faded it has to be to bother.
const BLACK_TARGET: f32 = 0.05;
const FADED_BLACK: f32 = 0.08;
const MAX_WHITES: f32 = 15.0;
const MAX_BLACKS: f32 = 10.0;
/// How far above white exposure may push highlights, for Highlights to bring back.
const HIGHLIGHT_ALLOWANCE: f32 = 0.5;
const MAX_LIFT: f32 = 3.0;
const MAX_CUT: f32 = 0.8;
/// A smaller exposure change does nothing a viewer would want and costs accuracy to the scene.
const SMALLEST_EXPOSURE: f32 = 0.1;
/// Output luma above which a pixel counts as a highlight worth protecting.
const HIGHLIGHT_LUMA: f32 = 0.93;
const DEEP_SHADOW_LUMA: f32 = 0.1;
/// Output luma above which a pixel counts towards a photo being mostly bright.
const BRIGHT_LUMA: f32 = 0.8;
/// Output luma at which a pixel reads as clipped.
const WHITE_CLIP: f32 = 0.995;
const BLACK_CLIP: f32 = 0.008;
/// Extra clipping Auto may add, as a share of the photo.
const NEW_CLIPPING: f32 = 0.002;
/// Below this a pixel's colour is mostly noise, so it says nothing about saturation.
const SATURATION_FLOOR_EV: f32 = -8.0;
/// Points measured along the long edge of the cropped photo.
const SAMPLES: usize = 512;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AutoTone {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub vibrance: f32,
    pub saturation: f32,
}

impl AutoTone {
    /// Sets the sliders Auto owns and leaves the rest of the panel alone.
    pub fn apply_to(&self, basic: &mut Basic) {
        basic.exposure = self.exposure;
        basic.contrast = self.contrast;
        basic.highlights = self.highlights;
        basic.shadows = self.shadows;
        basic.whites = self.whites;
        basic.blacks = self.blacks;
        basic.vibrance = self.vibrance;
        basic.saturation = self.saturation;
    }
}

/// The part of develop.wgsl that turns scene log luminance into output light, given a pixel's
/// surroundings match it. That holds for the broad areas that decide a histogram.
#[derive(Clone, Debug, PartialEq)]
pub struct ToneModel {
    /// Stops the render adds on top of the Exposure slider.
    pub exposure_offset: f32,
    pub white_ev: f32,
    pub black_ev: f32,
    /// The profile's base curve, as `rendering::base_curve` builds it.
    pub curve: Vec<f32>,
}

fn bump(low: f32, high: f32, value: f32) -> f32 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn srgb_encode(light: f32) -> f32 {
    if light <= 0.003_130_8 { light * 12.92 } else { 1.055 * light.powf(1.0 / 2.4) - 0.055 }
}

impl ToneModel {
    fn base_tone(&self, light: f32) -> f32 {
        let along = (light / BASE_CURVE_REACH).clamp(0.0, 1.0).sqrt() * (BASE_CURVE_SIZE - 1) as f32;
        let index = (along as usize).min(BASE_CURVE_SIZE - 2);
        let part = along - index as f32;
        self.curve[index] * (1.0 - part) + self.curve[index + 1] * part
    }

    /// Log luminance after the sliders, before the base curve.
    pub fn shaped(&self, ev: f32, tone: &AutoTone) -> f32 {
        let exposure = tone.exposure + self.exposure_offset;
        let exposed = ev + exposure;
        let percent = |value: f32| value / 100.0;
        let mut gain = exposure
            + percent(tone.highlights) * 1.6 * bump(-3.5, -0.5, exposed)
            + percent(tone.shadows) * 1.8 * (1.0 - bump(-7.0, -2.5, exposed));
        gain += percent(tone.whites) * 1.5 * bump(self.white_ev - 2.5, self.white_ev, exposed);
        gain += percent(tone.blacks) * 2.0 * (1.0 - bump(self.black_ev, self.black_ev + 3.0, exposed));
        gain += (exposed - PIVOT_EV) * percent(tone.contrast) * 0.55
            * bump(-11.0, -6.0, exposed)
            * (1.0 - bump(0.5, 3.0, exposed));
        ev + gain
    }

    /// Output luma for a scene log luminance, sRGB-encoded.
    pub fn output(&self, ev: f32, tone: &AutoTone) -> f32 {
        srgb_encode(self.base_tone(self.shaped(ev, tone).exp2()))
    }
}

/// What Auto looks at.
#[derive(Clone, Debug, PartialEq)]
pub struct ToneStats {
    /// Share of the photo in each bin of scene log luminance, summing to one.
    pub histogram: Vec<f32>,
    /// Mean saturation in the working space, 0 for grey.
    pub saturation: f32,
    /// Saturation of the most colourful tenth of the photo.
    pub vivid: f32,
    /// Exposure value at ISO 100 the photo was taken at, when the file says.
    pub exposure_value: Option<f32>,
    pub model: ToneModel,
}

/// The exposure settings as one number: what a light meter would have read.
pub fn exposure_value(metadata: &Metadata) -> Option<f32> {
    let usable = metadata.iso > 0.0 && metadata.shutter > 0.0 && metadata.aperture > 0.0;
    usable.then(|| (metadata.aperture * metadata.aperture / metadata.shutter).log2() - (metadata.iso / 100.0).log2())
}

fn bin_center(bin: usize) -> f32 {
    LOW_EV + (bin as f32 + 0.5) / BINS_PER_STOP as f32
}

pub(crate) fn bin_of(ev: f32) -> usize {
    (((ev - LOW_EV) * BINS_PER_STOP as f32).max(0.0) as usize).min(TONE_BINS - 1)
}

impl ToneStats {
    fn bins(&self) -> impl Iterator<Item = (f32, f32)> + '_ {
        self.histogram.iter().enumerate().map(|(bin, share)| (bin_center(bin), *share))
    }

    /// The log luminance a share of the photo sits below.
    pub fn quantile(&self, share: f32) -> f32 {
        let total: f32 = self.histogram.iter().sum();
        let wanted = share * total;
        let mut below = 0.0;
        for (bin, count) in self.histogram.iter().enumerate() {
            if below + count >= wanted {
                let part = if *count > 0.0 { (wanted - below) / count } else { 0.5 };
                return LOW_EV + (bin as f32 + part) / BINS_PER_STOP as f32;
            }
            below += count;
        }
        HIGH_EV
    }

    /// The mean output luma the model predicts.
    pub fn mean_output(&self, tone: &AutoTone) -> f32 {
        self.bins().map(|(ev, share)| share * self.model.output(ev, tone)).sum()
    }

    fn share_where(&self, tone: &AutoTone, test: impl Fn(f32) -> bool) -> f32 {
        self.bins().filter(|(ev, _)| test(self.model.output(*ev, tone))).map(|(_, share)| share).sum()
    }

    /// How bright the middle of the scene really was, in EV at ISO 100.
    fn scene_light(&self) -> Option<f32> {
        Some(self.exposure_value? + self.quantile(0.5) - PIVOT_EV)
    }

    /// 1 for a scene that was dark when it was taken.
    fn night(&self) -> f32 {
        self.scene_light().map_or(0.0, |light| 1.0 - bump(NIGHT_LIGHT[0], NIGHT_LIGHT[1], light))
    }
}

/// Measures the photo as cropped. Each point of the output is followed back to the guide, so
/// what the crop and the geometry tools cut off doesn't count.
pub(crate) fn measure(
    guide: &GuideImage,
    geometry: &Geometry,
    aspect: f32,
    output_shape: f32,
    exposure_value: Option<f32>,
    model: ToneModel,
) -> ToneStats {
    let (columns, rows) = if output_shape >= 1.0 {
        (SAMPLES, ((SAMPLES as f32 / output_shape) as usize).max(1))
    } else {
        (((SAMPLES as f32 * output_shape) as usize).max(1), SAMPLES)
    };
    let mut histogram = vec![0.0f32; TONE_BINS];
    let mut saturations = Vec::with_capacity(columns * rows);
    for row in 0..rows {
        for column in 0..columns {
            let view = [(column as f32 + 0.5) / columns as f32, (row as f32 + 0.5) / rows as f32];
            let [u, v] = geometry.source_uv(view, aspect);
            if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                continue;
            }
            let x = (u * guide.width as f32) as usize;
            let y = (v * guide.height as f32) as usize;
            let [log_luma, darkest] = guide.texels[y * guide.width + x];
            histogram[bin_of(log_luma)] += 1.0;
            if log_luma > SATURATION_FLOOR_EV {
                saturations.push((1.0 - darkest / log_luma.exp2()).clamp(0.0, 1.0));
            }
        }
    }
    let total = histogram.iter().sum::<f32>().max(1.0);
    histogram.iter_mut().for_each(|share| *share /= total);
    saturations.sort_by(f32::total_cmp);
    let count = saturations.len().max(1) as f32;
    ToneStats {
        histogram,
        saturation: saturations.iter().sum::<f32>() / count,
        vivid: saturations.get((saturations.len() as f32 * 0.9) as usize).copied().unwrap_or(0.0),
        exposure_value,
        model,
    }
}

/// The exposure that takes a monotonic quantity to `wanted`, or the nearest end of the range.
fn solve(range: [f32; 2], wanted: f32, measure: impl Fn(f32) -> f32) -> f32 {
    let [mut low, mut high] = range;
    if measure(high) <= wanted {
        return high;
    }
    if measure(low) >= wanted {
        return low;
    }
    for _ in 0..24 {
        let middle = (low + high) * 0.5;
        if measure(middle) < wanted { low = middle } else { high = middle }
    }
    (low + high) * 0.5
}

fn solve_exposure(stats: &ToneStats, night: f32) -> f32 {
    let neutral = AutoTone::default();
    let at = |exposure: f32| AutoTone { exposure, ..neutral };
    // A night scene is meant to be dark, so it has no lower limit at all.
    let band_low = BAND_LOW * (1.0 - night);
    let top = stats.quantile(0.995);
    let headroom = solve([-MAX_CUT, MAX_LIFT], HIGHLIGHT_LIMIT, |exposure| stats.model.output(top, &at(exposure)));
    let exposure = if stats.mean_output(&neutral) < band_low {
        let target = LIFT_TARGET * (1.0 - night);
        let wanted = solve([0.0, MAX_LIFT], target, |exposure| stats.mean_output(&at(exposure)));
        wanted.min(headroom.max(0.0) + HIGHLIGHT_ALLOWANCE)
    } else {
        // Snow sits near white on purpose, so a mostly bright photo only counts as overexposed
        // once it has really clipped.
        let bright = stats.share_where(&neutral, |luma| luma > BRIGHT_LUMA);
        let clipped = stats.share_where(&neutral, |luma| luma >= WHITE_CLIP);
        headroom.min(0.0) * 0.5 * (1.0 - bump(0.35, 0.7, bright)).max(bump(0.02, 0.2, clipped))
    };
    if exposure.abs() < SMALLEST_EXPOSURE { 0.0 } else { exposure }
}

fn solve_highlights(stats: &ToneStats, tone: &AutoTone) -> f32 {
    let near_white = |tone: &AutoTone| stats.share_where(tone, |luma| luma > HIGHLIGHT_LUMA);
    let share = near_white(tone);
    // A small area near white is detail worth bringing back. Much more than that is a bright
    // sky or snow the photo is about, and taking it down greys the picture. That's judged
    // before exposure moved anything, which would otherwise shrink a sky into a small area.
    let small_area = bump(0.02, 0.08, share) * (1.0 - bump(0.1, 0.3, share.max(near_white(&AutoTone::default()))));
    // What has actually clipped is lost either way, however much of the photo it is.
    let clipped = bump(0.02, 0.2, stats.share_where(tone, |luma| luma >= WHITE_CLIP));
    -25.0 * small_area.max(clipped)
}

fn solve_shadows(stats: &ToneStats, tone: &AutoTone, night: f32) -> f32 {
    // Only real backlight. Shadows a camera chose to keep dark stay dark.
    let share = stats.share_where(tone, |luma| luma < DEEP_SHADOW_LUMA);
    30.0 * bump(0.2, 0.5, share) * (1.0 - 0.75 * night)
}

fn solve_contrast(stats: &ToneStats, tone: &AutoTone) -> f32 {
    let (low, high) = (stats.quantile(0.1), stats.quantile(0.9));
    let spread = stats.model.output(high, tone) - stats.model.output(low, tone);
    ((0.45 - spread) * 60.0).clamp(0.0, 10.0)
}

fn solve_whites(stats: &ToneStats, tone: &AutoTone, night: f32) -> f32 {
    let top = stats.quantile(0.997);
    let at = |whites: f32| stats.model.output(top, &AutoTone { whites, ..*tone });
    if at(0.0) >= WHITE_TARGET {
        return 0.0;
    }
    solve([0.0, MAX_WHITES * (1.0 - night)], WHITE_TARGET, at)
}

fn solve_blacks(stats: &ToneStats, tone: &AutoTone, night: f32) -> f32 {
    let bottom = stats.quantile(0.003);
    let at = |blacks: f32| stats.model.output(bottom, &AutoTone { blacks, ..*tone });
    if at(0.0) <= FADED_BLACK {
        return 0.0;
    }
    solve([-MAX_BLACKS * (1.0 - night), 0.0], BLACK_TARGET, at)
}

/// Takes Whites and Blacks back until Auto adds no more clipping than it's allowed to.
fn limit_clipping(stats: &ToneStats, tone: &mut AutoTone) {
    let neutral = AutoTone::default();
    let whites_now = |tone: &AutoTone| stats.share_where(tone, |luma| luma >= WHITE_CLIP);
    let blacks_now = |tone: &AutoTone| stats.share_where(tone, |luma| luma <= BLACK_CLIP);
    let allowed_whites = whites_now(&neutral) + NEW_CLIPPING;
    let allowed_blacks = blacks_now(&neutral) + NEW_CLIPPING;
    while whites_now(tone) > allowed_whites && tone.whites > -30.0 {
        tone.whites -= 2.0;
    }
    while blacks_now(tone) > allowed_blacks && tone.blacks < 20.0 {
        tone.blacks += 2.0;
    }
}

/// Colour helps most on a muted photo. A nearly grey one, such as snow, gets almost none, since
/// all there is to bring out is its white balance cast.
fn solve_color(stats: &ToneStats, tone: &mut AutoTone) {
    let has_colour = bump(0.06, 0.12, stats.saturation);
    let eased = 1.0 - bump(0.7, 0.9, stats.vivid);
    tone.vibrance = ((0.3 - stats.saturation) * 30.0).clamp(0.0, 8.0) * has_colour * eased;
    tone.saturation = ((0.2 - stats.saturation) * 10.0).clamp(0.0, 2.0) * has_colour;
}

fn rounded(tone: AutoTone) -> AutoTone {
    let whole = |value: f32| value.round() + 0.0;
    AutoTone {
        exposure: (tone.exposure / 0.05).round() * 0.05 + 0.0,
        contrast: whole(tone.contrast),
        highlights: whole(tone.highlights),
        shadows: whole(tone.shadows),
        whites: whole(tone.whites),
        blacks: whole(tone.blacks),
        vibrance: whole(tone.vibrance),
        saturation: whole(tone.saturation),
    }
}

/// The Basic sliders for a photo.
pub fn auto_tone(stats: &ToneStats) -> AutoTone {
    let night = stats.night();
    let mut tone = AutoTone { exposure: solve_exposure(stats, night), ..AutoTone::default() };
    tone.contrast = solve_contrast(stats, &tone);
    tone.highlights = solve_highlights(stats, &tone);
    tone.shadows = solve_shadows(stats, &tone, night);
    tone.whites = solve_whites(stats, &tone, night);
    tone.blacks = solve_blacks(stats, &tone, night);
    limit_clipping(stats, &mut tone);
    solve_color(stats, &mut tone);
    rounded(tone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rendering::base_curve;
    use unlight_color::CameraProfile;
    use unlight_raw::ColorData;

    const SUNNY: Option<f32> = Some(14.0);
    const STREET_AT_NIGHT: Option<f32> = Some(5.0);

    fn curve() -> Vec<f32> {
        base_curve(&CameraProfile::new(&ColorData {
            xyz_to_camera: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            as_shot_multipliers: [1.0; 3],
            daylight_multipliers: [1.0; 3],
            baseline_exposure: 0.0,
            dng: None,
        }))
    }

    /// A photo as bell curves of (centre in stops, share, width in stops).
    fn scene(bells: &[(f32, f32, f32)], exposure_value: Option<f32>, saturation: f32) -> ToneStats {
        let mut histogram: Vec<f32> = (0..TONE_BINS)
            .map(|bin| {
                let ev = bin_center(bin);
                bells.iter().map(|(center, share, width)| share * (-((ev - center) / width).powi(2) * 0.5).exp() / width).sum()
            })
            .collect();
        let total: f32 = histogram.iter().sum();
        histogram.iter_mut().for_each(|share| *share /= total);
        let mut stats = ToneStats {
            histogram,
            saturation,
            vivid: saturation * 1.5,
            exposure_value,
            model: ToneModel { exposure_offset: 0.0, white_ev: 0.0, black_ev: -9.0, curve: curve() },
        };
        stats.model.white_ev = stats.quantile(0.998);
        stats.model.black_ev = stats.quantile(0.002);
        stats
    }

    fn ordinary() -> ToneStats {
        scene(&[(-3.4, 0.8, 1.1), (-1.6, 0.2, 0.35)], SUNNY, 0.3)
    }

    /// Nearly everything in the dark, with a few street lights at the top of the range.
    fn night_with_lights(exposure_value: Option<f32>) -> ToneStats {
        scene(&[(-9.0, 0.95, 1.4), (-0.2, 0.05, 0.4)], exposure_value, 0.3)
    }

    /// Bright and narrow, with only a little dark in it.
    fn snow(center: f32) -> ToneStats {
        scene(&[(center, 0.92, 0.5), (center - 3.0, 0.08, 0.8)], SUNNY, 0.03)
    }

    fn neutral() -> AutoTone {
        AutoTone::default()
    }

    fn mean(stats: &ToneStats, tone: &AutoTone) -> f32 {
        stats.mean_output(tone)
    }

    #[test]
    fn a_photo_that_already_looks_right_is_left_nearly_alone() {
        let stats = ordinary();
        let tone = auto_tone(&stats);
        assert_eq!(tone.exposure, 0.0, "{tone:?}");
        assert!((mean(&stats, &tone) - mean(&stats, &neutral())).abs() < 0.03, "{tone:?}");
    }

    #[test]
    fn an_underexposed_daylight_photo_comes_up_into_the_band() {
        let stats = scene(&[(-8.2, 0.8, 1.5), (-5.2, 0.2, 0.5)], SUNNY, 0.3);
        let tone = auto_tone(&stats);
        assert!(mean(&stats, &neutral()) < 0.2);
        assert!(tone.exposure > 1.0, "{tone:?}");
        assert!(mean(&stats, &tone) > LIFT_TARGET - 0.04, "{}", mean(&stats, &tone));
    }

    #[test]
    fn a_night_scene_with_lights_stays_dark() {
        let stats = night_with_lights(STREET_AT_NIGHT);
        let tone = auto_tone(&stats);
        assert!(mean(&stats, &tone) < 0.2, "{tone:?} gave {}", mean(&stats, &tone));
        assert!(tone.exposure <= 0.0, "{tone:?}");
    }

    #[test]
    fn a_night_scene_with_no_lights_is_told_from_a_mistake_by_how_dark_it_really_was() {
        let dark = |exposure_value| scene(&[(-8.5, 1.0, 1.4)], exposure_value, 0.3);
        let night = dark(Some(3.0));
        let mistake = dark(SUNNY);
        assert_eq!(night.histogram, mistake.histogram);
        let (kept, lifted) = (auto_tone(&night), auto_tone(&mistake));
        assert!(kept.exposure <= 0.0, "{kept:?}");
        assert!(lifted.exposure > 1.0, "{lifted:?}");
        assert!(mean(&night, &kept) < 0.2, "{}", mean(&night, &kept));
    }

    #[test]
    fn a_dark_photo_with_no_exposure_data_is_held_back_by_its_lights() {
        let tone = auto_tone(&night_with_lights(None));
        assert!(tone.exposure <= HIGHLIGHT_ALLOWANCE + 0.05, "{tone:?}");
    }

    #[test]
    fn snow_stays_bright() {
        for center in [-1.8, -1.2, -0.7] {
            let stats = snow(center);
            let tone = auto_tone(&stats);
            let (before, after) = (mean(&stats, &neutral()), mean(&stats, &tone));
            assert!(before > 0.6, "{before}");
            assert!(after > before - 0.03, "{center}: {tone:?} took {before} to {after}");
            assert!(after > 0.6, "{center}: {tone:?} gave {after}");
        }
    }

    #[test]
    fn grey_snow_is_never_pushed_towards_mid_grey() {
        let stats = snow(-2.6);
        let tone = auto_tone(&stats);
        assert!(tone.exposure >= 0.0 && tone.highlights == 0.0, "{tone:?}");
        assert!(mean(&stats, &tone) >= mean(&stats, &neutral()) - 0.01);
    }

    #[test]
    fn a_blown_photo_comes_down_and_loses_clipping() {
        let stats = scene(&[(0.9, 0.6, 0.5), (-1.5, 0.4, 1.0)], Some(12.0), 0.2);
        let clipped = |tone: &AutoTone| stats.share_where(tone, |luma| luma >= WHITE_CLIP);
        let tone = auto_tone(&stats);
        assert!(tone.exposure < 0.0 && tone.highlights < 0.0, "{tone:?}");
        assert!(clipped(&tone) < clipped(&neutral()), "{tone:?}");
    }

    #[test]
    fn a_bright_sky_is_not_treated_as_highlights_to_recover() {
        let stats = scene(&[(-5.0, 0.68, 1.2), (-0.35, 0.32, 0.12)], Some(9.0), 0.2);
        assert!(stats.share_where(&neutral(), |luma| luma > HIGHLIGHT_LUMA) > 0.25);
        assert_eq!(auto_tone(&stats).highlights, 0.0);
    }

    #[test]
    fn real_backlight_lifts_the_shadows_but_night_keeps_them() {
        let bells = [(-9.0, 0.5, 0.8), (-1.2, 0.5, 0.6)];
        let backlit = scene(&bells, SUNNY, 0.3);
        let night = scene(&bells, Some(5.0), 0.3);
        assert!(auto_tone(&backlit).shadows > 15.0, "{:?}", auto_tone(&backlit));
        assert!(auto_tone(&night).shadows < auto_tone(&backlit).shadows * 0.5);
    }

    #[test]
    fn a_flat_photo_gets_contrast_and_a_contrasty_one_does_not() {
        assert!(auto_tone(&scene(&[(-3.2, 1.0, 0.5)], SUNNY, 0.3)).contrast > 3.0);
        assert_eq!(auto_tone(&scene(&[(-7.0, 0.5, 0.7), (-1.5, 0.5, 0.5)], SUNNY, 0.3)).contrast, 0.0);
    }

    #[test]
    fn a_dull_top_gets_whites_and_a_faded_bottom_gets_blacks() {
        let tone = auto_tone(&scene(&[(-3.5, 1.0, 0.4)], SUNNY, 0.3));
        assert!(tone.whites > 0.0 && tone.blacks < 0.0, "{tone:?}");
        assert!(tone.whites <= MAX_WHITES && tone.blacks >= -MAX_BLACKS, "{tone:?}");
    }

    #[test]
    fn colour_is_modest_and_goes_to_muted_photos() {
        let with_saturation = |saturation| auto_tone(&scene(&[(-3.2, 1.0, 1.6)], SUNNY, saturation));
        let muted = with_saturation(0.16);
        assert!(muted.vibrance > 2.0 && muted.vibrance <= 8.0, "{muted:?}");
        assert!(muted.saturation <= 2.0, "{muted:?}");
        let vivid = with_saturation(0.6);
        assert_eq!((vivid.vibrance, vivid.saturation), (0.0, 0.0), "{vivid:?}");
        let grey = with_saturation(0.03);
        assert_eq!((grey.vibrance, grey.saturation), (0.0, 0.0), "{grey:?}");
    }

    #[test]
    fn auto_never_adds_clipping_beyond_its_allowance() {
        let scenes = [
            ordinary(),
            snow(-1.2),
            snow(-0.4),
            night_with_lights(STREET_AT_NIGHT),
            scene(&[(-8.2, 0.8, 1.5), (-5.2, 0.2, 0.5)], SUNNY, 0.3),
            scene(&[(-3.5, 1.0, 0.4)], SUNNY, 0.3),
            scene(&[(-9.0, 0.5, 0.8), (-1.2, 0.5, 0.6)], SUNNY, 0.3),
        ];
        let tolerance = NEW_CLIPPING + 0.001;
        for stats in scenes {
            let tone = auto_tone(&stats);
            let whites = |tone: &AutoTone| stats.share_where(tone, |luma| luma >= WHITE_CLIP);
            let blacks = |tone: &AutoTone| stats.share_where(tone, |luma| luma <= BLACK_CLIP);
            assert!(whites(&tone) <= whites(&neutral()) + tolerance, "{tone:?}");
            assert!(blacks(&tone) <= blacks(&neutral()) + tolerance, "{tone:?}");
        }
    }

    #[test]
    fn sliders_land_on_steps_the_panel_can_show() {
        let tone = auto_tone(&scene(&[(-8.0, 1.0, 1.0)], SUNNY, 0.2));
        assert!((tone.exposure * 20.0 - (tone.exposure * 20.0).round()).abs() < 1e-3, "{tone:?}");
        assert_eq!(tone.whites, tone.whites.round());
    }

    #[test]
    fn the_same_photo_always_gets_the_same_answer() {
        assert_eq!(auto_tone(&ordinary()), auto_tone(&ordinary()));
    }

    #[test]
    fn only_the_sliders_auto_owns_are_touched() {
        let mut basic = Basic { clarity: 30.0, texture: -10.0, dehaze: 5.0, exposure: 2.0, ..Basic::default() };
        AutoTone { contrast: 4.0, ..AutoTone::default() }.apply_to(&mut basic);
        assert_eq!((basic.clarity, basic.texture, basic.dehaze), (30.0, -10.0, 5.0));
        assert_eq!((basic.exposure, basic.contrast), (0.0, 4.0));
    }

    #[test]
    fn the_exposure_value_comes_from_the_exif_triangle() {
        let metadata = Metadata { iso: 100.0, shutter: 1.0 / 125.0, aperture: 16.0, ..Metadata::default() };
        assert!((exposure_value(&metadata).unwrap() - 14.97).abs() < 0.01);
        let doubled = Metadata { iso: 200.0, ..metadata.clone() };
        assert!((exposure_value(&doubled).unwrap() - 13.97).abs() < 0.01);
        assert_eq!(exposure_value(&Metadata::default()), None);
    }

    #[test]
    fn quantiles_read_the_histogram() {
        let stats = scene(&[(-4.0, 1.0, 1.0)], None, 0.2);
        assert!((stats.quantile(0.5) + 4.0).abs() < 0.1);
        assert!(stats.quantile(0.9) > stats.quantile(0.1));
    }

    #[test]
    fn extreme_values_land_in_the_end_bins() {
        assert_eq!(bin_of(-40.0), 0);
        assert_eq!(bin_of(40.0), TONE_BINS - 1);
    }

    #[test]
    fn an_empty_photo_is_not_a_crash() {
        let stats = ToneStats {
            histogram: vec![0.0; TONE_BINS],
            saturation: 0.0,
            vivid: 0.0,
            exposure_value: None,
            model: ToneModel { exposure_offset: 0.0, white_ev: 0.0, black_ev: -9.0, curve: curve() },
        };
        let tone = auto_tone(&stats);
        assert!(tone.exposure.is_finite() && tone.contrast.is_finite());
    }
}
