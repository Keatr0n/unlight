//! The Library against a catalog of 100,000 rows, seeded directly with SQL since that many
//! files would take far too long to write. Budgets are generous, for a test build on a machine
//! that may be busy; the printed times are what's worth reading.

mod support;

use rusqlite::{Connection, params};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use support::{Fixture, WINDOW, open_app, settle};
use unlight_app::{action::Action, state::library::PAGE_SIZE};
use unlight_catalog::Filter;

const ROWS: i64 = 100_000;
const FRAME_BUDGET: Duration = Duration::from_millis(100);
const QUERY_BUDGET: Duration = Duration::from_secs(2);

/// The flake's check shares the machine with every other build, so it widens these.
fn budget(base: Duration) -> Duration {
    let scale = std::env::var("UNLIGHT_BUDGET_SCALE").ok().and_then(|value| value.parse().ok());
    base * scale.unwrap_or(1)
}

fn seed(path: &Path) {
    let mut conn = Connection::open(path).expect("database");
    let tx = conn.transaction().expect("transaction");
    tx.execute("INSERT INTO folders (id, path) VALUES (1, '/photos')", [])
        .expect("folder");
    {
        let mut photo = tx
            .prepare(
                "INSERT INTO photos (id, folder_id, file_name, size, mtime_ns, fingerprint, status,
                     camera, lens, capture_time, width, height, orientation)
                 VALUES (?1, 1, ?2, 1, 1, ?3, 0, 'Acme', 'Lens', ?4, 6000, 4000, 0)",
            )
            .expect("photo statement");
        let mut variant = tx
            .prepare("INSERT INTO variants (id, photo_id, rating) VALUES (?1, ?1, ?2)")
            .expect("variant statement");
        for id in 1..=ROWS {
            let mut fingerprint = [0u8; 16];
            fingerprint[..8].copy_from_slice(&id.to_le_bytes());
            let name = format!("IMG_{id:06}.ARW");
            photo
                .execute(params![id, name, fingerprint.as_slice(), 1_000_000 + id])
                .expect("photo");
            variant.execute(params![id, id % 6]).expect("variant");
        }
    }
    tx.commit().expect("commit");
}

fn frame_time(step: impl FnOnce()) -> Duration {
    let start = Instant::now();
    step();
    start.elapsed()
}

#[test]
fn a_hundred_thousand_rows_stay_interactive() {
    let fixture = Fixture::empty();
    let store = fixture.store();
    seed(&fixture.config().catalog);
    let mut app = open_app(fixture, store, WINDOW, 1.0, |app| {
        app.state.apply(Action::ShowGrid)
    });
    let start = Instant::now();
    settle(&mut app, |app| {
        app.state.library.count() == Some(ROWS as u64) && app.state.focused().is_some()
    });
    println!("first rows after {:?}", start.elapsed());

    let frame = frame_time(|| app.step());
    println!("grid frame {frame:?}");
    assert!(frame < budget(FRAME_BUDGET), "{frame:?}");

    let start = Instant::now();
    app.state_mut().state.library.step(ROWS as isize - 1);
    settle(&mut app, |app| {
        app.state
            .focused()
            .is_some_and(|row| row.path.ends_with("IMG_100000.ARW"))
    });
    let deep = start.elapsed();
    println!("to the last row in {deep:?}");
    assert!(deep < budget(QUERY_BUDGET), "{deep:?}");
    let frame = frame_time(|| app.step());
    println!("grid frame at the end {frame:?}");
    assert!(frame < budget(FRAME_BUDGET), "{frame:?}");

    let start = Instant::now();
    let filter = Filter {
        text: Some("IMG_0999".into()),
        ..Filter::default()
    };
    app.state_mut().state.library.set_filter(filter);
    settle(&mut app, |app| {
        app.state
            .library
            .count()
            .is_some_and(|count| count > 0 && count < PAGE_SIZE * 2)
    });
    let filtered = start.elapsed();
    println!("text filter in {filtered:?}");
    assert!(filtered < budget(QUERY_BUDGET), "{filtered:?}");
}
