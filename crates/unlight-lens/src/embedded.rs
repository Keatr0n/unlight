//! Gives meaning to the tables cameras embed in their raws. None of this is documented by the
//! makers, so each reading below was checked by registering our uncorrected render against the
//! camera's own corrected JPEG. All of them normalise the radius to half the image diagonal.
//! Every table is sanity-checked: a misread file falls through to the lens database rather
//! than bending the photo.

use crate::{
    profile::{Frame, LensProfile, Source},
    spline::Spline,
};
use unlight_raw::{EmbeddedLens, FujifilmLens, NikonLens, PanasonicLens, SonyLens};

/// No rectilinear lens corrects by more than this at the corner, so a larger value means the
/// table was misread.
const MAX_DISTORTION: f64 = 0.3;

const SONY_DISTORTION_ONE: f64 = 16384.0;
const SONY_CHROMATIC_ONE: f64 = 2097152.0;

const NEWTON_ITERATIONS: usize = 12;
const RADIUS_ITERATIONS: usize = 16;

fn even_knots(count: usize) -> Vec<f64> {
    (0..count).map(|index| index as f64 / (count - 1) as f64).collect()
}

fn sony_spline(values: &[i16], one: f64) -> Spline {
    Spline::new(even_knots(values.len()), values.iter().map(|&value| f64::from(value) / one).collect())
}

/// Sony's vignette table goes unused on purpose. Files carry one, but on the A7R IV sample the
/// raw's sky shows none of the 0.9 EV falloff it describes, and neither does the camera's JPEG,
/// so applying it brightened the corners by that much.
fn sony_profile(lens: &SonyLens, frame: Frame) -> Option<LensProfile> {
    let half_diagonal = frame.half_diagonal();
    let mut profile = LensProfile::empty(frame);
    if lens.distortion.len() < 2 {
        return None;
    }
    let distortion = sony_spline(&lens.distortion, SONY_DISTORTION_ONE);
    if distortion.at(1.0).abs() > MAX_DISTORTION {
        return None;
    }
    profile.set_distortion(Source::Sony, |radius| 1.0 + distortion.at(radius / half_diagonal));

    let (red, blue) = lens.chromatic.split_at(lens.chromatic.len() / 2);
    if red.len() >= 2 {
        let (red, blue) = (sony_spline(red, SONY_CHROMATIC_ONE), sony_spline(blue, SONY_CHROMATIC_ONE));
        profile.set_chromatic_by_output(Source::Sony, |radius| {
            let radius = radius / half_diagonal;
            [1.0 + red.at(radius), 1.0 + blue.at(radius)]
        });
    }
    Some(profile)
}

/// Fujifilm's first knot isn't at the centre, where nothing needs correcting.
fn fuji_spline(knots: &[f64], values: &[f64], at_centre: f64) -> Spline {
    let with_centre = |first: f64, rest: &[f64]| {
        std::iter::once(first).chain(rest.iter().copied()).collect::<Vec<_>>()
    };
    Spline::new(with_centre(0.0, knots), with_centre(at_centre, values))
}

/// Fujifilm's tables are keyed by source radius, unlike Sony's: they stop changing at exactly
/// the sensor corner, and registration against the JPEG agrees. So finding the source radius
/// for an output radius means solving `source = output·scale(source)`.
fn fujifilm_profile(lens: &FujifilmLens, frame: Frame) -> Option<LensProfile> {
    let ascending = lens.knots.windows(2).all(|pair| pair[0] < pair[1]);
    let plausible = lens.knots.first().is_some_and(|&first| first > 0.0) && ascending;
    let bounded = lens.distortion.iter().all(|value| value.abs() / 100.0 < MAX_DISTORTION);
    if !plausible || !bounded {
        return None;
    }
    let half_diagonal = frame.half_diagonal();
    let mut profile = LensProfile::empty(frame);
    let distortion = fuji_spline(&lens.knots, &lens.distortion, 0.0);
    let scale = |source: f64| 1.0 + distortion.at(source / half_diagonal) / 100.0;
    profile.set_distortion(Source::Fujifilm, |radius| {
        scale((0..RADIUS_ITERATIONS).fold(radius, |source, _| radius * scale(source)))
    });
    if !lens.chromatic_red.is_empty() && !lens.chromatic_blue.is_empty() {
        let at_centre = |values: &[f64]| values[0];
        let red = fuji_spline(&lens.knots, &lens.chromatic_red, at_centre(&lens.chromatic_red));
        let blue = fuji_spline(&lens.knots, &lens.chromatic_blue, at_centre(&lens.chromatic_blue));
        profile.set_chromatic(Source::Fujifilm, |radius| {
            let radius = radius / half_diagonal;
            [1.0 + red.at(radius), 1.0 + blue.at(radius)]
        });
    }
    if !lens.vignetting.is_empty() {
        let vignette = fuji_spline(&lens.knots, &lens.vignetting, 100.0);
        profile.set_vignette(Source::Fujifilm, |radius| {
            -(vignette.at(radius / half_diagonal) / 100.0).max(f64::MIN_POSITIVE).log2()
        });
    }
    Some(profile)
}

/// Solves `Ru(Rd)` for `Rd`, since the tag maps source to output and the engine needs the
/// reverse.
fn panasonic_source_radius(lens: &PanasonicLens, output: f64) -> f64 {
    let forward = |rd: f64| {
        let rd2 = rd * rd;
        lens.scale * rd * (1.0 + rd2 * (lens.a + rd2 * (lens.b + rd2 * lens.c)))
    };
    let slope = |rd: f64| {
        let rd2 = rd * rd;
        lens.scale * (1.0 + rd2 * (3.0 * lens.a + rd2 * (5.0 * lens.b + rd2 * 7.0 * lens.c)))
    };
    (0..NEWTON_ITERATIONS).fold(output, |rd, _| rd - (forward(rd) - output) / slope(rd))
}

fn panasonic_profile(lens: &PanasonicLens, frame: Frame) -> Option<LensProfile> {
    let to_tag = frame.height / lens.radius;
    let edge = frame.half_diagonal() * to_tag;
    if (panasonic_source_radius(lens, edge) / edge - 1.0).abs() > MAX_DISTORTION {
        return None;
    }
    let mut profile = LensProfile::empty(frame);
    profile.set_distortion(Source::Panasonic, |radius| {
        let output = (radius * to_tag).max(1e-6);
        panasonic_source_radius(lens, output) / output
    });
    Some(profile)
}

/// A plain cubic in the radius, odd terms included, which fits a camera JPEG noticeably better
/// than the even polynomial most lens models use.
fn nikon_profile(lens: &NikonLens, frame: Frame) -> Option<LensProfile> {
    let half_diagonal = frame.half_diagonal();
    let [k1, k2, k3] = lens.distortion;
    let scale = |radius: f64| {
        let radius = radius / half_diagonal;
        1.0 + radius * (k1 + radius * (k2 + radius * k3))
    };
    if (scale(half_diagonal) - 1.0).abs() > MAX_DISTORTION {
        return None;
    }
    let mut profile = LensProfile::empty(frame);
    profile.set_distortion(Source::Nikon, scale);
    Some(profile)
}

pub(crate) fn profile(lens: &EmbeddedLens, frame: Frame) -> Option<LensProfile> {
    match lens {
        EmbeddedLens::Sony(lens) => sony_profile(lens, frame),
        EmbeddedLens::Fujifilm(lens) => fujifilm_profile(lens, frame),
        EmbeddedLens::Panasonic(lens) => panasonic_profile(lens, frame),
        EmbeddedLens::Nikon(lens) => nikon_profile(lens, frame),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Frame = Frame { width: 6000.0, height: 4000.0 };

    #[test]
    fn sony_table_reads_as_a_fraction_of_the_half_diagonal() {
        let distortion: Vec<i16> = (0..16).map(|index| -(index * 40)).collect();
        let lens = SonyLens { distortion, chromatic: Vec::new() };
        let profile = sony_profile(&lens, FRAME).unwrap();
        let corner = FRAME.half_diagonal() as f32;
        let expected = 1.0 + (-15.0 * 40.0) / 16384.0;
        assert!((profile.distortion_scale(corner) - expected).abs() < 1e-5);
        assert_eq!(profile.chromatic, None);
    }

    #[test]
    fn a_wild_sony_table_is_refused() {
        let lens = SonyLens { distortion: vec![0, 16000], chromatic: Vec::new() };
        assert_eq!(sony_profile(&lens, FRAME), None);
    }

    fn fuji() -> FujifilmLens {
        let knots: Vec<f64> = (1..=9).map(|index| (f64::from(index) / 8.0).sqrt()).collect();
        FujifilmLens {
            distortion: knots.iter().map(|radius| -8.0 * radius * radius).collect(),
            chromatic_red: vec![-1e-4; 9],
            chromatic_blue: vec![1e-3; 9],
            vignetting: knots.iter().map(|radius| 100.0 - 50.0 * radius * radius).collect(),
            knots,
        }
    }

    #[test]
    fn fuji_percentages_become_ratios() {
        let profile = fujifilm_profile(&fuji(), FRAME).unwrap();
        let corner = FRAME.half_diagonal() as f32;
        assert!((profile.vignette_stops(corner) - 1.0).abs() < 1e-3);
        let [red, blue] = profile.chromatic_scale(corner);
        assert!((red - 0.9999).abs() < 1e-6 && (blue - 1.001).abs() < 1e-6);
    }

    #[test]
    fn fuji_correction_fades_to_nothing_at_the_centre() {
        let profile = fujifilm_profile(&fuji(), FRAME).unwrap();
        assert!((profile.distortion_scale(0.0) - 1.0).abs() < 1e-6);
        assert!(profile.vignette_stops(0.0).abs() < 1e-6);
    }

    #[test]
    fn fuji_scale_is_read_at_the_source_radius() {
        let profile = fujifilm_profile(&fuji(), FRAME).unwrap();
        let half_diagonal = FRAME.half_diagonal();
        let output = 0.8 * half_diagonal;
        let scale = f64::from(profile.distortion_scale(output as f32));
        let source = output * scale / half_diagonal;
        assert!((scale - (1.0 - 0.08 * source * source)).abs() < 1e-4, "{scale}");
    }

    #[test]
    fn nikon_terms_form_a_cubic_in_the_radius() {
        let lens = NikonLens { distortion: [0.02, -0.01, 0.04], vignette: [0.0; 3] };
        let profile = nikon_profile(&lens, FRAME).unwrap();
        let half_diagonal = FRAME.half_diagonal() as f32;
        assert!((profile.distortion_scale(half_diagonal) - 1.05).abs() < 1e-4);
        let expected = 1.0 + 0.5 * 0.02 - 0.25 * 0.01 + 0.125 * 0.04;
        assert!((profile.distortion_scale(half_diagonal * 0.5) - expected).abs() < 1e-5);
    }

    #[test]
    fn fuji_tables_out_of_order_are_refused() {
        let mut lens = fuji();
        lens.knots.swap(2, 3);
        assert_eq!(fujifilm_profile(&lens, FRAME), None);
    }

    #[test]
    fn panasonic_inversion_satisfies_the_tagged_equation() {
        let lens = PanasonicLens { scale: 1.005, a: 0.03, b: -0.01, c: 0.003, radius: 3240.0 };
        for output in [0.1, 0.5, 0.9, 1.0] {
            let rd = panasonic_source_radius(&lens, output);
            let rd2 = rd * rd;
            let forward = lens.scale * rd * (1.0 + rd2 * (lens.a + rd2 * (lens.b + rd2 * lens.c)));
            assert!((forward - output).abs() < 1e-9, "{forward} vs {output}");
        }
    }

    #[test]
    fn panasonic_normalises_by_the_tagged_radius() {
        let lens = PanasonicLens { scale: 1.0, a: 0.05, b: 0.0, c: 0.0, radius: 2000.0 };
        let profile = panasonic_profile(&lens, FRAME).unwrap();
        // 0.5 image heights is 2000 pixels here, which is 1 in the tag's units.
        let source = f64::from(profile.distortion_scale(0.5));
        assert!((source * (1.0 + 0.05 * source * source) - 1.0).abs() < 1e-5, "{source}");
    }
}
