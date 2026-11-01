//! Connects stored masks to the engine, which looks them up by the id in a recipe.

use crate::mask::MaskStore;
use std::sync::OnceLock;
use unlight_engine::{MaskRaster, MaskRasterSource};

const FULL_COVERAGE: f32 = 65535.0;

/// Serves `MaskShape::Raster` from a mask store. A mask that can't be used selects nothing, as
/// the engine does for any raster it can't find, and says why on stderr.
#[derive(Debug)]
pub struct StoredMasks(MaskStore);

impl StoredMasks {
    pub fn new(store: MaskStore) -> Self {
        Self(store)
    }
}

impl MaskRasterSource for StoredMasks {
    fn load(&self, id: &str) -> Option<MaskRaster> {
        let mask = self.0.load(id).inspect_err(|error| eprintln!("can't use mask {id}: {error}")).ok()?;
        let coverage = mask.values.iter().map(|&value| f32::from(value) / FULL_COVERAGE).collect();
        Some(MaskRaster { width: mask.width, height: mask.height, coverage })
    }
}

/// The masks in the default location, for `Session::set_mask_rasters` and `ExportOptions`.
pub fn stored_masks() -> Option<&'static dyn MaskRasterSource> {
    static SOURCE: OnceLock<Option<StoredMasks>> = OnceLock::new();
    let source = SOURCE.get_or_init(|| MaskStore::open_default().ok().map(StoredMasks::new));
    source.as_ref().map(|masks| masks as &dyn MaskRasterSource)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask::MaskRaster as StoredRaster;
    use tempfile::TempDir;

    #[test]
    fn a_stored_mask_reaches_the_engine_as_unit_coverage() {
        let directory = TempDir::new().unwrap();
        let store = MaskStore::new(directory.path());
        let id = store.save(&StoredRaster { width: 2, height: 1, values: vec![0, 65535] }).unwrap();

        let raster = StoredMasks::new(store).load(&id).unwrap();

        assert_eq!((raster.width, raster.height), (2, 1));
        assert_eq!(raster.coverage, [0.0, 1.0]);
    }

    #[test]
    fn a_missing_or_unsafe_id_selects_nothing() {
        let directory = TempDir::new().unwrap();
        let source = StoredMasks::new(MaskStore::new(directory.path()));
        assert!(source.load(&"a".repeat(64)).is_none());
        assert!(source.load("../../etc/passwd").is_none());
    }
}
