//! Optional local AI tools. Everything here runs on this computer: the only network traffic is
//! a model download the user asked for, and photos never leave the machine.

mod cancel;
mod denoise;
mod download;
mod error;
mod guided;
mod image;
mod inference;
mod manifest;
mod mask;
mod paths;
mod rasters;
mod select;
mod store;
mod worker;

pub use cancel::CancelToken;
pub use denoise::LinearImage;
pub use download::Progress;
pub use error::{Error, Result};
pub use image::{RgbImage, Turn, WORKING_EDGE, working_size};
pub use manifest::{Manifest, Model, Network, OutputScale, RestorationSpec, SegmentationSpec, Task};
pub use mask::{MaskRaster, MaskStore};
pub use paths::data_directory;
pub use rasters::{StoredMasks, stored_masks};
pub use store::{ModelStatus, ModelStore};
pub use worker::{Selection, Ticket, Worker};
