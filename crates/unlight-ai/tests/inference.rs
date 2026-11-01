mod support;

use std::fs;
use support::{brightness_model, identity_model, restoration, runtime_available, segmentation, sqrt_model};
use tempfile::TempDir;
use unlight_ai::{Error, LinearImage, MaskRaster, MaskStore, Model, ModelStore, RgbImage, Turn, Worker};

const SIDE: u32 = 16;

struct Setup {
    _directory: TempDir,
    models: ModelStore,
    masks: MaskStore,
    model: Model,
    worker: Worker,
}

fn setup() -> Setup {
    let bytes = brightness_model(u64::from(SIDE));
    setup_with(&bytes, segmentation(&bytes, SIDE))
}

fn setup_with(bytes: &[u8], model: Model) -> Setup {
    let directory = TempDir::new().unwrap();
    let models = ModelStore::new(directory.path().join("models"));
    let path = models.path(&model);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    let masks = MaskStore::new(directory.path().join("masks"));
    let worker = Worker::new(models.clone(), masks.clone(), || {});
    Setup { _directory: directory, models, masks, model, worker }
}

/// White on the left half, black on the right.
fn half_white(width: u32, height: u32) -> RgbImage {
    let pixels = (0..height)
        .flat_map(|_| (0..width).flat_map(move |x| [if x < width / 2 { 255 } else { 0 }; 3]))
        .collect();
    RgbImage { width, height, pixels }
}

fn at(mask: &MaskRaster, x: u32, y: u32) -> u16 {
    mask.values[(y * mask.width + x) as usize]
}

#[test]
fn a_selection_runs_from_verified_file_to_stored_mask() {
    if !runtime_available() {
        return;
    }
    let setup = setup();
    let selection = setup.worker.select(&setup.model, half_white(64, 48), Turn::None).wait().unwrap();

    let mask = setup.masks.load(&selection.mask_id).unwrap();
    assert_eq!((mask.width, mask.height), (64, 48));
    assert_eq!(at(&mask, 8, 24), 65535, "the bright half is selected");
    assert_eq!(at(&mask, 56, 24), 0, "the dark half is not");
}

#[test]
fn a_mask_comes_back_in_the_orientation_of_the_sensor() {
    if !runtime_available() {
        return;
    }
    let setup = setup();
    let selection = setup.worker.select(&setup.model, half_white(64, 48), Turn::Clockwise).wait().unwrap();

    let mask = setup.masks.load(&selection.mask_id).unwrap();
    assert_eq!((mask.width, mask.height), (64, 48));
    assert_eq!(at(&mask, 8, 24), 65535);
    assert_eq!(at(&mask, 56, 24), 0);
}

#[test]
fn the_model_stays_loaded_between_selections() {
    if !runtime_available() {
        return;
    }
    let setup = setup();
    let first = setup.worker.select(&setup.model, half_white(32, 32), Turn::None).wait().unwrap();
    let second = setup.worker.select(&setup.model, half_white(32, 32), Turn::None).wait().unwrap();
    assert_eq!(first.mask_id, second.mask_id);
    assert!(second.load_time < first.load_time);
}

#[test]
fn a_model_altered_on_disk_never_reaches_the_runtime() {
    let setup = setup();
    fs::write(setup.models.path(&setup.model), b"pretend this is a poisoned model").unwrap();

    let result = setup.worker.select(&setup.model, half_white(32, 32), Turn::None).wait();

    assert!(matches!(result, Err(Error::HashMismatch { .. })));
}

#[test]
fn a_model_that_was_never_downloaded_is_reported_clearly() {
    let setup = setup();
    fs::remove_file(setup.models.path(&setup.model)).unwrap();

    let result = setup.worker.select(&setup.model, half_white(32, 32), Turn::None).wait();

    assert!(matches!(result, Err(Error::ModelMissing(_))));
}

#[test]
fn a_job_cancelled_while_queued_never_runs() {
    if !runtime_available() {
        return;
    }
    let setup = setup();
    let ahead = setup.worker.select(&setup.model, half_white(2048, 2048), Turn::None);
    let queued = setup.worker.select(&setup.model, half_white(32, 32), Turn::None);
    queued.cancel();

    assert!(matches!(queued.wait(), Err(Error::Cancelled)));
    assert!(ahead.wait().is_ok(), "cancelling one job must not disturb another");
}

/// Bright and varied enough that a seam or a wrong tile position would show, with some values
/// above white and some below black, as a real demosaiced photo has.
fn scene(width: u32, height: u32) -> LinearImage {
    let pixels = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let wave = ((x as f32 * 0.31).sin() + (y as f32 * 0.17).cos()) * 0.25 + 0.5;
            [wave, wave * 0.5, 1.0 - wave, 1.0]
        })
        .collect();
    LinearImage { width, height, pixels }
}

#[test]
fn a_model_that_changes_nothing_leaves_no_seams_between_tiles() {
    if !runtime_available() {
        return;
    }
    let bytes = identity_model(32);
    let setup = setup_with(&bytes, restoration(&bytes, 32, 8));
    let mut image = scene(100, 70);
    image.pixels[1234] = [40.0, -0.5, 1.5, 1.0];

    let result = setup.worker.denoise(&setup.model, image.clone(), 1.0).wait().unwrap();

    for (index, (before, after)) in image.pixels.iter().zip(&result.pixels).enumerate() {
        for channel in 0..4 {
            assert!((before[channel] - after[channel]).abs() < 2e-3, "pixel {index}: {before:?} became {after:?}");
        }
    }
}

#[test]
fn strength_blends_between_the_photo_and_the_result() {
    if !runtime_available() {
        return;
    }
    let bytes = sqrt_model(32);
    let setup = setup_with(&bytes, restoration(&bytes, 32, 8));
    let (width, height) = (64u32, 48u32);
    let pixels = (0..width * height)
        .map(|index| {
            let level = if (index + index / width) % 2 == 0 { 0.1 } else { 0.5 };
            [level, level, level, 1.0]
        })
        .collect();
    let image = LinearImage { width, height, pixels };
    let run = |strength| setup.worker.denoise(&setup.model, image.clone(), strength).wait().unwrap();

    let (none, half, full) = (run(0.0), run(0.5), run(1.0));

    assert_eq!(none.pixels, image.pixels);
    let index = (20 * width + 30) as usize;
    let (original, middle, whole) = (image.pixels[index][1], half.pixels[index][1], full.pixels[index][1]);
    assert!((whole - original).abs() > 0.01, "the model changes the photo: {original} became {whole}");
    assert!((middle - (original + whole) / 2.0).abs() < 1e-4, "{original}, {middle}, {whole}");
}

#[test]
fn a_photo_smaller_than_a_tile_still_works() {
    if !runtime_available() {
        return;
    }
    let bytes = identity_model(32);
    let setup = setup_with(&bytes, restoration(&bytes, 32, 8));
    let image = scene(20, 12);

    let result = setup.worker.denoise(&setup.model, image.clone(), 1.0).wait().unwrap();

    assert_eq!((result.width, result.height), (20, 12));
    assert!((result.pixels[37][0] - image.pixels[37][0]).abs() < 2e-3);
}

#[test]
fn denoising_reports_each_tile_and_can_be_cancelled_between_them() {
    if !runtime_available() {
        return;
    }
    let bytes = identity_model(32);
    let setup = setup_with(&bytes, restoration(&bytes, 32, 8));

    let finished = setup.worker.denoise(&setup.model, scene(64, 32), 1.0);
    while finished.poll().is_none() {
        std::thread::yield_now();
    }
    assert_eq!(finished.progress().done, finished.progress().total);
    assert_eq!(finished.progress().total, 3);

    let ahead = setup.worker.denoise(&setup.model, scene(2000, 2000), 1.0);
    let queued = setup.worker.denoise(&setup.model, scene(64, 32), 1.0);
    queued.cancel();
    ahead.cancel();
    assert!(matches!(queued.wait(), Err(Error::Cancelled)));
    assert!(matches!(ahead.wait(), Err(Error::Cancelled)));
}

#[test]
fn a_model_for_another_job_is_refused_rather_than_run() {
    let setup = setup();
    let result = setup.worker.denoise(&setup.model, scene(32, 32), 1.0).wait();
    assert!(matches!(result, Err(Error::Manifest(_))));
}
