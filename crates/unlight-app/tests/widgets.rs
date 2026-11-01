//! Golden images of the custom widgets. These guard the look: refresh them with
//! `UPDATE_SNAPSHOTS=1 cargo test -p unlight-app --test widgets` after a deliberate change.

use eframe::egui::{Ui, vec2};
use egui_kittest::Harness;
use unlight_app::{
    renderer::Histogram,
    state::edit::Step,
    ui::{
        theme,
        widgets::{
            controls::{SectionHeader, check_row, section, segmented},
            curve::PointCurve,
            histogram,
            slider::{Slider, Spec},
            wheel,
        },
    },
};
use unlight_recipe::{Curve, CurvePoint, GradeWheel};

const EXPOSURE: Spec = Spec::bipolar("Exposure", 5.0).decimals(2, 0.05);
const AMOUNT: Spec = Spec::amount("Amount", 150.0);

fn snapshot(name: &str, size: (f32, f32), mut add: impl FnMut(&mut Ui)) {
    let mut harness = Harness::builder()
        .with_size(vec2(size.0, size.1))
        .with_context_setup(theme::install)
        .build_ui(move |ui| add(ui));
    harness.run();
    harness.snapshot(name);
}

fn slope_histogram() -> Histogram {
    let channel =
        |peak: f32| (0..256).map(move |bin| (20_000.0 / (1.0 + ((bin as f32 - peak) / 30.0).powi(2))) as u32);
    Histogram::new(
        channel(90.0)
            .chain(channel(120.0))
            .chain(channel(150.0))
            .collect(),
    )
}

#[test]
fn sliders() {
    snapshot("sliders", (320.0, 96.0), |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let (mut exposure, mut amount, mut tint) = (0.75, 40.0, -30.0);
        assert_eq!(
            Slider::new(&EXPOSURE, &mut exposure, 0.0).show(ui).change,
            None::<Step>
        );
        Slider::new(&AMOUNT, &mut amount, 40.0).show(ui);
        let track = [theme::LABEL_GREEN, theme::TEXT, theme::LABEL_PURPLE];
        Slider::new(&Spec::bipolar("Tint", 150.0), &mut tint, 0.0)
            .gradient(&track)
            .show(ui);
    });
}

#[test]
fn section_with_controls() {
    snapshot("section", (320.0, 120.0), |ui| {
        let header = SectionHeader {
            title: "Effects",
            open: true,
            enabled: Some(true),
            modified: true,
        };
        section(ui, &header, |ui| {
            let mut choice = 1;
            segmented(ui, &mut choice, &[(0, "Parametric"), (1, "Point")]);
            let mut checked = true;
            check_row(ui, &mut checked, "Enable Profile Corrections");
        });
    });
}

#[test]
fn histogram_with_clipping() {
    snapshot("histogram", (320.0, 120.0), |ui| {
        let mut data = slope_histogram();
        data.bins[255] = 10_000;
        data.bins[256] = 10_000;
        let data = Histogram::new(data.bins);
        histogram::show(ui, Some(&data), false, "clipping");
    });
}

#[test]
fn color_wheel() {
    snapshot("wheel", (160.0, 160.0), |ui| {
        let mut grade = GradeWheel {
            hue: 210.0,
            saturation: 60.0,
            luminance: 0.0,
        };
        wheel::show(ui, &mut grade, "Shadows", 140.0);
    });
}

#[test]
fn point_curve() {
    snapshot("curve", (320.0, 256.0), |ui| {
        let mut curve = Curve(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.3, 0.2),
            CurvePoint::new(0.7, 0.8),
            CurvePoint::new(1.0, 1.0),
        ]);
        let sampler = |curve: &Curve| {
            let tone = unlight_recipe::ToneCurve {
                rgb: curve.clone(),
                ..Default::default()
            };
            unlight_engine::build_lut(&tone)
                .iter()
                .map(|entry| entry[3])
                .collect::<Vec<f32>>()
        };
        let histogram = slope_histogram();
        PointCurve {
            curve: &mut curve,
            sampler: &sampler,
            color: theme::HANDLE,
            histogram: Some(&histogram),
            label: "Point Curve",
        }
        .show(ui);
    });
}
