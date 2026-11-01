//! The white balance picker. There's no closed form from a rendered pixel back to temperature
//! and tint once tone and colour have been applied, so it searches: render, measure how far
//! the pixel is from neutral, and take a Newton step in mired and tint.

const ITERATIONS: usize = 6;
const NEUTRAL_ENOUGH: f32 = 0.004;
const MIRED_PROBE: f32 = 8.0;
const TINT_PROBE: f32 = 4.0;
const MIRED_LIMITS: (f32, f32) = (20.0, 500.0);
const TINT_LIMIT: f32 = 150.0;

fn to_mired(temperature: f32) -> f32 {
    1e6 / temperature
}

/// Distance from neutral on the blue–yellow and green–magenta axes.
fn cast(rgb: [f32; 3]) -> [f32; 2] {
    let [r, g, b] = rgb.map(|channel| channel.max(1e-4).ln());
    [r - b, g - (r + b) * 0.5]
}

fn solve_2x2(j: [[f32; 2]; 2], error: [f32; 2]) -> Option<[f32; 2]> {
    let determinant = j[0][0] * j[1][1] - j[0][1] * j[1][0];
    if determinant.abs() < 1e-9 {
        return None;
    }
    Some([
        (error[0] * j[1][1] - error[1] * j[0][1]) / determinant,
        (error[1] * j[0][0] - error[0] * j[1][0]) / determinant,
    ])
}

/// Finds the temperature and tint that make the measured colour neutral. `measure` returns
/// the linear RGB of the picked spot for a white balance, or `None` to give up.
pub fn solve(start: (f32, f32), mut measure: impl FnMut(f32, f32) -> Option<[f32; 3]>) -> Option<(f32, f32)> {
    let (mut mired, mut tint) = (to_mired(start.0), start.1);
    let at = |mired: f32, tint: f32, measure: &mut dyn FnMut(f32, f32) -> Option<[f32; 3]>| {
        measure(1e6 / mired, tint).map(cast)
    };
    for _ in 0..ITERATIONS {
        let base = at(mired, tint, &mut measure)?;
        if base[0].abs().max(base[1].abs()) < NEUTRAL_ENOUGH {
            break;
        }
        let along_mired = at(mired + MIRED_PROBE, tint, &mut measure)?;
        let along_tint = at(mired, tint + TINT_PROBE, &mut measure)?;
        let jacobian = [
            [
                (along_mired[0] - base[0]) / MIRED_PROBE,
                (along_tint[0] - base[0]) / TINT_PROBE,
            ],
            [
                (along_mired[1] - base[1]) / MIRED_PROBE,
                (along_tint[1] - base[1]) / TINT_PROBE,
            ],
        ];
        let [step_mired, step_tint] = solve_2x2(jacobian, base)?;
        mired = (mired - step_mired).clamp(MIRED_LIMITS.0, MIRED_LIMITS.1);
        tint = (tint - step_tint).clamp(-TINT_LIMIT, TINT_LIMIT);
    }
    Some((1e6 / mired, tint))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pretend camera whose cast disappears at a known white balance.
    fn scene(temperature: f32, tint: f32) -> Option<[f32; 3]> {
        let warm = (to_mired(temperature) - to_mired(4200.0)) * 0.004;
        let green = (tint - 12.0) * 0.006;
        Some([
            (warm - green * 0.5).exp() * 0.4,
            (green).exp() * 0.4,
            (-warm - green * 0.5).exp() * 0.4,
        ])
    }

    #[test]
    fn it_finds_the_neutral_white_balance() {
        let (temperature, tint) = solve((6500.0, 0.0), scene).unwrap();
        assert!((temperature - 4200.0).abs() < 40.0, "{temperature}");
        assert!((tint - 12.0).abs() < 1.0, "{tint}");
    }

    #[test]
    fn it_stops_when_already_neutral() {
        let mut calls = 0;
        let result = solve((4200.0, 12.0), |temperature, tint| {
            calls += 1;
            scene(temperature, tint)
        });
        assert_eq!(calls, 1);
        assert!(result.is_some());
    }

    #[test]
    fn a_failed_measurement_gives_up() {
        assert_eq!(solve((5000.0, 0.0), |_, _| None), None);
    }

    #[test]
    fn a_grey_scene_that_never_changes_gives_up() {
        assert_eq!(solve((5000.0, 0.0), |_, _| Some([0.5, 0.2, 0.2])), None);
    }
}
