//! Drawing. Everything here reads and writes `AppState`; the logic it calls lives in `state`.

#[cfg(feature = "ai")]
pub mod ai_tools;
pub mod crop_overlay;
pub mod dialogs;
pub mod filmstrip;
pub mod guides_overlay;
pub mod left_panel;
pub mod library;
pub mod local_overlay;
pub mod local_panels;
pub mod panels;
pub mod proof_panel;
pub mod right_panel;
pub mod theme;
pub mod toolbar;
pub mod top_bar;
pub mod viewport;
pub mod widgets;
