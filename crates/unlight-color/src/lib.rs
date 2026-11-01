//! Colour science that runs on the CPU: camera calibration, chromatic adaptation and
//! colour difference. Pixel work happens in the engine's shaders.

pub mod camera;
pub mod dcp;
pub mod difference;
pub mod matrix;
pub mod output;
pub mod profiles;
pub mod proof;
pub mod temperature;

pub use camera::CameraProfile;
pub use dcp::{Dcp, DcpError, HueSatTable};
pub use difference::{delta_e_2000, srgb_to_lab};
pub use matrix::{Mat3, Vec3};
pub use profiles::{BUILT_IN_PROFILE, PhotoColor, ProfileChoices, ProfileLibrary, profile_directory};
pub use proof::{ProofIntent, ProofLut};
