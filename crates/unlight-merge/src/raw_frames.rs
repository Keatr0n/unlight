//! Frames from raw files. Decoding and demosaicing belong to the engine, which this crate
//! doesn't depend on, so a [`SceneReader`] supplies them. A raw is decoded twice at most: once
//! here for its small copy and again when its turn comes in the merge, and neither is kept.

use crate::{Frame, FrameSource, Image, MergeError, Progress, Result};
use std::path::PathBuf;
use unlight_color::CameraProfile;
use unlight_raw::RawImage;

/// What the engine does for a merge. Pixels come in the engine's scene space: linear camera
/// colour, divided by the as-shot neutral, before anything in Develop has touched them.
pub trait SceneReader: Sync {
    /// The whole frame, as RGBA.
    fn full(&self, raw: &RawImage) -> std::result::Result<Vec<[f32; 4]>, String>;

    /// A small copy with a long edge of at most `max_edge`, giving width, height and RGB pixels.
    fn reduced(&self, raw: &RawImage, max_edge: u32) -> std::result::Result<(u32, u32, Vec<[f32; 3]>), String>;
}

pub struct RawFrames<'a> {
    frames: Vec<Frame>,
    reader: &'a dyn SceneReader,
}

impl<'a> RawFrames<'a> {
    /// Opens `paths`, reading each one's small copy. Takes a few seconds a frame, so it reports
    /// progress and honours cancellation.
    pub fn open(paths: &[PathBuf], max_edge: u32, reader: &'a dyn SceneReader, progress: &Progress) -> Result<Self> {
        let mut frames = Vec::with_capacity(paths.len());
        for (index, path) in paths.iter().enumerate() {
            progress.step("Reading photos", index as f32 / paths.len().max(1) as f32)?;
            let raw = unlight_raw::load(path)?;
            if raw.cfa.is_full_colour() {
                return Err(MergeError::AlreadyMerged(path.clone()));
            }
            let (width, height, pixels) = reader.reduced(&raw, max_edge).map_err(MergeError::Source)?;
            let neutral = CameraProfile::new(&raw.color).as_shot_neutral();
            frames.push(Frame {
                path: path.clone(),
                width: raw.width as usize,
                height: raw.height as usize,
                metadata: raw.metadata,
                color: raw.color,
                opcodes: raw.opcodes,
                embedded_lens: raw.embedded_lens,
                balance: neutral.map(|value| (1.0 / value) as f32),
                reduced: Image { width: width as usize, height: height as usize, pixels },
            });
        }
        Ok(Self { frames, reader })
    }
}

impl FrameSource for RawFrames<'_> {
    fn frames(&self) -> &[Frame] {
        &self.frames
    }

    fn full(&self, index: usize) -> Result<Image> {
        let raw = unlight_raw::load(&self.frames[index].path)?;
        let pixels = self.reader.full(&raw).map_err(MergeError::Source)?;
        Ok(Image::from_rgba(raw.width as usize, raw.height as usize, &pixels))
    }
}
