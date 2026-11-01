//! Photo Merge: HDR and panorama, on the CPU, on linear full-colour frames. Nothing here touches
//! a GPU, so every stage can be tested on synthetic data. The engine only supplies frames,
//! through [`FrameSource`], and renders the merged file like any other photo.

mod align;
mod blend;
mod boundary;
mod bundle;
mod crop;
mod deghost;
mod error;
mod exposure;
mod features;
mod frames;
mod hdr;
mod homography;
mod image;
mod lens;
mod linalg;
mod matching;
mod output;
mod panorama;
mod preview;
mod progress;
mod projection;
mod raw_frames;
mod refine;
mod resample;
mod seam;
mod stitch_gains;

pub use align::{Alignment, align_frames, measure_alignment};
pub use blend::Blender;
pub use boundary::BoundaryWarp;
pub use bundle::{Camera, Correspondence, PairMatches, adjust, estimate_focal};
pub use crop::largest_rectangle;
pub use error::{MergeError, Result};
pub use exposure::{ExposureEstimate, ExposureSource};
pub use features::{Feature, detect_features};
pub use frames::{Frame, FrameSource};
pub use hdr::{ANALYSIS_EDGE, Deghost, HdrOptions, HdrReport, merge_hdr};
pub use homography::{Homography, RansacFit, fit_homography};
pub use image::{Image, Plane, Rgb};
pub use lens::FrameLens;
pub use matching::match_features;
pub use output::{Merged, merged_path, write_merged};
pub use panorama::{DEFAULT_MAX_EDGE, PanoramaOptions, PanoramaReport, merge_panorama};
pub use progress::{Progress, ProgressState};
pub use projection::{Canvas, Projection};
pub use raw_frames::{RawFrames, SceneReader};
pub use seam::{Layer, NO_FRAME, SeamGraph, select_seams};
pub use stitch_gains::{OverlapSample, StitchGains, solve_gains};
