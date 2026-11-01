//! GPU raw processing. Preview and export run the same WGSL pipeline; `Precision` only
//! changes the storage formats.

mod auto_tone;
mod compare;
mod curves;
mod demosaic;
mod denoise;
mod error;
mod export;
mod full_image;
mod geometry;
mod gpu;
mod guides;
mod heal_search;
mod lines;
mod local;
mod masks;
mod mosaic_repair;
mod noise;
mod optics;
mod params;
mod pipelines;
mod proof;
mod rendering;
mod repair;
mod scene_io;
mod scheduler;
mod session;
mod source;
mod stall;
mod timing;
mod tools;
mod upright;

pub use auto_tone::{AutoTone, TONE_BINS, ToneModel, ToneStats, auto_tone, exposure_value};
pub use curves::{LUT_SIZE, build_lut};
pub use compare::{PreviewDifference, PreviewPair, preview_difference, preview_pair};
pub use demosaic::xtrans_hex_table;
pub use error::{EngineError, Result};
pub use unlight_color::output::OutputSpace;
pub use export::{
    ExportOptions, ExportedImage, FaultInjection, ReducedScene, export, export_with_scene, read_reduced, read_scene,
};
pub use geometry::{Geometry, Optics, ViewRect};
pub use gpu::GpuContext;
pub use lines::{
    LumaImage, Segment, Straightness, detect_segments, join_collinear, measure_long_lines, straightness,
};
pub use masks::{MaskRaster, MaskRasterSource};
pub use unlight_lens::{LensError, LensProfile, PhotoLens, Source as LensSource};
pub use noise::NoiseModel;
pub use pipelines::{Engine, Precision};
pub use proof::{Proof, ProofTarget};
pub use unlight_color::{
    BUILT_IN_PROFILE, ProfileChoices, ProfileLibrary, ProofIntent, ProofLut, profile_directory,
};
pub use scheduler::{SchedulerConfig, Tile, TileScheduler};
pub use session::{Session, ViewRequest, ViewTarget};
pub use source::{Preview, SourceImage};
pub use stall::{StallOutcome, stall_device};
pub use timing::{FrameTimings, GpuProfiler};
pub use tools::{ImageStats, resolve_neutral};
