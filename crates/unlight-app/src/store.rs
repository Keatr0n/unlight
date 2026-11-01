//! The catalog from the app's side. Writes go through one thread in order, so saving never
//! holds up a frame and a later write can't overtake an earlier one. Page queries run on
//! another, and scans and sidecars each have their own.

use crate::{
    action::turn_label,
    state::{CatalogLists, library::PageRequest, write::Write},
};
use anyhow::{Context, Result};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};
use unlight_catalog::{
    CancelToken, Catalog, Filter, LibraryRow, Page, PreviewCache, ScanProgress, ScanReport, Snapshot, Sort,
    VariantId,
};
use unlight_recipe::Orientation;

const SCAN_PROGRESS_INTERVAL: Duration = Duration::from_millis(40);
/// Pause between sidecars, so a burst of them leaves the catalog free for edits.
const SIDECAR_GAP: Duration = Duration::from_millis(15);
const SELECT_ALL_PAGE: u64 = 4096;

pub struct StoreConfig {
    pub catalog: PathBuf,
    pub previews: PathBuf,
    /// How long after the last write to bring sidecars up to date. `None` leaves them alone.
    pub sidecar_delay: Option<Duration>,
}

/// Where the app keeps its files, under the XDG data directory.
pub fn data_directory() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")))
        .context("neither XDG_DATA_HOME nor HOME is set")?;
    Ok(base.join("unlight"))
}

impl StoreConfig {
    pub fn at_home() -> Result<Self> {
        let directory = data_directory()?;
        std::fs::create_dir_all(&directory).with_context(|| format!("creating {}", directory.display()))?;
        Ok(Self {
            catalog: directory.join("catalog.sqlite"),
            previews: unlight_catalog::default_cache_dir()?,
            sidecar_delay: Some(Duration::from_secs(3)),
        })
    }
}

pub enum StoreEvent {
    Page {
        generation: u64,
        index: u64,
        rows: Vec<LibraryRow>,
    },
    Count {
        generation: u64,
        count: u64,
    },
    Lists(Box<CatalogLists>),
    Snapshots {
        variant: VariantId,
        snapshots: Vec<Snapshot>,
    },
    VariantCreated(VariantId),
    /// These variants are gone from the catalog, and every copy of a removed photo with them.
    VariantsRemoved(Vec<VariantId>),
    FolderRemoved,
    /// Flags, ratings or labels changed, which may move rows in or out of a filtered view.
    MetadataChanged,
    /// Keywords changed on these variants.
    KeywordsChanged,
    /// Recipes changed behind the app's back, by pasting onto a selection.
    RecipesChanged(Vec<VariantId>),
    SelectAll {
        generation: u64,
        ids: Vec<VariantId>,
    },
    ScanProgress(ScanProgress),
    ScanFinished(std::result::Result<ScanReport, String>),
    Failed(String),
}

type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
struct Outbox {
    events: Sender<StoreEvent>,
    waker: Arc<Mutex<Waker>>,
}

impl Outbox {
    fn send(&self, event: StoreEvent) {
        if self.events.send(event).is_ok() {
            let waker = self.waker.lock().unwrap_or_else(PoisonError::into_inner).clone();
            waker();
        }
    }
}

enum Job {
    Write(Box<Write>),
    Flush(Sender<()>),
}

enum Read {
    Page(PageRequest),
    Count {
        generation: u64,
        filter: Filter,
    },
    Lists,
    SelectAll {
        generation: u64,
        filter: Filter,
        sort: Sort,
    },
}

pub struct Store {
    pub catalog: Arc<Catalog>,
    pub previews: Arc<PreviewCache>,
    writes: Option<Sender<Job>>,
    reads: Option<Sender<Read>>,
    events: Mutex<Receiver<StoreEvent>>,
    outbox: Outbox,
    newest_generation: Arc<AtomicU64>,
    scan_cancel: Mutex<Option<CancelToken>>,
    stopping: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
    scans: Mutex<Vec<thread::JoinHandle<()>>>,
    session_file: PathBuf,
}

fn load_lists(catalog: &Catalog) -> unlight_catalog::Result<CatalogLists> {
    Ok(CatalogLists {
        folders: catalog.folders()?,
        collections: catalog.collections()?,
        keywords: catalog.keywords()?,
        presets: catalog.presets()?,
        cameras: catalog.cameras()?,
        lenses: catalog.lenses()?,
        sidecar_conflicts: catalog.sidecar_conflicts()?,
    })
}

fn select_all(catalog: &Catalog, filter: &Filter, sort: Sort) -> unlight_catalog::Result<Vec<VariantId>> {
    let mut ids = Vec::new();
    loop {
        let page = Page {
            offset: ids.len() as u64,
            limit: SELECT_ALL_PAGE,
        };
        let rows = catalog.query(filter, sort, page)?;
        let finished = (rows.len() as u64) < SELECT_ALL_PAGE;
        ids.extend(rows.into_iter().map(|row| row.variant));
        if finished {
            return Ok(ids);
        }
    }
}

fn read_once(
    catalog: &Catalog,
    read: Read,
    outbox: &Outbox,
    newest: &AtomicU64,
) -> unlight_catalog::Result<()> {
    let stale = |generation: u64| generation < newest.load(Ordering::SeqCst);
    match read {
        Read::Page(request) if !stale(request.generation) => {
            let rows = catalog.query(&request.filter, request.sort, request.page())?;
            outbox.send(StoreEvent::Page {
                generation: request.generation,
                index: request.index,
                rows,
            });
        }
        Read::Count { generation, filter } if !stale(generation) => {
            outbox.send(StoreEvent::Count {
                generation,
                count: catalog.count(&filter)?,
            });
        }
        Read::SelectAll {
            generation,
            filter,
            sort,
        } if !stale(generation) => {
            outbox.send(StoreEvent::SelectAll {
                generation,
                ids: select_all(catalog, &filter, sort)?,
            });
        }
        Read::Lists => outbox.send(StoreEvent::Lists(Box::new(load_lists(catalog)?))),
        _ => {}
    }
    Ok(())
}

/// Removes what was asked for and returns everything that went with it.
fn remove_variants(catalog: &Catalog, variants: &[VariantId]) -> unlight_catalog::Result<Vec<VariantId>> {
    let mut photos = HashSet::new();
    let mut copies = Vec::new();
    for id in variants {
        let variant = catalog.variant(*id)?;
        if variant.copy_name.is_some() {
            copies.push(variant);
        } else {
            photos.insert(variant.photo);
        }
    }
    let photos: Vec<_> = photos.into_iter().collect();
    let mut removed = Vec::new();
    for photo in &photos {
        removed.extend(catalog.variants_of(*photo)?.iter().map(|variant| variant.id));
    }
    for copy in copies.iter().filter(|copy| !photos.contains(&copy.photo)) {
        catalog.delete_variant(copy.id)?;
        removed.push(copy.id);
    }
    catalog.remove_photos(&photos)?;
    Ok(removed)
}

fn add_keywords(catalog: &Catalog, variants: &[VariantId], names: &[String]) -> unlight_catalog::Result<()> {
    for name in names {
        let keyword = catalog.create_keyword(None, name)?;
        catalog.add_keyword(variants, keyword)?;
    }
    Ok(())
}

fn paste_onto(
    catalog: &Catalog,
    variants: &[VariantId],
    settings: &crate::state::clipboard::CopiedSettings,
) -> unlight_catalog::Result<()> {
    for variant in variants {
        let mut recipe = catalog.variant(*variant)?.recipe;
        settings.paste_onto(&mut recipe);
        catalog.save_recipe(*variant, &recipe, "Paste Settings")?;
    }
    Ok(())
}

fn turn_variants(catalog: &Catalog, variants: &[VariantId], turn: Orientation) -> unlight_catalog::Result<()> {
    for variant in variants {
        let mut recipe = catalog.variant(*variant)?.recipe;
        recipe.orientation = recipe.orientation.then(turn);
        catalog.save_recipe(*variant, &recipe, turn_label(turn))?;
    }
    Ok(())
}

/// Applies one write. Returns what the UI should hear about it.
fn apply(catalog: &Catalog, write: Write) -> unlight_catalog::Result<Vec<StoreEvent>> {
    let lists =
        || -> unlight_catalog::Result<StoreEvent> { Ok(StoreEvent::Lists(Box::new(load_lists(catalog)?))) };
    Ok(match write {
        Write::SaveRecipe {
            variant,
            recipe,
            label,
        } => {
            catalog.save_recipe(variant, &recipe, &label)?;
            Vec::new()
        }
        Write::Undo(variant) => {
            catalog.undo(variant)?;
            Vec::new()
        }
        Write::Redo(variant) => {
            catalog.redo(variant)?;
            Vec::new()
        }
        Write::GoToStep(variant, seq) => {
            catalog.go_to_history_step(variant, seq)?;
            Vec::new()
        }
        Write::CreateSnapshot { variant, name } => {
            catalog.create_snapshot(variant, &name)?;
            vec![StoreEvent::Snapshots {
                variant,
                snapshots: catalog.snapshots(variant)?,
            }]
        }
        Write::DeleteSnapshot { variant, snapshot } => {
            catalog.delete_snapshot(snapshot)?;
            vec![StoreEvent::Snapshots {
                variant,
                snapshots: catalog.snapshots(variant)?,
            }]
        }
        Write::SetFlag(ids, flag) => {
            catalog.set_flag(&ids, flag)?;
            vec![StoreEvent::MetadataChanged]
        }
        Write::SetRating(ids, rating) => {
            catalog.set_rating(&ids, rating)?;
            vec![StoreEvent::MetadataChanged]
        }
        Write::SetLabel(ids, label) => {
            catalog.set_label(&ids, label)?;
            vec![StoreEvent::MetadataChanged]
        }
        Write::VirtualCopy(source) => {
            vec![StoreEvent::VariantCreated(catalog.create_virtual_copy(source)?)]
        }
        Write::TurnVariants { variants, turn } => {
            turn_variants(catalog, &variants, turn)?;
            vec![StoreEvent::RecipesChanged(variants)]
        }
        Write::RemoveVariants(variants) => {
            let removed = remove_variants(catalog, &variants)?;
            vec![lists()?, StoreEvent::VariantsRemoved(removed)]
        }
        Write::RemoveFolder(folder) => {
            catalog.remove_folder(folder)?;
            vec![lists()?, StoreEvent::FolderRemoved]
        }
        Write::PasteSettings { variants, settings } => {
            paste_onto(catalog, &variants, &settings)?;
            vec![StoreEvent::RecipesChanged(variants)]
        }
        Write::AddKeywords { variants, names } => {
            add_keywords(catalog, &variants, &names)?;
            vec![lists()?, StoreEvent::KeywordsChanged]
        }
        Write::RemoveKeyword { variants, keyword } => {
            catalog.remove_keyword(&variants, keyword)?;
            vec![StoreEvent::KeywordsChanged, StoreEvent::MetadataChanged]
        }
        Write::CreateCollection { name, variants } => {
            let id = catalog.create_collection(&name)?;
            catalog.add_to_collection(id, &variants)?;
            vec![lists()?]
        }
        Write::AddToCollection { collection, variants } => {
            catalog.add_to_collection(collection, &variants)?;
            vec![StoreEvent::MetadataChanged]
        }
        Write::RemoveFromCollection { collection, variants } => {
            catalog.remove_from_collection(collection, &variants)?;
            vec![StoreEvent::MetadataChanged]
        }
        Write::SaveSmartCollection {
            existing,
            name,
            rules,
        } => {
            match existing {
                Some(id) => {
                    catalog.rename_collection(id, &name)?;
                    catalog.set_smart_rules(id, &rules)?;
                }
                None => {
                    catalog.create_smart_collection(&name, &rules)?;
                }
            }
            vec![lists()?, StoreEvent::MetadataChanged]
        }
        Write::DeleteCollection(id) => {
            catalog.delete_collection(id)?;
            vec![lists()?]
        }
        Write::SavePreset {
            group,
            name,
            settings,
        } => {
            catalog.save_preset(&group, &name, &settings)?;
            vec![lists()?]
        }
        Write::DeletePreset(id) => {
            catalog.delete_preset(id)?;
            vec![lists()?]
        }
        Write::ResolveSidecar { photo, keep_catalog } => {
            if keep_catalog {
                catalog.write_sidecar(photo)?;
            } else {
                catalog.read_sidecar(photo)?;
            }
            let variants = catalog
                .variants_of(photo)?
                .into_iter()
                .map(|variant| variant.id)
                .collect();
            vec![lists()?, StoreEvent::RecipesChanged(variants)]
        }
    })
}

/// Brings sidecars up to date whenever the catalog has been quiet for `delay`. Whatever is
/// left when the app closes stays marked dirty, so the next run finishes it.
fn sync_sidecars(
    catalog: &Catalog,
    wake: &Receiver<()>,
    delay: Duration,
    stopping: &AtomicBool,
    outbox: &Outbox,
) {
    let mut wait = delay;
    loop {
        match wake.recv_timeout(wait) {
            Ok(()) => {
                wait = delay;
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => wait = Duration::from_secs(3600),
        }
        let Ok(dirty) = catalog.dirty_sidecars() else {
            continue;
        };
        // Writing would overwrite someone else's change to the file, so those wait for the user.
        let conflicts = catalog.sidecar_conflicts().unwrap_or_default();
        for photo in dirty.into_iter().filter(|photo| !conflicts.contains(photo)) {
            if stopping.load(Ordering::SeqCst) {
                return;
            }
            if let Err(error) = catalog.write_sidecar(photo) {
                outbox.send(StoreEvent::Failed(format!("Couldn't write a sidecar: {error}")));
            }
            thread::sleep(SIDECAR_GAP);
        }
    }
}

impl Store {
    pub fn open(config: &StoreConfig) -> Result<Self> {
        let catalog = Catalog::open(&config.catalog)
            .with_context(|| format!("opening the catalog at {}", config.catalog.display()))?;
        let catalog = Arc::new(catalog);
        let previews = Arc::new(
            PreviewCache::open(&config.previews, unlight_catalog::DEFAULT_CAPACITY_BYTES)
                .with_context(|| format!("opening the preview cache at {}", config.previews.display()))?,
        );
        let (event_sender, events) = mpsc::channel();
        let noop: Waker = Arc::new(|| {});
        let outbox = Outbox {
            events: event_sender,
            waker: Arc::new(Mutex::new(noop)),
        };
        let stopping = Arc::new(AtomicBool::new(false));
        let newest_generation = Arc::new(AtomicU64::new(0));
        let mut threads = Vec::new();

        let (sidecar_wake, sidecar_rx) = mpsc::channel();
        if let Some(delay) = config.sidecar_delay {
            let (catalog, outbox, stopping) = (catalog.clone(), outbox.clone(), stopping.clone());
            threads.push(spawn("unlight-sidecars", move || {
                sync_sidecars(&catalog, &sidecar_rx, delay, &stopping, &outbox);
            })?);
        }

        let (writes, write_rx) = mpsc::channel::<Job>();
        let (writer_catalog, writer_outbox) = (catalog.clone(), outbox.clone());
        threads.push(spawn("unlight-writer", move || {
            for job in write_rx {
                let write = match job {
                    Job::Write(write) => *write,
                    Job::Flush(done) => {
                        let _ = done.send(());
                        continue;
                    }
                };
                match apply(&writer_catalog, write) {
                    Ok(events) => events.into_iter().for_each(|event| writer_outbox.send(event)),
                    Err(error) => writer_outbox.send(StoreEvent::Failed(format!("Couldn't save: {error}"))),
                }
                let _ = sidecar_wake.send(());
            }
        })?);

        let (reads, read_rx) = mpsc::channel::<Read>();
        let (reader_catalog, reader_outbox, newest) =
            (catalog.clone(), outbox.clone(), newest_generation.clone());
        threads.push(spawn("unlight-reader", move || {
            for read in read_rx {
                if let Err(error) = read_once(&reader_catalog, read, &reader_outbox, &newest) {
                    reader_outbox.send(StoreEvent::Failed(format!("Couldn't read the catalog: {error}")));
                }
            }
        })?);

        Ok(Self {
            catalog,
            previews,
            writes: Some(writes),
            reads: Some(reads),
            events: Mutex::new(events),
            outbox,
            newest_generation,
            scan_cancel: Mutex::new(None),
            stopping,
            threads,
            scans: Mutex::default(),
            session_file: config.catalog.with_file_name("session.json"),
        })
    }

    pub fn session_file(&self) -> &Path {
        &self.session_file
    }

    /// Sets what to call when an event arrives, since an idle UI isn't polling.
    pub fn set_waker(&self, waker: Waker) {
        *self.outbox.waker.lock().unwrap_or_else(PoisonError::into_inner) = waker;
    }

    pub fn try_next(&self) -> Option<StoreEvent> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_recv()
            .ok()
    }

    pub fn write(&self, write: Write) {
        if let Some(writes) = &self.writes {
            let _ = writes.send(Job::Write(Box::new(write)));
        }
    }

    fn read(&self, read: Read) {
        if let Some(reads) = &self.reads {
            let _ = reads.send(read);
        }
    }

    /// Replies to anything older than this generation stop being worth computing.
    pub fn note_generation(&self, generation: u64) {
        self.newest_generation.fetch_max(generation, Ordering::SeqCst);
    }

    pub fn request_page(&self, request: PageRequest) {
        self.read(Read::Page(request));
    }

    pub fn request_count(&self, generation: u64, filter: Filter) {
        self.read(Read::Count { generation, filter });
    }

    pub fn request_lists(&self) {
        self.read(Read::Lists);
    }

    pub fn request_select_all(&self, generation: u64, filter: Filter, sort: Sort) {
        self.read(Read::SelectAll {
            generation,
            filter,
            sort,
        });
    }

    /// Starts a scan on its own thread.
    pub fn start_scan(&self, root: PathBuf) {
        let cancel = CancelToken::default();
        *self.scan_cancel.lock().unwrap_or_else(PoisonError::into_inner) = Some(cancel.clone());
        let (catalog, outbox) = (self.catalog.clone(), self.outbox.clone());
        let spawned = spawn("unlight-scan", move || {
            let mut last_sent = Instant::now();
            let report = catalog.scan(&root, &cancel, |progress| {
                if last_sent.elapsed() >= SCAN_PROGRESS_INTERVAL {
                    last_sent = Instant::now();
                    outbox.send(StoreEvent::ScanProgress(progress));
                }
            });
            outbox.send(StoreEvent::ScanFinished(
                report.map_err(|error| error.to_string()),
            ));
        });
        match spawned {
            Ok(thread) => self
                .scans
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(thread),
            Err(error) => self.outbox.send(StoreEvent::ScanFinished(Err(error.to_string()))),
        }
    }

    pub fn cancel_scan(&self) {
        if let Some(cancel) = self
            .scan_cancel
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            cancel.cancel();
        }
    }

    /// Waits for every write made so far to reach the catalog.
    pub fn flush(&self) {
        let (done, wait) = mpsc::channel();
        if let Some(writes) = &self.writes
            && writes.send(Job::Flush(done)).is_ok()
        {
            let _ = wait.recv();
        }
    }
}

fn spawn(name: &str, work: impl FnOnce() + Send + 'static) -> Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
        .context("starting a worker thread")
}

/// Everything queued is written before this returns, so closing the app loses nothing.
impl Drop for Store {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.cancel_scan();
        self.writes = None;
        self.reads = None;
        let scans = std::mem::take(&mut *self.scans.lock().unwrap_or_else(PoisonError::into_inner));
        for thread in self.threads.drain(..).chain(scans) {
            let _ = thread.join();
        }
    }
}
