//! Select Subject and Select Sky. Nothing here starts until a tool is first used, and no model
//! is fetched until the user has seen its size and licence and said yes.

use crate::{
    darkroom::LivePhoto,
    state::{AppState, edit::Step},
};
use std::path::PathBuf;
use unlight_ai::{
    Manifest, MaskStore, Model, ModelStatus, ModelStore, Progress, RgbImage, Selection, Task, Ticket, Turn, Worker,
    working_size,
};
use unlight_catalog::VariantId;
use unlight_engine::{Result, ViewRequest};
use unlight_recipe::{Mask, MaskComponent, MaskShape, Recipe};

pub fn tool_name(task: Task) -> &'static str {
    match task {
        Task::Sky => "Select Sky",
        _ => "Select Subject",
    }
}

fn mask_name(task: Task) -> &'static str {
    match task {
        Task::Sky => "Sky",
        _ => "Subject",
    }
}

/// What the panel should offer for a tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Ready,
    NeedsModel,
    /// The user clicked a tool whose model isn't here, so the panel asks.
    Asking { prompt: String },
    Downloading(Progress),
    Working,
    Unavailable,
}

struct Services {
    manifest: Manifest,
    models: ModelStore,
    worker: Worker,
}

impl Services {
    fn new(models: ModelStore, masks: MaskStore) -> unlight_ai::Result<Self> {
        let worker = Worker::new(models.clone(), masks, || {});
        Ok(Self { manifest: Manifest::bundled()?, models, worker })
    }

    fn open() -> unlight_ai::Result<Self> {
        Self::new(ModelStore::open_default()?, MaskStore::open_default()?)
    }

    fn model(&self, task: Task) -> Option<&Model> {
        self.manifest.for_task(task)
    }
}

struct Running {
    task: Task,
    variant: VariantId,
    ticket: Ticket<Selection>,
}

#[derive(Default)]
pub struct AiTools {
    services: Option<Services>,
    asking: Option<Task>,
    download: Option<(Task, Ticket<PathBuf>)>,
    wanted: Option<(Task, VariantId)>,
    running: Option<Running>,
    /// The last failure, for the panel to show.
    pub message: Option<String>,
}

impl AiTools {
    /// Uses these stores instead of the default ones.
    pub fn with_stores(models: ModelStore, masks: MaskStore) -> unlight_ai::Result<Self> {
        Ok(Self { services: Some(Services::new(models, masks)?), ..Self::default() })
    }

    fn services(&mut self) -> Option<&Services> {
        if self.services.is_none() {
            match Services::open() {
                Ok(services) => self.services = Some(services),
                Err(error) => self.message = Some(error.to_string()),
            }
        }
        self.services.as_ref()
    }

    pub fn status(&mut self, task: Task) -> ToolStatus {
        if let Some((_, ticket)) = self.download.as_ref().filter(|(active, _)| *active == task) {
            return ToolStatus::Downloading(ticket.progress());
        }
        let working = self.wanted.is_some_and(|(active, _)| active == task)
            || self.running.as_ref().is_some_and(|running| running.task == task);
        if working {
            return ToolStatus::Working;
        }
        let asking = self.asking == Some(task);
        let Some(services) = self.services() else {
            return ToolStatus::Unavailable;
        };
        let Some(model) = services.model(task) else {
            return ToolStatus::Unavailable;
        };
        match services.models.status(model) {
            ModelStatus::Ready => ToolStatus::Ready,
            _ if asking => ToolStatus::Asking { prompt: model.consent_prompt() },
            _ => ToolStatus::NeedsModel,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.download.is_some() || self.wanted.is_some() || self.running.is_some()
    }

    /// Asks before anything is fetched: the panel shows the size and licence next.
    pub fn ask_to_download(&mut self, task: Task) {
        self.asking = Some(task);
    }

    pub fn decline_download(&mut self) {
        self.asking = None;
    }

    /// The user has seen the prompt and said yes.
    pub fn download(&mut self, task: Task) {
        self.asking = None;
        let Some(services) = self.services() else { return };
        let Some(model) = services.model(task) else { return };
        let ticket = services.worker.download(model);
        self.download = Some((task, ticket));
    }

    /// Asks for a selection on this photo. The app supplies the picture on its next pump.
    pub fn select(&mut self, task: Task, variant: VariantId) {
        self.message = None;
        self.wanted = Some((task, variant));
    }

    pub fn cancel(&mut self) {
        if let Some((_, ticket)) = &self.download {
            ticket.cancel();
        }
        if let Some(running) = &self.running {
            running.ticket.cancel();
        }
        self.wanted = None;
    }

    fn start(&mut self, image: RgbImage, turn: Turn) {
        let Some((task, variant)) = self.wanted.take() else { return };
        let Some(services) = self.services() else { return };
        let Some(model) = services.model(task) else { return };
        let ticket = services.worker.select(model, image, turn);
        self.running = Some(Running { task, variant, ticket });
    }

    fn poll_download(&mut self) {
        let Some((_, ticket)) = &self.download else { return };
        let Some(result) = ticket.poll() else { return };
        self.download = None;
        self.note_failure(result.map(|_| ()));
    }

    fn poll_selection(&mut self) -> Option<(Task, VariantId, String)> {
        let running = self.running.as_ref()?;
        let result = running.ticket.poll()?;
        let (task, variant) = (running.task, running.variant);
        self.running = None;
        match result {
            Ok(selection) => Some((task, variant, selection.mask_id)),
            Err(error) => {
                self.note_failure(Err(error));
                None
            }
        }
    }

    fn note_failure(&mut self, result: unlight_ai::Result<()>) {
        match result {
            Err(unlight_ai::Error::Cancelled) | Ok(()) => {}
            Err(error) => self.message = Some(error.to_string()),
        }
    }
}

/// The photo with no edits at all: the mask must cover the whole frame, and shouldn't change
/// because a slider moved.
fn render_for_selection(live: &mut LivePhoto) -> Result<(RgbImage, Turn)> {
    let recipe = Recipe::default();
    let [width, height] = working_size(live.session.source.width, live.session.source.height);
    let target = live.session.view_target(width, height);
    live.session.render(&recipe, &ViewRequest::default(), &target, None)?;
    let image = RgbImage::from_rgba(width, height, &live.session.read_view(&target)?);
    Ok((image, Turn::from_libraw(live.metadata.orientation)))
}

/// A finished selection becomes a new mask, but only on the photo it was started on: the user
/// may well have moved on while it ran.
fn add_selection(state: &mut AppState, task: Task, variant: VariantId, id: String) {
    let Some(edit) = state.edit_mut().filter(|edit| edit.variant == variant) else {
        return;
    };
    let name = mask_name(task);
    edit.recipe.masks.push(Mask {
        name: name.to_owned(),
        components: vec![MaskComponent { shape: MaskShape::Raster { id, inverted: false }, subtract: false }],
        ..Mask::default()
    });
    let index = edit.recipe.masks.len() - 1;
    edit.note("Add Mask", name, Step::New);
    state.local.mask = Some(index);
}

/// Called each frame with the photo on screen. Hands it to the tools if they are waiting for
/// it, and collects anything finished.
pub fn pump(state: &mut AppState, live: Option<&mut LivePhoto>) {
    if state.ai.wanted.is_some() {
        match live.map(render_for_selection) {
            Some(Ok((image, turn))) => state.ai.start(image, turn),
            Some(Err(error)) => {
                state.ai.wanted = None;
                state.ai.message = Some(error.to_string());
            }
            None => {}
        }
    }
    state.ai.poll_download();
    if let Some((task, variant, id)) = state.ai.poll_selection() {
        add_selection(state, task, variant, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::edit::PhotoEdit;

    fn state_editing(variant: VariantId) -> AppState {
        let mut state = AppState::default();
        state.edits.insert(variant, PhotoEdit::new(variant, Recipe::default()));
        state
    }

    #[test]
    fn nothing_is_busy_until_a_tool_is_used() {
        assert!(!AiTools::default().is_busy());
    }

    #[test]
    fn asking_is_separate_from_downloading() {
        let mut tools = AiTools::default();
        tools.ask_to_download(Task::Subject);
        assert!(!tools.is_busy(), "asking must not start a download");
        tools.decline_download();
        assert_eq!(tools.asking, None);
    }

    #[test]
    fn a_selection_for_another_photo_is_dropped() {
        let (shown, other) = (VariantId(1), VariantId(2));
        let mut state = state_editing(shown);
        add_selection(&mut state, Task::Sky, other, "a".repeat(64));
        assert!(state.edits[&shown].recipe.masks.is_empty());
    }
}
