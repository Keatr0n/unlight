//! Real camera files. These read the directory named by `UNLIGHT_SAMPLES` and skip when it's
//! unset, as it is under `nix flake check`. Nothing is written into that directory: scans of
//! it use a temp catalog, and sidecar tests work on symlinks in a temp folder.

mod common;

use common::{TestCatalog, sample_files, samples_dir};
use std::{collections::BTreeMap, fs, os::unix::fs::symlink, path::PathBuf};
use unlight_catalog::{
    CancelToken, Catalog, Filter, Page, PhotoStatus, PreviewCache, PreviewSize, Sort, xmp,
};
use unlight_raw::read_metadata;

const ALL: Page = Page { offset: 0, limit: 1000 };

fn linked_samples(test: &TestCatalog) -> Option<Vec<PathBuf>> {
    let samples = samples_dir()?;
    fs::create_dir_all(test.root()).unwrap();
    let files = sample_files(&samples);
    for file in &files {
        symlink(file, test.root().join(file.file_name().unwrap())).unwrap();
    }
    Some(files)
}

#[test]
fn metadata_reads_on_every_sample() {
    let Some(samples) = samples_dir() else { return };
    for path in sample_files(&samples) {
        let info = read_metadata(&path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
        assert!(!info.metadata.make.is_empty(), "{path:?}");
        assert!(info.width > 1000 && info.height > 1000, "{path:?}");
        assert!(info.metadata.timestamp > 0, "{path:?}");
    }
}

#[test]
fn every_sample_imports_with_metadata() {
    let test = TestCatalog::new();
    let Some(files) = linked_samples(&test) else { return };
    let report = test.scan();
    assert_eq!(report.added, files.len());
    assert_eq!(report.unsupported, 0);
    assert!(report.failures.is_empty(), "{:?}", report.failures);

    let rows = test.catalog.query(&Filter::default(), Sort::default(), ALL).unwrap();
    let cameras: BTreeMap<String, String> = rows
        .iter()
        .map(|row| {
            let photo = test.catalog.photo(row.photo).unwrap();
            assert_eq!(photo.status, PhotoStatus::Available);
            assert!(photo.iso.is_some(), "{:?}", photo.path);
            (photo.path.file_name().unwrap().to_string_lossy().into_owned(), photo.camera)
        })
        .collect();
    assert_eq!(cameras["sony-a7rm4.ARW"], "Sony ILCE-7RM4");
    assert_eq!(cameras["fuji-xt4.RAF"], "Fujifilm X-T4");
    assert_eq!(cameras["canon-eosr.CR3"], "Canon EOS R");
    assert_eq!(cameras["pixel4xl.dng"], "Google Pixel 4 XL");
    assert_eq!(cameras["gh5.RW2"], "Panasonic DC-GH5");
    assert_eq!(cameras["nikon-z8-compressed.NEF"], "Nikon Z 8");
    assert_eq!(cameras["hasselblad-h6d.fff"], "Hasselblad H6D-100cMS");

    assert_eq!(test.scan().unchanged, files.len());
}

#[test]
fn every_sample_gives_previews_of_both_sizes() {
    let test = TestCatalog::new();
    let Some(_) = linked_samples(&test) else { return };
    test.scan();
    let cache = PreviewCache::open(&test.dir.path().join("cache"), 1 << 30).unwrap();
    for row in test.catalog.query(&Filter::default(), Sort::default(), ALL).unwrap() {
        for size in [PreviewSize::Grid, PreviewSize::Loupe] {
            let image = cache
                .get(&row.preview_source(), size)
                .unwrap_or_else(|error| panic!("{:?}: {error}", row.path));
            assert!(image.width.max(image.height) <= size.long_edge(), "{:?}", row.path);
            assert_eq!(image.pixels.len(), (image.width * image.height * 4) as usize);
            let cached = cache.get_cached(row.fingerprint, size).unwrap();
            assert_eq!((cached.width, cached.height), (image.width, image.height));
        }
    }
}

#[test]
fn sidecars_land_beside_the_links_not_in_the_samples_directory() {
    let test = TestCatalog::new();
    let Some(files) = linked_samples(&test) else { return };
    test.scan();
    let rows = test.catalog.query(&Filter::default(), Sort::default(), ALL).unwrap();
    test.catalog.set_rating(&[rows[0].variant], 4).unwrap();
    let sidecar = test.catalog.write_sidecar(rows[0].photo).unwrap();
    assert!(sidecar.starts_with(test.root().canonicalize().unwrap()), "{sidecar:?}");
    assert_eq!(xmp::read_sidecar(&sidecar).unwrap().rating, Some(4));
    let samples = samples_dir().unwrap();
    assert_eq!(sample_files(&samples), files, "the samples directory is unchanged");
}

#[test]
fn a_rescan_of_the_samples_directory_itself_is_read_only() {
    let Some(samples) = samples_dir() else { return };
    let dir = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(&dir.path().join("catalog.db")).unwrap();
    let before = sample_files(&samples);
    catalog.scan(&samples, &CancelToken::default(), |_| {}).unwrap();
    assert_eq!(sample_files(&samples), before);
}
