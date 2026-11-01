//! The application: wires state, the catalog, background work, rendering and drawing together
//! each frame.

use crate::{
    action::{Action, BINDINGS},
    color_jobs::{self, ColorEvent, ColorJobs},
    darkroom::Darkroom,
    export::{ExportEvent, ExportJob, Exporter},
    merge::{MergeJob, MergeOutcome, Merger},
    renderer::ViewRenderer,
    state::{
        AppState, DecodeState, Dialog, Effect, FolderUse, Lights, Module,
        edit::PhotoEdit,
        history::{History, HistoryEntry},
        library::is_raw,
        proof::ProofChoice,
        transfer::{ExportRun, ImportPhase, describe_report},
    },
    store::{Store, StoreEvent},
    ui::{
        dialogs, filmstrip, left_panel, library,
        right_panel::{self, RightPanelInput},
        theme, toolbar, top_bar,
        viewport::{self, ViewportMemory},
    },
    wayland_drop::{self, DropInbox},
};
use eframe::egui::{self, CentralPanel, Color32, Id, LayerId, Order, Rect, Ui, ViewportCommand, pos2};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};
use unlight_catalog::{Catalog, Filter, FolderFilter, LibraryRow, VariantId};
use unlight_engine::{EngineError, GpuContext};
use unlight_merge::{HdrOptions, PanoramaOptions};
use unlight_raw::MergeKind;
use unlight_recipe::Recipe;

/// A folder picked in the system chooser, and which dialog asked for it.
type ChosenFolder = (FolderUse, PathBuf);

/// What outlives a window, for reopening on a new device after losing the GPU.
#[derive(Default)]
pub struct Handoff {
    state: Mutex<Option<AppState>>,
    pub drops: DropInbox,
}

impl Handoff {
    pub fn take_state(&self) -> Option<AppState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    fn store_state(&self, state: AppState) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = Some(state);
    }
}

/// How often to look in on a merge. Progress moves in seconds, so this needn't be often.
const MERGE_POLL: Duration = Duration::from_millis(200);

pub struct App {
    pub state: AppState,
    pub store: Arc<Store>,
    pub darkroom: Darkroom,
    pub renderer: ViewRenderer,
    pub viewport: ViewportMemory,
    context: Arc<GpuContext>,
    handoff: &'static Handoff,
    focused: Option<VariantId>,
    preloaded: Option<VariantId>,
    /// Set once the GPU is gone. Nothing may reach the painter from then on: egui-wgpu panics
    /// on a lost device rather than returning an error.
    closing: bool,
    exporter: Option<Exporter>,
    merger: Option<Merger>,
    color_jobs: ColorJobs,
    /// A scan's root, until the folder list that includes it arrives.
    import_root: Option<PathBuf>,
    chosen_folders: (Sender<ChosenFolder>, Receiver<ChosenFolder>),
    ctx: egui::Context,
}

/// Reads everything about a variant's edit from the catalog.
pub fn load_edit(catalog: &Catalog, variant: VariantId) -> unlight_catalog::Result<PhotoEdit> {
    let stored = catalog.variant(variant)?;
    let steps = catalog.history(variant)?;
    let recipes = catalog.history_recipes(variant)?;
    let entries = steps
        .steps
        .into_iter()
        .zip(recipes)
        .map(|(step, (seq, recipe))| HistoryEntry {
            seq,
            label: step.label,
            recipe,
        })
        .collect();
    let history = History::restored(entries, steps.position, stored.recipe.clone());
    Ok(PhotoEdit::restored(
        variant,
        stored.recipe,
        history,
        catalog.snapshots(variant)?,
    ))
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        context: Arc<GpuContext>,
        store: Arc<Store>,
        state: AppState,
        handoff: &'static Handoff,
    ) -> Self {
        theme::install(&cc.egui_ctx);
        handoff.drops.set_waker(&cc.egui_ctx);
        wayland_drop::listen(&handoff.drops, cc);
        let waker = cc.egui_ctx.clone();
        store.set_waker(Arc::new(move || waker.request_repaint()));
        store.request_lists();
        let waker = cc.egui_ctx.clone();
        let color_jobs = ColorJobs::new(Arc::new(move || waker.request_repaint()));
        Self {
            darkroom: Darkroom::new(context.clone(), store.previews.clone(), &cc.egui_ctx),
            renderer: ViewRenderer::default(),
            viewport: ViewportMemory::default(),
            state,
            store,
            context,
            handoff,
            focused: None,
            preloaded: None,
            closing: false,
            exporter: None,
            merger: None,
            color_jobs,
            import_root: None,
            chosen_folders: mpsc::channel(),
            ctx: cc.egui_ctx.clone(),
        }
    }

    pub fn context(&self) -> &GpuContext {
        &self.context
    }

    pub fn handoff(&self) -> &Handoff {
        self.handoff
    }

    fn send_requests(&mut self) {
        let library = &mut self.state.library;
        self.store.note_generation(library.generation());
        if library.take_count_request() {
            self.store
                .request_count(library.generation(), library.filter().clone());
        }
        for request in library.take_page_requests().into_iter().rev() {
            self.store.request_page(request);
        }
    }

    fn send_writes(&mut self) {
        for write in self.state.take_writes() {
            self.store.write(write);
        }
    }

    fn ensure_edit(&mut self, row: &LibraryRow) {
        if self.state.edits.contains_key(&row.variant) {
            return;
        }
        match load_edit(&self.store.catalog, row.variant) {
            Ok(edit) => self.state.store_edit(edit),
            Err(error) => self
                .state
                .report_error(format!("Couldn't load the edit: {error}")),
        }
    }

    fn refresh_details(&mut self, row: &LibraryRow) {
        let catalog = &self.store.catalog;
        self.state.photo = catalog.photo(row.photo).ok();
        self.state.keywords = catalog.keywords_of(row.variant).unwrap_or_default();
    }

    fn sync_selection(&mut self) {
        let Some(row) = self.state.focused().cloned() else {
            return;
        };
        if self.focused == Some(row.variant) {
            return;
        }
        self.ensure_edit(&row);
        self.refresh_details(&row);
        let recipe = self
            .state
            .edit()
            .map(|edit| edit.recipe.clone())
            .unwrap_or_default();
        let neighbours = self.state.preview_neighbours();
        self.darkroom.focus_on(&row, &recipe, &neighbours, &self.state);
        self.focused = Some(row.variant);
        self.renderer.clear_histogram(self.state.now);
        self.viewport.picking_white_balance = false;
        self.ctx.request_repaint();
    }

    fn recipe_of(&self, row: &LibraryRow) -> Recipe {
        let cached = self.state.edits.get(&row.variant).map(|edit| edit.recipe.clone());
        cached
            .or_else(|| {
                self.store
                    .catalog
                    .variant(row.variant)
                    .ok()
                    .map(|variant| variant.recipe)
            })
            .unwrap_or_default()
    }

    /// Starts decoding the neighbours once the current photo is done and the user isn't in
    /// the middle of something their GPU work would interrupt.
    fn sync_preload(&mut self) {
        let Some(row) = self.state.focused().cloned() else {
            return;
        };
        let done = self.darkroom.live(&row.path).is_some()
            || matches!(self.state.decode_state(), DecodeState::Failed(_));
        if !done || self.preloaded == Some(row.variant) || self.state.gesture_active {
            return;
        }
        let neighbours: Vec<(LibraryRow, Recipe)> = self
            .state
            .decode_neighbours()
            .into_iter()
            .map(|neighbour| {
                let recipe = self.recipe_of(&neighbour);
                (neighbour, recipe)
            })
            .collect();
        self.darkroom.preload(&row.path, &neighbours, &self.state);
        self.preloaded = Some(row.variant);
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() || egui::Popup::is_any_open(ctx) {
            return;
        }
        if let Some(focused) = ctx.memory(|memory| memory.focused()) {
            ctx.memory_mut(|memory| memory.surrender_focus(focused));
        }
        let pressed: Vec<Action> = ctx.input_mut(|input| {
            BINDINGS
                .iter()
                .filter(|(modifiers, key, _)| input.consume_key(*modifiers, *key))
                .map(|(_, _, action)| *action)
                .collect()
        });
        let dialog_open = self.state.dialog.is_some();
        for action in pressed {
            if !dialog_open || matches!(action, Action::Confirm | Action::Cancel) {
                self.perform(action);
            }
        }
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::ZoomIn => self.step_zoom(1.0),
            Action::ZoomOut => self.step_zoom(-1.0),
            Action::Cancel if self.viewport.picking_white_balance => {
                self.viewport.picking_white_balance = false;
            }
            other => self.state.apply(other),
        }
    }

    fn step_zoom(&mut self, steps: f32) {
        let Some(canvas) = self.viewport.canvas else {
            return;
        };
        let output = self.viewport.output;
        let ratio = self.state.view.step_ratio(steps, canvas, output);
        let center = [canvas[0] * 0.5, canvas[1] * 0.5];
        self.state.view.zoom_about(ratio, center, canvas, output);
        self.state.view.remember_ratio();
    }

    #[cfg(feature = "ai")]
    fn pump_ai_tools(&mut self, ctx: &egui::Context) {
        let path = self.state.focused().map(|row| row.path.clone());
        let live = path.as_ref().and_then(|path| self.darkroom.live_mut(path));
        crate::ai_tools::pump(&mut self.state, live);
        if self.state.ai.is_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn run_effects(&mut self, ctx: &egui::Context) {
        for effect in std::mem::take(&mut self.state.effects) {
            match effect {
                Effect::SetFullscreen(fullscreen) => {
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(fullscreen));
                }
                Effect::ChooseFolder(purpose) => self.choose_folder(purpose),
                Effect::StartImport { root, .. } => {
                    self.import_root = Some(root.clone());
                    self.store.start_scan(root);
                }
                Effect::CancelImport => self.store.cancel_scan(),
                Effect::StartExport => self.start_export(),
                Effect::CancelExport => {
                    if let Some(exporter) = &self.exporter {
                        exporter.cancel();
                    }
                }
                Effect::StartMerge => self.start_merge(),
                Effect::CancelMerge => {
                    if let Some(merger) = &self.merger {
                        merger.cancel();
                    }
                }
                Effect::ConstrainCrop => self.constrain_crop(),
                Effect::AutoTone => self.auto_tone(),
                Effect::RescanProfiles => self.color_jobs.rescan_profiles(),
                Effect::OpenProfileFolder => self.open_profile_folder(),
                Effect::SearchProofProfiles => self.color_jobs.search_proof_profiles(),
                Effect::ChooseProofProfile => self.color_jobs.choose_proof_profile(),
                Effect::SelectAll => {
                    let library = &self.state.library;
                    let filter = library.filter().clone();
                    self.store
                        .request_select_all(library.generation(), filter, library.sort());
                }
            }
        }
    }

    /// Fills in the Basic sliders from the photo, as one step of history.
    fn auto_tone(&mut self) {
        let Some(row) = self.state.focused().cloned() else {
            return;
        };
        let Some(recipe) = self.state.edit().map(PhotoEdit::effective) else {
            return;
        };
        let Some(live) = self.darkroom.live_mut(&row.path) else {
            self.state.notify("The photo is still loading");
            return;
        };
        match live.session.auto_tone(&recipe) {
            Ok(tone) => {
                if let Some(edit) = self.state.edit_mut() {
                    tone.apply_to(&mut edit.recipe.basic);
                    edit.note("Auto Tone", "", crate::state::edit::Step::New);
                }
            }
            Err(EngineError::DeviceLost(_)) => self.state.device_lost = true,
            Err(error) => self.state.report_error(format!("Auto tone failed: {error}")),
        }
    }

    fn open_profile_folder(&mut self) {
        match color_jobs::open_profile_folder() {
            Ok(directory) => self.state.notify(format!("Camera profiles live in {}", directory.display())),
            Err(message) => self.state.notify(message),
        }
    }

    fn pump_color_jobs(&mut self) {
        while let Some(event) = self.color_jobs.try_next() {
            match event {
                ColorEvent::Baked { key, outcome } => {
                    if let Err(message) = &outcome {
                        self.state.report_error(format!("Couldn't use that profile: {message}"));
                    }
                    self.state.proof.finish_bake(key, outcome);
                }
                ColorEvent::ProofProfiles(found) => {
                    found.into_iter().for_each(|profile| self.state.proof.add_profile(profile));
                }
                ColorEvent::ChosenProofProfile(Ok(profile)) => {
                    self.state.proof.choice = ProofChoice::Icc(profile.path.clone());
                    self.state.proof.add_profile(profile);
                }
                ColorEvent::ChosenProofProfile(Err(message)) => {
                    self.state.report_error(format!("Couldn't read that profile: {message}"));
                }
                ColorEvent::ProfilesRescanned { found, rejected } => {
                    self.renderer.invalidate();
                    let refused = if rejected > 0 { format!(", {rejected} couldn't be read") } else { String::new() };
                    let noun = if found == 1 { "profile" } else { "profiles" };
                    self.state.notify(format!("{found} camera {noun} found{refused}"));
                }
            }
        }
        if let Some(key) = self.state.proof.take_bake() {
            self.color_jobs.bake(key);
        }
    }

    fn constrain_crop(&mut self) {
        let Some(row) = self.state.focused().cloned() else {
            return;
        };
        let Some(recipe) = self.state.edit().map(PhotoEdit::effective) else {
            return;
        };
        let Some(live) = self.darkroom.live_mut(&row.path) else {
            return;
        };
        let tool = self.state.crop_tool;
        let sensor = live.sensor_aspect();
        let shape = tool.lock.ratio(sensor, tool.portrait).unwrap_or(sensor);
        match live.session.largest_crop(&recipe, shape) {
            Ok(crop) => {
                if let Some(edit) = self.state.edit_mut() {
                    edit.recipe.crop = unlight_recipe::Crop {
                        angle: edit.recipe.crop.angle,
                        ..crop
                    };
                    edit.note("Constrain Crop", "", crate::state::edit::Step::New);
                }
            }
            Err(error) => self
                .state
                .report_error(format!("Couldn't constrain the crop: {error}")),
        }
    }

    fn start_export(&mut self) {
        let Some(form) = &self.state.export_form else {
            return;
        };
        let variants = self.state.library.selection();
        let job = ExportJob {
            variants: variants.clone(),
            settings: form.settings.clone(),
            directory: PathBuf::from(form.directory.trim()),
        };
        let waker = self.ctx.clone();
        let wake = Arc::new(move || waker.request_repaint());
        match Exporter::start(self.store.catalog.clone(), job, wake) {
            Ok(exporter) => {
                self.exporter = Some(exporter);
                self.state.export_run = Some(ExportRun {
                    total: variants.len(),
                    ..ExportRun::default()
                });
            }
            Err(error) => self.state.report_error(format!("{error:#}")),
        }
    }

    fn pump_export(&mut self) {
        let Some(exporter) = &self.exporter else {
            return;
        };
        let Some(run) = &mut self.state.export_run else {
            return;
        };
        let mut finished = false;
        while let Some(event) = exporter.try_next() {
            match event {
                ExportEvent::Started { name } => run.current = name,
                ExportEvent::Finished { skipped, .. } => {
                    run.done += 1;
                    run.skipped += usize::from(skipped);
                }
                ExportEvent::Failed(failure) => {
                    run.done += 1;
                    run.failures.push(failure);
                }
                ExportEvent::Done { cancelled } => {
                    run.finished = true;
                    run.cancelled = cancelled;
                    finished = true;
                }
            }
        }
        if finished {
            self.exporter = None;
            let summary = run.summary();
            self.state.notify(summary);
        }
    }

    fn start_merge(&mut self) {
        let Some(form) = self.state.merge_form.clone() else {
            return;
        };
        let job = match form.kind {
            MergeKind::Hdr => MergeJob::Hdr {
                sources: form.sources,
                options: HdrOptions { auto_align: form.auto_align, deghost: form.deghost },
            },
            MergeKind::Panorama => MergeJob::Panorama {
                sources: form.sources,
                options: PanoramaOptions {
                    projection: form.projection,
                    boundary_warp: form.boundary_warp / 100.0,
                    auto_crop: form.auto_crop,
                    max_edge: self.context.device.limits().max_texture_dimension_2d,
                },
            },
        };
        let waker = self.ctx.clone();
        match Merger::start(job, Arc::new(move || waker.request_repaint())) {
            Ok(merger) => self.merger = Some(merger),
            Err(error) => {
                self.state.merge_run = None;
                self.state.report_error(format!("{error:#}"));
            }
        }
    }

    fn pump_merge(&mut self) {
        let Some(merger) = &self.merger else {
            return;
        };
        if let Some(run) = &mut self.state.merge_run {
            let progress = merger.state();
            run.stage = progress.stage;
            run.fraction = progress.fraction;
        }
        let Some(outcome) = merger.try_finish() else {
            self.ctx.request_repaint_after(MERGE_POLL);
            return;
        };
        self.merger = None;
        match outcome {
            MergeOutcome::Finished(path) => {
                self.finish_dialog();
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                self.state.notify(format!("Merged into {name}"));
                self.state.open_paths(&[path]);
            }
            MergeOutcome::Cancelled => {
                self.finish_dialog();
                self.state.notify("Merge cancelled");
            }
            MergeOutcome::Failed(message) => {
                self.state.report_error(format!("Merge failed: {message}"));
                if let Some(run) = &mut self.state.merge_run {
                    run.failure = Some(message);
                }
                self.state.dialog = Some(Dialog::PhotoMerge);
            }
        }
    }

    fn finish_dialog(&mut self) {
        self.state.merge_run = None;
        self.state.merge_form = None;
        if self.state.dialog == Some(Dialog::PhotoMerge) {
            self.state.dialog = None;
        }
    }

    /// The portal dialog blocks until closed, so it runs beside the UI.
    fn choose_folder(&self, purpose: FolderUse) {
        let sender = self.chosen_folders.0.clone();
        let waker = self.ctx.clone();
        let spawned = thread::Builder::new()
            .name("unlight-choose".into())
            .spawn(move || {
                if let Some(path) = rfd::FileDialog::new().pick_folder()
                    && sender.send((purpose, path)).is_ok()
                {
                    waker.request_repaint();
                }
            });
        if let Err(error) = spawned {
            eprintln!("couldn't open the folder chooser: {error}");
        }
    }

    fn take_chosen_folders(&mut self) {
        while let Ok((purpose, path)) = self.chosen_folders.1.try_recv() {
            let text = path.display().to_string();
            match purpose {
                FolderUse::Import => self.state.import.folder = text,
                FolderUse::Export => {
                    if let Some(form) = &mut self.state.export_form {
                        form.directory = text;
                    }
                }
            }
        }
    }

    fn take_dropped_paths(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect()
        });
        let mut opened = vec![dropped, self.handoff.drops.take()];
        opened.retain(|paths| !paths.is_empty());
        for paths in opened {
            let usable: Vec<PathBuf> = paths
                .into_iter()
                .filter(|path| path.is_dir() || is_raw(path))
                .collect();
            if usable.is_empty() {
                self.state.report_error("That isn't a raw file or a folder");
            } else {
                self.state.open_paths(&usable);
            }
        }
    }

    /// Shows a scan's folder and the file that was opened, once the folder is in the list.
    fn land_on_import(&mut self) {
        let Some(root) = &self.import_root else {
            return;
        };
        // A folder list can arrive mid-scan, when the file being looked for isn't in yet.
        if !matches!(self.state.import.phase, ImportPhase::Finished(_)) {
            return;
        }
        let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
        let found = self
            .state
            .lists
            .folders
            .iter()
            .find(|folder| folder.path == canonical)
            .map(|folder| folder.id);
        if let Some(folder) = found {
            self.import_root = None;
            let filter = Filter {
                folder: Some(FolderFilter {
                    folder,
                    include_subfolders: true,
                }),
                ..Filter::default()
            };
            self.state.library.set_filter(filter);
            if let Some(path) = self.state.import.focus.take() {
                self.state
                    .library
                    .select_path(path.canonicalize().unwrap_or(path));
            }
        }
    }

    fn finish_scan(&mut self, outcome: Result<unlight_catalog::ScanReport, String>) {
        match outcome {
            Ok(report) => {
                if self.state.dialog.is_none() {
                    let summary = describe_report(&report).join(", ");
                    self.state.notify(summary);
                }
                self.state.import.phase = ImportPhase::Finished(report);
            }
            Err(message) => {
                self.state.report_error(format!("Import failed: {message}"));
                self.state.import.phase = ImportPhase::Failed(message);
                self.import_root = None;
            }
        }
        self.store.request_lists();
        self.state.library.refresh();
        self.state.flush_edits();
        self.send_writes();
        self.state.edits.retain(|id, _| self.focused == Some(*id));
    }

    fn pump_store(&mut self) {
        while let Some(event) = self.store.try_next() {
            match event {
                StoreEvent::Page {
                    generation,
                    index,
                    rows,
                } => {
                    self.state.library.apply_page(generation, index, rows);
                }
                StoreEvent::Count { generation, count } => self.state.library.apply_count(generation, count),
                StoreEvent::Lists(lists) => {
                    self.state.lists = *lists;
                    self.land_on_import();
                }
                StoreEvent::Snapshots { variant, snapshots } => {
                    if let Some(edit) = self.state.edits.get_mut(&variant) {
                        edit.snapshots = snapshots;
                    }
                }
                StoreEvent::VariantCreated(variant) => {
                    self.state.library.refresh();
                    self.state.library.select_variant(variant);
                    self.state.notify("Virtual copy created");
                }
                StoreEvent::MetadataChanged => {
                    if self.state.library.depends_on_metadata() {
                        self.state.library.refresh();
                    }
                }
                StoreEvent::KeywordsChanged => {
                    if let Some(row) = self.state.focused().cloned() {
                        self.refresh_details(&row);
                    }
                    if self.state.library.filter().keyword.is_some() {
                        self.state.library.refresh();
                    }
                }
                StoreEvent::RecipesChanged(variants) => {
                    self.state.flush_edits();
                    self.send_writes();
                    self.state.edits.retain(|id, _| !variants.contains(id));
                    self.state.library.refresh();
                    if let Some(row) = self.state.focused().cloned() {
                        self.ensure_edit(&row);
                    }
                }
                StoreEvent::SelectAll { generation, ids } => {
                    if generation == self.state.library.generation() {
                        self.state.library.set_selection(ids);
                    }
                }
                StoreEvent::ScanProgress(progress) => {
                    if self.state.import.is_running() {
                        self.state.import.phase = ImportPhase::Running(Some(progress));
                    }
                }
                StoreEvent::ScanFinished(outcome) => self.finish_scan(outcome),
                StoreEvent::Failed(message) => self.state.report_error(message),
            }
        }
    }

    /// When the GPU goes away, everything unsaved goes to the catalog and the state is handed
    /// over, and `run` opens a new window on a new device.
    fn handle_device_loss(&mut self, ctx: &egui::Context) -> bool {
        if self.closing {
            return true;
        }
        if !self.state.device_lost && !self.context.is_lost() {
            return false;
        }
        self.closing = true;
        self.state.flush_edits();
        self.send_writes();
        let mut state = std::mem::take(&mut self.state);
        state.device_lost = false;
        state.notify("The GPU was reset. Your edits are safe.");
        state.decode.clear();
        self.handoff.store_state(state);
        ctx.send_viewport_cmd(ViewportCommand::Close);
        true
    }

    /// Shows that the window will take what's being dragged over it.
    fn drop_overlay(&self, ui: &Ui) {
        let ctx = ui.ctx();
        let dragging =
            self.handoff.drops.hovering() || ctx.input(|input| !input.raw.hovered_files.is_empty());
        if !dragging {
            return;
        }
        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::unique("drop overlay")));
        painter.rect_filled(screen, 0.0, Color32::from_black_alpha(120));
        painter.rect_stroke(
            screen.shrink(8.0),
            8.0,
            (2.0, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
        painter.text(
            screen.center(),
            egui::Align2::CENTER_CENTER,
            "Drop to import",
            theme::medium(18.0),
            theme::TEXT,
        );
    }

    fn lights_overlay(&self, ui: &Ui) {
        let alpha = match self.state.lights {
            Lights::On => return,
            Lights::Dim => 190,
            Lights::Off => 255,
        };
        let screen = ui.ctx().content_rect();
        let shade = Color32::from_black_alpha(alpha);
        let painter = ui
            .ctx()
            .layer_painter(LayerId::new(Order::Foreground, Id::unique("lights out")));
        match self.viewport.photo_rect {
            Some(photo) => {
                let bands = [
                    Rect::from_min_max(screen.min, pos2(screen.right(), photo.top())),
                    Rect::from_min_max(pos2(screen.left(), photo.bottom()), screen.max),
                    Rect::from_min_max(
                        pos2(screen.left(), photo.top()),
                        pos2(photo.left(), photo.bottom()),
                    ),
                    Rect::from_min_max(
                        pos2(photo.right(), photo.top()),
                        pos2(screen.right(), photo.bottom()),
                    ),
                ];
                for band in bands {
                    painter.rect_filled(band, 0.0, shade);
                }
            }
            None => {
                painter.rect_filled(screen, 0.0, shade);
            }
        }
    }

    fn develop(&mut self, ui: &mut Ui, frame: &eframe::Frame) {
        let path = self.state.focused().map(|row| row.path.clone());
        let live = path.as_ref().and_then(|path| self.darkroom.live(path));
        let histogram = self.renderer.histogram().filter(|_| live.is_some());
        let input = RightPanelInput {
            live,
            histogram,
            picking_white_balance: &mut self.viewport.picking_white_balance,
        };
        right_panel::show(ui, &mut self.state, input);
        left_panel::show(ui, &mut self.state);
        CentralPanel::no_frame().show(ui, |ui| {
            let scale = self
                .viewport
                .canvas
                .map(|canvas| self.state.view.scale(canvas, self.viewport.output));
            toolbar::show(ui, &mut self.state, scale);
            CentralPanel::no_frame().show(ui, |ui| {
                viewport::show(ui, self, frame.wgpu_render_state());
            });
        });
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.state.now = ctx.input(|input| input.time);
        if self.handle_device_loss(ctx) {
            return;
        }
        self.pump_store();
        self.darkroom.pump(ctx, &mut self.state);
        self.pump_export();
        self.pump_merge();
        self.pump_color_jobs();
        #[cfg(feature = "ai")]
        self.pump_ai_tools(ctx);
        self.take_dropped_paths(ctx);
        self.take_chosen_folders();
        self.handle_shortcuts(ctx);
        self.run_effects(ctx);
        self.send_requests();
        self.sync_selection();
        self.sync_preload();
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        if self.closing {
            return;
        }
        let ctx = ui.ctx().clone();
        if self.state.panels.top {
            top_bar::show(ui, &mut self.state);
        }
        filmstrip::show(ui, &mut self.state, &mut self.darkroom);
        match self.state.module {
            Module::Develop => self.develop(ui, frame),
            Module::Library => library::show(ui, self),
        }
        if self.state.device_lost {
            ctx.request_discard("the GPU was lost while rendering");
            self.handle_device_loss(&ctx);
            return;
        }
        dialogs::show(ui, &mut self.state);
        self.lights_overlay(ui);
        self.drop_overlay(ui);

        self.state.gesture_active = ctx.input(|input| input.pointer.any_down());
        self.state.end_frame();
        self.send_writes();
        self.send_requests();
        self.sync_selection();
        self.darkroom.end_frame();
        if let Some(seconds) = self.state.status_time_left() {
            ctx.request_repaint_after(Duration::from_secs_f64(seconds + 0.02));
        }
        if self.state.is_settling() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// Saves everything on the way out, however the window went away.
impl Drop for App {
    fn drop(&mut self) {
        self.state.flush_edits();
        self.send_writes();
        if !self.closing {
            let _ = self.state.session().save(self.store.session_file());
        }
    }
}
