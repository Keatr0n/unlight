//! Connects the white balance picker to the engine.

use crate::{
    darkroom::LivePhoto,
    state::{AppState, edit::Step, white_balance::solve},
};
use unlight_engine::{Session, ViewRequest};
use unlight_recipe::{Recipe, WhiteBalance};

const SPOT_PIXELS: u32 = 8;
/// Half the sampled square, as a fraction of the view.
const SPOT_RADIUS: f32 = 0.003;

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn average_linear(pixels: &[u8]) -> [f32; 3] {
    let mut sum = [0.0; 3];
    let count = (pixels.len() / 4).max(1) as f32;
    for pixel in pixels.as_chunks::<4>().0 {
        for (total, value) in sum.iter_mut().zip(pixel) {
            *total += srgb_to_linear(f32::from(*value) / 255.0);
        }
    }
    sum.map(|total| total / count)
}

/// The photo's own white balance as temperature and tint.
pub fn as_shot(session: &Session) -> (f32, f32) {
    let profile = &session.source.profile;
    let (temperature, tint) = profile.temperature_for_neutral(&profile.as_shot_neutral());
    (temperature as f32, tint as f32)
}

/// Makes the picked spot neutral. Tools are off while it measures, so that only white
/// balance is being solved for, and it samples the preview, so a pick never waits on a
/// full-resolution demosaic.
pub fn pick(state: &mut AppState, live: &mut LivePhoto, point: [f32; 2]) {
    let Some(edit) = state.edit() else {
        return;
    };
    let mut probe = Recipe {
        crop: edit.recipe.crop,
        transform: edit.recipe.transform.clone(),
        lens: edit.recipe.lens,
        ..Recipe::default()
    };
    let start = match edit.recipe.white_balance {
        WhiteBalance::AsShot => live.as_shot,
        WhiteBalance::Custom { temperature, tint } => (temperature, tint),
    };
    let target = live.session.view_target(SPOT_PIXELS, SPOT_PIXELS);
    let [x, y] = live.orientation.then(edit.recipe.orientation).to_sensor(point);
    let region = [
        (x - SPOT_RADIUS).clamp(0.0, 1.0),
        (y - SPOT_RADIUS).clamp(0.0, 1.0),
        SPOT_RADIUS * 2.0,
        SPOT_RADIUS * 2.0,
    ];
    let view = ViewRequest {
        region,
        ..ViewRequest::default()
    };
    let solved = solve(start, |temperature, tint| {
        probe.white_balance = WhiteBalance::Custom { temperature, tint };
        live.session.render_progressive(&probe, &view, &target, 0).ok()?;
        live.session
            .read_view(&target)
            .ok()
            .map(|pixels| average_linear(&pixels))
    });
    match solved {
        Some((temperature, tint)) => {
            if let Some(edit) = state.edit_mut() {
                let (temperature, tint) = (temperature.round(), tint.round());
                edit.recipe.white_balance = WhiteBalance::Custom { temperature, tint };
                edit.note(
                    "White Balance",
                    format!("{temperature:.0} K, tint {tint:+.0}"),
                    Step::New,
                );
            }
        }
        None => state.notify("That spot has no colour to balance on"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averaging_undoes_the_display_encoding() {
        let average = average_linear(&[188, 188, 188, 255, 188, 188, 188, 255]);
        assert!((average[0] - 0.503).abs() < 0.005, "{average:?}");
    }
}
