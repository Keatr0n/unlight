//! Everything that changes the catalog. State only produces these; the store applies them in
//! order on its writer thread, so the UI never waits on a write.

use super::clipboard::CopiedSettings;
use unlight_catalog::{
    CollectionId, Flag, FolderId, KeywordId, Label, PartialRecipe, PhotoId, PresetId, SmartRules, SnapshotId,
    VariantId,
};
use unlight_recipe::Recipe;

#[derive(Clone, Debug, PartialEq)]
pub enum Write {
    SaveRecipe {
        variant: VariantId,
        recipe: Box<Recipe>,
        label: String,
    },
    Undo(VariantId),
    Redo(VariantId),
    GoToStep(VariantId, i64),
    CreateSnapshot {
        variant: VariantId,
        name: String,
    },
    DeleteSnapshot {
        variant: VariantId,
        snapshot: SnapshotId,
    },
    SetFlag(Vec<VariantId>, Flag),
    SetRating(Vec<VariantId>, u8),
    SetLabel(Vec<VariantId>, Label),
    VirtualCopy(VariantId),
    /// Takes variants out of the catalog. A master takes its photo and any copies with it.
    RemoveVariants(Vec<VariantId>),
    /// Takes a folder, its subfolders and their photos out of the catalog.
    RemoveFolder(FolderId),
    PasteSettings {
        variants: Vec<VariantId>,
        settings: Box<CopiedSettings>,
    },
    AddKeywords {
        variants: Vec<VariantId>,
        names: Vec<String>,
    },
    RemoveKeyword {
        variants: Vec<VariantId>,
        keyword: KeywordId,
    },
    CreateCollection {
        name: String,
        variants: Vec<VariantId>,
    },
    AddToCollection {
        collection: CollectionId,
        variants: Vec<VariantId>,
    },
    RemoveFromCollection {
        collection: CollectionId,
        variants: Vec<VariantId>,
    },
    SaveSmartCollection {
        existing: Option<CollectionId>,
        name: String,
        rules: SmartRules,
    },
    DeleteCollection(CollectionId),
    SavePreset {
        group: String,
        name: String,
        settings: PartialRecipe,
    },
    DeletePreset(PresetId),
    /// Settles a sidecar conflict, one way or the other.
    ResolveSidecar {
        photo: PhotoId,
        keep_catalog: bool,
    },
}
