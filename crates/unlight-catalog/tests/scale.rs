//! The two scale targets from the plan: 10,000 files scanned in under 60 s, and filters over
//! 100,000 rows that stay interactive. `examples/catalog_measure.rs` reports release-build numbers.

mod common;

use common::{SyntheticRaw, TestCatalog};
use rusqlite::{Connection, params};
use std::time::{Duration, Instant};
use unlight_catalog::{
    Comparison, DateRange, Filter, Flag, FolderFilter, FolderId, KeywordId, Label, MatchMode, Page,
    RatingFilter, Rule, SmartRules, Sort, SortKey, TextOp,
};

const SCAN_FILES: u32 = 10_000;
const SCAN_BUDGET: Duration = Duration::from_secs(60);
const LIBRARY_ROWS: i64 = 100_000;
/// A generous bound for an unoptimised test build on a machine that may be busy. The example
/// measures the release figure.
const QUERY_BUDGET: Duration = Duration::from_millis(250);
const PAGE: Page = Page { offset: 0, limit: 200 };

/// The flake's check shares the machine with every other build, so it widens these with
/// `UNLIGHT_BUDGET_SCALE`. The query plan tests below are what catch a regression there.
fn budget(base: Duration) -> Duration {
    let scale = std::env::var("UNLIGHT_BUDGET_SCALE").ok().and_then(|value| value.parse().ok());
    base * scale.unwrap_or(1)
}

#[test]
fn ten_thousand_files_scan_within_budget_and_rescan_touches_nothing() {
    let test = TestCatalog::new();
    for seed in 0..SCAN_FILES {
        SyntheticRaw::seeded(seed).write(&test.path(&format!(
            "{}/{}/{seed}.dng",
            seed % 50,
            seed % 7
        )));
    }

    let started = Instant::now();
    let report = test.scan();
    let elapsed = started.elapsed();
    println!("first scan: {elapsed:?} for {SCAN_FILES} synthetic files");
    assert_eq!(report.added, SCAN_FILES as usize);
    assert!(report.failures.is_empty(), "{:?}", report.failures.first());
    assert!(elapsed < budget(SCAN_BUDGET), "{elapsed:?}");

    let started = Instant::now();
    let rescan = test.scan();
    let elapsed = started.elapsed();
    println!("rescan: {elapsed:?}");
    assert_eq!(rescan.unchanged, SCAN_FILES as usize);
    assert_eq!(rescan.added + rescan.changed + rescan.moved, 0);
    assert!(elapsed < budget(SCAN_BUDGET / 6), "{elapsed:?}");
}

/// Fills a catalog directly with SQL, which is far quicker than creating 100,000 files.
fn seed_library(path: &std::path::Path) -> (Vec<FolderId>, KeywordId) {
    let mut conn = Connection::open(path).unwrap();
    let tx = conn.transaction().unwrap();
    let mut folders = Vec::new();
    tx.execute("INSERT INTO folders (id, path) VALUES (1, '/photos')", []).unwrap();
    for index in 0..200 {
        let id = index + 2;
        tx.execute(
            "INSERT INTO folders (id, parent_id, path) VALUES (?1, 1, ?2)",
            params![id, format!("/photos/{index:03}")],
        )
        .unwrap();
        folders.push(FolderId(id));
    }
    let keyword_ids: Vec<i64> = (0..50)
        .map(|index| {
            tx.execute("INSERT INTO keywords (name) VALUES (?1)", [format!("keyword{index}")])
                .unwrap();
            tx.last_insert_rowid()
        })
        .collect();
    {
        let mut photo = tx
            .prepare(
                "INSERT INTO photos (id, folder_id, file_name, size, mtime_ns, fingerprint, camera,
                     lens, iso, capture_time, width, height)
                 VALUES (?1, ?2, ?3, 1000, 0, ?4, ?5, ?6, ?7, ?8, 6000, 4000)",
            )
            .unwrap();
        let mut variant = tx
            .prepare(
                "INSERT INTO variants (id, photo_id, rating, flag, label, edited_at)
                 VALUES (?1, ?1, ?2, ?3, ?4, ?5)",
            )
            .unwrap();
        let mut keyword = tx
            .prepare(
                "INSERT OR IGNORE INTO variant_keywords (variant_id, keyword_id) VALUES (?1, ?2)",
            )
            .unwrap();
        for id in 1..=LIBRARY_ROWS {
            let shuffled = id.wrapping_mul(2_654_435_761) & 0xffff_ffff;
            let fingerprint = {
                let mut bytes = [0u8; 16];
                bytes[..8].copy_from_slice(&id.to_le_bytes());
                bytes
            };
            photo
                .execute(params![
                    id,
                    folders[(id % 200) as usize].0,
                    format!("IMG_{id:06}.ARW"),
                    fingerprint.as_slice(),
                    format!("Camera {}", id % 8),
                    format!("Lens {}", id % 25),
                    100 << (id % 7),
                    1_400_000_000 + shuffled % 300_000_000,
                ])
                .unwrap();
            let edited = (id % 4 == 0).then_some(1_700_000_000_000 + shuffled);
            variant.execute(params![id, id % 6, (id % 3) - 1, id % 6, edited]).unwrap();
            keyword.execute(params![id, keyword_ids[(id % 50) as usize]]).unwrap();
            keyword.execute(params![id, keyword_ids[(id % 37) as usize + 13]]).unwrap();
        }
    }
    tx.commit().unwrap();
    (folders, KeywordId(keyword_ids[7]))
}

fn cases(folders: &[FolderId], keyword: KeywordId) -> Vec<(&'static str, Filter)> {
    let at_least_three = RatingFilter { comparison: Comparison::GreaterOrEqual, value: 3 };
    vec![
        ("everything", Filter::default()),
        ("rating", Filter { rating: Some(at_least_three), ..Filter::default() }),
        ("flag", Filter { flag: Some(Flag::Pick), ..Filter::default() }),
        ("label", Filter { label: Some(Label::Green), ..Filter::default() }),
        ("camera", Filter { camera: Some("Camera 3".into()), ..Filter::default() }),
        ("lens", Filter { lens: Some("Lens 12".into()), ..Filter::default() }),
        (
            "date range",
            Filter {
                captured: Some(DateRange { from: Some(1_500_000_000), until: Some(1_510_000_000) }),
                ..Filter::default()
            },
        ),
        ("keyword", Filter { keyword: Some(keyword), ..Filter::default() }),
        (
            "folder",
            Filter {
                folder: Some(FolderFilter { folder: folders[5], include_subfolders: true }),
                ..Filter::default()
            },
        ),
        ("text", Filter { text: Some("IMG_0123".into()), ..Filter::default() }),
        ("text rare", Filter { text: Some("keyword13".into()), ..Filter::default() }),
        ("edited", Filter { edited: Some(true), ..Filter::default() }),
        (
            "combined",
            Filter {
                rating: Some(at_least_three),
                camera: Some("Camera 4".into()),
                edited: Some(true),
                text: Some("Lens".into()),
                ..Filter::default()
            },
        ),
    ]
}

fn timed<T>(mut work: impl FnMut() -> T) -> (T, Duration) {
    let mut best = Duration::MAX;
    let mut value = None;
    for _ in 0..3 {
        let started = Instant::now();
        value = Some(work());
        best = best.min(started.elapsed());
    }
    (value.expect("ran three times"), best)
}

#[test]
fn filters_over_100k_rows_stay_interactive() {
    let test = TestCatalog::new();
    let (folders, keyword) = seed_library(&test.dir.path().join("catalog.db"));
    test.catalog.optimize().unwrap();

    let sorts = [SortKey::CaptureTime, SortKey::FileName, SortKey::Rating, SortKey::EditTime];
    let mut worst = (Duration::ZERO, String::new());
    for (name, filter) in cases(&folders, keyword) {
        let (total, count_time) = timed(|| test.catalog.count(&filter).unwrap());
        assert!(total > 0 || name == "text", "{name} matched nothing");
        if count_time > worst.0 {
            worst = (count_time, format!("{name} count"));
        }
        for key in sorts {
            for descending in [false, true] {
                let sort = Sort { key, descending };
                let (rows, query_time) = timed(|| test.catalog.query(&filter, sort, PAGE).unwrap());
                assert_eq!(rows.len() as u64, total.min(PAGE.limit), "{name} {key:?}");
                if query_time > worst.0 {
                    worst = (query_time, format!("{name} sorted by {key:?} desc={descending}"));
                }
            }
        }
    }
    println!("slowest: {:?} on {}", worst.0, worst.1);
    assert!(worst.0 < budget(QUERY_BUDGET), "{worst:?}");
}

#[test]
fn smart_collections_over_100k_rows_stay_interactive() {
    let test = TestCatalog::new();
    seed_library(&test.dir.path().join("catalog.db"));
    test.catalog.optimize().unwrap();
    let rules = SmartRules {
        mode: MatchMode::All,
        rules: vec![
            Rule::Rating { op: Comparison::GreaterOrEqual, value: 2 },
            Rule::Group(SmartRules {
                mode: MatchMode::Any,
                rules: vec![
                    Rule::Camera { op: TextOp::Contains, value: "Camera 1".into() },
                    Rule::Keyword { op: TextOp::Is, value: "keyword9".into() },
                ],
            }),
        ],
    };
    let collection = test.catalog.create_smart_collection("big", &rules).unwrap();
    let filter = Filter { collection: Some(collection), ..Filter::default() };
    let (total, count_time) = timed(|| test.catalog.count(&filter).unwrap());
    let (rows, query_time) = timed(|| test.catalog.query(&filter, Sort::default(), PAGE).unwrap());
    println!("smart collection: count {count_time:?}, page {query_time:?}, {total} matches");
    assert_eq!(rows.len(), PAGE.limit as usize);
    assert!(count_time < budget(QUERY_BUDGET) && query_time < budget(QUERY_BUDGET));
}

#[test]
fn optimizing_gives_the_planner_statistics() {
    let test = TestCatalog::new();
    seed_library(&test.dir.path().join("catalog.db"));
    test.catalog.optimize().unwrap();
    let conn = Connection::open(test.dir.path().join("catalog.db")).unwrap();
    let analysed: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_stat1 WHERE tbl IN ('photos', 'variants')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(analysed >= 2, "only {analysed} rows of statistics");
}

#[test]
fn deep_pages_stay_interactive() {
    let test = TestCatalog::new();
    seed_library(&test.dir.path().join("catalog.db"));
    test.catalog.optimize().unwrap();
    let deep = Page { offset: 95_000, limit: 200 };
    for key in [SortKey::CaptureTime, SortKey::FileName, SortKey::Rating, SortKey::EditTime] {
        let sort = Sort { key, descending: true };
        let (rows, elapsed) = timed(|| test.catalog.query(&Filter::default(), sort, deep).unwrap());
        println!("page at offset 95000 sorted by {key:?}: {elapsed:?}");
        assert_eq!(rows.len(), 200);
        assert!(elapsed < budget(QUERY_BUDGET), "{key:?}: {elapsed:?}");
    }
}

#[test]
fn time_name_and_edit_sorts_walk_an_index_instead_of_sorting_every_row() {
    let test = TestCatalog::new();
    seed_library(&test.dir.path().join("catalog.db"));
    test.catalog.optimize().unwrap();
    for key in [SortKey::CaptureTime, SortKey::FileName, SortKey::EditTime] {
        let plan =
            test.catalog.explain_query(&Filter::default(), Sort { key, descending: true }).unwrap();
        println!("{key:?}: {plan:?}");
        assert!(!plan.iter().any(|step| step.contains("TEMP B-TREE")), "{key:?} sorts: {plan:?}");
    }
}

#[test]
fn selective_filters_use_an_index() {
    let test = TestCatalog::new();
    let (folders, keyword) = seed_library(&test.dir.path().join("catalog.db"));
    test.catalog.optimize().unwrap();
    let selective = [
        Filter { camera: Some("Camera 3".into()), ..Filter::default() },
        Filter {
            captured: Some(DateRange { from: Some(1_500_000_000), until: Some(1_500_100_000) }),
            ..Filter::default()
        },
        Filter { keyword: Some(keyword), ..Filter::default() },
        Filter {
            folder: Some(FolderFilter { folder: folders[0], include_subfolders: false }),
            ..Filter::default()
        },
    ];
    for filter in selective {
        let sort = Sort { key: SortKey::FileName, descending: false };
        let plan = test.catalog.explain_query(&filter, sort).unwrap();
        println!("{plan:?}");
        assert!(plan.iter().any(|step| step.contains("USING")), "{filter:?}: {plan:?}");
    }
}
