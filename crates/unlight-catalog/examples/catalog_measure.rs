//! Prints the catalog's performance figures. Build with `--release` for numbers worth quoting:
//!
//!   UNLIGHT_SAMPLES=~/.cache/unlight-samples cargo run --release -p unlight-catalog --example catalog_measure
//!
//! Everything it writes goes in a temp directory and the samples are only ever read. The
//! 10,000-file tree is built from reflinks, so that directory must be on the same btrfs or XFS
//! filesystem as the samples. Set `TMPDIR` if it isn't.

use std::{
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
use unlight_catalog::{
    CancelToken, Catalog, Filter, Fingerprint, Page, PreviewCache, PreviewSize, PreviewSource, Sort,
};
use unlight_raw::{load_preview_sized, read_metadata};

const TREE_FILES: usize = 10_000;
const METADATA_ROUNDS: usize = 50;
const PREVIEW_ROUNDS: usize = 7;

fn sample_files(directory: &Path) -> Vec<PathBuf> {
    let entries = fs::read_dir(directory).expect("read UNLIGHT_SAMPLES");
    let mut files: Vec<PathBuf> = entries
        .map(|entry| entry.expect("list samples").path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    files
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

fn repeat(rounds: usize, mut work: impl FnMut()) -> Duration {
    let times = (0..rounds).map(|_| {
        let started = Instant::now();
        work();
        started.elapsed()
    });
    median(times.collect())
}

fn measure_metadata(files: &[PathBuf]) {
    println!(
        "metadata read: first (cold if the page cache was dropped), then median of {METADATA_ROUNDS}"
    );
    let (mut first_total, mut warm_total) = (Duration::ZERO, Duration::ZERO);
    for file in files {
        let first = repeat(1, || {
            read_metadata(file).expect("read metadata");
        });
        let warm = repeat(METADATA_ROUNDS, || {
            read_metadata(file).expect("read metadata");
        });
        first_total += first;
        warm_total += warm;
        let name = file.file_name().unwrap_or_default().to_string_lossy();
        println!("  {name:28} first {:7.3} ms, warm {:7.3} ms", millis(first), millis(warm));
    }
    let mean = |total| millis(total) / files.len() as f64;
    println!("  mean: first {:.3} ms, warm {:.3} ms", mean(first_total), mean(warm_total));
}

/// Fills `root` with clones of the samples, each made distinct by a few appended bytes, so the
/// tree holds as many different raws as a real library at almost no disk cost. A clone has its
/// own page cache, which makes the first scan a true cold read. `--reflink=always` fails
/// rather than fall back to a full copy, which would need hundreds of gigabytes.
fn build_tree(root: &Path, files: &[PathBuf]) {
    for index in 0..TREE_FILES.div_ceil(files.len()) {
        let folder = root.join(format!("{index:04}"));
        fs::create_dir_all(&folder).expect("create tree folder");
        let cloned = Command::new("cp")
            .arg("--reflink=always")
            .arg("--target-directory")
            .arg(&folder)
            .args(files)
            .status()
            .expect("run cp");
        assert!(cloned.success(), "reflink failed, point TMPDIR at the samples' filesystem");
        for file in files {
            let clone = folder.join(file.file_name().unwrap_or_default());
            let mut clone = OpenOptions::new().append(true).open(clone).expect("open clone");
            clone.write_all(&(index as u64).to_le_bytes()).expect("mark clone");
        }
    }
}

fn measure_scan(work: &Path, files: &[PathBuf]) {
    let root = work.join("tree");
    build_tree(&root, files);
    let catalog = Catalog::open(&work.join("catalog.db")).expect("open catalog");
    let cancel = CancelToken::default();

    let started = Instant::now();
    let report = catalog.scan(&root, &cancel, |_| {}).expect("scan");
    let first = started.elapsed();
    println!(
        "first scan, cold: {} real raws in {:.2} s, {:.0} files/s ({} added, {} unsupported)",
        report.discovered,
        first.as_secs_f64(),
        report.discovered as f64 / first.as_secs_f64(),
        report.added,
        report.unsupported,
    );

    let started = Instant::now();
    let report = catalog.scan(&root, &cancel, |_| {}).expect("rescan");
    println!("unchanged rescan: {} files in {:.0} ms", report.unchanged, millis(started.elapsed()));

    let page = Page { offset: 0, limit: 200 };
    let time = repeat(9, || {
        catalog.query(&Filter::default(), Sort::default(), page).expect("query");
    });
    println!("first page of the 10k grid: {:.2} ms", millis(time));
}

fn measure_preview(work: &Path, file: &Path) {
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let cache_dir = work.join("preview-cache");
    let _ = fs::remove_dir_all(&cache_dir);
    let cache = PreviewCache::open(&cache_dir, u64::MAX).expect("open cache");
    let source = PreviewSource { fingerprint: Fingerprint([1; 16]), path: file, orientation: 0 };

    let extract = repeat(PREVIEW_ROUNDS, || {
        load_preview_sized(file, PreviewSize::Loupe.long_edge()).expect("extract preview");
    });
    let render = |size| {
        repeat(PREVIEW_ROUNDS, || {
            let _ = fs::remove_dir_all(&cache_dir);
            let cache = PreviewCache::open(&cache_dir, u64::MAX).expect("open cache");
            cache.get(&source, size).expect("render");
        })
    };
    let grid_miss = render(PreviewSize::Grid);
    let loupe_miss = render(PreviewSize::Loupe);

    let image = cache.get(&source, PreviewSize::Loupe).expect("render");
    cache.get(&source, PreviewSize::Grid).expect("render");
    let loupe_hit = repeat(PREVIEW_ROUNDS, || {
        cache.get(&source, PreviewSize::Loupe).expect("hit");
    });
    let grid_hit = repeat(PREVIEW_ROUNDS, || {
        cache.get(&source, PreviewSize::Grid).expect("hit");
    });
    println!(
        "  {name:28} {:5}x{:<5} extract {:6.1} | loupe miss {:6.1} hit {:5.1} | grid miss {:6.1} hit {:5.1} ms",
        image.width,
        image.height,
        millis(extract),
        millis(loupe_miss),
        millis(loupe_hit),
        millis(grid_miss),
        millis(grid_hit),
    );
}

fn main() {
    let samples = PathBuf::from(env::var_os("UNLIGHT_SAMPLES").expect("set UNLIGHT_SAMPLES"));
    let files = sample_files(&samples);
    let work = env::temp_dir().join(format!("unlight-measure-{}", std::process::id()));
    fs::create_dir_all(&work).expect("create work directory");

    measure_metadata(&files);
    measure_scan(&work, &files);
    println!("previews, median of {PREVIEW_ROUNDS}:");
    for file in &files {
        measure_preview(&work, file);
    }
    fs::remove_dir_all(&work).expect("remove work directory");
}
