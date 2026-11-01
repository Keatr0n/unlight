//! Checks against real weights. `nix flake check` has neither network nor models, so each test
//! skips unless the model has been installed with `unlight-cli ai download`.

use std::time::{Duration, Instant};
use unlight_ai::{
    Error, LinearImage, Manifest, MaskRaster, MaskStore, Model, ModelStatus, ModelStore, RgbImage, Task, Turn, Worker,
};

fn installed(task: Task) -> Option<(Model, ModelStore, MaskStore)> {
    std::env::var_os("ORT_DYLIB_PATH")?;
    let model = Manifest::bundled().unwrap().for_task(task)?.clone();
    let store = ModelStore::open_default().ok()?;
    if store.status(&model) != ModelStatus::Ready {
        eprintln!("skipping: {} isn't installed", model.name);
        return None;
    }
    Some((model, store, MaskStore::open_default().ok()?))
}

/// A dark disc on a pale background, off centre so turning it can't go unnoticed.
fn disc(width: u32, height: u32) -> RgbImage {
    let (cx, cy, radius) = (width as f32 * 0.35, height as f32 * 0.6, height as f32 * 0.25);
    let mut pixels = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            let inside = (x as f32 - cx).hypot(y as f32 - cy) < radius;
            pixels.extend(if inside { [40, 60, 150] } else { [225, 222, 215] });
        }
    }
    RgbImage { width, height, pixels }
}

fn load_mask(masks: &MaskStore, id: &str) -> MaskRaster {
    masks.load(id).unwrap()
}

#[test]
fn the_subject_model_finds_an_obvious_subject_and_reports_warm_timing() {
    let Some((model, store, masks)) = installed(Task::Subject) else { return };
    let worker = Worker::new(store, masks.clone(), || {});
    let image = disc(1536, 1024);

    let cold = worker.select(&model, image.clone(), Turn::None).wait().unwrap();
    let warm = worker.select(&model, image, Turn::None).wait().unwrap();
    eprintln!("load {:?}, first run {:?}, warm run {:?}", cold.load_time, cold.run_time, warm.run_time);

    assert_eq!(warm.load_time.as_secs(), 0, "the model should still be in memory");
    let mask = load_mask(&masks, &cold.mask_id);
    let value = |x: f32, y: f32| mask.values[(y as u32 * mask.width + x as u32) as usize];
    assert!(value(1536.0 * 0.35, 1024.0 * 0.6) > 60000, "the disc is the subject");
    assert!(value(1400.0, 100.0) < 5000, "the background is not");
}

#[test]
fn a_run_can_be_stopped_part_way_through() {
    let Some((model, store, masks)) = installed(Task::Denoise) else { return };
    let worker = Worker::new(store, masks, || {});
    let pixels = vec![[0.2, 0.3, 0.25, 1.0]; 512 * 512];
    let image = LinearImage { width: 512, height: 512, pixels };

    let ticket = worker.denoise(&model, image, 1.0);
    std::thread::sleep(Duration::from_secs(2));
    ticket.cancel();
    let cancelled = Instant::now();
    let result = ticket.wait();

    assert!(matches!(result, Err(Error::Cancelled)), "expected a cancellation, got {result:?}");
    eprintln!("stopped {:?} after the request", cancelled.elapsed());
    assert!(cancelled.elapsed() < Duration::from_secs(1), "the run should stop at the next graph node");
}

#[test]
fn the_sensor_orientation_gives_the_same_mask_turned() {
    let Some((model, store, masks)) = installed(Task::Sky) else { return };
    let worker = Worker::new(store, masks.clone(), || {});
    let (long, short) = (600usize, 400usize);
    let upright = disc(long as u32, short as u32);
    // The sensor image a clockwise turn stands up into `upright`.
    let sideways = RgbImage {
        width: short as u32,
        height: long as u32,
        pixels: (0..long)
            .flat_map(|y| (0..short).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let from = (x * long + (long - 1 - y)) * 3;
                [upright.pixels[from], upright.pixels[from + 1], upright.pixels[from + 2]]
            })
            .collect(),
    };

    let plain = worker.select(&model, upright, Turn::None).wait().unwrap();
    let turned = worker.select(&model, sideways, Turn::Clockwise).wait().unwrap();

    let (plain, turned) = (load_mask(&masks, &plain.mask_id), load_mask(&masks, &turned.mask_id));
    assert_eq!((turned.width, turned.height), (short as u32, long as u32));
    for up_y in 0..short {
        for up_x in 0..long {
            let expected = plain.values[up_y * long + up_x];
            let actual = turned.values[(long - 1 - up_x) * short + up_y];
            assert_eq!(expected, actual, "at {up_x},{up_y}");
        }
    }
    let (low, high) = plain.values.iter().fold((u16::MAX, 0), |(low, high), &value| (low.min(value), high.max(value)));
    assert!(high - low > 30000, "a flat mask would pass this for the wrong reason: {low}..{high}");
}
