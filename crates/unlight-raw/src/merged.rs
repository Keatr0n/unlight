//! Merged photos: a linear, scene-referred, full-colour image that stands in for a raw. It
//! decodes to a `RawImage` like any other file, so everything that opens photos just works,
//! with `Cfa::FULL_COLOUR` telling the engine there's no mosaic to demosaic.
//!
//! The file is little endian, in this order: magic, header length and JSON header, preview
//! length and JPEG, a table of strip offsets, then the strips. Each strip holds three planes of
//! half floats, delta coded along the row, split into high and low bytes and deflated. Noise
//! lives in the low bytes, so the split keeps the high bytes compressing well.

use crate::{ColorData, EmbeddedLens, Error, Metadata, Opcodes, PhotoInfo, Preview, RawImage, Result};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
};

/// Lowercase extension of a merged file.
pub const EXTENSION: &str = "ulr";

const MAGIC: [u8; 8] = *b"ULRAW\0\x01\n";
const STRIP_ROWS: usize = 128;
const CHANNELS: usize = 3;
/// The texels a merged image decodes to are RGBA, and alpha is always this half float 1.
const OPAQUE: u16 = 0x3c00;
/// A header bigger than this isn't one of ours. The largest thing in it is a DNG gain map.
const MAX_HEADER: u32 = 256 << 20;
const MAX_PREVIEW: u32 = 256 << 20;
const MAX_EDGE: u32 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeKind {
    Hdr,
    Panorama,
}

impl MergeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Hdr => "HDR",
            Self::Panorama => "Panorama",
        }
    }
}

/// How a merged photo came to be.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MergeInfo {
    pub kind: MergeKind,
    /// The photos it was made from, as they were named at the time.
    pub sources: Vec<PathBuf>,
    /// Lens corrections are in the pixels already, so nothing may apply them again.
    pub lens_applied: bool,
}

#[derive(Serialize, Deserialize)]
struct Header {
    width: u32,
    height: u32,
    color: ColorData,
    metadata: Metadata,
    opcodes: Opcodes,
    embedded_lens: Option<EmbeddedLens>,
}

/// Everything a merged file stores. `pixels` are row-major linear RGB, balanced to the neutral
/// that `color` gives, the same as a demosaiced raw.
pub struct MergedPhoto<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [[f32; 3]],
    pub color: &'a ColorData,
    pub metadata: &'a Metadata,
    pub opcodes: &'a Opcodes,
    pub embedded_lens: Option<&'a EmbeddedLens>,
    pub preview_jpeg: &'a [u8],
}

fn malformed(path: &Path, detail: impl std::fmt::Display) -> Error {
    Error::Merged { path: path.to_owned(), message: detail.to_string() }
}

/// Single to half precision, rounding to nearest. Anything too large for a half saturates at
/// the largest finite one and NaN becomes zero, since a texture holding either poisons blurs.
pub fn half_from_f32(value: f32) -> u16 {
    if value.is_nan() {
        return 0;
    }
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let magnitude = bits & 0x7fff_ffff;
    const LARGEST_HALF: u32 = 0x477f_e000;
    if magnitude >= LARGEST_HALF {
        return sign | 0x7bff;
    }
    if magnitude >= 0x3880_0000 {
        let rebased = magnitude - (112 << 23);
        return sign | ((rebased + 0xfff + ((rebased >> 13) & 1)) >> 13) as u16;
    }
    if magnitude < 0x3300_0000 {
        return sign;
    }
    let exponent = magnitude >> 23;
    let mantissa = (magnitude & 0x007f_ffff) | 0x0080_0000;
    let shift = 126 - exponent;
    let halfway = 1u32 << (shift - 1);
    let rounded = (mantissa + halfway - 1 + ((mantissa >> shift) & 1)) >> shift;
    sign | rounded as u16
}

/// Half to single precision. Every half is exact in a single.
pub fn f32_from_half(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exponent = u32::from(bits >> 10) & 0x1f;
    let fraction = u32::from(bits & 0x3ff);
    let magnitude = match exponent {
        0 => (fraction as f32 / (1u32 << 24) as f32).to_bits(),
        31 => 0x7f80_0000 | (fraction << 13),
        _ => ((exponent + 112) << 23) | (fraction << 13),
    };
    f32::from_bits(sign | magnitude)
}

/// Whether a path names a merged file. Only the extension decides, so that opening one costs
/// no more than opening any other photo.
pub fn is_merged(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case(EXTENSION))
}

fn strip_count(height: u32) -> usize {
    (height as usize).div_ceil(STRIP_ROWS)
}

fn encode_strip(pixels: &[[f32; 3]], width: usize) -> std::io::Result<Vec<u8>> {
    let samples = pixels.len();
    let mut planes = vec![0u8; samples * CHANNELS * 2];
    let (high, low) = planes.split_at_mut(samples * CHANNELS);
    for channel in 0..CHANNELS {
        let high = &mut high[samples * channel..samples * (channel + 1)];
        let low = &mut low[samples * channel..samples * (channel + 1)];
        for (row, line) in pixels.chunks_exact(width).enumerate() {
            let start = row * width;
            let mut previous = 0u16;
            for (x, pixel) in line.iter().enumerate() {
                let value = half_from_f32(pixel[channel]);
                let delta = value.wrapping_sub(previous);
                previous = value;
                high[start + x] = (delta >> 8) as u8;
                low[start + x] = delta as u8;
            }
        }
    }
    let mut encoder = ZlibEncoder::new(Vec::with_capacity(samples * 4), Compression::fast());
    encoder.write_all(&planes)?;
    encoder.finish()
}

fn decode_strip(packed: &[u8], width: usize, rows: usize, out: &mut [u16]) -> std::io::Result<()> {
    let samples = width * rows;
    let mut planes = vec![0u8; samples * CHANNELS * 2];
    ZlibDecoder::new(packed).read_exact(&mut planes)?;
    let (high, low) = planes.split_at(samples * CHANNELS);
    for channel in 0..CHANNELS {
        let high = &high[samples * channel..samples * (channel + 1)];
        let low = &low[samples * channel..samples * (channel + 1)];
        for row in 0..rows {
            let start = row * width;
            let mut value = 0u16;
            for x in 0..width {
                let delta = u16::from_be_bytes([high[start + x], low[start + x]]);
                value = value.wrapping_add(delta);
                out[(start + x) * 4 + channel] = value;
            }
        }
    }
    (0..samples).for_each(|pixel| out[pixel * 4 + 3] = OPAQUE);
    Ok(())
}

/// Writes a merged file by renaming it into place, so a cancelled or crashed merge never leaves
/// something that looks like a photo.
pub fn write(path: &Path, photo: &MergedPhoto<'_>) -> Result<()> {
    let (width, height) = (photo.width as usize, photo.height as usize);
    if photo.pixels.len() != width * height {
        return Err(malformed(path, "pixel count doesn't match the size"));
    }
    let header = Header {
        width: photo.width,
        height: photo.height,
        color: photo.color.clone(),
        metadata: photo.metadata.clone(),
        opcodes: photo.opcodes.clone(),
        embedded_lens: photo.embedded_lens.cloned(),
    };
    let header = serde_json::to_vec(&header).map_err(|error| malformed(path, error))?;
    let strips: Vec<Vec<u8>> = photo
        .pixels
        .par_chunks(width * STRIP_ROWS)
        .map(|strip| encode_strip(strip, width))
        .collect::<std::io::Result<_>>()
        .map_err(|error| malformed(path, error))?;

    let temporary = path.with_extension(format!("{EXTENSION}.tmp{}", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut file = std::io::BufWriter::new(File::create(&temporary)?);
        file.write_all(&MAGIC)?;
        file.write_all(&(header.len() as u32).to_le_bytes())?;
        file.write_all(&header)?;
        file.write_all(&(photo.preview_jpeg.len() as u32).to_le_bytes())?;
        file.write_all(photo.preview_jpeg)?;
        let table_start = (MAGIC.len() + 4 + header.len() + 4 + photo.preview_jpeg.len()) as u64;
        let mut offset = table_start + (strips.len() as u64 + 1) * 8;
        for strip in &strips {
            file.write_all(&offset.to_le_bytes())?;
            offset += strip.len() as u64;
        }
        file.write_all(&offset.to_le_bytes())?;
        for strip in &strips {
            file.write_all(strip)?;
        }
        file.into_inner().map_err(std::io::IntoInnerError::into_error)?.sync_all()?;
        fs::rename(&temporary, path)
    })();
    written.map_err(|error| {
        let _ = fs::remove_file(&temporary);
        malformed(path, error)
    })
}

struct Layout {
    header: Header,
    preview: std::ops::Range<u64>,
    table: u64,
}

fn read_u32(file: &File, at: u64) -> std::io::Result<u32> {
    let mut bytes = [0; 4];
    file.read_exact_at(&mut bytes, at)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_layout(path: &Path, file: &File) -> Result<Layout> {
    let io = |error: std::io::Error| malformed(path, error);
    let mut magic = [0; 8];
    file.read_exact_at(&mut magic, 0).map_err(io)?;
    if magic != MAGIC {
        return Err(malformed(path, "not a merged Unlight photo"));
    }
    let header_length = read_u32(file, 8).map_err(io)?;
    if header_length > MAX_HEADER {
        return Err(malformed(path, "the header is too long"));
    }
    let mut bytes = vec![0; header_length as usize];
    file.read_exact_at(&mut bytes, 12).map_err(io)?;
    let mut header: Header = serde_json::from_slice(&bytes).map_err(|error| malformed(path, error))?;
    if header.width == 0 || header.height == 0 || header.width > MAX_EDGE || header.height > MAX_EDGE {
        return Err(malformed(path, format!("impossible size {}×{}", header.width, header.height)));
    }
    header.metadata.path = path.to_owned();
    let preview_at = 12 + u64::from(header_length);
    let preview_length = read_u32(file, preview_at).map_err(io)?;
    if preview_length > MAX_PREVIEW {
        return Err(malformed(path, "the preview is too long"));
    }
    let preview = preview_at + 4..preview_at + 4 + u64::from(preview_length);
    Ok(Layout { header, table: preview.end, preview })
}

fn open_merged(path: &Path) -> Result<(File, Layout)> {
    let file = File::open(path).map_err(|error| malformed(path, error))?;
    let layout = read_layout(path, &file)?;
    Ok((file, layout))
}

/// The headers of a merged file, without touching its pixels.
pub(crate) fn read_info(path: &Path) -> Result<PhotoInfo> {
    let (_, Layout { header, .. }) = open_merged(path)?;
    Ok(PhotoInfo { metadata: header.metadata, width: header.width, height: header.height })
}

pub(crate) fn read_preview(path: &Path) -> Result<Preview> {
    let (file, layout) = open_merged(path)?;
    if layout.preview.is_empty() {
        return Err(Error::NoPreview);
    }
    let mut bytes = vec![0; (layout.preview.end - layout.preview.start) as usize];
    file.read_exact_at(&mut bytes, layout.preview.start).map_err(|error| malformed(path, error))?;
    Ok(Preview::Jpeg(bytes))
}

/// Decodes a merged file. `samples` comes back as half float RGBA, four to a pixel.
pub(crate) fn read(path: &Path) -> Result<RawImage> {
    let (file, Layout { header, table, .. }) = open_merged(path)?;
    let (width, height) = (header.width as usize, header.height as usize);
    let strips = strip_count(header.height);
    let size = file.metadata().map_err(|error| malformed(path, error))?.len();
    let mut offsets = vec![0u8; (strips + 1) * 8];
    file.read_exact_at(&mut offsets, table).map_err(|error| malformed(path, error))?;
    let offsets: Vec<u64> = offsets
        .as_chunks::<8>()
        .0
        .iter()
        .map(|bytes| u64::from_le_bytes(*bytes))
        .collect();
    if offsets.windows(2).any(|pair| pair[0] > pair[1]) || offsets[strips] > size {
        return Err(malformed(path, "the strip table doesn't fit the file"));
    }

    let mut samples = vec![0u16; width * height * 4];
    samples
        .par_chunks_mut(width * STRIP_ROWS * 4)
        .enumerate()
        .try_for_each(|(index, strip)| {
            let mut packed = vec![0; (offsets[index + 1] - offsets[index]) as usize];
            file.read_exact_at(&mut packed, offsets[index])?;
            decode_strip(&packed, width, strip.len() / (width * 4), strip)
        })
        .map_err(|error| malformed(path, error))?;

    Ok(RawImage {
        width: header.width,
        height: header.height,
        samples,
        cfa: crate::Cfa::FULL_COLOUR,
        black: [[0.0; 6]; 6],
        white: 1.0,
        color: header.color,
        opcodes: header.opcodes,
        metadata: header.metadata,
        embedded_lens: header.embedded_lens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cfa, MergeKind};

    fn color() -> ColorData {
        ColorData {
            xyz_to_camera: [[1.0, 0.25, 0.0], [0.0, 1.0, 0.5], [0.0, 0.0, 1.0]],
            as_shot_multipliers: [2.0, 1.0, 1.5],
            daylight_multipliers: [2.1, 1.0, 1.4],
            baseline_exposure: 0.25,
            dng: None,
        }
    }

    fn metadata() -> Metadata {
        Metadata {
            make: "Test".into(),
            model: "Camera".into(),
            lens: "50mm".into(),
            iso: 200.0,
            shutter: 0.01,
            merge: Some(MergeInfo {
                kind: MergeKind::Hdr,
                sources: vec!["a.ARW".into(), "b.ARW".into()],
                lens_applied: false,
            }),
            ..Metadata::default()
        }
    }

    fn write_photo(path: &Path, width: usize, height: usize, pixels: &[[f32; 3]]) {
        let photo = MergedPhoto {
            width: width as u32,
            height: height as u32,
            pixels,
            color: &color(),
            metadata: &metadata(),
            opcodes: &Opcodes::default(),
            embedded_lens: None,
            preview_jpeg: b"\xff\xd8jpeg",
        };
        write(path, &photo).unwrap();
    }

    fn gradient(width: usize, height: usize) -> Vec<[f32; 3]> {
        (0..width * height)
            .map(|index| {
                let (x, y) = ((index % width) as f32, (index / width) as f32);
                [x * 0.01, y * 0.25 - 3.0, (x + y) * 1e-4]
            })
            .collect()
    }

    #[test]
    fn half_conversion_round_trips_every_half() {
        for bits in 0..=u16::MAX {
            let exponent = (bits >> 10) & 0x1f;
            if exponent == 31 {
                continue;
            }
            assert_eq!(half_from_f32(f32_from_half(bits)), bits, "{bits:#x}");
        }
    }

    #[test]
    fn half_conversion_rounds_to_the_nearest_half() {
        let next_up = f32_from_half(0x3c01);
        assert_eq!(half_from_f32(1.0 + (next_up - 1.0) * 0.4), 0x3c00);
        assert_eq!(half_from_f32(1.0 + (next_up - 1.0) * 0.6), 0x3c01);
        let tiny = f32_from_half(1);
        assert_eq!(half_from_f32(tiny * 0.4), 0);
        assert_eq!(half_from_f32(tiny * 0.6), 1);
        assert_eq!(half_from_f32(tiny * 2.6), 3);
    }

    #[test]
    fn half_conversion_saturates_and_drops_nan() {
        assert_eq!(half_from_f32(1e9), 0x7bff);
        assert_eq!(half_from_f32(-1e9), 0xfbff);
        assert_eq!(half_from_f32(f32::INFINITY), 0x7bff);
        assert_eq!(half_from_f32(f32::NAN), 0);
    }

    #[test]
    fn half_conversion_is_accurate_across_the_range() {
        for step in 0..3100 {
            let value = 1e-6 * 1.008f32.powi(step);
            let round_trip = f32_from_half(half_from_f32(value));
            let tolerance = (value * 5.1e-4).max(3.0e-8);
            assert!((round_trip - value).abs() <= tolerance, "{value} became {round_trip}");
        }
    }

    #[test]
    fn a_merged_file_round_trips_to_half_precision() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photo.ulr");
        let (width, height) = (333, 257);
        let pixels = gradient(width, height);
        write_photo(&path, width, height, &pixels);

        let raw = read(&path).unwrap();
        assert_eq!((raw.width, raw.height), (width as u32, height as u32));
        assert_eq!(raw.cfa, Cfa::FULL_COLOUR);
        assert_eq!(raw.samples.len(), width * height * 4);
        assert_eq!(raw.color, color());
        for (pixel, texel) in pixels.iter().zip(raw.samples.as_chunks::<4>().0) {
            for channel in 0..3 {
                assert_eq!(texel[channel], half_from_f32(pixel[channel]));
            }
            assert_eq!(f32_from_half(texel[3]), 1.0);
        }
        let expected = Metadata { path: path.clone(), ..metadata() };
        assert_eq!(raw.metadata, expected);
    }

    #[test]
    fn headers_and_preview_read_without_the_pixels() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photo.ULR");
        write_photo(&path, 16, 8, &gradient(16, 8));
        assert!(is_merged(&path));
        let info = read_info(&path).unwrap();
        assert_eq!((info.width, info.height), (16, 8));
        assert_eq!(info.metadata.model, "Camera");
        assert_eq!(read_preview(&path).unwrap(), Preview::Jpeg(b"\xff\xd8jpeg".to_vec()));
    }

    #[test]
    fn the_public_entry_points_open_merged_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photo.ulr");
        write_photo(&path, 16, 8, &gradient(16, 8));
        assert_eq!(crate::read_metadata(&path).unwrap().width, 16);
        assert_eq!(crate::load(&path).unwrap().height, 8);
        assert!(matches!(crate::load_preview(&path).unwrap(), Preview::Jpeg(_)));
        assert!(matches!(crate::load_preview_sized(&path, 512).unwrap(), Preview::Jpeg(_)));
    }

    #[test]
    fn writing_leaves_no_temporary_behind() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photo.ulr");
        write_photo(&path, 4, 4, &gradient(4, 4));
        let names: Vec<_> = fs::read_dir(directory.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(names, ["photo.ulr"]);
    }

    #[test]
    fn damaged_files_are_errors_rather_than_panics() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photo.ulr");
        write_photo(&path, 64, 300, &gradient(64, 300));
        let good = fs::read(&path).unwrap();

        for length in [0, 4, 11, 40, good.len() / 2, good.len() - 1] {
            fs::write(&path, &good[..length]).unwrap();
            assert!(read(&path).is_err(), "truncated to {length}");
        }
        let mut flipped = good.clone();
        flipped[0] ^= 0xff;
        fs::write(&path, &flipped).unwrap();
        assert!(read(&path).is_err());

        let mut inflated = good.clone();
        inflated[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(&path, &inflated).unwrap();
        assert!(read(&path).is_err());

        let mut noisy = good;
        let strips_start = noisy.len() - 2000;
        noisy[strips_start..strips_start + 64].fill(0x5a);
        fs::write(&path, &noisy).unwrap();
        assert!(read(&path).is_err());
    }
}
