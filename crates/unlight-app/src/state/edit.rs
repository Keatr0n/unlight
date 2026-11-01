//! One variant's edit: the live recipe, its history and snapshots. Widgets change `recipe`
//! directly and call `note`; `settle` turns that into a history step once the gesture ends,
//! and everything that should reach the catalog waits in `writes`.

use super::{history::History, sections::Section, write::Write};
use unlight_catalog::{Snapshot, SnapshotId, VariantId};
use unlight_recipe::Recipe;

/// Wheel ticks, and a click that may turn out to be half a double-click, wait this long for
/// the next one, so a burst of them is one step in the catalog.
const SETTLE_SECONDS: f64 = 0.35;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// A step of its own the moment the gesture ends, such as a drag.
    New,
    /// May be followed straight away by more of the same.
    Burst,
}

#[derive(Clone, Debug, PartialEq)]
struct PendingChange {
    control: String,
    value: String,
    step: Step,
    noted_at: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PhotoEdit {
    pub variant: VariantId,
    pub recipe: Recipe,
    pub snapshots: Vec<Snapshot>,
    history: History,
    pending: Option<PendingChange>,
    writes: Vec<Write>,
    /// The clock, kept so `note` needs no argument for it.
    now: f64,
}

impl PhotoEdit {
    pub fn new(variant: VariantId, recipe: Recipe) -> Self {
        Self::restored(variant, recipe.clone(), History::new(recipe), Vec::new())
    }

    pub fn restored(variant: VariantId, recipe: Recipe, history: History, snapshots: Vec<Snapshot>) -> Self {
        Self {
            variant,
            recipe,
            snapshots,
            history,
            pending: None,
            writes: Vec::new(),
            now: 0.0,
        }
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    /// Takes what should go to the catalog.
    pub fn take_writes(&mut self) -> Vec<Write> {
        std::mem::take(&mut self.writes)
    }

    /// Records what the next history step should be called.
    pub fn note(&mut self, control: &str, value: impl Into<String>, step: Step) {
        self.pending = Some(PendingChange {
            control: control.into(),
            value: value.into(),
            step,
            noted_at: self.now,
        });
    }

    /// Whether a change is waiting out its burst, so the UI knows to wake up for it.
    pub fn is_settling(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.step == Step::Burst)
    }

    /// Commits outstanding changes unless the user is still mid-gesture.
    pub fn settle(&mut self, gesture_active: bool, now: f64) {
        self.now = now;
        let waiting = self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.step == Step::Burst && now - pending.noted_at < SETTLE_SECONDS);
        if !gesture_active && !waiting {
            self.commit();
        }
    }

    /// Commits whatever is outstanding, whatever the user is doing.
    pub fn commit(&mut self) {
        let pending = self.pending.take();
        if self.recipe == *self.history.current() {
            return;
        }
        let label = match pending {
            Some(change) if change.value.is_empty() => change.control,
            Some(change) => format!("{}  {}", change.control, change.value),
            None => "Edit".to_owned(),
        };
        self.history.push(label.clone(), self.recipe.clone());
        self.writes.push(Write::SaveRecipe {
            variant: self.variant,
            recipe: Box::new(self.recipe.clone()),
            label,
        });
    }

    fn restore(&mut self) {
        self.recipe = self.history.current().clone();
        self.pending = None;
    }

    /// Steps back, returning the name of what was undone.
    pub fn undo(&mut self) -> Option<String> {
        self.commit();
        let label = self.history.current_label().to_owned();
        self.history.undo().then(|| {
            self.restore();
            self.writes.push(Write::Undo(self.variant));
            label
        })
    }

    pub fn redo(&mut self) -> Option<String> {
        self.commit();
        self.history.redo().then(|| {
            self.restore();
            self.writes.push(Write::Redo(self.variant));
            self.history.current_label().to_owned()
        })
    }

    pub fn jump_to(&mut self, index: usize) {
        self.commit();
        self.history.jump(index);
        self.restore();
        self.writes
            .push(Write::GoToStep(self.variant, self.history.current_seq()));
    }

    pub fn reset_section(&mut self, section: Section) {
        section.reset(&mut self.recipe);
        self.note(&format!("Reset {}", section.title()), "", Step::New);
    }

    /// Which way up a photo goes isn't an edit to undo with the rest.
    pub fn reset_all(&mut self) {
        self.recipe = Recipe { orientation: self.recipe.orientation, ..Recipe::default() };
        self.note("Reset All", "", Step::New);
    }

    pub fn take_snapshot(&mut self, name: String) {
        self.commit();
        self.writes.push(Write::CreateSnapshot {
            variant: self.variant,
            name,
        });
    }

    pub fn delete_snapshot(&mut self, id: SnapshotId) {
        self.snapshots.retain(|snapshot| snapshot.id != id);
        self.writes.push(Write::DeleteSnapshot {
            variant: self.variant,
            snapshot: id,
        });
    }

    pub fn restore_snapshot(&mut self, index: usize) {
        if let Some(snapshot) = self.snapshots.get(index) {
            self.recipe = snapshot.recipe.clone();
            let label = format!("Snapshot: {}", snapshot.name);
            self.note(&label, "", Step::New);
        }
    }

    pub fn is_bypassed(&self, section: Section) -> bool {
        section
            .panel()
            .is_some_and(|panel| self.recipe.disabled_panels.contains(&panel))
    }

    /// Switches a panel on or off. That's an edit like any other, so it can be undone.
    pub fn set_bypassed(&mut self, section: Section, bypassed: bool) {
        let Some(panel) = section.panel() else {
            return;
        };
        self.recipe.disabled_panels.retain(|existing| *existing != panel);
        if bypassed {
            self.recipe.disabled_panels.push(panel);
        }
        let state = if bypassed { "Off" } else { "On" };
        self.note(section.title(), state, Step::New);
    }

    /// The recipe to render.
    pub fn effective(&self) -> Recipe {
        self.recipe.effective()
    }

    /// The state to compare against, framed like the current edit so the two line up.
    pub fn before(&self) -> Recipe {
        let mut recipe = self.history.initial().clone();
        recipe.crop = self.recipe.crop;
        recipe.transform = self.recipe.transform.clone();
        recipe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit() -> PhotoEdit {
        PhotoEdit::new(VariantId(1), Recipe::default())
    }

    fn drag_to(edit: &mut PhotoEdit, exposure: f32) {
        edit.recipe.basic.exposure = exposure;
        edit.note("Exposure", format!("{exposure:+.2}"), Step::New);
    }

    fn saved_labels(edit: &mut PhotoEdit) -> Vec<String> {
        edit.take_writes()
            .into_iter()
            .filter_map(|write| match write {
                Write::SaveRecipe { label, .. } => Some(label),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_change_waits_for_the_gesture_to_end() {
        let mut edit = edit();
        drag_to(&mut edit, 0.5);
        edit.settle(true, 0.0);
        assert_eq!(edit.history().entries().len(), 1);
        drag_to(&mut edit, 1.0);
        edit.settle(true, 0.1);
        assert!(saved_labels(&mut edit).is_empty());
        edit.settle(false, 0.2);
        assert_eq!(edit.history().entries().len(), 2);
        assert_eq!(saved_labels(&mut edit), ["Exposure  +1.00"]);
    }

    #[test]
    fn two_quick_drags_stay_two_steps() {
        let mut edit = edit();
        drag_to(&mut edit, 0.5);
        edit.settle(false, 0.0);
        drag_to(&mut edit, 1.0);
        edit.settle(false, 0.1);
        assert_eq!(edit.history().entries().len(), 3);
    }

    #[test]
    fn wheel_ticks_wait_and_save_once() {
        let mut edit = edit();
        for tick in 1..=5 {
            edit.settle(false, f64::from(tick) * 0.1);
            edit.recipe.basic.contrast = tick as f32;
            edit.note("Contrast", format!("+{tick}"), Step::Burst);
            edit.settle(false, f64::from(tick) * 0.1);
        }
        assert!(edit.is_settling());
        assert!(saved_labels(&mut edit).is_empty());
        edit.settle(false, 2.0);
        assert_eq!(saved_labels(&mut edit), ["Contrast  +5"]);
        assert_eq!(edit.history().entries().len(), 2);
    }

    #[test]
    fn a_click_undone_by_a_double_click_leaves_no_step() {
        let mut edit = edit();
        edit.settle(false, 0.0);
        edit.recipe.basic.exposure = 1.0;
        edit.note("Exposure", "+1.00", Step::Burst);
        edit.settle(false, 0.1);
        edit.recipe.basic.exposure = 0.0;
        edit.note("Exposure", "0.00", Step::Burst);
        edit.settle(false, 2.0);
        assert!(edit.take_writes().is_empty());
        assert_eq!(edit.history().entries().len(), 1);
    }

    #[test]
    fn a_no_op_drag_leaves_no_step() {
        let mut edit = edit();
        drag_to(&mut edit, 0.0);
        edit.settle(false, 0.0);
        assert_eq!(edit.history().entries().len(), 1);
        assert!(edit.take_writes().is_empty());
    }

    #[test]
    fn undo_restores_the_recipe_and_tells_the_catalog() {
        let mut edit = edit();
        drag_to(&mut edit, 1.0);
        edit.settle(false, 0.0);
        edit.take_writes();
        assert_eq!(edit.undo().as_deref(), Some("Exposure  +1.00"));
        assert_eq!(edit.recipe.basic.exposure, 0.0);
        assert_eq!(edit.take_writes(), [Write::Undo(VariantId(1))]);
        assert_eq!(edit.redo().as_deref(), Some("Exposure  +1.00"));
        assert_eq!(edit.recipe.basic.exposure, 1.0);
    }

    #[test]
    fn undoing_a_change_still_in_its_burst_saves_it_first() {
        let mut edit = edit();
        edit.recipe.basic.contrast = 10.0;
        edit.note("Contrast", "+10", Step::Burst);
        edit.undo();
        let writes = edit.take_writes();
        assert!(matches!(writes[0], Write::SaveRecipe { .. }));
        assert_eq!(writes[1], Write::Undo(VariantId(1)));
    }

    #[test]
    fn a_panel_switch_is_an_undoable_edit() {
        let mut edit = edit();
        edit.recipe.effects.grain.amount = 40.0;
        edit.settle(false, 0.0);
        edit.set_bypassed(Section::Effects, true);
        edit.settle(false, 1.0);
        assert_eq!(edit.effective().effects.grain.amount, 0.0);
        assert_eq!(edit.recipe.effects.grain.amount, 40.0);
        edit.undo();
        assert_eq!(edit.effective().effects.grain.amount, 40.0);
    }

    #[test]
    fn snapshots_restore_as_a_new_step() {
        let mut edit = edit();
        drag_to(&mut edit, 1.0);
        edit.settle(false, 0.0);
        edit.snapshots.push(Snapshot {
            id: SnapshotId(1),
            variant: VariantId(1),
            name: "Bright".into(),
            recipe: edit.recipe.clone(),
            created_at: 0,
        });
        drag_to(&mut edit, -1.0);
        edit.settle(false, 1.0);
        edit.restore_snapshot(0);
        edit.settle(false, 2.0);
        assert_eq!(edit.recipe.basic.exposure, 1.0);
        assert_eq!(edit.history().current_label(), "Snapshot: Bright");
        assert_eq!(edit.history().entries().len(), 4);
    }

    #[test]
    fn before_keeps_the_current_framing() {
        let mut edit = edit();
        edit.recipe.basic.exposure = 2.0;
        edit.recipe.crop.left = 0.25;
        let before = edit.before();
        assert_eq!(before.basic.exposure, 0.0);
        assert_eq!(before.crop.left, 0.25);
    }
}
