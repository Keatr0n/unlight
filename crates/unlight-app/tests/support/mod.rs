//! Shared harness: the real app on the real engine and a real catalog, in a temp directory
//! and without a window.

#![allow(dead_code)]

#[path = "../../../unlight-catalog/tests/common/mod.rs"]
pub mod raws;

use eframe::egui::{self, Vec2, vec2};
use egui_kittest::{Harness, HarnessBuilder, kittest::Queryable};
use std::{
    env, fs,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use unlight_app::{
    app::App,
    shared_wgpu_setup,
    state::{AppState, edit::PhotoEdit},
    store::{Store, StoreConfig},
};
use unlight_catalog::{CancelToken, LibraryRow};
use unlight_engine::{Engine, GpuContext, Precision, Session};
use unlight_raw::{Cfa, ColorData, Metadata, Opcodes, RawImage};
use unlight_recipe::Recipe;

use raws::SyntheticRaw;

pub const LOAD_TIMEOUT: Duration = Duration::from_secs(120);
pub const WINDOW: Vec2 = vec2(1600.0, 1000.0);

/// Where sample raws live, when this machine has them.
pub fn samples() -> Option<PathBuf> {
    env::var_os("UNLIGHT_SAMPLES")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

pub fn sample(name: &str) -> Option<PathBuf> {
    Some(samples()?.join(name)).filter(|path| path.is_file())
}

/// A catalog, a preview cache and a folder of raws in a temp directory.
pub struct Fixture {
    pub dir: TempDir,
    pub photos: PathBuf,
}

impl Fixture {
    pub fn empty() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let photos = dir.path().join("photos");
        fs::create_dir_all(&photos).expect("photo folder");
        let photos = photos.canonicalize().expect("photo folder");
        Self { dir, photos }
    }

    /// Writes synthetic DNGs that LibRaw really opens, each with an embedded preview.
    pub fn with_photos(count: u32) -> Self {
        let fixture = Self::empty();
        for index in 0..count {
            fixture.add_photo(index, &format!("photo-{index:03}.dng"));
        }
        fixture
    }

    pub fn add_photo(&self, seed: u32, name: &str) -> PathBuf {
        let path = self.photos.join(name);
        let captured = format!("2024:03:15 10:{:02}:{:02}", seed / 60, seed % 60);
        let raw = SyntheticRaw {
            preview: Some((320, 200)),
            captured,
            ..SyntheticRaw::seeded(seed)
        };
        raw.write(&path);
        path
    }

    pub fn config(&self) -> StoreConfig {
        StoreConfig {
            catalog: self.dir.path().join("catalog.sqlite"),
            previews: self.dir.path().join("previews"),
            sidecar_delay: None,
        }
    }

    pub fn store(&self) -> Arc<Store> {
        Arc::new(Store::open(&self.config()).expect("store"))
    }

    pub fn scan(&self, store: &Store) {
        store
            .catalog
            .scan(&self.photos, &CancelToken::default(), |_| {})
            .expect("scan");
    }
}

/// The harness and what it runs on, which have to go away in that order.
pub struct TestApp {
    pub harness: Harness<'static, App>,
    pub fixture: Fixture,
}

impl Deref for TestApp {
    type Target = Harness<'static, App>;

    fn deref(&self) -> &Self::Target {
        &self.harness
    }
}

impl DerefMut for TestApp {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.harness
    }
}

pub fn builder(size: Vec2, pixels_per_point: f32, context: &GpuContext) -> HarnessBuilder<App> {
    Harness::builder()
        .with_size(size)
        .with_pixels_per_point(pixels_per_point)
        .with_max_steps(64)
        .with_step_dt(0.02)
        .wgpu_setup(shared_wgpu_setup(context))
}

fn leaked_handoff() -> &'static unlight_app::app::Handoff {
    Box::leak(Box::default())
}

/// The app on a fresh device over a fixture, with `setup` run before the first frame.
pub fn open_app(
    fixture: Fixture,
    store: Arc<Store>,
    size: Vec2,
    pixels_per_point: f32,
    setup: impl FnOnce(&mut App) + 'static,
) -> TestApp {
    let context = GpuContext::new().expect("no Vulkan device for the app tests");
    let harness = builder(size, pixels_per_point, &context).build_eframe(move |cc| {
        let mut app = App::new(cc, context, store, AppState::default(), leaked_handoff());
        setup(&mut app);
        app
    });
    TestApp { harness, fixture }
}

/// The app with real files opened as if from outside, for the sample tests.
pub fn app(paths: &[PathBuf], size: Vec2, pixels_per_point: f32) -> TestApp {
    let paths = paths.to_vec();
    let fixture = Fixture::empty();
    let store = fixture.store();
    open_app(fixture, store, size, pixels_per_point, move |app| {
        app.state.open_paths(&paths)
    })
}

/// The app over `count` scanned photos, waiting until the first one is selected.
pub fn library_app(count: u32) -> TestApp {
    let fixture = Fixture::with_photos(count);
    let store = fixture.store();
    fixture.scan(&store);
    let mut test = open_app(fixture, store, WINDOW, 1.0, |_| {});
    settle(&mut test, |app| app.state.edit().is_some());
    test
}

/// Steps until `done` holds, while background work finishes.
pub fn settle(harness: &mut Harness<'static, App>, done: impl Fn(&App) -> bool) {
    let start = Instant::now();
    while !done(harness.state()) {
        assert!(start.elapsed() < LOAD_TIMEOUT, "timed out waiting for the app");
        harness.step();
        thread::sleep(Duration::from_millis(5));
    }
    harness.step();
    harness.step();
}

pub fn live(app: &App) -> bool {
    app.state
        .focused()
        .is_some_and(|row| app.darkroom.live(&row.path).is_some())
}

/// Lets a burst of edits run out, then waits for the catalog to catch up.
pub fn let_settle(harness: &mut Harness<'static, App>) {
    for _ in 0..24 {
        harness.step();
    }
    harness.state().store.flush();
}

/// Writes the frame to `UNLIGHT_SNAPSHOTS` when that's set, for looking at by hand.
pub fn save(harness: &mut Harness<'static, App>, name: &str) {
    let Some(directory) = env::var_os("UNLIGHT_SNAPSHOTS").map(PathBuf::from) else {
        return;
    };
    fs::create_dir_all(&directory).expect("creating the snapshot directory");
    let image = harness.render().expect("rendering the frame");
    let path: PathBuf = Path::new(&directory).join(format!("{name}.png"));
    image.save(&path).expect("writing the snapshot");
}

const XYZ_TO_SRGB_CAMERA: [[f32; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.969266, 1.8760108, 0.041556],
    [0.0556434, -0.2040259, 1.0572252],
];
const BLACK: f32 = 64.0;
const WHITE: f32 = 16383.0;

fn bayer() -> Cfa {
    let mut pattern = [[0u8; 6]; 6];
    for (row, sites) in pattern.iter_mut().enumerate() {
        for (col, site) in sites.iter_mut().enumerate() {
            *site = match (row % 2, col % 2) {
                (0, 0) => 0,
                (1, 1) => 2,
                _ => 1,
            };
        }
    }
    Cfa { period: 2, pattern }
}

/// A raw of a soft gradient, sized for tests that look at what's rendered.
pub fn synthetic_raw(width: u32, height: u32, orientation: i32, tint: [f32; 3]) -> RawImage {
    let cfa = bayer();
    let samples = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let level = 0.15 + 0.5 * (x + y) as f32 / (width + height) as f32;
            let value = level * tint[cfa.color_at(x, y) as usize];
            (BLACK + value.clamp(0.0, 1.0) * (WHITE - BLACK)).round() as u16
        })
        .collect();
    RawImage {
        width,
        height,
        samples,
        cfa,
        black: [[BLACK; 6]; 6],
        white: WHITE,
        color: ColorData {
            xyz_to_camera: XYZ_TO_SRGB_CAMERA,
            as_shot_multipliers: [1.0; 3],
            daylight_multipliers: [1.0; 3],
            baseline_exposure: 0.0,
            dng: None,
        },
        opcodes: Opcodes::default(),
        metadata: Metadata {
            orientation,
            ..Metadata::default()
        },
        embedded_lens: None,
    }
}

/// The app with `count` catalogued photos, the first decoded from `raw` so that what's on
/// screen is a known image at a known size.
pub fn editing_raw(raw: &RawImage, count: u32) -> TestApp {
    let fixture = Fixture::with_photos(count);
    let store = fixture.store();
    fixture.scan(&store);
    let context = GpuContext::new().expect("no Vulkan device for the app tests");
    let engine = Engine::new(context.clone(), Precision::F16).expect("engine");
    let session = Session::open(engine, raw, &Recipe::default()).expect("session");
    let metadata = raw.metadata.clone();
    let path = fixture.photos.join("photo-000.dng");
    let harness = builder(WINDOW, 1.0, &context).build_eframe(move |cc| {
        let mut app = App::new(cc, context, store, AppState::default(), leaked_handoff());
        app.darkroom.adopt(path, session, metadata);
        app
    });
    let mut test = TestApp { harness, fixture };
    settle(&mut test, |app| {
        app.state.edit().is_some() && app.viewport.photo_rect.is_some()
    });
    test
}

pub fn editing_app() -> TestApp {
    editing_raw(&synthetic_raw(600, 400, 0, [1.0; 3]), 2)
}

pub fn edit<'a>(harness: &'a Harness<'static, App>) -> &'a PhotoEdit {
    harness.state().state.edit().expect("a selected photo")
}

pub fn recipe<'a>(harness: &'a Harness<'static, App>) -> &'a Recipe {
    &edit(harness).recipe
}

pub fn steps(harness: &Harness<'static, App>) -> usize {
    edit(harness).history().entries().len()
}

pub fn row(harness: &Harness<'static, App>, index: usize) -> LibraryRow {
    harness
        .state()
        .state
        .library
        .row(index)
        .expect("a loaded row")
        .clone()
}

/// Drags in stages, as a hand would, stepping the app in between.
pub fn drag(harness: &mut Harness<'static, App>, from: egui::Pos2, to: egui::Pos2) {
    harness.hover_at(from);
    harness.step();
    harness.drag_at(from);
    harness.step();
    for stage in 1..=4 {
        harness.hover_at(from + (to - from) * stage as f32 / 4.0);
        harness.step();
    }
    harness.drop_at(to);
    harness.step();
    harness.step();
}

pub fn slider_rect(harness: &Harness<'static, App>, name: &str) -> egui::Rect {
    harness.get_by_label(name).rect()
}
