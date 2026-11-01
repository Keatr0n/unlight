//! Live photos. Keeps a session for the current photo and its neighbours, previews and
//! thumbnails as textures, and turns finished background work into state.

use crate::{
    loader::{Job, Loaded, Loader, PreviewJob},
    state::{AppState, DecodeState, view::Orientation},
};
use eframe::egui;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};
use unlight_catalog::{LibraryRow, PhotoStatus, PreviewCache, RgbaImage};
use unlight_engine::{GpuContext, Session};
use unlight_raw::Metadata;
use unlight_recipe::Recipe;

/// Each session holds around a gigabyte of VRAM once zoomed, so keep only what's next.
const MAX_SESSIONS: usize = 3;
/// Enough for a full survey and the neighbours around it.
const MAX_PREVIEWS: usize = 24;
/// About 500 MB of texture at the grid's size.
const MAX_THUMBNAILS: usize = 1000;

pub struct LivePhoto {
    pub path: PathBuf,
    pub session: Session,
    pub metadata: Metadata,
    pub orientation: Orientation,
    /// The file's own white balance as temperature and tint.
    pub as_shot: (f32, f32),
    /// Tells two sessions apart in render caches.
    pub serial: u64,
}

impl LivePhoto {
    /// Width over height of the sensor image, before cropping or orientation.
    pub fn sensor_aspect(&self) -> f32 {
        self.session.source.width as f32 / self.session.source.height as f32
    }
}

struct Thumbnail {
    texture: egui::TextureHandle,
    last_shown: u64,
}

pub struct Darkroom {
    loader: Loader,
    live: Vec<LivePhoto>,
    decoding: HashSet<PathBuf>,
    previews: HashMap<PathBuf, egui::TextureHandle>,
    preview_order: VecDeque<PathBuf>,
    previews_wanted: HashSet<PathBuf>,
    thumbnails: HashMap<PathBuf, Thumbnail>,
    thumbnails_wanted: HashSet<PathBuf>,
    shown_rows: Vec<PreviewJob>,
    last_shown_paths: Vec<PathBuf>,
    frame: u64,
    next_serial: u64,
    focus: Option<PathBuf>,
}

fn to_texture(ctx: &egui::Context, name: &str, image: &RgbaImage) -> egui::TextureHandle {
    let size = [image.width as usize, image.height as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, &image.pixels);
    ctx.load_texture(name, color, egui::TextureOptions::LINEAR)
}

impl Darkroom {
    pub fn new(context: Arc<GpuContext>, previews: Arc<PreviewCache>, ctx: &egui::Context) -> Self {
        let waker = ctx.clone();
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || waker.request_repaint());
        Self {
            loader: Loader::new(context, previews, repaint),
            live: Vec::new(),
            decoding: HashSet::new(),
            previews: HashMap::new(),
            preview_order: VecDeque::new(),
            previews_wanted: HashSet::new(),
            thumbnails: HashMap::new(),
            thumbnails_wanted: HashSet::new(),
            shown_rows: Vec::new(),
            last_shown_paths: Vec::new(),
            frame: 0,
            next_serial: 0,
            focus: None,
        }
    }

    pub fn thumbnail(&self, path: &Path) -> Option<&egui::TextureHandle> {
        self.thumbnails.get(path).map(|thumbnail| &thumbnail.texture)
    }

    /// The best image available without the raw: the preview, or the thumbnail until it lands.
    pub fn preview(&self, path: &Path) -> Option<&egui::TextureHandle> {
        self.previews.get(path).or_else(|| self.thumbnail(path))
    }

    pub fn has_full_preview(&self, path: &Path) -> bool {
        self.previews.contains_key(path)
    }

    pub fn live_mut(&mut self, path: &Path) -> Option<&mut LivePhoto> {
        self.live.iter_mut().find(|live| live.path == path)
    }

    pub fn live(&self, path: &Path) -> Option<&LivePhoto> {
        self.live.iter().find(|live| live.path == path)
    }

    /// Adds a session opened elsewhere as if it had come from the loader.
    pub fn adopt(&mut self, path: PathBuf, session: Session, metadata: Metadata) {
        self.decoding.remove(&path);
        self.next_serial += 1;
        let orientation = Orientation::from_libraw(metadata.orientation);
        let as_shot = crate::white_balance::as_shot(&session);
        self.live.retain(|live| live.path != path);
        self.live.push(LivePhoto {
            path,
            session,
            metadata,
            orientation,
            as_shot,
            serial: self.next_serial,
        });
    }

    /// Says a row is on screen this frame.
    pub fn show_row(&mut self, row: &LibraryRow) {
        if let Some(thumbnail) = self.thumbnails.get_mut(&row.path) {
            thumbnail.last_shown = self.frame;
        } else if !self.shown_rows.iter().any(|shown| shown.path == row.path) {
            self.shown_rows.push(PreviewJob::of(row));
        }
    }

    /// Queues thumbnails for what's on screen, dropping requests that scrolled away.
    pub fn end_frame(&mut self) {
        self.frame += 1;
        let shown = std::mem::take(&mut self.shown_rows);
        let paths: Vec<PathBuf> = shown.iter().map(|row| row.path.clone()).collect();
        if paths != self.last_shown_paths {
            for dropped in self.loader.set_thumbnails(shown.iter().cloned()) {
                self.thumbnails_wanted.remove(&dropped.path);
            }
            self.thumbnails_wanted.extend(paths.iter().cloned());
            self.last_shown_paths = paths;
        }
        self.evict_thumbnails();
    }

    fn evict_thumbnails(&mut self) {
        if self.thumbnails.len() <= MAX_THUMBNAILS {
            return;
        }
        let mut ages: Vec<(u64, PathBuf)> = self
            .thumbnails
            .iter()
            .map(|(path, thumbnail)| (thumbnail.last_shown, path.clone()))
            .collect();
        ages.sort_unstable();
        for (_, path) in ages.into_iter().take(self.thumbnails.len() - MAX_THUMBNAILS) {
            self.thumbnails.remove(&path);
        }
    }

    fn needs_decode(&self, path: &Path, state: &AppState) -> bool {
        self.live(path).is_none()
            && !self.decoding.contains(path)
            && !matches!(state.decode.get(path), Some(DecodeState::Failed(_)))
    }

    fn queue_preview(&mut self, row: &LibraryRow) {
        if !self.previews.contains_key(&row.path) && self.previews_wanted.insert(row.path.clone()) {
            self.loader.push(Job::Preview(PreviewJob::of(row)));
        }
    }

    fn queue_decode(&mut self, row: &LibraryRow, recipe: &Recipe, state: &AppState) {
        if row.status == PhotoStatus::Available && self.needs_decode(&row.path, state) {
            self.decoding.insert(row.path.clone());
            self.loader.push(Job::Session {
                path: row.path.clone(),
                recipe: Box::new(recipe.clone()),
            });
        }
    }

    /// Queues work for a newly focused photo, replacing whatever hadn't started for the last
    /// one: its preview and decode first, then the neighbours' previews, which is what makes
    /// the next key press instant.
    pub fn focus_on(
        &mut self,
        row: &LibraryRow,
        recipe: &Recipe,
        neighbours: &[LibraryRow],
        state: &AppState,
    ) {
        self.focus = Some(row.path.clone());
        for cancelled in self.loader.cancel_photo_jobs() {
            match cancelled {
                Job::Preview(preview) => self.previews_wanted.remove(&preview.path),
                Job::Session { path, .. } => self.decoding.remove(&path),
                Job::Thumbnail(_) => false,
            };
        }
        self.queue_preview(row);
        self.queue_decode(row, recipe, state);
        for neighbour in neighbours {
            self.queue_preview(neighbour);
        }
        self.evict(&row.path, neighbours);
    }

    /// Previews for rows the compare and survey views want.
    pub fn want_preview(&mut self, row: &LibraryRow) {
        self.queue_preview(row);
    }

    /// Decodes the neighbours. Called once the current photo is up, so that their GPU work
    /// doesn't compete with it.
    pub fn preload(&mut self, current: &Path, neighbours: &[(LibraryRow, Recipe)], state: &AppState) {
        let rows: Vec<LibraryRow> = neighbours.iter().map(|(row, _)| row.clone()).collect();
        self.evict(current, &rows);
        for (row, recipe) in neighbours {
            self.queue_decode(row, recipe, state);
        }
    }

    fn evict(&mut self, current: &Path, neighbours: &[LibraryRow]) {
        let keep: HashSet<&Path> = std::iter::once(current)
            .chain(neighbours.iter().map(|row| row.path.as_path()))
            .collect();
        let mut index = 0;
        while index < self.live.len() {
            if self.live.len() > MAX_SESSIONS && !keep.contains(self.live[index].path.as_path()) {
                let evicted = self.live.remove(index);
                self.loader.recycle(evicted.session);
            } else {
                index += 1;
            }
        }
    }

    /// Takes in finished work.
    pub fn pump(&mut self, ctx: &egui::Context, state: &mut AppState) {
        while let Some(loaded) = self.loader.try_next() {
            match loaded {
                Loaded::Thumbnail { path, image } => {
                    self.thumbnails_wanted.remove(&path);
                    let texture = to_texture(ctx, "thumbnail", &image);
                    self.thumbnails.insert(
                        path,
                        Thumbnail {
                            texture,
                            last_shown: self.frame,
                        },
                    );
                }
                Loaded::ThumbnailFailed { path } => {
                    self.thumbnails_wanted.remove(&path);
                }
                Loaded::Preview { path, image } => self.store_preview(ctx, path, &image),
                Loaded::PreviewFailed { path } => {
                    self.previews_wanted.remove(&path);
                }
                Loaded::Session {
                    path,
                    session,
                    metadata,
                } => {
                    state.decode.insert(path.clone(), DecodeState::Ready);
                    self.adopt(path, *session, metadata);
                }
                Loaded::SessionFailed {
                    path,
                    message,
                    device_lost,
                } => {
                    self.decoding.remove(&path);
                    state.device_lost |= device_lost;
                    state.decode.insert(path, DecodeState::Failed(message));
                }
            }
        }
    }

    fn store_preview(&mut self, ctx: &egui::Context, path: PathBuf, image: &RgbaImage) {
        self.previews_wanted.remove(&path);
        let texture = to_texture(ctx, "preview", image);
        self.previews.insert(path.clone(), texture);
        self.preview_order.push_back(path);
        if self.preview_order.len() > MAX_PREVIEWS {
            let focus = self.focus.as_ref();
            let oldest = self.preview_order.iter().position(|path| Some(path) != focus);
            if let Some(path) = oldest.and_then(|index| self.preview_order.remove(index)) {
                self.previews.remove(&path);
            }
        }
    }
}
