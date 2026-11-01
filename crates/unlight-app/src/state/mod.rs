//! Application state and the logic that changes it. Nothing here draws or touches a database:
//! changes to the catalog leave as `Write`s, so all of it runs in plain unit tests.

pub mod clipboard;
pub mod crop;
pub mod dates;
pub mod edit;
pub mod guides;
pub mod history;
pub mod library;
pub mod local;
pub mod merge;
pub mod profiles;
pub mod proof;
pub mod rules;
pub mod sections;
pub mod session;
pub mod transfer;
pub mod view;
pub mod white_balance;
pub mod write;

use crate::action::Action;
use clipboard::{CopiedSettings, SettingSelection, narrow_preset, partial_recipe};
use crop::{AspectLock, CropRect, CropTool, fit_ratio};
use edit::{PhotoEdit, Step};
use library::Library;
use local::LocalTools;
use proof::ProofState;
use rules::SmartEditor;
use sections::Section;
use session::Session;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
use transfer::{ExportForm, ExportRun, ImportPhase, ImportState};
use unlight_catalog::{
    Collection, CollectionId, Filter, Flag, Folder, FolderFilter, FolderId, Keyword, Label, LibraryRow, Photo, PhotoId,
    PhotoStatus, Preset, PresetId, VariantId,
};
use unlight_recipe::{Crop, GuideLine, Recipe, Upright};
use view::{Orientation, ViewState, Zoom};
use write::Write;

const STATUS_SECONDS: f64 = 2.5;
/// Neighbours kept decoded either side of the current photo.
const DECODE_REACH: usize = 1;
/// Neighbours whose previews are kept ready, so stepping to them is instant.
const PREVIEW_REACH: usize = 3;
/// Edits kept in memory. Everything is in the catalog, this just keeps stepping back cheap.
const MAX_EDITS: usize = 64;
pub const MAX_GUIDES: usize = 4;
pub const SURVEY_LIMIT: usize = 12;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Module {
    Library,
    #[default]
    Develop,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LibraryView {
    #[default]
    Grid,
    Loupe,
    Compare,
    Survey,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    None,
    Crop,
    Guides,
    Heal,
    Masks,
    RedEye,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compare {
    #[default]
    Off,
    Split,
    SideBySide,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lights {
    #[default]
    On,
    Dim,
    Off,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backdrop {
    Black,
    #[default]
    Dark,
    Medium,
    Light,
}

impl Backdrop {
    pub const ALL: [Self; 4] = [Self::Black, Self::Dark, Self::Medium, Self::Light];

    pub fn name(self) -> &'static str {
        match self {
            Self::Black => "Black",
            Self::Dark => "Dark Grey",
            Self::Medium => "Medium Grey",
            Self::Light => "Light Grey",
        }
    }

    /// Grey level of the area around the photo.
    pub fn level(self) -> u8 {
        match self {
            Self::Black => 0,
            Self::Dark => 26,
            Self::Medium => 64,
            Self::Light => 118,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Panels {
    pub left: bool,
    pub right: bool,
    pub filmstrip: bool,
    pub top: bool,
}

impl Default for Panels {
    fn default() -> Self {
        Self {
            left: true,
            right: true,
            filmstrip: true,
            top: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dialog {
    CopySettings,
    SavePreset { name: String, group: String },
    ApplyPresetPart(PresetId),
    NewCollection { name: String },
    SmartCollection,
    Import,
    Export,
    PhotoMerge,
    RemovePhotos { variants: Vec<VariantId> },
    RemoveFolder { folder: FolderId, name: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub text: String,
    pub kind: StatusKind,
    expires: f64,
}

/// Requests the state can't carry out itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    SetFullscreen(bool),
    /// Opens the folder chooser for the given dialog.
    ChooseFolder(FolderUse),
    StartImport {
        root: PathBuf,
        focus: Option<PathBuf>,
    },
    CancelImport,
    StartExport,
    CancelExport,
    StartMerge,
    CancelMerge,
    ConstrainCrop,
    SelectAll,
    AutoTone,
    RescanProfiles,
    OpenProfileFolder,
    SearchProofProfiles,
    ChooseProofProfile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderUse {
    Import,
    Export,
}

/// Where a photo's raw decode stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DecodeState {
    #[default]
    Pending,
    Ready,
    Failed(String),
}

/// What the catalog lists for the panels and pickers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CatalogLists {
    pub folders: Vec<Folder>,
    pub collections: Vec<Collection>,
    pub keywords: Vec<Keyword>,
    pub presets: Vec<Preset>,
    pub cameras: Vec<String>,
    pub lenses: Vec<String>,
    /// Photos whose sidecar changed on disk while the catalog had changes of its own.
    pub sidecar_conflicts: Vec<PhotoId>,
}

/// The dates typed into the filter bar, kept as typed while they don't parse yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DateForm {
    pub from: String,
    pub until: String,
}

pub struct AppState {
    pub module: Module,
    pub library_view: LibraryView,
    pub library: Library,
    pub edits: HashMap<VariantId, PhotoEdit>,
    pub lists: CatalogLists,
    /// The focused photo's file details.
    pub photo: Option<Photo>,
    pub keywords: Vec<Keyword>,
    pub view: ViewState,
    pub tool: Tool,
    pub crop_tool: CropTool,
    pub local: LocalTools,
    pub compare: Compare,
    /// Divider position in split view, 0..1 across the photo.
    pub split: f32,
    pub clipping: bool,
    pub proof: ProofState,
    pub panels: Panels,
    pub lights: Lights,
    pub backdrop: Backdrop,
    pub dialog: Option<Dialog>,
    pub copy_selection: SettingSelection,
    pub clipboard: Option<CopiedSettings>,
    pub open_sections: HashSet<Section>,
    pub decode: HashMap<PathBuf, DecodeState>,
    pub effects: Vec<Effect>,
    #[cfg(feature = "ai")]
    pub ai: crate::ai_tools::AiTools,
    pub writes: Vec<Write>,
    pub now: f64,
    pub gesture_active: bool,
    pub device_lost: bool,
    pub import: ImportState,
    pub export_form: Option<ExportForm>,
    pub export_run: Option<ExportRun>,
    pub merge_form: Option<merge::MergeForm>,
    pub merge_run: Option<merge::MergeRun>,
    pub smart_editor: Option<SmartEditor>,
    pub date_form: DateForm,
    pub keyword_entry: String,
    pub collection_name: String,
    /// Width of a grid cell in points.
    pub grid_cell: f32,
    /// Columns the grid laid out last frame, for arrow keys.
    pub grid_columns: usize,
    /// A preset under the pointer, shown on the photo without being applied.
    pub preview_recipe: Option<Box<Recipe>>,
    status: Option<Status>,
    fullscreen: bool,
    last_compare: Compare,
    crop_on_entry: Option<Crop>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            module: Module::default(),
            library_view: LibraryView::default(),
            library: Library::new(),
            edits: HashMap::new(),
            lists: CatalogLists::default(),
            photo: None,
            keywords: Vec::new(),
            view: ViewState::default(),
            tool: Tool::default(),
            crop_tool: CropTool::default(),
            local: LocalTools::default(),
            compare: Compare::default(),
            split: 0.5,
            clipping: false,
            proof: ProofState::default(),
            panels: Panels::default(),
            lights: Lights::default(),
            backdrop: Backdrop::default(),
            dialog: None,
            copy_selection: SettingSelection::default(),
            clipboard: None,
            open_sections: HashSet::from([Section::Basic, Section::ToneCurve]),
            decode: HashMap::new(),
            effects: Vec::new(),
            #[cfg(feature = "ai")]
            ai: Default::default(),
            writes: Vec::new(),
            now: 0.0,
            gesture_active: false,
            device_lost: false,
            import: ImportState::default(),
            export_form: None,
            export_run: None,
            merge_form: None,
            merge_run: None,
            smart_editor: None,
            date_form: DateForm::default(),
            keyword_entry: String::new(),
            collection_name: String::new(),
            grid_cell: 196.0,
            grid_columns: 1,
            preview_recipe: None,
            status: None,
            fullscreen: false,
            last_compare: Compare::Split,
            crop_on_entry: None,
        }
    }
}

impl AppState {
    pub fn notify(&mut self, text: impl Into<String>) {
        self.set_status(text, StatusKind::Info);
    }

    pub fn report_error(&mut self, text: impl Into<String>) {
        self.set_status(text, StatusKind::Error);
    }

    fn set_status(&mut self, text: impl Into<String>, kind: StatusKind) {
        let seconds = if kind == StatusKind::Error {
            STATUS_SECONDS * 3.0
        } else {
            STATUS_SECONDS
        };
        self.status = Some(Status {
            text: text.into(),
            kind,
            expires: self.now + seconds,
        });
    }

    /// The current status message, if it's still fresh.
    pub fn status(&self) -> Option<&Status> {
        self.status.as_ref().filter(|status| status.expires > self.now)
    }

    /// Seconds until the status message goes away, so the UI can wake up for it.
    pub fn status_time_left(&self) -> Option<f64> {
        self.status().map(|status| status.expires - self.now)
    }

    pub fn focused(&self) -> Option<&LibraryRow> {
        self.library.focused()
    }

    pub fn edit(&self) -> Option<&PhotoEdit> {
        self.edits.get(&self.focused()?.variant)
    }

    pub fn edit_mut(&mut self) -> Option<&mut PhotoEdit> {
        let id = self.focused()?.variant;
        self.edits.get_mut(&id)
    }

    /// Keeps an edit in memory. Older ones can go: they're in the catalog already.
    pub fn store_edit(&mut self, edit: PhotoEdit) {
        if self.edits.len() >= MAX_EDITS {
            let focused = self.focused().map(|row| row.variant);
            let idle: Vec<VariantId> = self
                .edits
                .iter()
                .filter(|(id, edit)| Some(**id) != focused && !edit.is_settling())
                .map(|(id, _)| *id)
                .take(MAX_EDITS / 4)
                .collect();
            for id in idle {
                self.edits.remove(&id);
            }
        }
        self.edits.insert(edit.variant, edit);
    }

    pub fn decode_state(&self) -> DecodeState {
        let Some(row) = self.focused() else {
            return DecodeState::default();
        };
        match row.status {
            PhotoStatus::Missing => {
                DecodeState::Failed(format!("The file is missing: {}", row.path.display()))
            }
            PhotoStatus::Unsupported => DecodeState::Failed(
                self.photo
                    .as_ref()
                    .and_then(|photo| photo.status_detail.clone())
                    .unwrap_or_else(|| "This file isn't supported".to_owned()),
            ),
            PhotoStatus::Available => self.decode.get(&row.path).cloned().unwrap_or_default(),
        }
    }

    /// Photos worth decoding ahead of time, around the focus.
    pub fn decode_neighbours(&self) -> Vec<LibraryRow> {
        self.library
            .neighbours(DECODE_REACH)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Photos whose previews should be ready before the user gets to them.
    pub fn preview_neighbours(&self) -> Vec<LibraryRow> {
        self.library
            .neighbours(PREVIEW_REACH)
            .into_iter()
            .cloned()
            .collect()
    }

    pub fn is_section_open(&self, section: Section) -> bool {
        self.open_sections.contains(&section)
    }

    pub fn set_section_open(&mut self, section: Section, open: bool) {
        if open {
            self.open_sections.insert(section);
        } else {
            self.open_sections.remove(&section);
        }
    }

    /// Imports whatever was opened from outside the app: a folder, or a file and its folder.
    pub fn open_paths(&mut self, paths: &[PathBuf]) {
        let Some(first) = paths.first() else {
            return;
        };
        let (root, focus) = if first.is_dir() {
            (first.clone(), None)
        } else {
            let folder = first.parent().filter(|parent| !parent.as_os_str().is_empty());
            (
                folder.unwrap_or(Path::new(".")).to_path_buf(),
                Some(first.clone()),
            )
        };
        self.import.folder = root.display().to_string();
        self.import.focus = focus.clone();
        self.import.phase = ImportPhase::Running(None);
        self.effects.push(Effect::StartImport { root, focus });
    }

    /// Ends the frame: what changed becomes a history step unless a gesture is under way.
    pub fn end_frame(&mut self) {
        let focused = self.focused().map(|row| row.variant);
        let (busy, now) = (self.gesture_active, self.now);
        for (id, edit) in &mut self.edits {
            edit.settle(busy && Some(*id) == focused, now);
            self.writes.append(&mut edit.take_writes());
        }
    }

    /// Whether a change is waiting to see if more of it follows.
    pub fn is_settling(&self) -> bool {
        self.edits.values().any(PhotoEdit::is_settling)
    }

    /// Commits everything at once, for closing the window or losing the GPU.
    pub fn flush_edits(&mut self) {
        for edit in self.edits.values_mut() {
            edit.commit();
            self.writes.append(&mut edit.take_writes());
        }
    }

    pub fn take_writes(&mut self) -> Vec<Write> {
        std::mem::take(&mut self.writes)
    }

    pub fn session(&self) -> Session {
        Session {
            variant: self.focused().map(|row| row.variant.0),
            develop: self.module == Module::Develop,
            folder: self
                .library
                .filter()
                .folder
                .map(|filter| (filter.folder.0, filter.include_subfolders)),
            collection: self.library.filter().collection.map(|id| id.0),
        }
    }

    pub fn restore_session(&mut self, session: &Session) {
        self.module = if session.develop {
            Module::Develop
        } else {
            Module::Library
        };
        let folder = session.folder.map(|(id, include_subfolders)| FolderFilter {
            folder: unlight_catalog::FolderId(id),
            include_subfolders,
        });
        let collection = session.collection.map(CollectionId);
        self.library.set_filter(Filter {
            folder,
            collection,
            ..Filter::default()
        });
        if let Some(variant) = session.variant {
            self.library.select_variant(VariantId(variant));
        }
    }

    pub fn apply(&mut self, action: Action) {
        match action {
            Action::ShowGrid => self.show_library(LibraryView::Grid),
            Action::ShowLoupe => self.show_library(LibraryView::Loupe),
            Action::ShowCompare => self.show_library(LibraryView::Compare),
            Action::ShowSurvey => self.show_library(LibraryView::Survey),
            Action::ShowDevelop => self.module = Module::Develop,
            Action::SetFlag(Flag::Reject) if self.tool == Tool::Crop => self.flip_crop(),
            Action::SetFlag(flag) => self.set_flag(flag),
            Action::SetRating(rating) => self.set_rating(rating),
            Action::ToggleLabel(label) => self.toggle_label(label),
            Action::NextPhoto => self.step_photo(1),
            Action::PreviousPhoto => self.step_photo(-1),
            Action::PhotoAbove => self.step_rows(-1),
            Action::PhotoBelow => self.step_rows(1),
            Action::SelectAll => self.effects.push(Effect::SelectAll),
            Action::SelectNone => self.library.clear_selection(),
            Action::ToggleClipping => {
                self.clipping = !self.clipping;
                self.notify(if self.clipping {
                    "Clipping shown"
                } else {
                    "Clipping hidden"
                });
            }
            Action::ToggleTool(tool) => self.toggle_tool(tool),
            Action::ToggleCompare => self.toggle_compare(),
            Action::CompareSplit => self.set_compare(Compare::Split),
            Action::CompareSideBySide => self.set_compare(Compare::SideBySide),
            Action::ToggleZoom => self.view.toggle_at(self.view.center),
            Action::ZoomIn | Action::ZoomOut => {}
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::ToggleSidePanels => {
                let show = !(self.panels.left || self.panels.right);
                self.panels.left = show;
                self.panels.right = show;
            }
            Action::ToggleAllPanels => {
                let show = !(self.panels.left || self.panels.right || self.panels.filmstrip);
                self.panels = Panels {
                    left: show,
                    right: show,
                    filmstrip: show,
                    top: show,
                };
            }
            Action::ToggleFilmstrip => self.panels.filmstrip = !self.panels.filmstrip,
            Action::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                self.effects.push(Effect::SetFullscreen(self.fullscreen));
            }
            Action::CycleLights => {
                self.lights = match self.lights {
                    Lights::On => Lights::Dim,
                    Lights::Dim => Lights::Off,
                    Lights::Off => Lights::On,
                };
            }
            Action::CopySettings => self.dialog = Some(Dialog::CopySettings),
            Action::PasteSettings => self.paste_settings(),
            Action::ResetAll => self.reset_all(),
            Action::ToggleBlackAndWhite => self.toggle_black_and_white(),
            Action::AutoTone => self.effects.push(Effect::AutoTone),
            Action::ToggleSoftProof => self.toggle_soft_proof(),
            Action::ToggleGamutWarning => self.proof.gamut_warning = !self.proof.gamut_warning,
            Action::VirtualCopy => {
                if let Some(row) = self.focused() {
                    self.writes.push(Write::VirtualCopy(row.variant));
                }
            }
            Action::OpenImport => self.open_import(),
            Action::OpenExport => self.open_export(),
            Action::PhotoMerge(kind) => self.open_merge(kind),
            Action::Confirm => self.confirm(),
            Action::Cancel => self.cancel(),
            Action::ToggleAspectLock => self.toggle_aspect_lock(),
            Action::ToggleMaskOverlay => self.local.overlay = !self.local.overlay,
            Action::ToolSmaller => self.resize_tool(-1),
            Action::ToolLarger => self.resize_tool(1),
            Action::DeleteSelected => self.delete_selected(),
        }
    }

    pub fn ask_to_remove_selection(&mut self) {
        let variants = self.library.selection();
        if !variants.is_empty() {
            self.dialog = Some(Dialog::RemovePhotos { variants });
        }
    }

    /// Unsaved edits go first, so nothing tries to save to a photo that has already left.
    pub fn remove_variants(&mut self, variants: Vec<VariantId>) {
        self.dialog = None;
        self.flush_edits();
        self.writes.push(Write::RemoveVariants(variants));
    }

    pub fn remove_folder(&mut self, folder: FolderId) {
        self.dialog = None;
        self.flush_edits();
        self.writes.push(Write::RemoveFolder(folder));
    }

    pub fn variants_removed(&mut self, variants: &[VariantId]) {
        self.edits.retain(|id, _| !variants.contains(id));
        self.library.forget(variants);
    }

    /// There's no telling which photos went without asking, but edits reload from the catalog.
    pub fn folder_removed(&mut self) {
        self.edits.clear();
        let filter = self.library.filter();
        let filtered = filter.folder.map(|filter| filter.folder);
        if filtered.is_some_and(|id| !self.lists.folders.iter().any(|folder| folder.id == id)) {
            self.library.set_filter(Filter { folder: None, ..filter.clone() });
        }
        self.library.forget(&[]);
    }

    fn show_library(&mut self, view: LibraryView) {
        self.module = Module::Library;
        self.library_view = view;
    }

    fn step_photo(&mut self, delta: isize) {
        self.leave_tool();
        self.library.step(delta);
    }

    /// Up and down move a whole row in the grid, and work like left and right elsewhere.
    fn step_rows(&mut self, rows: isize) {
        let in_grid = self.module == Module::Library && self.library_view == LibraryView::Grid;
        self.step_photo(if in_grid {
            rows * self.grid_columns.max(1) as isize
        } else {
            rows
        });
    }

    fn set_flag(&mut self, flag: Flag) {
        let ids = self.library.selection();
        if ids.is_empty() {
            return;
        }
        self.library.update_rows(&ids, |row| row.flag = flag);
        self.writes.push(Write::SetFlag(ids, flag));
        self.notify(match flag {
            Flag::Pick => "Flagged as Pick",
            Flag::Reject => "Flagged as Rejected",
            Flag::Unflagged => "Flag removed",
        });
    }

    fn set_rating(&mut self, rating: u8) {
        let ids = self.library.selection();
        if ids.is_empty() {
            return;
        }
        self.library.update_rows(&ids, |row| row.rating = rating);
        self.writes.push(Write::SetRating(ids, rating));
        self.notify(match rating {
            0 => "Rating removed".to_owned(),
            1 => "1 star".to_owned(),
            stars => format!("{stars} stars"),
        });
    }

    fn toggle_label(&mut self, label: Label) {
        let Some(current) = self.focused().map(|row| row.label) else {
            return;
        };
        let chosen = if current == label { Label::None } else { label };
        let ids = self.library.selection();
        self.library.update_rows(&ids, |row| row.label = chosen);
        self.writes.push(Write::SetLabel(ids, chosen));
        self.notify(format!("Label: {}", library::label_name(chosen)));
    }

    fn toggle_tool(&mut self, tool: Tool) {
        if self.tool == tool {
            self.leave_tool();
            return;
        }
        self.leave_tool();
        self.module = Module::Develop;
        self.tool = tool;
        if tool == Tool::Crop {
            self.view.zoom = Zoom::Fit;
            self.crop_on_entry = self.edit().map(|edit| edit.recipe.crop);
        }
        if tool == Tool::Guides {
            self.view.zoom = Zoom::Fit;
            if let Some(edit) = self.edit_mut()
                && edit.recipe.transform.upright != Upright::Guided
            {
                edit.recipe.transform.upright = Upright::Guided;
                edit.note("Upright", "Guided", Step::New);
            }
        }
    }

    fn leave_tool(&mut self) {
        self.tool = Tool::None;
        self.crop_on_entry = None;
    }

    fn confirm(&mut self) {
        if self.dialog == Some(Dialog::CopySettings) {
            self.confirm_copy();
        } else if self.dialog.is_none() && matches!(self.tool, Tool::Crop | Tool::Guides) {
            self.leave_tool();
        }
    }

    fn cancel(&mut self) {
        if self.dialog.take().is_some() {
            return;
        }
        if self.tool == Tool::Crop
            && let Some(original) = self.crop_on_entry
            && let Some(edit) = self.edit_mut()
            && edit.recipe.crop != original
        {
            edit.recipe.crop = original;
            edit.note("Cancel Crop", "", Step::New);
        }
        if self.lights != Lights::On {
            self.lights = Lights::On;
        }
        self.leave_tool();
    }

    fn toggle_compare(&mut self) {
        if self.compare == Compare::Off {
            self.compare = self.last_compare;
        } else {
            self.last_compare = self.compare;
            self.compare = Compare::Off;
        }
    }

    fn set_compare(&mut self, compare: Compare) {
        self.compare = if self.compare == compare {
            Compare::Off
        } else {
            compare
        };
        if compare != Compare::Off {
            self.last_compare = compare;
        }
    }

    fn undo(&mut self) {
        let undone = self.edit_mut().and_then(PhotoEdit::undo);
        self.notify(undone.map_or("Nothing to undo".to_owned(), |label| format!("Undo: {label}")));
    }

    fn redo(&mut self) {
        let redone = self.edit_mut().and_then(PhotoEdit::redo);
        self.notify(redone.map_or("Nothing to redo".to_owned(), |label| format!("Redo: {label}")));
    }

    /// The Copy dialog's button: remembers the recipe and the ticked groups.
    pub fn confirm_copy(&mut self) {
        self.dialog = None;
        if let Some(edit) = self.edit() {
            let selection = self.copy_selection;
            self.clipboard = Some(CopiedSettings::new(edit.recipe.clone(), selection));
            self.notify(format!("Copied {} setting groups", selection.count()));
        }
    }

    fn paste_settings(&mut self) {
        let Some(copied) = self.clipboard.clone() else {
            self.notify("Nothing copied yet");
            return;
        };
        let focused = self.focused().map(|row| row.variant);
        let others: Vec<VariantId> = self
            .library
            .selection()
            .into_iter()
            .filter(|id| Some(*id) != focused)
            .collect();
        if let Some(edit) = self.edit_mut() {
            copied.paste_onto(&mut edit.recipe);
            edit.note("Paste Settings", "", Step::New);
        }
        let count = others.len() + 1;
        if !others.is_empty() {
            self.writes.push(Write::PasteSettings {
                variants: others,
                settings: Box::new(copied),
            });
        }
        self.notify(if count > 1 {
            format!("Settings pasted onto {count} photos")
        } else {
            "Settings pasted".to_owned()
        });
    }

    fn reset_all(&mut self) {
        if let Some(edit) = self.edit_mut() {
            edit.reset_all();
            self.notify("All settings reset");
        }
    }

    fn toggle_black_and_white(&mut self) {
        if let Some(edit) = self.edit_mut() {
            let enabled = !edit.recipe.black_and_white.enabled;
            edit.recipe.black_and_white.enabled = enabled;
            let name = if enabled { "Black & White" } else { "Color" };
            edit.note("Treatment", name, Step::New);
        }
    }

    fn toggle_soft_proof(&mut self) {
        self.proof.enabled = !self.proof.enabled;
        if self.proof.enabled && !self.proof.searched {
            self.proof.searched = true;
            self.effects.push(Effect::SearchProofProfiles);
        }
    }

    fn toggle_aspect_lock(&mut self) {
        if self.tool == Tool::Crop {
            self.crop_tool.lock = match self.crop_tool.lock {
                AspectLock::Free => AspectLock::Original,
                _ => AspectLock::Free,
            };
        }
    }

    fn flip_crop(&mut self) {
        self.crop_tool.portrait = !self.crop_tool.portrait;
    }

    /// The crop rectangle in display orientation, for a photo with this aspect.
    pub fn crop_rect(&self, orientation: Orientation) -> Option<CropRect> {
        self.edit()
            .map(|edit| CropRect::from_crop(&edit.recipe.crop, orientation))
    }

    /// Applies a newly chosen aspect ratio to the current crop.
    pub fn apply_aspect_lock(&mut self, orientation: Orientation, photo_aspect: f32) {
        let tool = self.crop_tool;
        let Some(ratio) = tool.lock.ratio(photo_aspect, tool.portrait) else {
            return;
        };
        if let Some(edit) = self.edit_mut() {
            let rect = CropRect::from_crop(&edit.recipe.crop, orientation);
            fit_ratio(&rect, ratio, photo_aspect).apply_to(&mut edit.recipe.crop, orientation);
            edit.note("Crop", tool.lock.label(), Step::New);
        }
    }

    /// Adds a guide for Guided Upright. Past the limit, the oldest makes way.
    pub fn add_guide(&mut self, guide: GuideLine) {
        if let Some(edit) = self.edit_mut() {
            let guides = &mut edit.recipe.transform.guides;
            if guides.len() >= MAX_GUIDES {
                guides.remove(0);
            }
            guides.push(guide);
            edit.note("Upright Guide", "", Step::New);
        }
    }

    pub fn remove_guide(&mut self, index: usize) {
        if let Some(edit) = self.edit_mut()
            && index < edit.recipe.transform.guides.len()
        {
            edit.recipe.transform.guides.remove(index);
            edit.note("Remove Guide", "", Step::New);
        }
    }

    fn open_import(&mut self) {
        if !self.import.is_running() {
            self.import.phase = ImportPhase::Choosing;
        }
        self.dialog = Some(Dialog::Import);
    }

    /// The selected photos' files, as the grid lists them.
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.library.selected_rows().into_iter().map(|row| row.path).collect()
    }

    /// Whether Photo Merge can start on what's selected.
    pub fn can_merge(&self) -> bool {
        self.merge_run.is_none() && merge::merge_problem(&self.selected_paths()).is_none()
    }

    fn open_merge(&mut self, kind: unlight_raw::MergeKind) {
        if self.merge_run.is_some() {
            self.dialog = Some(Dialog::PhotoMerge);
            return;
        }
        let sources = self.selected_paths();
        if let Some(problem) = merge::merge_problem(&sources) {
            self.notify(problem);
            return;
        }
        self.merge_form = Some(merge::MergeForm::new(kind, sources));
        self.dialog = Some(Dialog::PhotoMerge);
    }

    fn open_export(&mut self) {
        if self.library.selection().is_empty() {
            self.notify("Select some photos to export");
            return;
        }
        if self.export_form.is_none() {
            self.export_form = ExportForm::new(String::new());
        }
        self.dialog = Some(Dialog::Export);
    }

    /// Saves the ticked groups of the current edit as a preset.
    pub fn save_preset(&mut self, group: &str, name: &str) {
        let Some(recipe) = self.edit().map(|edit| edit.recipe.clone()) else {
            return;
        };
        let settings = partial_recipe(&recipe, &self.copy_selection);
        self.writes.push(Write::SavePreset {
            group: group.to_owned(),
            name: name.to_owned(),
            settings,
        });
        self.dialog = None;
    }

    /// Applies a preset, or just the ticked part of it.
    pub fn apply_preset(&mut self, preset: &Preset, only_selected: bool) {
        let settings = if only_selected {
            narrow_preset(&preset.settings, &self.copy_selection)
        } else {
            preset.settings.clone()
        };
        let Some(edit) = self.edit_mut() else {
            return;
        };
        match settings.apply_to(&edit.recipe) {
            Ok(recipe) => {
                edit.recipe = recipe;
                edit.note(&format!("Preset: {}", preset.name), "", Step::New);
            }
            Err(error) => self.report_error(format!("Couldn't apply that preset: {error}")),
        }
    }

    pub fn preset_named(&self, id: PresetId) -> Option<&Preset> {
        self.lists.presets.iter().find(|preset| preset.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::{library::tests::loaded, *};
    use unlight_recipe::Recipe;

    fn state_with_photos(count: u64) -> AppState {
        let mut state = AppState {
            library: loaded(count),
            ..AppState::default()
        };
        for id in 0..count as i64 {
            state.store_edit(PhotoEdit::new(VariantId(id), Recipe::default()));
        }
        state
    }

    fn edit_field(state: &mut AppState, change: impl Fn(&mut Recipe)) {
        change(&mut state.edit_mut().expect("a photo").recipe);
    }

    #[test]
    fn g_and_d_switch_modules() {
        let mut state = state_with_photos(1);
        state.apply(Action::ShowGrid);
        assert_eq!(state.module, Module::Library);
        state.apply(Action::ShowDevelop);
        assert_eq!(state.module, Module::Develop);
    }

    #[test]
    fn flags_ratings_and_labels_show_at_once_and_go_to_the_catalog() {
        let mut state = state_with_photos(2);
        state.apply(Action::SetFlag(Flag::Pick));
        state.apply(Action::SetRating(4));
        state.apply(Action::ToggleLabel(Label::Green));
        let row = state.library.row(0).expect("row");
        assert_eq!((row.flag, row.rating, row.label), (Flag::Pick, 4, Label::Green));
        assert_eq!(state.library.row(1).map(|row| row.rating), Some(0));
        let writes = state.take_writes();
        assert_eq!(writes[0], Write::SetFlag(vec![VariantId(0)], Flag::Pick));
        assert_eq!(writes[1], Write::SetRating(vec![VariantId(0)], 4));
        assert_eq!(writes[2], Write::SetLabel(vec![VariantId(0)], Label::Green));
    }

    #[test]
    fn a_rating_covers_the_whole_selection() {
        let mut state = state_with_photos(4);
        state.library.click(2, library::ClickKind::Extend);
        state.apply(Action::SetRating(5));
        assert_eq!(
            state.take_writes(),
            [Write::SetRating(
                vec![VariantId(0), VariantId(1), VariantId(2)],
                5
            )]
        );
        assert_eq!(state.library.row(2).map(|row| row.rating), Some(5));
    }

    #[test]
    fn pressing_a_label_twice_clears_it() {
        let mut state = state_with_photos(1);
        state.apply(Action::ToggleLabel(Label::Red));
        state.apply(Action::ToggleLabel(Label::Red));
        assert_eq!(state.library.row(0).map(|row| row.label), Some(Label::None));
    }

    #[test]
    fn in_the_crop_tool_x_turns_the_crop_instead_of_rejecting() {
        let mut state = state_with_photos(1);
        state.apply(Action::ToggleTool(Tool::Crop));
        state.apply(Action::SetFlag(Flag::Reject));
        assert!(state.crop_tool.portrait);
        assert_eq!(state.library.row(0).map(|row| row.flag), Some(Flag::Unflagged));
    }

    #[test]
    fn escape_undoes_the_crop_and_closes_the_tool() {
        let mut state = state_with_photos(1);
        state.apply(Action::ToggleTool(Tool::Crop));
        edit_field(&mut state, |recipe| recipe.crop.left = 0.3);
        state.apply(Action::Cancel);
        assert_eq!(state.tool, Tool::None);
        assert_eq!(state.edit().map(|edit| edit.recipe.crop.left), Some(0.0));
    }

    #[test]
    fn enter_keeps_the_crop() {
        let mut state = state_with_photos(1);
        state.apply(Action::ToggleTool(Tool::Crop));
        edit_field(&mut state, |recipe| recipe.crop.left = 0.3);
        state.apply(Action::Confirm);
        assert_eq!(state.tool, Tool::None);
        assert_eq!(state.edit().map(|edit| edit.recipe.crop.left), Some(0.3));
    }

    #[test]
    fn tab_hides_and_restores_the_side_panels() {
        let mut state = state_with_photos(1);
        state.apply(Action::ToggleSidePanels);
        assert!(!state.panels.left && !state.panels.right && state.panels.filmstrip);
        state.apply(Action::ToggleSidePanels);
        assert!(state.panels.left && state.panels.right);
    }

    #[test]
    fn backslash_returns_to_the_last_compare_mode() {
        let mut state = state_with_photos(1);
        state.apply(Action::CompareSideBySide);
        assert_eq!(state.compare, Compare::SideBySide);
        state.apply(Action::ToggleCompare);
        assert_eq!(state.compare, Compare::Off);
        state.apply(Action::ToggleCompare);
        assert_eq!(state.compare, Compare::SideBySide);
    }

    #[test]
    fn fullscreen_is_requested_through_an_effect() {
        let mut state = state_with_photos(1);
        state.apply(Action::ToggleFullscreen);
        assert_eq!(state.effects, [Effect::SetFullscreen(true)]);
    }

    #[test]
    fn copy_then_paste_moves_only_the_selection() {
        let mut state = state_with_photos(2);
        edit_field(&mut state, |recipe| {
            recipe.basic.exposure = 1.0;
            recipe.crop.left = 0.2;
        });
        state.apply(Action::CopySettings);
        assert_eq!(state.dialog, Some(Dialog::CopySettings));
        state.apply(Action::Confirm);
        assert_eq!(state.dialog, None);
        state.library.select(1);
        state.apply(Action::PasteSettings);
        state.end_frame();
        let pasted = state.edit().expect("edit");
        assert_eq!(pasted.recipe.basic.exposure, 1.0);
        assert_eq!(pasted.recipe.crop.left, 0.0);
        assert_eq!(pasted.history().entries().len(), 2);
    }

    #[test]
    fn pasting_onto_a_selection_sends_the_rest_to_the_catalog() {
        let mut state = state_with_photos(3);
        edit_field(&mut state, |recipe| recipe.basic.exposure = 1.0);
        state.apply(Action::CopySettings);
        state.apply(Action::Confirm);
        state.library.click(2, library::ClickKind::Extend);
        state.apply(Action::PasteSettings);
        let others = state.take_writes().into_iter().find_map(|write| match write {
            Write::PasteSettings { variants, .. } => Some(variants),
            _ => None,
        });
        assert_eq!(others, Some(vec![VariantId(0), VariantId(1)]));
    }

    #[test]
    fn status_messages_expire() {
        let mut state = AppState::default();
        state.notify("hello");
        assert!(state.status().is_some());
        state.now = 100.0;
        assert!(state.status().is_none());
    }

    #[test]
    fn stepping_leaves_the_crop_tool() {
        let mut state = state_with_photos(2);
        state.apply(Action::ToggleTool(Tool::Crop));
        state.apply(Action::NextPhoto);
        assert_eq!(state.tool, Tool::None);
        assert_eq!(state.library.focus(), Some(1));
    }

    #[test]
    fn up_and_down_move_a_grid_row() {
        let mut state = state_with_photos(30);
        state.module = Module::Library;
        state.grid_columns = 5;
        state.apply(Action::PhotoBelow);
        assert_eq!(state.library.focus(), Some(5));
        state.apply(Action::PhotoAbove);
        assert_eq!(state.library.focus(), Some(0));
    }

    #[test]
    fn choosing_an_aspect_crops_the_photo_to_it() {
        let mut state = state_with_photos(1);
        state.crop_tool.lock = AspectLock::Ratio(1, 1);
        state.apply_aspect_lock(Orientation::Normal, 1.5);
        let crop = state.edit().map(|edit| edit.recipe.crop).unwrap_or_default();
        assert!(((crop.right - crop.left) * 1.5 / (crop.bottom - crop.top) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn opening_a_file_imports_its_folder_and_remembers_the_file() {
        let mut state = AppState::default();
        state.open_paths(&["/photos/a.arw".into()]);
        assert_eq!(
            state.effects,
            [Effect::StartImport {
                root: "/photos".into(),
                focus: Some("/photos/a.arw".into())
            }]
        );
    }

    #[test]
    fn nothing_opened_means_nothing_imported() {
        let mut state = AppState::default();
        state.open_paths(&[]);
        assert!(state.effects.is_empty());
    }

    #[test]
    fn a_missing_file_says_where_it_was() {
        let mut state = state_with_photos(1);
        state
            .library
            .update_rows(&[VariantId(0)], |row| row.status = PhotoStatus::Missing);
        let DecodeState::Failed(message) = state.decode_state() else {
            panic!("expected a failure");
        };
        assert!(message.contains("/p/0.arw"), "{message}");
    }

    #[test]
    fn export_needs_something_selected() {
        let mut state = AppState::default();
        state.apply(Action::OpenExport);
        assert_eq!(state.dialog, None);
        let mut state = state_with_photos(1);
        state.apply(Action::OpenExport);
        assert_eq!(state.dialog, Some(Dialog::Export));
    }

    #[test]
    fn guides_stop_at_the_limit() {
        let mut state = state_with_photos(1);
        for index in 0..=MAX_GUIDES {
            let start = [index as f32 * 0.1, 0.0];
            state.add_guide(GuideLine {
                start,
                end: [0.5, 1.0],
            });
        }
        let guides = &state.edit().expect("edit").recipe.transform.guides;
        assert_eq!(guides.len(), MAX_GUIDES);
        assert_eq!(guides[0].start[0], 0.1);
    }

    #[test]
    fn the_guides_tool_turns_upright_on() {
        let mut state = state_with_photos(1);
        state.apply(Action::ToggleTool(Tool::Guides));
        assert_eq!(
            state.edit().map(|edit| edit.recipe.transform.upright),
            Some(Upright::Guided)
        );
    }

    #[test]
    fn soft_proofing_changes_nothing_about_the_photo() {
        let mut state = state_with_photos(1);
        let before = state.edit().expect("edit").clone();
        state.apply(Action::ToggleSoftProof);
        state.apply(Action::ToggleGamutWarning);
        state.end_frame();
        assert!(state.proof.enabled);
        assert_eq!(state.edit(), Some(&before));
        assert!(state.take_writes().is_empty());
    }

    #[test]
    fn the_first_proof_looks_for_profiles_once() {
        let mut state = state_with_photos(1);
        state.apply(Action::ToggleSoftProof);
        state.apply(Action::ToggleSoftProof);
        state.apply(Action::ToggleSoftProof);
        assert_eq!(state.effects, [Effect::SearchProofProfiles]);
    }

    #[test]
    fn auto_goes_through_an_effect_because_it_needs_the_photo() {
        let mut state = state_with_photos(1);
        state.apply(Action::AutoTone);
        assert_eq!(state.effects, [Effect::AutoTone]);
    }

    #[test]
    fn a_preset_changes_only_what_it_names() {
        let mut state = state_with_photos(1);
        edit_field(&mut state, |recipe| recipe.basic.contrast = 10.0);
        state.copy_selection.set_all(false);
        state.copy_selection.set(clipboard::SettingGroup::Presence, true);
        let mut source = Recipe::default();
        source.basic.vibrance = 25.0;
        let settings = partial_recipe(&source, &state.copy_selection);
        let preset = Preset {
            id: PresetId(1),
            group: "Mine".into(),
            name: "Pop".into(),
            settings,
        };
        state.apply_preset(&preset, false);
        state.end_frame();
        let edit = state.edit().expect("edit");
        assert_eq!(
            (edit.recipe.basic.contrast, edit.recipe.basic.vibrance),
            (10.0, 25.0)
        );
        assert_eq!(edit.history().current_label(), "Preset: Pop");
    }

    #[test]
    fn the_session_comes_back() {
        let state = state_with_photos(3);
        let session = state.session();
        let mut fresh = AppState {
            library: loaded(3),
            ..AppState::default()
        };
        fresh.restore_session(&session);
        assert_eq!(fresh.library.selection(), [VariantId(0)]);
    }
}
