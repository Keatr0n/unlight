//! Soft masks on disk, under `$XDG_DATA_HOME/unlight/masks`. A recipe keeps only the id, which
//! is the hash of the mask's contents, so the same mask is stored once and damage is detected.
//! The files are ordinary 16-bit grayscale PNGs covering the whole sensor image.

use crate::{
    error::{Error, Result, io_error},
    paths::data_directory,
    store::to_hex,
};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
    process,
};

const ID_LENGTH: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskRaster {
    pub width: u32,
    pub height: u32,
    /// Row-major coverage, where 65535 is fully selected.
    pub values: Vec<u16>,
}

impl MaskRaster {
    pub(crate) fn from_unit(width: u32, height: u32, coverage: &[f32]) -> Self {
        let values = coverage.iter().map(|&value| (value.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).collect();
        Self { width, height, values }
    }

    /// The content hash a `MaskShape::Raster` refers to this mask by.
    pub fn id(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.width.to_le_bytes());
        hasher.update(self.height.to_le_bytes());
        for value in &self.values {
            hasher.update(value.to_le_bytes());
        }
        to_hex(&hasher.finalize())
    }

    pub fn write_png(&self, path: &Path) -> Result<()> {
        let format_error = |error: png::EncodingError| Error::MaskFormat {
            path: path.to_owned(),
            message: error.to_string(),
        };
        let file = File::create(path).map_err(io_error(path))?;
        let mut encoder = png::Encoder::new(BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Sixteen);
        let bytes: Vec<u8> = self.values.iter().flat_map(|value| value.to_be_bytes()).collect();
        let mut writer = encoder.write_header().map_err(format_error)?;
        writer.write_image_data(&bytes).map_err(format_error)?;
        writer.finish().map_err(format_error)
    }

    fn read_png(path: &Path) -> Result<Self> {
        let format_error = |message: String| Error::MaskFormat { path: path.to_owned(), message };
        let file = File::open(path).map_err(io_error(path))?;
        let mut reader = png::Decoder::new(BufReader::new(file))
            .read_info()
            .map_err(|error| format_error(error.to_string()))?;
        let mut bytes = vec![0; reader.output_buffer_size().unwrap_or(0)];
        let info = reader.next_frame(&mut bytes).map_err(|error| format_error(error.to_string()))?;
        if info.color_type != png::ColorType::Grayscale || info.bit_depth != png::BitDepth::Sixteen {
            return Err(format_error("not 16-bit grayscale".to_owned()));
        }
        let values = bytes[..info.buffer_size()].as_chunks::<2>().0.iter().map(|&pair| u16::from_be_bytes(pair)).collect();
        Ok(Self { width: info.width, height: info.height, values })
    }
}

#[derive(Clone, Debug)]
pub struct MaskStore {
    root: PathBuf,
}

impl MaskStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn open_default() -> Result<Self> {
        Ok(Self::new(data_directory()?.join("masks")))
    }

    /// Where a mask lives. Ids come from recipes, which can arrive in sidecars from anywhere,
    /// so one that isn't a plain hash is refused rather than joined onto a path.
    pub fn path(&self, id: &str) -> Result<PathBuf> {
        let is_hash = id.len() == ID_LENGTH
            && id.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !is_hash {
            return Err(Error::InvalidMaskId(id.to_owned()));
        }
        Ok(self.root.join(format!("{id}.png")))
    }

    /// Stores a mask and returns its id.
    pub fn save(&self, mask: &MaskRaster) -> Result<String> {
        let id = mask.id();
        let path = self.path(&id)?;
        if path.exists() {
            return Ok(id);
        }
        fs::create_dir_all(&self.root).map_err(io_error(&self.root))?;
        let partial = self.root.join(format!("{id}.{}.tmp", process::id()));
        let written = mask.write_png(&partial).and_then(|()| fs::rename(&partial, &path).map_err(io_error(&path)));
        if written.is_err() {
            let _ = fs::remove_file(&partial);
        }
        written.map(|()| id)
    }

    /// Loads a mask, checking it is still what its name says it is.
    pub fn load(&self, id: &str) -> Result<MaskRaster> {
        let mask = MaskRaster::read_png(&self.path(id)?)?;
        if mask.id() != id {
            return Err(Error::DamagedMask(id.to_owned()));
        }
        Ok(mask)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use tempfile::TempDir;

    fn sample() -> MaskRaster {
        MaskRaster { width: 3, height: 2, values: vec![0, 1, 256, 32768, 65534, 65535] }
    }

    #[test]
    fn a_mask_survives_a_round_trip_exactly() {
        let directory = TempDir::new().unwrap();
        let store = MaskStore::new(directory.path().join("masks"));
        let id = store.save(&sample()).unwrap();
        assert_eq!(store.load(&id).unwrap(), sample());
    }

    #[test]
    fn the_file_is_named_by_its_content_and_stored_once() {
        let directory = TempDir::new().unwrap();
        let store = MaskStore::new(directory.path());
        let (first, second) = (store.save(&sample()).unwrap(), store.save(&sample()).unwrap());
        assert_eq!(first, second);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        assert!(store.path(&first).unwrap().exists());
    }

    #[test]
    fn different_masks_get_different_ids() {
        let mut other = sample();
        other.values[0] = 1;
        assert_ne!(sample().id(), other.id());
        let reshaped = MaskRaster { width: 2, height: 3, ..sample() };
        assert_ne!(sample().id(), reshaped.id());
    }

    #[test]
    fn a_swapped_file_is_reported_as_damaged() {
        let directory = TempDir::new().unwrap();
        let store = MaskStore::new(directory.path());
        let id = store.save(&sample()).unwrap();
        let mut other = sample();
        other.values[2] = 7;
        other.write_png(&store.path(&id).unwrap()).unwrap();
        assert!(matches!(store.load(&id), Err(Error::DamagedMask(_))));
    }

    #[test]
    fn a_file_that_is_not_a_png_is_an_error_not_a_panic() {
        let directory = TempDir::new().unwrap();
        let store = MaskStore::new(directory.path());
        let id = store.save(&sample()).unwrap();
        fs::write(store.path(&id).unwrap(), b"not a png").unwrap();
        assert!(matches!(store.load(&id), Err(Error::MaskFormat { .. })));
    }

    #[test]
    fn ids_that_could_escape_the_mask_directory_are_refused() {
        let directory = TempDir::new().unwrap();
        let store = MaskStore::new(directory.path());
        let nasty = ["../../../etc/passwd", "", "abc", &"A".repeat(64), &format!("../{}", "a".repeat(61))];
        for id in nasty {
            assert!(matches!(store.load(id), Err(Error::InvalidMaskId(_))), "{id}");
        }
    }

    #[test]
    fn a_missing_mask_is_a_not_found_error() {
        let directory = TempDir::new().unwrap();
        let error = MaskStore::new(directory.path()).load(&"a".repeat(64)).unwrap_err();
        assert!(matches!(error, Error::Io { source, .. } if source.kind() == ErrorKind::NotFound));
    }

    #[test]
    fn coverage_is_clamped_to_the_full_range() {
        let mask = MaskRaster::from_unit(4, 1, &[-1.0, 0.0, 1.0, 2.0]);
        assert_eq!(mask.values, [0, 0, 65535, 65535]);
    }
}
