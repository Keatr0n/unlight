//! The undo stack, mirrored from the catalog with every step's recipe in memory, so moving
//! through it costs nothing and the catalog just follows along.

use unlight_catalog::HISTORY_CAP;
use unlight_recipe::Recipe;

const FIRST_STEP: &str = "Start";

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    /// The catalog's step number.
    pub seq: i64,
    pub label: String,
    pub recipe: Recipe,
}

#[derive(Clone, Debug, PartialEq)]
pub struct History {
    entries: Vec<HistoryEntry>,
    cursor: usize,
}

impl History {
    /// A photo with no steps yet. The catalog makes the same first step on the first edit.
    pub fn new(initial: Recipe) -> Self {
        let first = HistoryEntry {
            seq: 1,
            label: FIRST_STEP.into(),
            recipe: initial,
        };
        Self {
            entries: vec![first],
            cursor: 0,
        }
    }

    /// Steps as the catalog stored them. `position` is the `seq` the photo is on.
    pub fn restored(entries: Vec<HistoryEntry>, position: Option<i64>, current: Recipe) -> Self {
        let cursor = entries.iter().position(|entry| Some(entry.seq) == position);
        match cursor {
            Some(cursor) => Self { entries, cursor },
            None => Self::new(current),
        }
    }

    pub fn current(&self) -> &Recipe {
        &self.entries[self.cursor].recipe
    }

    pub fn initial(&self) -> &Recipe {
        &self.entries[0].recipe
    }

    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn current_seq(&self) -> i64 {
        self.entries[self.cursor].seq
    }

    /// Adds a step, dropping any redo branch and the oldest steps past the catalog's cap.
    pub fn push(&mut self, label: String, recipe: Recipe) {
        self.entries.truncate(self.cursor + 1);
        let seq = self.current_seq() + 1;
        self.entries.push(HistoryEntry { seq, label, recipe });
        self.entries.retain(|entry| entry.seq > seq - HISTORY_CAP);
        self.cursor = self.entries.len() - 1;
    }

    pub fn undo(&mut self) -> bool {
        let moved = self.cursor > 0;
        self.cursor = self.cursor.saturating_sub(1);
        moved
    }

    pub fn redo(&mut self) -> bool {
        let moved = self.cursor + 1 < self.entries.len();
        if moved {
            self.cursor += 1;
        }
        moved
    }

    pub fn jump(&mut self, index: usize) {
        self.cursor = index.min(self.entries.len() - 1);
    }

    /// The label of the step undo would remove.
    pub fn current_label(&self) -> &str {
        &self.entries[self.cursor].label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_exposure(exposure: f32) -> Recipe {
        let mut recipe = Recipe::default();
        recipe.basic.exposure = exposure;
        recipe
    }

    #[test]
    fn undo_and_redo_walk_the_stack() {
        let mut history = History::new(Recipe::default());
        history.push("a".into(), with_exposure(1.0));
        history.push("b".into(), with_exposure(2.0));
        assert!(history.undo());
        assert_eq!(history.current().basic.exposure, 1.0);
        assert!(history.redo());
        assert_eq!(history.current().basic.exposure, 2.0);
        assert!(!history.redo());
    }

    #[test]
    fn a_new_step_discards_the_redo_branch() {
        let mut history = History::new(Recipe::default());
        history.push("a".into(), with_exposure(1.0));
        history.push("b".into(), with_exposure(2.0));
        history.undo();
        history.push("c".into(), with_exposure(3.0));
        assert_eq!(history.entries().len(), 3);
        assert!(!history.redo());
        assert_eq!(history.current_seq(), 3);
    }

    #[test]
    fn undo_stops_at_the_first_step() {
        let mut history = History::new(Recipe::default());
        assert!(!history.undo());
        assert_eq!(history.cursor(), 0);
    }

    #[test]
    fn steps_past_the_catalogs_cap_fall_off_the_bottom() {
        let mut history = History::new(Recipe::default());
        for step in 0..HISTORY_CAP + 10 {
            history.push(format!("step {step}"), with_exposure(step as f32));
        }
        assert_eq!(history.entries().len() as i64, HISTORY_CAP);
        assert_eq!(history.cursor(), history.entries().len() - 1);
    }

    #[test]
    fn jumping_keeps_every_entry_until_the_next_edit() {
        let mut history = History::new(Recipe::default());
        history.push("a".into(), with_exposure(1.0));
        history.push("b".into(), with_exposure(2.0));
        history.jump(0);
        assert_eq!(history.entries().len(), 3);
        assert_eq!(history.current(), &Recipe::default());
    }

    #[test]
    fn restoring_lands_on_the_stored_position() {
        let entries = vec![
            HistoryEntry {
                seq: 4,
                label: "Start".into(),
                recipe: Recipe::default(),
            },
            HistoryEntry {
                seq: 5,
                label: "a".into(),
                recipe: with_exposure(1.0),
            },
            HistoryEntry {
                seq: 6,
                label: "b".into(),
                recipe: with_exposure(2.0),
            },
        ];
        let history = History::restored(entries, Some(5), with_exposure(1.0));
        assert_eq!(history.cursor(), 1);
        assert_eq!(history.current().basic.exposure, 1.0);
    }

    #[test]
    fn a_photo_with_no_steps_starts_from_its_recipe() {
        let history = History::restored(Vec::new(), None, with_exposure(0.5));
        assert_eq!(history.entries().len(), 1);
        assert_eq!(history.current().basic.exposure, 0.5);
    }
}
