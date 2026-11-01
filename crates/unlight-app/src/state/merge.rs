//! The Photo Merge dialog's state. The merge itself happens on a worker.

use std::path::PathBuf;
use unlight_merge::{Deghost, Projection};
use unlight_raw::{MergeKind, merged};

#[derive(Clone, Debug, PartialEq)]
pub struct MergeForm {
    pub kind: MergeKind,
    pub sources: Vec<PathBuf>,
    pub auto_align: bool,
    pub deghost: Deghost,
    pub projection: Projection,
    /// From 0 to 100, like the slider.
    pub boundary_warp: f32,
    pub auto_crop: bool,
}

impl MergeForm {
    pub fn new(kind: MergeKind, sources: Vec<PathBuf>) -> Self {
        Self {
            kind,
            sources,
            auto_align: true,
            deghost: Deghost::None,
            projection: Projection::Spherical,
            boundary_warp: 0.0,
            auto_crop: true,
        }
    }
}

/// What the dialog shows while a merge runs, and how it ended if that was badly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MergeRun {
    pub stage: String,
    pub fraction: f32,
    pub failure: Option<String>,
}

impl MergeRun {
    pub fn is_running(&self) -> bool {
        self.failure.is_none()
    }

    /// The note in the top bar while a merge runs behind the dialog.
    pub fn summary(&self) -> String {
        if self.stage.is_empty() {
            return "Merging".to_owned();
        }
        format!("Merging: {} {:.0}%", self.stage.to_lowercase(), self.fraction * 100.0)
    }
}

/// Why these photos can't be merged, if they can't.
pub fn merge_problem(paths: &[PathBuf]) -> Option<&'static str> {
    if paths.len() < 2 {
        return Some("Select at least two photos to merge");
    }
    paths
        .iter()
        .any(|path| merged::is_merged(path))
        .then_some("Merged photos can't be merged again")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn two_raws_can_be_merged() {
        assert_eq!(merge_problem(&paths(&["a.ARW", "b.ARW"])), None);
    }

    #[test]
    fn one_photo_or_a_merged_one_cant() {
        assert!(merge_problem(&paths(&["a.ARW"])).is_some());
        assert!(merge_problem(&paths(&["a.ARW", "b-HDR.ulr"])).is_some());
    }

    #[test]
    fn the_summary_says_how_far_along_it_is() {
        let run = MergeRun { stage: "Blending".into(), fraction: 0.42, failure: None };
        assert_eq!(run.summary(), "Merging: blending 42%");
        assert_eq!(MergeRun::default().summary(), "Merging");
    }
}
