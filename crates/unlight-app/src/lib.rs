//! The Unlight desktop application. `state` holds everything that matters and needs no window,
//! `ui` draws it, and the rest connects the two to the catalog and the engine.

pub mod action;
#[cfg(feature = "ai")]
pub mod ai_tools;
pub mod app;
pub mod color_jobs;
pub mod darkroom;
pub mod export;
pub mod loader;
pub mod merge;
pub mod renderer;
pub mod state;
pub mod store;
pub mod ui;
pub mod wayland_drop;
pub mod white_balance;

use anyhow::{Result, anyhow};
use app::{App, Handoff};
use eframe::{
    egui,
    egui_wgpu::{SurfaceConfig, WgpuConfiguration, WgpuSetup, WgpuSetupExisting},
};
use state::{AppState, session::Session};
use std::{
    env,
    os::unix::process::CommandExt,
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    process::Command,
    sync::Arc,
};
use store::{Store, StoreConfig};
use unlight_engine::GpuContext;

const WINDOW_SIZE: [f32; 2] = [1600.0, 1000.0];
const MIN_WINDOW_SIZE: [f32; 2] = [960.0, 600.0];

/// Where `MaskShape::Raster` finds its data. Without the AI tools, those masks select nothing.
#[cfg(feature = "ai")]
pub fn mask_rasters() -> Option<&'static dyn unlight_engine::MaskRasterSource> {
    unlight_ai::stored_masks()
}

#[cfg(not(feature = "ai"))]
pub fn mask_rasters() -> Option<&'static dyn unlight_engine::MaskRasterSource> {
    None
}

/// Hands the engine's device to eframe, so the viewport can show engine textures directly.
pub fn shared_wgpu_setup(context: &GpuContext) -> WgpuSetup {
    WgpuSetup::Existing(WgpuSetupExisting {
        instance: context.instance.clone(),
        adapter: context.adapter.clone(),
        device: context.device.clone(),
        queue: context.queue.clone(),
    })
}

fn native_options(context: &GpuContext) -> eframe::NativeOptions {
    let wgpu_options = WgpuConfiguration {
        wgpu_setup: shared_wgpu_setup(context),
        surface: SurfaceConfig::LOW_LATENCY,
        ..Default::default()
    };
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        viewport: egui::ViewportBuilder::default()
            .with_title("Unlight")
            .with_app_id("unlight")
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size(MIN_WINDOW_SIZE)
            .with_drag_and_drop(true),
        ..Default::default()
    }
}

/// A process restart, for when eframe itself died on a lost GPU. Every edit is in the catalog
/// by then and the session file says where the user was.
fn restart() -> anyhow::Error {
    let program = match env::current_exe() {
        Ok(program) => program,
        Err(error) => return error.into(),
    };
    Command::new(program).exec().into()
}

/// Runs the app until its window closes. A lost GPU comes back here with the state handed
/// over, and the app simply reopens on a new device.
pub fn run(paths: &[PathBuf]) -> Result<()> {
    let store = Arc::new(Store::open(&StoreConfig::at_home()?)?);
    let mut state = AppState::default();
    if paths.is_empty() {
        state.restore_session(&Session::load(store.session_file()));
    } else {
        state.open_paths(paths);
    }
    let handoff: &'static Handoff = Box::leak(Box::default());
    loop {
        let context = GpuContext::new()?;
        let watch = context.clone();
        let options = native_options(&context);
        let shared = store.clone();
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            eframe::run_native(
                "unlight",
                options,
                Box::new(move |cc| Ok(Box::new(App::new(cc, context, shared, state, handoff)))),
            )
        }));
        match outcome {
            Ok(result) => result.map_err(|error| anyhow!("{error}"))?,
            Err(_) if watch.is_lost() => {
                store.flush();
                return Err(restart());
            }
            Err(panic) => panic::resume_unwind(panic),
        }
        match handoff.take_state() {
            Some(carried_state) => state = carried_state,
            None => return Ok(()),
        }
    }
}
