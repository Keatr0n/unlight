//! The plan's stability gate: a 1000-photo batch on the real GPU with no device lost and no
//! memory creeping up. It takes the better part of an hour, so it only runs when asked:
//!
//! `UNLIGHT_SAMPLES=... cargo test --release -p unlight-app --test soak -- --ignored --nocapture`

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use unlight_app::{
    export::{ExportEvent, ExportJob, Exporter},
    state::transfer::ExportForm,
};
use unlight_catalog::{CancelToken, Catalog, Filter, Page, Sort, VariantId};
use unlight_output::CollisionPolicy;
use unlight_recipe::{LocalAdjustments, Mask, MaskComponent, MaskShape, Recipe};

const PHOTOS: usize = 1000;
/// Files LibRaw is known to refuse, which would only count as failures here.
const UNDECODABLE: &str = "nikon-z8-compressed";
/// Decoded raws and export buffers come and go, so memory is compared once it has warmed up.
const WARM_UP: usize = 100;
const ALLOWED_GROWTH_KIB: u64 = 1024 * 1024;

fn edited(index: usize) -> Recipe {
    let mut recipe = Recipe::default();
    match index % 7 {
        0 => {}
        1 => {
            recipe.detail.noise_reduction.luminance = 60.0;
            recipe.detail.noise_reduction.color = 50.0;
        }
        2 => {
            recipe.basic.dehaze = 40.0;
            recipe.basic.clarity = 40.0;
            recipe.basic.texture = 40.0;
        }
        3 => recipe.lens.profile_enabled = true,
        4 => {
            let shape = MaskShape::Radial { center: [0.5, 0.5], radii: [0.3, 0.2], angle: 15.0, feather: 0.6, inverted: false };
            let components = vec![MaskComponent { shape, subtract: false }];
            let adjustments = LocalAdjustments { exposure: 0.8, clarity: 30.0, ..LocalAdjustments::default() };
            recipe.masks.push(Mask { components, adjustments, ..Mask::default() });
        }
        5 => {
            recipe.transform.rotate = 2.0;
            recipe.effects.vignette.amount = -30.0;
            recipe.effects.grain.amount = 30.0;
        }
        _ => recipe.detail.sharpening.amount = 120.0,
    }
    recipe
}

fn resident_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status.lines().find(|line| line.starts_with("VmRSS:")).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[test]
#[ignore = "takes most of an hour on the real GPU"]
fn a_thousand_photo_batch_loses_no_device() {
    let samples = std::env::var("UNLIGHT_SAMPLES").expect("UNLIGHT_SAMPLES");
    let dir = tempfile::tempdir().unwrap();
    let photos = dir.path().join("photos");
    std::fs::create_dir(&photos).unwrap();
    for entry in std::fs::read_dir(samples).unwrap().flatten() {
        let name = entry.file_name();
        if entry.path().is_file() && !name.to_string_lossy().contains(UNDECODABLE) {
            std::fs::copy(entry.path(), photos.join(name)).unwrap();
        }
    }

    let catalog = Arc::new(Catalog::open(&dir.path().join("catalog.sqlite")).unwrap());
    catalog.scan(&photos, &CancelToken::default(), |_| {}).unwrap();
    let originals: Vec<VariantId> = catalog
        .query(&Filter::default(), Sort::default(), Page { offset: 0, limit: 100 })
        .unwrap()
        .into_iter()
        .map(|row| row.variant)
        .collect();
    assert!(originals.len() >= 9, "{originals:?}");
    let variants: Vec<VariantId> = (0..PHOTOS)
        .map(|index| {
            let copy = catalog.create_virtual_copy(originals[index % originals.len()]).unwrap();
            catalog.save_recipe(copy, &edited(index), "soak").unwrap();
            copy
        })
        .collect();

    let output = dir.path().join("exports");
    let mut settings = ExportForm::new(output.display().to_string()).unwrap().settings;
    settings.collision = CollisionPolicy::Overwrite;
    settings.filename_template = "{name}".to_owned();
    let job = ExportJob { variants, settings, directory: output };
    let exporter = Exporter::start(catalog, job, Arc::new(|| {})).unwrap();

    let start = Instant::now();
    let (mut finished, mut device_losses, mut failures) = (0, 0, Vec::new());
    let mut warmed_up = None;
    let mut highest = 0;
    loop {
        match exporter.try_next() {
            Some(ExportEvent::Done { cancelled }) => {
                assert!(!cancelled);
                break;
            }
            Some(ExportEvent::Failed(failure)) => failures.push(failure.message),
            Some(ExportEvent::Finished { device_losses: lost, .. }) => {
                finished += 1;
                device_losses += lost;
                let resident = resident_kib();
                if finished == WARM_UP {
                    warmed_up = Some(resident);
                }
                highest = highest.max(resident);
                if finished % 50 == 0 {
                    let minutes = start.elapsed().as_secs_f32() / 60.0;
                    println!("{finished} done in {minutes:.1} min, {} MiB, {device_losses} lost", resident / 1024);
                }
            }
            Some(ExportEvent::Started { .. }) => {}
            None => thread::sleep(Duration::from_millis(5)),
        }
    }
    let warmed_up = warmed_up.unwrap();
    println!("{finished} in {:?}, peak {} MiB, warmed up at {} MiB", start.elapsed(), highest / 1024, warmed_up / 1024);
    assert_eq!(failures, Vec::<String>::new());
    assert_eq!(finished, PHOTOS);
    assert_eq!(device_losses, 0);
    assert!(highest < warmed_up + ALLOWED_GROWTH_KIB, "{highest} KiB after warming up at {warmed_up} KiB");
}
