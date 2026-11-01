//! How long moving to the next photo takes on the real app, engine and GPU. The numbers need a
//! quiet machine and the real raws, so this only runs when asked:
//!
//! `UNLIGHT_SAMPLES=... cargo test --release -p unlight-app --test latency -- --ignored --nocapture`

mod support;

use std::{fs, thread, time::{Duration, Instant}};
use support::{Fixture, WINDOW, live, open_app, samples, settle};
use unlight_app::{action::Action, app::App};

const STEPS: usize = 30;
const UNDECODABLE: &str = "nikon-z8-compressed";

fn percentile(times: &[f64], share: f64) -> f64 {
    let mut sorted = times.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[((sorted.len() - 1) as f64 * share) as usize]
}

fn report(name: &str, times: &[f64]) {
    println!(
        "{name:<22} p50 {:7.1} ms  p95 {:7.1} ms  max {:7.1} ms",
        percentile(times, 0.5),
        percentile(times, 0.95),
        percentile(times, 1.0)
    );
}

fn shown(app: &App) -> bool {
    app.state.focused().is_some_and(|row| app.darkroom.preview(&row.path).is_some())
}

#[test]
#[ignore = "needs a quiet machine and the real raws"]
fn next_photo_latency() {
    let samples = samples().expect("UNLIGHT_SAMPLES");
    let fixture = Fixture::empty();
    for entry in fs::read_dir(samples).unwrap().flatten() {
        let name = entry.file_name();
        if entry.path().is_file() && !name.to_string_lossy().contains(UNDECODABLE) {
            fs::copy(entry.path(), fixture.photos.join(name)).unwrap();
        }
    }
    let folder = fixture.photos.clone();
    let store = fixture.store();
    fixture.scan(&store);
    let mut test = open_app(fixture, store, WINDOW, 1.0, |_| {});
    settle(&mut test, |app| app.state.edit().is_some());
    test.harness.state_mut().state.apply(Action::ShowDevelop);
    settle(&mut test, live);

    let photos = fs::read_dir(&folder).unwrap().count();
    let (mut position, mut forwards) = (0, true);
    // A person dwells on each photo a moment, which is what lets the neighbours preload.
    for dwell in [1500, 300, 0] {
        let (mut to_image, mut to_editable) = (Vec::new(), Vec::new());
        for step in 0..STEPS {
            let pause = Instant::now();
            while pause.elapsed() < Duration::from_millis(dwell) {
                test.harness.step();
                thread::sleep(Duration::from_millis(5));
            }
            forwards = (forwards || position == 0) && position + 1 < photos;
            position = if forwards { position + 1 } else { position - 1 };
            let action = if forwards { Action::NextPhoto } else { Action::PreviousPhoto };
            let before = test.harness.state().state.focused().unwrap().path.clone();
            test.harness.state_mut().state.apply(action);
            let start = Instant::now();
            let moved = |app: &App| app.state.focused().is_some_and(|row| row.path != before);
            let mut image = None;
            loop {
                // Counting a frame in each measurement, since a photo isn't seen until it's drawn.
                test.harness.step();
                if image.is_none() && moved(test.harness.state()) && shown(test.harness.state()) {
                    image = Some(start.elapsed());
                }
                if moved(test.harness.state()) && live(test.harness.state()) {
                    break;
                }
                assert!(start.elapsed() < Duration::from_secs(30), "stuck on step {step}");
            }
            let editable = start.elapsed();
            to_image.push(image.unwrap_or(editable).as_secs_f64() * 1000.0);
            to_editable.push(editable.as_secs_f64() * 1000.0);
        }
        println!("dwelling {dwell} ms on each photo");
        report("to first image", &to_image);
        report("to editable", &to_editable);
    }
}
