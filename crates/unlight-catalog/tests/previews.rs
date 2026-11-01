mod common;

use common::{SyntheticRaw, TestCatalog};
use std::{
    fs,
    path::Path,
    sync::{Arc, Barrier},
    thread,
};
use unlight_catalog::{
    Error, Filter, Fingerprint, Page, PreviewCache, PreviewSize, PreviewSource, RgbaImage, Sort,
};

const NO_LIMIT: u64 = u64::MAX;

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self { dir: tempfile::tempdir().unwrap() }
    }

    fn cache(&self, capacity: u64) -> PreviewCache {
        PreviewCache::open(&self.dir.path().join("cache"), capacity).unwrap()
    }

    fn raw(&self, name: &str, raw: SyntheticRaw) -> std::path::PathBuf {
        let path = self.dir.path().join(name);
        raw.write(&path);
        path
    }

    fn cache_files(&self) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        let mut folders = vec![self.dir.path().join("cache")];
        while let Some(folder) = folders.pop() {
            for entry in fs::read_dir(folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files
    }
}

fn source(path: &Path, seed: u8, orientation: i32) -> PreviewSource<'_> {
    PreviewSource { fingerprint: Fingerprint([seed; 16]), path, orientation }
}

fn with_preview(width: u32, height: u32) -> SyntheticRaw {
    SyntheticRaw { preview: Some((width, height)), ..SyntheticRaw::default() }
}

fn pixel(image: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
    let start = (y * image.width + x) as usize * 4;
    image.pixels[start..start + 4].try_into().unwrap()
}

fn is_red(pixel: [u8; 4]) -> bool {
    pixel[0] > 150 && pixel[2] < 100
}

fn is_blue(pixel: [u8; 4]) -> bool {
    pixel[2] > 150 && pixel[0] < 100
}

#[test]
fn a_jpeg_preview_comes_back_as_rgba_at_the_right_size() {
    let fixture = Fixture::new();
    let cache = fixture.cache(NO_LIMIT);
    let path = fixture.raw("a.dng", with_preview(4000, 3000));

    let grid = cache.get(&source(&path, 1, 0), PreviewSize::Grid).unwrap();
    assert_eq!((grid.width, grid.height), (512, 384));
    assert_eq!(grid.pixels.len(), 512 * 384 * 4);
    assert!(grid.pixels.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255));
    assert!(is_red(pixel(&grid, 100, 100)));
    assert!(is_blue(pixel(&grid, 400, 300)));

    let loupe = cache.get(&source(&path, 1, 0), PreviewSize::Loupe).unwrap();
    assert_eq!((loupe.width, loupe.height), (2048, 1536));
}

#[test]
fn a_small_preview_is_not_enlarged() {
    let fixture = Fixture::new();
    let cache = fixture.cache(NO_LIMIT);
    let path = fixture.raw("a.dng", with_preview(320, 240));
    let loupe = cache.get(&source(&path, 1, 0), PreviewSize::Loupe).unwrap();
    assert_eq!((loupe.width, loupe.height), (320, 240));
}

#[test]
fn orientation_is_applied_before_the_preview_is_stored() {
    let fixture = Fixture::new();
    let cache = fixture.cache(NO_LIMIT);
    let path = fixture.raw("a.dng", with_preview(800, 600));

    let clockwise = cache.get(&source(&path, 1, 6), PreviewSize::Grid).unwrap();
    assert_eq!((clockwise.width, clockwise.height), (384, 512));
    assert!(is_red(pixel(&clockwise, 300, 100)), "the top-left quadrant moves to the top right");
    assert!(is_blue(pixel(&clockwise, 50, 100)));

    let counter = cache.get(&source(&path, 2, 5), PreviewSize::Grid).unwrap();
    assert!(is_red(pixel(&counter, 100, 400)), "the top-left quadrant moves to the bottom left");

    let upside_down = cache.get(&source(&path, 3, 3), PreviewSize::Grid).unwrap();
    assert_eq!((upside_down.width, upside_down.height), (512, 384));
    assert!(
        is_red(pixel(&upside_down, 400, 300)),
        "the top-left quadrant moves to the bottom right"
    );

    let reloaded =
        fixture.cache(NO_LIMIT).get_cached(Fingerprint([1; 16]), PreviewSize::Grid).unwrap();
    assert_eq!((reloaded.width, reloaded.height), (384, 512));
}

#[test]
fn the_orientation_from_a_real_import_reaches_the_preview() {
    let test = TestCatalog::new();
    SyntheticRaw { orientation: 6, ..with_preview(800, 600) }.write(&test.path("a.dng"));
    test.scan();
    let rows = test
        .catalog
        .query(&Filter::default(), Sort::default(), Page { offset: 0, limit: 1 })
        .unwrap();
    assert_eq!(rows[0].orientation, 6);

    let cache = PreviewCache::open(&test.dir.path().join("cache"), NO_LIMIT).unwrap();
    let grid = cache.get(&rows[0].preview_source(), PreviewSize::Grid).unwrap();
    assert_eq!((grid.width, grid.height), (384, 512));
}

#[test]
fn a_second_request_is_served_from_disk_without_the_raw() {
    let fixture = Fixture::new();
    let cache = fixture.cache(NO_LIMIT);
    let path = fixture.raw("a.dng", with_preview(1000, 750));
    let first = cache.get(&source(&path, 1, 0), PreviewSize::Grid).unwrap();

    fs::remove_file(&path).unwrap();
    let second = cache.get(&source(&path, 1, 0), PreviewSize::Grid).unwrap();
    assert_eq!((second.width, second.height), (first.width, first.height));
}

#[test]
fn get_cached_never_renders() {
    let fixture = Fixture::new();
    let cache = fixture.cache(NO_LIMIT);
    let path = fixture.raw("a.dng", with_preview(1000, 750));
    let key = Fingerprint([1; 16]);
    assert!(cache.get_cached(key, PreviewSize::Grid).is_none());
    cache.get(&source(&path, 1, 0), PreviewSize::Grid).unwrap();
    assert!(cache.get_cached(key, PreviewSize::Grid).is_some());
    assert!(cache.get_cached(key, PreviewSize::Loupe).is_none(), "sizes are separate entries");
}

#[test]
fn entries_survive_reopening_the_cache() {
    let fixture = Fixture::new();
    let path = fixture.raw("a.dng", with_preview(1000, 750));
    let first = fixture.cache(NO_LIMIT);
    first.get(&source(&path, 1, 0), PreviewSize::Grid).unwrap();
    let bytes = first.total_bytes();
    assert!(bytes > 0);
    drop(first);

    let reopened = fixture.cache(NO_LIMIT);
    assert_eq!(reopened.total_bytes(), bytes);
    assert!(reopened.get_cached(Fingerprint([1; 16]), PreviewSize::Grid).is_some());
}

#[test]
fn a_corrupt_entry_is_rebuilt() {
    let fixture = Fixture::new();
    let cache = fixture.cache(NO_LIMIT);
    let path = fixture.raw("a.dng", with_preview(1000, 750));
    cache.get(&source(&path, 1, 0), PreviewSize::Grid).unwrap();

    let entries = fixture.cache_files();
    assert_eq!(entries.len(), 1);
    fs::write(&entries[0], b"not a jpeg").unwrap();
    assert!(cache.get_cached(Fingerprint([1; 16]), PreviewSize::Grid).is_none());
    assert!(!entries[0].exists(), "the bad file is removed");

    let rebuilt = cache.get(&source(&path, 1, 0), PreviewSize::Grid).unwrap();
    assert_eq!((rebuilt.width, rebuilt.height), (512, 384));
}

#[test]
fn a_truncated_entry_is_rebuilt_rather_than_shown() {
    let fixture = Fixture::new();
    let cache = fixture.cache(NO_LIMIT);
    let path = fixture.raw("a.dng", with_preview(1000, 750));
    cache.get(&source(&path, 1, 0), PreviewSize::Grid).unwrap();
    let entries = fixture.cache_files();
    let bytes = fs::read(&entries[0]).unwrap();
    fs::write(&entries[0], &bytes[..bytes.len() / 2]).unwrap();
    assert!(cache.get_cached(Fingerprint([1; 16]), PreviewSize::Grid).is_none());
}

#[test]
fn an_entry_deleted_behind_our_back_is_a_miss() {
    let fixture = Fixture::new();
    let cache = fixture.cache(NO_LIMIT);
    let path = fixture.raw("a.dng", with_preview(1000, 750));
    cache.get(&source(&path, 1, 0), PreviewSize::Grid).unwrap();
    fs::remove_file(&fixture.cache_files()[0]).unwrap();
    assert!(cache.get_cached(Fingerprint([1; 16]), PreviewSize::Grid).is_none());
    assert_eq!(cache.total_bytes(), 0);
    cache.get(&source(&path, 1, 0), PreviewSize::Grid).unwrap();
}

#[test]
fn errors_for_bad_sources_and_no_entry_left_behind() {
    let fixture = Fixture::new();
    let cache = fixture.cache(NO_LIMIT);
    let garbage = fixture.dir.path().join("garbage.arw");
    fs::write(&garbage, b"not a raw").unwrap();
    let missing = fixture.dir.path().join("missing.arw");
    let no_preview = fixture.raw("none.dng", SyntheticRaw::default());

    assert!(matches!(cache.get(&source(&garbage, 1, 0), PreviewSize::Grid), Err(Error::Raw(_))));
    assert!(matches!(cache.get(&source(&missing, 2, 0), PreviewSize::Grid), Err(Error::Raw(_))));
    assert!(cache.get(&source(&no_preview, 3, 0), PreviewSize::Grid).is_err());
    assert_eq!(cache.total_bytes(), 0);
    assert!(fixture.cache_files().is_empty());
}

#[test]
fn the_size_cap_evicts_the_least_recently_used() {
    let fixture = Fixture::new();
    let path = fixture.raw("a.dng", with_preview(1000, 750));
    let unlimited = fixture.cache(NO_LIMIT);
    unlimited.get(&source(&path, 0, 0), PreviewSize::Grid).unwrap();
    let entry_bytes = unlimited.total_bytes();
    drop(unlimited);
    fs::remove_dir_all(fixture.dir.path().join("cache")).unwrap();

    let cache = fixture.cache(entry_bytes * 3 + entry_bytes / 2);
    for seed in 1..=3 {
        cache.get(&source(&path, seed, 0), PreviewSize::Grid).unwrap();
    }
    assert!(
        cache.get_cached(Fingerprint([1; 16]), PreviewSize::Grid).is_some(),
        "using 1 makes 2 the oldest"
    );
    cache.get(&source(&path, 4, 0), PreviewSize::Grid).unwrap();

    assert!(cache.get_cached(Fingerprint([2; 16]), PreviewSize::Grid).is_none());
    assert!(cache.get_cached(Fingerprint([1; 16]), PreviewSize::Grid).is_some());
    assert!(cache.get_cached(Fingerprint([3; 16]), PreviewSize::Grid).is_some());
    assert!(cache.get_cached(Fingerprint([4; 16]), PreviewSize::Grid).is_some());
    assert!(cache.total_bytes() <= entry_bytes * 3 + entry_bytes / 2);
    assert_eq!(fixture.cache_files().len(), 3);
}

#[test]
fn recency_survives_a_restart_through_file_times() {
    let fixture = Fixture::new();
    let path = fixture.raw("a.dng", with_preview(1000, 750));
    let first = fixture.cache(NO_LIMIT);
    for seed in 1..=3 {
        first.get(&source(&path, seed, 0), PreviewSize::Grid).unwrap();
        thread::sleep(std::time::Duration::from_millis(10));
    }
    first.get_cached(Fingerprint([1; 16]), PreviewSize::Grid).unwrap();
    let total = first.total_bytes();
    drop(first);

    let reopened = fixture.cache(total - 1);
    assert!(
        reopened.get_cached(Fingerprint([2; 16]), PreviewSize::Grid).is_none(),
        "2 was the oldest"
    );
    assert!(reopened.get_cached(Fingerprint([1; 16]), PreviewSize::Grid).is_some());
    assert!(reopened.get_cached(Fingerprint([3; 16]), PreviewSize::Grid).is_some());
}

#[test]
fn a_cap_too_small_for_one_entry_still_returns_the_image() {
    let fixture = Fixture::new();
    let cache = fixture.cache(1);
    let path = fixture.raw("a.dng", with_preview(1000, 750));
    let image = cache.get(&source(&path, 1, 0), PreviewSize::Grid).unwrap();
    assert_eq!(image.width, 512);
    assert_eq!(fixture.cache_files().len(), 1, "the newest entry is spared");
}

#[test]
fn leftover_temp_files_are_cleaned_up_on_open() {
    let fixture = Fixture::new();
    let path = fixture.raw("a.dng", with_preview(1000, 750));
    fixture.cache(NO_LIMIT).get(&source(&path, 1, 0), PreviewSize::Grid).unwrap();
    let entry = &fixture.cache_files()[0];
    let leftover = entry.with_file_name(".crashed.tmp");
    fs::write(&leftover, b"partial").unwrap();
    let cache = fixture.cache(NO_LIMIT);
    assert!(!leftover.exists());
    assert!(cache.get_cached(Fingerprint([1; 16]), PreviewSize::Grid).is_some());
}

#[test]
fn many_threads_asking_for_one_photo_all_get_the_same_image() {
    let fixture = Fixture::new();
    let cache = Arc::new(fixture.cache(NO_LIMIT));
    let path = Arc::new(fixture.raw("a.dng", with_preview(1600, 1200)));
    const THREADS: usize = 32;
    let barrier = Arc::new(Barrier::new(THREADS));
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let (cache, path, barrier) =
                (Arc::clone(&cache), Arc::clone(&path), Arc::clone(&barrier));
            thread::spawn(move || {
                barrier.wait();
                cache.get(&source(&path, 1, 0), PreviewSize::Grid).unwrap()
            })
        })
        .collect();
    let images: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
    assert!(images.iter().all(|image| (image.width, image.height) == (512, 384)));
    assert_eq!(fixture.cache_files().len(), 1);
    assert!(fixture.cache_files().iter().all(|file| !file.to_string_lossy().ends_with(".tmp")));
}

#[test]
fn concurrent_reads_writes_and_evictions_stay_consistent() {
    let fixture = Fixture::new();
    let path = Arc::new(fixture.raw("a.dng", with_preview(800, 600)));
    let probe = fixture.cache(NO_LIMIT);
    probe.get(&source(&path, 0, 0), PreviewSize::Grid).unwrap();
    let entry_bytes = probe.total_bytes();
    drop(probe);
    fs::remove_dir_all(fixture.dir.path().join("cache")).unwrap();

    let capacity = entry_bytes * 8;
    let cache = Arc::new(fixture.cache(capacity));
    let handles: Vec<_> = (0..16u8)
        .map(|worker| {
            let (cache, path) = (Arc::clone(&cache), Arc::clone(&path));
            thread::spawn(move || {
                for round in 0..30u8 {
                    let seed = (worker.wrapping_mul(7).wrapping_add(round)) % 24;
                    let image = cache.get(&source(&path, seed, 0), PreviewSize::Grid).unwrap();
                    assert_eq!((image.width, image.height), (512, 384));
                    assert!(is_red(pixel(&image, 100, 100)), "never a torn image");
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert!(cache.total_bytes() <= capacity + entry_bytes);
    let on_disk: u64 =
        fixture.cache_files().iter().map(|file| fs::metadata(file).unwrap().len()).sum();
    assert_eq!(on_disk, cache.total_bytes(), "the index matches the disk");
}

#[test]
fn nothing_is_written_outside_the_given_directory() {
    let fixture = Fixture::new();
    let cache = fixture.cache(NO_LIMIT);
    let path = fixture.raw("a.dng", with_preview(1000, 750));
    cache.get(&source(&path, 1, 0), PreviewSize::Loupe).unwrap();
    let source_dir_files: Vec<_> = fs::read_dir(fixture.dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(source_dir_files.len(), 2, "{source_dir_files:?}");
}
