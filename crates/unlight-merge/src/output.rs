//! Turns a finished merge into a file beside its sources.

use crate::{Frame, Image, Result, preview::preview_jpeg};
use std::path::{Path, PathBuf};
use unlight_color::Vec3;
use unlight_raw::{
    ColorData, EmbeddedLens, MergeInfo, MergeKind, Metadata, Opcodes,
    merged::{EXTENSION, MergedPhoto},
};

/// A finished merge, ready to be written.
#[derive(Clone, Debug)]
pub struct Merged {
    pub image: Image,
    pub color: ColorData,
    pub metadata: Metadata,
    pub opcodes: Opcodes,
    pub embedded_lens: Option<EmbeddedLens>,
    /// The white the pixels are balanced to.
    pub neutral: Vec3,
}

pub(crate) fn hdr_metadata(reference: &Frame, sources: Vec<PathBuf>) -> Metadata {
    Metadata {
        merge: Some(MergeInfo { kind: MergeKind::Hdr, sources, lens_applied: false }),
        path: PathBuf::new(),
        ..reference.metadata.clone()
    }
}

/// Lens corrections are baked into a panorama, and its focal length no longer means what it
/// did, so both the data and the 35 mm equivalent stay out of the merged photo. It's upright
/// on the canvas however the camera was held.
pub(crate) fn panorama_metadata(reference: &Frame, sources: Vec<PathBuf>) -> Metadata {
    Metadata {
        merge: Some(MergeInfo { kind: MergeKind::Panorama, sources, lens_applied: true }),
        path: PathBuf::new(),
        focal_length_35mm: 0.0,
        orientation: 0,
        ..reference.metadata.clone()
    }
}

/// The opcodes that are still to come once a frame is merged. Lists 1 and 2 apply before
/// demosaicing, so the pixels already have them.
pub(crate) fn merged_source_opcodes(reference: &Frame) -> Opcodes {
    Opcodes { list1: Vec::new(), list2: Vec::new(), list3: reference.opcodes.list3.clone() }
}

/// A free name for a merge of `sources`: `IMG_0001-HDR.ulr`, then `IMG_0001-HDR-2.ulr` and so on.
pub fn merged_path(sources: &[PathBuf], kind: MergeKind) -> Option<PathBuf> {
    let first = sources.first()?;
    let directory = first.parent().unwrap_or(Path::new(""));
    let stem = first.file_stem()?.to_string_lossy();
    let label = match kind {
        MergeKind::Hdr => "HDR",
        MergeKind::Panorama => "Pano",
    };
    (1u32..).find_map(|number| {
        let name = match number {
            1 => format!("{stem}-{label}.{EXTENSION}"),
            _ => format!("{stem}-{label}-{number}.{EXTENSION}"),
        };
        let path = directory.join(name);
        (!path.exists()).then_some(path)
    })
}

/// Writes a merge with a small rendered preview, for the Library to show.
pub fn write_merged(path: &Path, merged: &Merged) -> Result<()> {
    let preview = preview_jpeg(merged);
    let photo = MergedPhoto {
        width: merged.image.width as u32,
        height: merged.image.height as u32,
        pixels: &merged.image.pixels,
        color: &merged.color,
        metadata: &merged.metadata,
        opcodes: &merged.opcodes,
        embedded_lens: merged.embedded_lens.as_ref(),
        preview_jpeg: &preview,
    };
    unlight_raw::merged::write(path, &photo)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_raw::Cfa;

    #[test]
    fn names_go_beside_the_first_source_and_never_collide() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("IMG_0001.ARW");
        let sources = vec![first, directory.path().join("IMG_0002.ARW")];
        let name = merged_path(&sources, MergeKind::Hdr).unwrap();
        assert_eq!(name, directory.path().join("IMG_0001-HDR.ulr"));
        std::fs::write(&name, b"taken").unwrap();
        assert_eq!(merged_path(&sources, MergeKind::Hdr).unwrap(), directory.path().join("IMG_0001-HDR-2.ulr"));
        assert_eq!(merged_path(&sources, MergeKind::Panorama).unwrap(), directory.path().join("IMG_0001-Pano.ulr"));
    }

    #[test]
    fn a_written_merge_opens_as_a_photo() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("merged.ulr");
        let image = Image::from_fn(300, 200, |x, _| [x as f32 / 100.0; 3]);
        let reference = crate::frames::plain_frame(&image, 64);
        let merged = Merged {
            metadata: hdr_metadata(&reference, vec!["a.ARW".into(), "b.ARW".into()]),
            color: reference.color.clone(),
            opcodes: Opcodes::default(),
            embedded_lens: None,
            neutral: [1.0; 3],
            image,
        };
        write_merged(&path, &merged).unwrap();
        let raw = unlight_raw::load(&path).unwrap();
        assert_eq!((raw.width, raw.height), (300, 200));
        assert_eq!(raw.cfa, Cfa::FULL_COLOUR);
        let info = raw.metadata.merge.unwrap();
        assert_eq!(info.kind, MergeKind::Hdr);
        assert_eq!(info.sources.len(), 2);
        assert!(matches!(unlight_raw::load_preview(&path).unwrap(), unlight_raw::Preview::Jpeg(_)));
    }
}
