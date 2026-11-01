mod common;

use common::{SyntheticRaw, TestCatalog};
use rusqlite::Connection;
use std::{
    ffi::OsStr,
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    },
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime},
};
use unlight_catalog::{
    CancelToken, Catalog, Error, Filter, Flag, Label, Page, PhotoId, PhotoStatus, ScanPhase, Sort,
    VariantId,
};
use unlight_recipe::Recipe;

const ALL: Page = Page { offset: 0, limit: 10_000 };

fn rows(test: &TestCatalog) -> Vec<unlight_catalog::LibraryRow> {
    test.catalog.query(&Filter::default(), Sort::default(), ALL).unwrap()
}

fn only_photo(test: &TestCatalog) -> (PhotoId, VariantId) {
    let rows = rows(test);
    assert_eq!(rows.len(), 1);
    (rows[0].photo, rows[0].variant)
}

fn age_file(path: &Path) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(3600)).unwrap();
}

#[test]
fn imports_a_raw_with_its_metadata() {
    let test = TestCatalog::new();
    SyntheticRaw { iso: 800, ..SyntheticRaw::default() }.write(&test.path("a.dng"));
    let report = test.scan();
    assert_eq!((report.discovered, report.added, report.unsupported), (1, 1, 0));
    assert!(report.failures.is_empty(), "{:?}", report.failures);

    let (id, _) = only_photo(&test);
    let photo = test.catalog.photo(id).unwrap();
    assert_eq!(photo.status, PhotoStatus::Available);
    assert_eq!(photo.camera, "Acme Ranger");
    assert_eq!(photo.lens, "Acme 50mm");
    assert_eq!(photo.iso, Some(800.0));
    assert_eq!(photo.aperture, Some(2.8));
    assert_eq!(photo.focal_length, Some(50.0));
    assert!((photo.shutter.unwrap() - 0.004).abs() < 1e-6);
    assert_eq!((photo.width, photo.height), (64, 48));
    assert!(photo.capture_time.is_some());
    assert_eq!(photo.path, test.path("a.dng").canonicalize().unwrap());
}

#[test]
fn finds_raws_recursively_by_extension_in_any_case() {
    let test = TestCatalog::new();
    let raw = SyntheticRaw::default();
    for (seed, name) in ["a.dng", "sub/b.DNG", "sub/deep/er/c.Dng"].iter().enumerate() {
        SyntheticRaw { seed: seed as u32, ..raw.clone() }.write(&test.path(name));
    }
    fs::write(test.path("notes.txt"), "not a photo").unwrap();
    fs::write(test.path("sub/photo.jpg"), "not a raw").unwrap();
    assert_eq!(test.scan().discovered, 3);
}

#[test]
fn hidden_folders_and_files_are_skipped() {
    let test = TestCatalog::new();
    SyntheticRaw::seeded(1).write(&test.path("a.dng"));
    SyntheticRaw::seeded(2).write(&test.path(".Trash/b.dng"));
    SyntheticRaw::seeded(3).write(&test.path("._c.dng"));
    assert_eq!(test.scan().added, 1);
}

#[test]
fn a_new_photo_has_one_master_variant() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("a.dng"));
    test.scan();
    let (photo, _) = only_photo(&test);
    let variants = test.catalog.variants_of(photo).unwrap();
    assert_eq!(variants.len(), 1);
    assert_eq!(variants[0].copy_name, None);
    assert_eq!(variants[0].recipe, Recipe::default());
}

#[test]
fn folders_form_a_tree_with_counts() {
    let test = TestCatalog::new();
    SyntheticRaw::seeded(1).write(&test.path("2024/march/a.dng"));
    SyntheticRaw::seeded(2).write(&test.path("2024/march/b.dng"));
    SyntheticRaw::seeded(3).write(&test.path("2024/c.dng"));
    test.scan();
    let folders = test.catalog.folders().unwrap();
    let root = test.root().canonicalize().unwrap();
    let find = |relative: &str| {
        folders.iter().find(|folder| folder.path == root.join(relative)).unwrap().clone()
    };
    let year = find("2024");
    let month = find("2024/march");
    assert_eq!(month.parent, Some(year.id));
    assert_eq!((month.photo_count, year.photo_count), (2, 1));
    assert_eq!(find("").photo_count, 0);
    assert_eq!(year.parent, Some(find("").id));
}

#[test]
fn scanning_a_parent_later_links_existing_subfolders() {
    let test = TestCatalog::new();
    SyntheticRaw::seeded(1).write(&test.path("a/b/one.dng"));
    SyntheticRaw::seeded(2).write(&test.path("a/two.dng"));
    test.catalog.scan(&test.path("a/b"), &CancelToken::default(), |_| {}).unwrap();
    let scan_of_parent =
        test.catalog.scan(&test.path("a"), &CancelToken::default(), |_| {}).unwrap();
    assert_eq!((scan_of_parent.added, scan_of_parent.unchanged), (1, 1));

    let folders = test.catalog.folders().unwrap();
    let parent = folders.iter().find(|folder| folder.path.ends_with("a")).unwrap();
    let child = folders.iter().find(|folder| folder.path.ends_with("a/b")).unwrap();
    assert_eq!(child.parent, Some(parent.id));
}

#[test]
fn a_rescan_leaves_unchanged_files_alone() {
    let test = TestCatalog::new();
    for seed in 0..5 {
        SyntheticRaw::seeded(seed).write(&test.path(&format!("{seed}.dng")));
    }
    test.scan();
    let (_, variant) = (0, rows(&test)[0].variant);
    test.catalog.set_rating(&[variant], 4).unwrap();

    let report = test.scan();
    assert_eq!((report.unchanged, report.added, report.changed), (5, 0, 0));
    assert_eq!(test.catalog.variant(variant).unwrap().rating, 4);
}

#[test]
fn a_rescan_does_not_open_unchanged_files() {
    let test = TestCatalog::new();
    let path = test.path("a.dng");
    SyntheticRaw::default().write(&path);
    test.scan();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&path).is_ok() {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        return; // Running as root, where permissions don't stop the read.
    }
    let report = test.scan();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(report.unchanged, 1);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
}

#[test]
fn a_changed_file_is_read_again_and_keeps_its_edits() {
    let test = TestCatalog::new();
    let path = test.path("a.dng");
    SyntheticRaw { iso: 100, ..SyntheticRaw::default() }.write(&path);
    test.scan();
    let (photo, variant) = only_photo(&test);
    let before = test.catalog.photo(photo).unwrap();
    let mut recipe = Recipe::default();
    recipe.basic.exposure = 1.0;
    test.catalog.save_recipe(variant, &recipe, "Exposure").unwrap();

    SyntheticRaw { iso: 3200, seed: 9, ..SyntheticRaw::default() }.write(&path);
    age_file(&path);
    let report = test.scan();
    assert_eq!((report.changed, report.added), (1, 0));

    let after = test.catalog.photo(photo).unwrap();
    assert_eq!(after.iso, Some(3200.0));
    assert_ne!(after.fingerprint, before.fingerprint);
    assert_eq!(test.catalog.variant(variant).unwrap().recipe, recipe);
}

#[test]
fn a_moved_file_keeps_its_photo_and_edits() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("old/a.dng"));
    test.scan();
    let (photo, variant) = only_photo(&test);
    test.catalog.set_rating(&[variant], 5).unwrap();
    let mut recipe = Recipe::default();
    recipe.basic.contrast = 20.0;
    test.catalog.save_recipe(variant, &recipe, "Contrast").unwrap();

    fs::create_dir_all(test.path("new/place")).unwrap();
    fs::rename(test.path("old/a.dng"), test.path("new/place/renamed.dng")).unwrap();
    let report = test.scan();
    assert_eq!((report.moved, report.added, report.missing), (1, 0, 0));

    let (moved_photo, moved_variant) = only_photo(&test);
    assert_eq!((moved_photo, moved_variant), (photo, variant));
    let info = test.catalog.photo(photo).unwrap();
    assert_eq!(info.status, PhotoStatus::Available);
    assert!(info.path.ends_with("new/place/renamed.dng"));
    let saved = test.catalog.variant(variant).unwrap();
    assert_eq!((saved.rating, saved.recipe), (5, recipe));
}

#[test]
fn a_file_moved_in_from_outside_the_scan_root_is_recognised() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("a/one.dng"));
    test.catalog.scan(&test.path("a"), &CancelToken::default(), |_| {}).unwrap();
    let (photo, _) = only_photo(&test);

    fs::create_dir_all(test.path("b")).unwrap();
    fs::rename(test.path("a/one.dng"), test.path("b/one.dng")).unwrap();
    let report = test.catalog.scan(&test.path("b"), &CancelToken::default(), |_| {}).unwrap();
    assert_eq!((report.moved, report.added), (1, 0));
    assert!(test.catalog.photo(photo).unwrap().path.ends_with("b/one.dng"));
}

#[test]
fn a_copy_of_a_file_is_a_new_photo_not_a_move() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("a.dng"));
    test.scan();
    fs::copy(test.path("a.dng"), test.path("copy.dng")).unwrap();
    let report = test.scan();
    assert_eq!((report.added, report.moved, report.missing), (1, 0, 0));
    assert_eq!(rows(&test).len(), 2);
}

#[test]
fn a_copy_of_a_file_outside_the_scan_root_is_a_new_photo_not_a_move() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("a/one.dng"));
    test.catalog.scan(&test.path("a"), &CancelToken::default(), |_| {}).unwrap();
    let (photo, _) = only_photo(&test);

    fs::create_dir_all(test.path("b")).unwrap();
    fs::copy(test.path("a/one.dng"), test.path("b/one.dng")).unwrap();
    let report = test.catalog.scan(&test.path("b"), &CancelToken::default(), |_| {}).unwrap();
    assert_eq!((report.added, report.moved), (1, 0));
    assert!(test.catalog.photo(photo).unwrap().path.ends_with("a/one.dng"));
}

#[test]
fn a_deleted_file_is_marked_missing_and_stays_in_the_catalog() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("a.dng"));
    test.scan();
    let (photo, variant) = only_photo(&test);
    test.catalog.set_flag(&[variant], Flag::Pick).unwrap();

    fs::remove_file(test.path("a.dng")).unwrap();
    assert_eq!(test.scan().missing, 1);
    assert_eq!(test.catalog.photo(photo).unwrap().status, PhotoStatus::Missing);
    assert_eq!(test.catalog.variant(variant).unwrap().flag, Flag::Pick);

    let missing = Filter { status: Some(PhotoStatus::Missing), ..Filter::default() };
    assert_eq!(test.catalog.count(&missing).unwrap(), 1);
    assert_eq!(test.scan().missing, 0, "already marked");
}

#[test]
fn a_missing_file_coming_back_is_restored() {
    let test = TestCatalog::new();
    let path = test.path("a.dng");
    SyntheticRaw::default().write(&path);
    test.scan();
    let (photo, _) = only_photo(&test);
    let holding = test.dir.path().join("holding.dng");
    fs::rename(&path, &holding).unwrap();
    test.scan();

    fs::rename(&holding, &path).unwrap();
    let report = test.scan();
    assert_eq!((report.restored, report.added, report.moved), (1, 0, 0));
    assert_eq!(test.catalog.photo(photo).unwrap().status, PhotoStatus::Available);
}

#[test]
fn a_file_libraw_cannot_open_is_unsupported_not_fatal() {
    let test = TestCatalog::new();
    SyntheticRaw::seeded(1).write(&test.path("good.dng"));
    fs::write(test.path("garbage.arw"), b"this is definitely not a raw file").unwrap();
    fs::write(test.path("empty.cr2"), b"").unwrap();
    let mut truncated = SyntheticRaw::seeded(2).bytes();
    truncated.truncate(12);
    fs::write(test.path("truncated.nef"), truncated).unwrap();
    let mut noise: Vec<u8> =
        (0..200_000u32).flat_map(|n| n.wrapping_mul(2_654_435_761).to_le_bytes()).collect();
    noise[..4].copy_from_slice(b"II*\0");
    fs::write(test.path("noise.raf"), noise).unwrap();

    let report = test.scan();
    assert_eq!((report.added, report.unsupported), (4 + 1, 4));
    assert!(report.failures.is_empty(), "{:?}", report.failures);

    let unsupported = Filter { status: Some(PhotoStatus::Unsupported), ..Filter::default() };
    let rows = test.catalog.query(&unsupported, Sort::default(), ALL).unwrap();
    assert_eq!(rows.len(), 4);
    let photo = test.catalog.photo(rows[0].photo).unwrap();
    assert!(photo.status_detail.is_some_and(|reason| !reason.is_empty()));
}

#[test]
fn an_unsupported_file_is_not_retried_until_it_changes() {
    let test = TestCatalog::new();
    let path = test.path("a.dng");
    fs::create_dir_all(test.root()).unwrap();
    fs::write(&path, b"not yet a raw").unwrap();
    assert_eq!(test.scan().unsupported, 1);
    let report = test.scan();
    assert_eq!((report.unsupported, report.unchanged), (0, 1));

    SyntheticRaw::default().write(&path);
    age_file(&path);
    let report = test.scan();
    assert_eq!((report.changed, report.unsupported), (1, 0));
    let (photo, _) = only_photo(&test);
    let photo = test.catalog.photo(photo).unwrap();
    assert_eq!(photo.status, PhotoStatus::Available);
    assert_eq!(photo.status_detail, None);
}

#[test]
fn an_unreadable_folder_is_reported_and_not_marked_missing() {
    let test = TestCatalog::new();
    SyntheticRaw::seeded(1).write(&test.path("open/a.dng"));
    SyntheticRaw::seeded(2).write(&test.path("locked/b.dng"));
    test.scan();

    let locked = test.path("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked).is_ok() {
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        return; // Running as root, where permissions don't stop the listing.
    }
    let report = test.scan();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(report.missing, 0);
    assert_eq!(report.failures.len(), 1);
    assert!(report.failures[0].path.ends_with("locked"));
}

#[test]
fn symlinked_folders_are_followed_and_loops_do_not_hang() {
    let test = TestCatalog::new();
    let outside = test.dir.path().join("outside");
    SyntheticRaw::default().write(&outside.join("a.dng"));
    fs::create_dir_all(test.root()).unwrap();
    symlink(&outside, test.path("linked")).unwrap();
    symlink(test.root(), test.path("loop")).unwrap();
    let report = test.scan();
    assert_eq!(report.added, 1);
    assert!(rows(&test)[0].path.ends_with("linked/a.dng"));
}

#[test]
fn a_name_that_is_not_utf8_is_reported_and_skipped() {
    let test = TestCatalog::new();
    SyntheticRaw::seeded(1).write(&test.path("good.dng"));
    let bad_name = OsStr::from_bytes(b"bad-\xff.dng");
    SyntheticRaw::seeded(2).write(&test.root().join(bad_name));
    let report = test.scan();
    assert_eq!(report.added, 1);
    assert_eq!(report.failures.len(), 1);
}

#[test]
fn overlapping_scans_take_turns() {
    let test = Arc::new(TestCatalog::new());
    for seed in 0..600 {
        SyntheticRaw::seeded(seed).write(&test.path(&format!("{seed}.dng")));
    }
    let scans: Vec<_> = (0..4)
        .map(|_| {
            let test = Arc::clone(&test);
            thread::spawn(move || test.scan())
        })
        .collect();
    let added: usize = scans.into_iter().map(|scan| scan.join().unwrap().added).sum();
    assert_eq!(added, 600);
    assert_eq!(rows(&test).len(), 600);
}

#[test]
fn a_missing_root_is_an_error() {
    let test = TestCatalog::new();
    let result = test.catalog.scan(&test.path("nope"), &CancelToken::default(), |_| {});
    assert!(matches!(result, Err(Error::Io { .. })));
}

#[test]
fn progress_reports_every_file() {
    let test = TestCatalog::new();
    for seed in 0..20 {
        SyntheticRaw::seeded(seed).write(&test.path(&format!("{seed}.dng")));
    }
    let mut updates = Vec::new();
    test.catalog
        .scan(&test.root(), &CancelToken::default(), |update| updates.push(update))
        .unwrap();
    let reading: Vec<_> =
        updates.iter().filter(|update| update.phase == ScanPhase::Reading).collect();
    assert_eq!(reading.len(), 20);
    assert_eq!(reading.iter().map(|update| update.done).max(), Some(20));
    assert!(reading.iter().all(|update| update.total == 20));
    assert!(reading.windows(2).all(|pair| pair[0].done < pair[1].done));
}

#[test]
fn cancelling_before_the_scan_changes_nothing() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("a.dng"));
    test.scan();
    fs::remove_file(test.path("a.dng")).unwrap();

    let cancel = CancelToken::default();
    cancel.cancel();
    let report = test.catalog.scan(&test.root(), &cancel, |_| {}).unwrap();
    assert!(report.cancelled);
    assert_eq!(report.missing, 0);
    assert_eq!(rows(&test)[0].status, PhotoStatus::Available);
}

#[test]
fn cancelling_midway_stops_and_a_rescan_finishes_the_job() {
    let test = TestCatalog::new();
    const FILES: usize = 3000;
    for seed in 0..FILES {
        SyntheticRaw::seeded(seed as u32).write(&test.path(&format!("{}/{seed}.dng", seed % 10)));
    }
    let cancel = CancelToken::default();
    let token = cancel.clone();
    let report = test
        .catalog
        .scan(&test.root(), &cancel, |update| {
            if update.phase == ScanPhase::Reading && update.done == 1 {
                token.cancel();
            }
        })
        .unwrap();
    assert!(report.cancelled);
    assert!(report.added < FILES, "added {}", report.added);
    assert_eq!(report.missing, 0);

    let added_first = rows(&test).len();
    assert_eq!(added_first, report.added);
    let second = test.scan();
    assert!(!second.cancelled);
    assert_eq!(second.added + added_first, FILES);
    assert_eq!(second.unchanged, added_first);
}

#[test]
fn a_cancelled_scan_marks_nothing_missing() {
    let test = TestCatalog::new();
    SyntheticRaw::seeded(0).write(&test.path("gone.dng"));
    test.scan();
    fs::remove_file(test.path("gone.dng")).unwrap();
    for seed in 1..2000 {
        SyntheticRaw::seeded(seed).write(&test.path(&format!("{seed}.dng")));
    }

    let cancel = CancelToken::default();
    let token = cancel.clone();
    let report = test
        .catalog
        .scan(&test.root(), &cancel, |update| {
            if update.phase == ScanPhase::Reading {
                token.cancel();
            }
        })
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(report.missing, 0);
    let missing = Filter { status: Some(PhotoStatus::Missing), ..Filter::default() };
    assert_eq!(test.catalog.count(&missing).unwrap(), 0);
    assert_eq!(test.scan().missing, 1, "the next full scan finishes the job");
}

#[test]
fn a_file_vanishing_between_walk_and_read_is_a_failure_not_a_crash() {
    let test = TestCatalog::new();
    SyntheticRaw::seeded(1).write(&test.path("stays.dng"));
    let doomed = test.path("doomed.dng");
    SyntheticRaw::seeded(2).write(&doomed);

    let target = doomed.clone();
    let report = test
        .catalog
        .scan(&test.root(), &CancelToken::default(), move |update| {
            if update.phase == ScanPhase::Reading {
                let _ = fs::remove_file(&target);
            }
        })
        .unwrap();
    assert_eq!(report.discovered, 2);
    assert_eq!(report.added + report.failures.len(), 2);
}

#[test]
fn reads_stay_available_during_a_scan() {
    let test = Arc::new(TestCatalog::new());
    for seed in 0..1500 {
        SyntheticRaw::seeded(seed).write(&test.path(&format!("{seed}.dng")));
    }
    let finished = Arc::new(Mutex::new(false));
    let reader = {
        let (test, finished) = (Arc::clone(&test), Arc::clone(&finished));
        thread::spawn(move || {
            let mut counts = Vec::new();
            while !*finished.lock().unwrap() {
                counts.push(test.catalog.count(&Filter::default()).unwrap());
            }
            counts
        })
    };
    test.scan();
    *finished.lock().unwrap() = true;
    let counts = reader.join().unwrap();
    assert!(counts.windows(2).all(|pair| pair[0] <= pair[1]), "committed rows never go back");
    assert_eq!(test.catalog.count(&Filter::default()).unwrap(), 1500);
}

#[test]
fn sidecars_are_read_at_import() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("a.dng"));
    fs::write(
        test.path("a.xmp"),
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
         <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
          <rdf:Description rdf:about=""
            xmlns:xmp="http://ns.adobe.com/xap/1.0/"
            xmlns:dc="http://purl.org/dc/elements/1.1/"
            xmlns:lr="http://ns.adobe.com/lightroom/1.0/"
            xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
            xmp:Rating="4" xmp:Label="Blue" crs:Exposure2012="+1.00">
           <dc:subject><rdf:Bag><rdf:li>Owl</rdf:li><rdf:li>Trip</rdf:li></rdf:Bag></dc:subject>
           <lr:hierarchicalSubject><rdf:Bag><rdf:li>Animals|Owl</rdf:li></rdf:Bag></lr:hierarchicalSubject>
          </rdf:Description>
         </rdf:RDF>
        </x:xmpmeta>"#,
    )
    .unwrap();
    let report = test.scan();
    assert_eq!(report.sidecars_read, 1);

    let (photo, variant) = only_photo(&test);
    let saved = test.catalog.variant(variant).unwrap();
    assert_eq!((saved.rating, saved.label), (4, Label::Blue));
    assert_eq!(saved.recipe, Recipe::default(), "crs: settings are deliberately not mapped");
    let names: Vec<_> =
        test.catalog.keywords_of(variant).unwrap().into_iter().map(|k| k.name).collect();
    assert_eq!(names, ["Owl", "Trip"]);
    let tree = test.catalog.keywords().unwrap();
    let animals = tree.iter().find(|keyword| keyword.name == "Animals").unwrap();
    let owl = tree.iter().find(|keyword| keyword.name == "Owl").unwrap();
    assert_eq!(owl.parent, Some(animals.id));
    assert!(test.catalog.dirty_sidecars().unwrap().is_empty(), "photo {photo:?} is in sync");
}

#[test]
fn darktable_style_sidecars_are_found() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("a.dng"));
    fs::write(
        test.path("a.dng.xmp"),
        r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
           <rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="2"/></rdf:RDF>"#,
    )
    .unwrap();
    assert_eq!(test.scan().sidecars_read, 1);
    assert_eq!(rows(&test)[0].rating, 2);
}

#[test]
fn a_malformed_sidecar_does_not_stop_the_import() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("a.dng"));
    fs::write(test.path("a.xmp"), "<x:xmpmeta><unclosed>").unwrap();
    let report = test.scan();
    assert_eq!((report.added, report.sidecars_read), (1, 0));
    assert_eq!(report.failures.len(), 1);
    assert!(report.failures[0].path.ends_with("a.xmp"));

    let again = test.scan();
    assert_eq!(again.failures.len(), 0, "the same bad file isn't reported every scan");
}

#[test]
fn a_sidecar_edited_elsewhere_is_picked_up_by_a_rescan() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("a.dng"));
    test.scan();
    let (photo, variant) = only_photo(&test);
    let sidecar = test.catalog.write_sidecar(photo).unwrap();
    assert_eq!(test.scan().sidecars_read, 0, "our own write isn't an outside change");

    let text =
        fs::read_to_string(&sidecar).unwrap().replace(r#"xmp:Rating="0""#, r#"xmp:Rating="5""#);
    fs::write(&sidecar, text).unwrap();
    age_file(&sidecar);
    assert_eq!(test.scan().sidecars_read, 1);
    assert_eq!(test.catalog.variant(variant).unwrap().rating, 5);
}

#[test]
fn both_sides_changing_is_a_conflict_that_keeps_the_catalog() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("a.dng"));
    test.scan();
    let (photo, variant) = only_photo(&test);
    let sidecar = test.catalog.write_sidecar(photo).unwrap();

    test.catalog.set_rating(&[variant], 3).unwrap();
    let text =
        fs::read_to_string(&sidecar).unwrap().replace(r#"xmp:Rating="0""#, r#"xmp:Rating="5""#);
    fs::write(&sidecar, text).unwrap();
    age_file(&sidecar);

    let report = test.scan();
    assert_eq!((report.sidecar_conflicts, report.sidecars_read), (1, 0));
    assert_eq!(test.catalog.variant(variant).unwrap().rating, 3);
    assert_eq!(test.catalog.sidecar_conflicts().unwrap(), [photo]);
    assert_eq!(test.scan().sidecar_conflicts, 0, "reported once");

    test.catalog.read_sidecar(photo).unwrap();
    assert_eq!(test.catalog.variant(variant).unwrap().rating, 5);
    assert!(test.catalog.sidecar_conflicts().unwrap().is_empty());
}

#[test]
fn a_new_catalog_rebuilds_edits_from_sidecars() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("a.dng"));
    test.scan();
    let (photo, variant) = only_photo(&test);
    let mut recipe = Recipe::default();
    recipe.basic.exposure = 0.75;
    test.catalog.save_recipe(variant, &recipe, "Exposure").unwrap();
    test.catalog.set_rating(&[variant], 2).unwrap();
    test.catalog.set_flag(&[variant], Flag::Reject).unwrap();
    test.catalog.set_label(&[variant], Label::Yellow).unwrap();
    let keyword = test.catalog.create_keyword(None, "Kept").unwrap();
    test.catalog.add_keyword(&[variant], keyword).unwrap();
    assert_eq!(test.catalog.dirty_sidecars().unwrap(), [photo]);
    test.catalog.write_sidecar(photo).unwrap();
    assert!(test.catalog.dirty_sidecars().unwrap().is_empty());

    let fresh = Catalog::open(&test.dir.path().join("fresh.db")).unwrap();
    fresh.scan(&test.root(), &CancelToken::default(), |_| {}).unwrap();
    let rows = fresh.query(&Filter::default(), Sort::default(), ALL).unwrap();
    let restored = fresh.variant(rows[0].variant).unwrap();
    assert_eq!(restored.recipe, recipe);
    assert_eq!((restored.rating, restored.flag, restored.label), (2, Flag::Reject, Label::Yellow));
    assert_eq!(fresh.keywords_of(restored.id).unwrap()[0].name, "Kept");
}

#[test]
fn an_import_leaves_no_foreign_key_violations() {
    let test = TestCatalog::new();
    SyntheticRaw::default().write(&test.path("a.dng"));
    test.scan();
    let conn = Connection::open(test.dir.path().join("catalog.db")).unwrap();
    let violations: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(violations, 0);
}
