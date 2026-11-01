//! Everything the user can trigger by name, and Lightroom's keyboard bindings for it. Tooltips
//! read the same table, so a shortcut shown is always a shortcut that works.

use crate::state::Tool;
use eframe::egui::{Key, KeyboardShortcut, ModifierNames, Modifiers};
use unlight_catalog::{Flag, Label};
use unlight_raw::MergeKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ShowGrid,
    ShowLoupe,
    ShowCompare,
    ShowSurvey,
    ShowDevelop,
    SetFlag(Flag),
    SetRating(u8),
    ToggleLabel(Label),
    NextPhoto,
    PreviousPhoto,
    PhotoAbove,
    PhotoBelow,
    SelectAll,
    SelectNone,
    ToggleClipping,
    ToggleTool(Tool),
    ToggleCompare,
    CompareSplit,
    CompareSideBySide,
    ToggleZoom,
    ZoomIn,
    ZoomOut,
    Undo,
    Redo,
    ToggleSidePanels,
    ToggleAllPanels,
    ToggleFilmstrip,
    ToggleFullscreen,
    CycleLights,
    CopySettings,
    PasteSettings,
    ResetAll,
    ToggleBlackAndWhite,
    AutoTone,
    ToggleSoftProof,
    ToggleGamutWarning,
    VirtualCopy,
    OpenImport,
    OpenExport,
    PhotoMerge(MergeKind),
    Confirm,
    Cancel,
    ToggleAspectLock,
    ToggleMaskOverlay,
    ToolSmaller,
    ToolLarger,
    DeleteSelected,
}

const NONE: Modifiers = Modifiers::NONE;
const CTRL: Modifiers = Modifiers::COMMAND;
const SHIFT: Modifiers = Modifiers::SHIFT;
const ALT: Modifiers = Modifiers::ALT;
const CTRL_SHIFT: Modifiers = Modifiers {
    shift: true,
    ..Modifiers::COMMAND
};

/// Checked in order, so longer chords come first.
pub const BINDINGS: &[(Modifiers, Key, Action)] = &[
    (CTRL_SHIFT, Key::Z, Action::Redo),
    (CTRL_SHIFT, Key::C, Action::CopySettings),
    (CTRL_SHIFT, Key::V, Action::PasteSettings),
    (CTRL_SHIFT, Key::R, Action::ResetAll),
    (CTRL_SHIFT, Key::E, Action::OpenExport),
    (CTRL_SHIFT, Key::I, Action::OpenImport),
    (CTRL, Key::H, Action::PhotoMerge(MergeKind::Hdr)),
    (CTRL, Key::M, Action::PhotoMerge(MergeKind::Panorama)),
    (CTRL, Key::Z, Action::Undo),
    (CTRL, Key::U, Action::AutoTone),
    (CTRL, Key::O, Action::OpenImport),
    (CTRL, Key::A, Action::SelectAll),
    (CTRL, Key::D, Action::SelectNone),
    (CTRL, Key::Quote, Action::VirtualCopy),
    (CTRL, Key::Equals, Action::ZoomIn),
    (CTRL, Key::Plus, Action::ZoomIn),
    (CTRL, Key::Minus, Action::ZoomOut),
    (SHIFT, Key::Tab, Action::ToggleAllPanels),
    (SHIFT, Key::S, Action::ToggleGamutWarning),
    (ALT, Key::Y, Action::CompareSplit),
    (NONE, Key::G, Action::ShowGrid),
    (NONE, Key::D, Action::ShowDevelop),
    (NONE, Key::E, Action::ShowLoupe),
    (NONE, Key::C, Action::ShowCompare),
    (NONE, Key::N, Action::ShowSurvey),
    (NONE, Key::P, Action::SetFlag(Flag::Pick)),
    (NONE, Key::X, Action::SetFlag(Flag::Reject)),
    (NONE, Key::U, Action::SetFlag(Flag::Unflagged)),
    (NONE, Key::Num0, Action::SetRating(0)),
    (NONE, Key::Num1, Action::SetRating(1)),
    (NONE, Key::Num2, Action::SetRating(2)),
    (NONE, Key::Num3, Action::SetRating(3)),
    (NONE, Key::Num4, Action::SetRating(4)),
    (NONE, Key::Num5, Action::SetRating(5)),
    (NONE, Key::Num6, Action::ToggleLabel(Label::Red)),
    (NONE, Key::Num7, Action::ToggleLabel(Label::Yellow)),
    (NONE, Key::Num8, Action::ToggleLabel(Label::Green)),
    (NONE, Key::Num9, Action::ToggleLabel(Label::Blue)),
    (NONE, Key::ArrowRight, Action::NextPhoto),
    (NONE, Key::ArrowLeft, Action::PreviousPhoto),
    (NONE, Key::ArrowUp, Action::PhotoAbove),
    (NONE, Key::ArrowDown, Action::PhotoBelow),
    (NONE, Key::J, Action::ToggleClipping),
    (NONE, Key::R, Action::ToggleTool(Tool::Crop)),
    (NONE, Key::Q, Action::ToggleTool(Tool::Heal)),
    (NONE, Key::M, Action::ToggleTool(Tool::Masks)),
    (NONE, Key::O, Action::ToggleMaskOverlay),
    (NONE, Key::OpenBracket, Action::ToolSmaller),
    (NONE, Key::CloseBracket, Action::ToolLarger),
    (NONE, Key::Delete, Action::DeleteSelected),
    (NONE, Key::Backslash, Action::ToggleCompare),
    (NONE, Key::Y, Action::CompareSideBySide),
    (NONE, Key::Z, Action::ToggleZoom),
    (NONE, Key::Tab, Action::ToggleSidePanels),
    (NONE, Key::F, Action::ToggleFullscreen),
    (NONE, Key::L, Action::CycleLights),
    (NONE, Key::V, Action::ToggleBlackAndWhite),
    (NONE, Key::S, Action::ToggleSoftProof),
    (NONE, Key::T, Action::ToggleFilmstrip),
    (NONE, Key::A, Action::ToggleAspectLock),
    (NONE, Key::Enter, Action::Confirm),
    (NONE, Key::Escape, Action::Cancel),
];

/// The key for an action as text, such as `Ctrl+Shift+Z`.
pub fn shortcut_text(action: Action) -> Option<String> {
    let (modifiers, key, _) = BINDINGS.iter().find(|(_, _, bound)| *bound == action)?;
    let shortcut = KeyboardShortcut::new(*modifiers, *key);
    Some(shortcut.format(&ModifierNames::NAMES, false))
}

/// Tooltip text that teaches the shortcut.
pub fn tip(label: &str, action: Action) -> String {
    match shortcut_text(action) {
        Some(keys) => format!("{label}  ({keys})"),
        None => label.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_two_bindings_share_a_key() {
        for (index, (modifiers, key, _)) in BINDINGS.iter().enumerate() {
            let duplicate = BINDINGS[index + 1..]
                .iter()
                .any(|(other_modifiers, other_key, _)| other_modifiers == modifiers && other_key == key);
            assert!(!duplicate, "{modifiers:?} {key:?} is bound twice");
        }
    }

    #[test]
    fn tooltips_show_the_real_binding() {
        assert_eq!(tip("Undo", Action::Undo), "Undo  (Ctrl+Z)");
        assert_eq!(tip("Redo", Action::Redo), "Redo  (Ctrl+Shift+Z)");
        assert_eq!(tip("Crop", Action::ToggleTool(Tool::Crop)), "Crop  (R)");
    }
}
