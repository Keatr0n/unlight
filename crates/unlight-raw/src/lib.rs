//! Raw decoding through LibRaw. Only the sensor mosaic and metadata come from LibRaw;
//! demosaicing and colour happen in the GPU engine.

mod embedded;
pub mod merged;
mod opcodes;

pub use embedded::{
    EmbeddedData, EmbeddedLens, FujifilmLens, NikonLens, PanasonicLens, SonyLens, parse_embedded,
};
pub use merged::{MergeInfo, MergeKind};
pub use opcodes::{
    FixVignetteRadial, GainMap, Opcode, Opcodes, WarpPlane, WarpRectilinear, parse_opcode_list,
};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    ffi::{CStr, CString, c_char},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use unlight_raw_sys as sys;

/// A clip spike needs this many samples at one level, and this many times the local
/// average, to count as sensor saturation rather than a few hot pixels.
const CLIP_SPIKE_MIN_SAMPLES: u64 = 32;
/// Stuck pixels also sit at the top code, so a real spike has to be a share of the frame too.
/// The 5D Mark IV has 178 of them in 30 MP, which would otherwise read as saturation.
const CLIP_SPIKE_MIN_SHARE: u64 = 50_000;
const CLIP_SPIKE_CONTRAST: u64 = 8;
const CLIP_SPIKE_WINDOW: usize = 128;
/// Skipped below a candidate level, because a spike can spread over two adjacent levels.
const CLIP_SPIKE_GAP: usize = 2;
/// Sentinel LibRaw leaves in `baseline_exposure` when the tag is absent.
const NO_BASELINE_EXPOSURE: f32 = -999.0;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("path contains a NUL byte")]
    InvalidPath,
    #[error("{operation} failed: {message}")]
    LibRaw { operation: &'static str, message: String },
    #[error("unsupported raw layout in {camera}: {detail}")]
    UnsupportedLayout { camera: String, detail: String },
    #[error("{camera} uses a compression LibRaw can't decode, the embedded preview may still work")]
    UnsupportedCompression { camera: String },
    #[error("no embedded preview found")]
    NoPreview,
    #[error("couldn't read merged photo {}: {message}", path.display())]
    Merged { path: PathBuf, message: String },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Colour filter layout, stored as a 6×6 tile so Bayer and X-Trans share one form.
/// Sites hold 0 for red, 1 for green and 2 for blue, relative to the visible origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cfa {
    pub period: u32,
    pub pattern: [[u8; 6]; 6],
}

impl Cfa {
    /// A photo with every colour at every pixel, which has nothing to demosaic. Its `samples`
    /// are half float RGBA, four to a pixel, rather than sensor values.
    pub const FULL_COLOUR: Self = Self { period: 1, pattern: [[1; 6]; 6] };

    pub fn color_at(&self, x: u32, y: u32) -> u8 {
        self.pattern[(y % 6) as usize][(x % 6) as usize]
    }

    pub fn is_xtrans(&self) -> bool {
        self.period == 6
    }

    pub fn is_full_colour(&self) -> bool {
        self.period == 1
    }
}

/// One DNG calibration illuminant.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DngIlluminant {
    /// EXIF LightSource code.
    pub illuminant: u16,
    /// XYZ to camera.
    pub color_matrix: [[f32; 3]; 3],
    /// Camera to XYZ D50, when the file provides one.
    pub forward_matrix: Option<[[f32; 3]; 3]>,
    pub calibration: [[f32; 3]; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DngColor {
    pub illuminants: Vec<DngIlluminant>,
    pub analog_balance: [f32; 3],
    pub as_shot_neutral: Option<[f32; 3]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorData {
    /// XYZ to camera for a D65 scene.
    pub xyz_to_camera: [[f32; 3]; 3],
    /// Camera white balance multipliers, green normalised to 1.
    pub as_shot_multipliers: [f32; 3],
    pub daylight_multipliers: [f32; 3],
    pub baseline_exposure: f32,
    pub dng: Option<DngColor>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub make: String,
    pub model: String,
    pub lens: String,
    pub iso: f32,
    pub shutter: f32,
    pub aperture: f32,
    pub focal_length: f32,
    /// 35 mm equivalent focal length, or 0 when the file doesn't state one.
    pub focal_length_35mm: f32,
    pub timestamp: i64,
    /// LibRaw flip code: 0 none, 3 is 180°, 5 is 90° CCW, 6 is 90° CW.
    pub orientation: i32,
    /// Where the file is, for tags LibRaw doesn't read. Empty for an image made in memory.
    #[serde(skip)]
    pub path: PathBuf,
    /// Set on a photo that was merged from others.
    pub merge: Option<MergeInfo>,
}

impl Metadata {
    pub fn camera(&self) -> String {
        format!("{} {}", self.make, self.model)
    }
}

/// What a file's headers say about the photo, without touching the sensor data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PhotoInfo {
    pub metadata: Metadata,
    /// Visible size before orientation is applied.
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug)]
pub struct RawImage {
    pub width: u32,
    pub height: u32,
    /// Row-major sensor values for the visible area.
    pub samples: Vec<u16>,
    pub cfa: Cfa,
    /// Black level per position in the 6×6 CFA tile.
    pub black: [[f32; 6]; 6],
    pub white: f32,
    pub color: ColorData,
    pub opcodes: Opcodes,
    pub metadata: Metadata,
    /// Correction data the camera stored in the file, if it has any we can read.
    pub embedded_lens: Option<EmbeddedLens>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Preview {
    Jpeg(Vec<u8>),
    Rgb8 { width: u32, height: u32, data: Vec<u8> },
}

struct Handle(*mut sys::libraw_data_t);

impl Handle {
    fn open(path: &Path) -> Result<Self> {
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::InvalidPath)?;
        // SAFETY: flags of 0 request default initialisation; a null result is checked.
        let raw = unsafe { sys::libraw_init(0) };
        if raw.is_null() {
            return Err(Error::LibRaw {
                operation: "libraw_init",
                message: "allocation failed".into(),
            });
        }
        let handle = Self(raw);
        // SAFETY: the handle is valid and `path` outlives the call.
        check("libraw_open_file", unsafe { sys::libraw_open_file(handle.0, path.as_ptr()) })?;
        Ok(handle)
    }

    fn data(&self) -> &sys::libraw_data_t {
        // SAFETY: the pointer is non-null and owned by this handle until drop.
        unsafe { &*self.0 }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the pointer came from libraw_init and is closed exactly once.
        unsafe { sys::libraw_close(self.0) };
    }
}

fn check(operation: &'static str, code: i32) -> Result<()> {
    if code == 0 {
        return Ok(());
    }
    // SAFETY: libraw_strerror returns a static string for any code.
    let message = unsafe { CStr::from_ptr(sys::libraw_strerror(code)) };
    Err(Error::LibRaw { operation, message: message.to_string_lossy().into_owned() })
}

fn text(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

fn metadata(data: &sys::libraw_data_t, path: &Path) -> Metadata {
    Metadata {
        make: text(&data.idata.make),
        model: text(&data.idata.model),
        lens: text(&data.lens.Lens),
        iso: data.other.iso_speed,
        shutter: data.other.shutter,
        aperture: data.other.aperture,
        focal_length: data.other.focal_len,
        focal_length_35mm: f32::from(data.lens.FocalLengthIn35mmFormat),
        timestamp: data.other.timestamp,
        orientation: data.sizes.flip,
        path: path.to_owned(),
        merge: None,
    }
}

/// LibRaw's `FC` macro. Returns 0–3, where 3 is the second green.
fn bayer_site(filters: u32, row: u32, col: u32) -> u8 {
    ((filters >> ((((row << 1) & 14) | (col & 1)) << 1)) & 3) as u8
}

fn read_cfa(data: &sys::libraw_data_t, camera: &str) -> Result<Cfa> {
    let filters = data.idata.filters;
    let mut pattern = [[0u8; 6]; 6];
    match filters {
        9 => {
            for (row, sites) in pattern.iter_mut().enumerate() {
                for (col, site) in sites.iter_mut().enumerate() {
                    *site = data.idata.xtrans[row][col] as u8;
                }
            }
            Ok(Cfa { period: 6, pattern })
        }
        0 | 1 => Err(Error::UnsupportedLayout {
            camera: camera.into(),
            detail: format!("no Bayer or X-Trans mosaic (filters = {filters})"),
        }),
        _ => {
            for row in 0..8 {
                for col in 0..2 {
                    let wraps = bayer_site(filters, row, col) & 1
                        == bayer_site(filters, row % 2, col) & 1;
                    if !wraps {
                        return Err(Error::UnsupportedLayout {
                            camera: camera.into(),
                            detail: format!("CFA is not 2×2 (filters = {filters:#x})"),
                        });
                    }
                }
            }
            for (row, sites) in pattern.iter_mut().enumerate() {
                for (col, site) in sites.iter_mut().enumerate() {
                    *site = match bayer_site(filters, row as u32, col as u32) {
                        3 => 1,
                        color => color,
                    };
                }
            }
            Ok(Cfa { period: 2, pattern })
        }
    }
}

/// Resolves LibRaw's layered black levels (global, per channel, repeating pattern) to one
/// value per CFA tile position.
fn read_black(data: &sys::libraw_data_t, cfa: &Cfa) -> [[f32; 6]; 6] {
    let color = &data.color;
    let (block_w, block_h) = (color.cblack[5] as usize, color.cblack[4] as usize);
    let cblack = &color.cblack;
    let tiles_evenly = block_w > 0 && block_h > 0 && 6 % block_w == 0 && 6 % block_h == 0;
    let block_mean = if block_w > 0 && block_h > 0 && !tiles_evenly {
        let cells = block_w * block_h;
        cblack[6..6 + cells].iter().map(|&v| v as f32).sum::<f32>() / cells as f32
    } else {
        0.0
    };

    let mut black = [[0.0; 6]; 6];
    for (row, sites) in black.iter_mut().enumerate() {
        for (col, site) in sites.iter_mut().enumerate() {
            let channel = if cfa.is_xtrans() {
                cfa.pattern[row][col] as usize
            } else {
                bayer_site(data.idata.filters, row as u32, col as u32) as usize
            };
            let block = if tiles_evenly {
                cblack[6 + (row % block_h) * block_w + col % block_w] as f32
            } else {
                block_mean
            };
            *site = color.black as f32 + cblack[channel] as f32 + block;
        }
    }
    black
}

/// Finds the level where the sensor saturated, which shows up as a spike at the top of the
/// histogram. Scans no lower than `floor` so the main body of the image can't match.
fn find_clip_spike(samples: &[u16], floor: usize) -> Option<u16> {
    let histogram = samples
        .par_chunks(1 << 16)
        .fold(
            || vec![0u64; 1 << 16],
            |mut bins, chunk| {
                chunk.iter().for_each(|&sample| bins[sample as usize] += 1);
                bins
            },
        )
        .reduce(
            || vec![0u64; 1 << 16],
            |mut totals, bins| {
                totals.iter_mut().zip(bins).for_each(|(total, count)| *total += count);
                totals
            },
        );

    let least = CLIP_SPIKE_MIN_SAMPLES.max(samples.len() as u64 / CLIP_SPIKE_MIN_SHARE);
    let start = floor.max(CLIP_SPIKE_WINDOW + CLIP_SPIKE_GAP);
    (start..histogram.len()).rev().find_map(|level| {
        let count = histogram[level];
        let end = level - CLIP_SPIKE_GAP;
        let below = &histogram[end - CLIP_SPIKE_WINDOW..end];
        // Counting only occupied levels keeps a quantised tone curve, whose levels sit far
        // apart, from making every level look like a spike over empty neighbours.
        let occupied = below.iter().filter(|&&count| count > 0).count() as u64;
        let local_mean = below.iter().sum::<u64>() / occupied.max(1);
        (count >= least && count >= CLIP_SPIKE_CONTRAST * local_mean.max(1))
            .then_some(level as u16)
    })
}

/// DNG files state their white level outright. Everything else uses a measured clip spike
/// when the photo has one. `linear_max` is a guess on most makes but holds on Canon, whose
/// saturation point moves with ISO.
fn read_white(data: &sys::libraw_data_t, samples: &[u16], black: &[[f32; 6]; 6]) -> f32 {
    let maximum = data.color.maximum as f32;
    if data.idata.dng_version != 0 {
        return maximum;
    }
    let black = black.iter().flatten().copied().fold(f32::MAX, f32::min);
    let floor = (black + (maximum - black) * 0.5) as usize;
    if let Some(level) = find_clip_spike(samples, floor) {
        return level as f32;
    }
    let linear_max = data.color.linear_max[0] as f32;
    let is_canon = text(&data.idata.normalized_make).eq_ignore_ascii_case("canon");
    if is_canon && linear_max > black { linear_max } else { maximum }
}

fn normalized_multipliers(values: &[f32; 4]) -> Option<[f32; 3]> {
    let green = if values[1] > 0.0 { values[1] } else { values[3] };
    (values[0] > 0.0 && green > 0.0 && values[2] > 0.0)
        .then(|| [values[0] / green, 1.0, values[2] / green])
}

fn matrix3<const R: usize, const C: usize>(source: &[[f32; C]; R]) -> [[f32; 3]; 3] {
    std::array::from_fn(|row| std::array::from_fn(|col| source[row][col]))
}

fn has_values<const R: usize, const C: usize>(matrix: &[[f32; C]; R]) -> bool {
    matrix.iter().flatten().any(|&value| value != 0.0)
}

/// LibRaw never sets `parsedfields` on these structs, so presence is judged by content.
fn read_dng_color(data: &sys::libraw_data_t) -> Option<DngColor> {
    if data.idata.dng_version == 0 {
        return None;
    }
    let color = &data.color;
    let illuminants: Vec<_> = color
        .dng_color
        .iter()
        .filter(|entry| has_values(&entry.colormatrix))
        .map(|entry| DngIlluminant {
            illuminant: entry.illuminant,
            color_matrix: matrix3(&entry.colormatrix),
            forward_matrix: has_values(&entry.forwardmatrix).then(|| matrix3(&entry.forwardmatrix)),
            calibration: if has_values(&entry.calibration) {
                matrix3(&entry.calibration)
            } else {
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
            },
        })
        .collect();
    if illuminants.is_empty() {
        return None;
    }
    let levels = &color.dng_levels;
    let balance = &levels.analogbalance;
    let neutral = &levels.asshotneutral;
    let has_neutral = neutral[..3].iter().all(|&value| value > 0.0);
    Some(DngColor {
        illuminants,
        analog_balance: [balance[0], balance[1], balance[2]],
        as_shot_neutral: has_neutral.then_some([neutral[0], neutral[1], neutral[2]]),
    })
}

fn read_color(data: &sys::libraw_data_t) -> ColorData {
    let color = &data.color;
    let daylight = normalized_multipliers(&color.pre_mul).unwrap_or([1.0; 3]);
    let baseline = color.dng_levels.baseline_exposure;
    ColorData {
        xyz_to_camera: matrix3(&color.cam_xyz),
        as_shot_multipliers: normalized_multipliers(&color.cam_mul).unwrap_or(daylight),
        daylight_multipliers: daylight,
        baseline_exposure: if baseline == NO_BASELINE_EXPOSURE { 0.0 } else { baseline },
        dng: read_dng_color(data),
    }
}

fn read_opcodes(data: &sys::libraw_data_t) -> Opcodes {
    let lists = data.color.dng_levels.rawopcodes.map(|list| {
        if list.data.is_null() || list.len == 0 {
            return Vec::new();
        }
        // SAFETY: LibRaw owns `len` bytes at `data` until the handle closes.
        let bytes = unsafe { std::slice::from_raw_parts(list.data as *const u8, list.len as usize) };
        parse_opcode_list(bytes).unwrap_or_default()
    });
    let [list1, list2, list3] = lists;
    Opcodes { list1, list2, list3 }
}

fn copy_visible(data: &sys::libraw_data_t) -> Vec<u16> {
    let sizes = &data.sizes;
    let (width, height) = (sizes.width as usize, sizes.height as usize);
    let pitch = sizes.raw_pitch as usize / 2;
    let (top, left) = (sizes.top_margin as usize, sizes.left_margin as usize);
    let visible = (top + height - 1) * pitch + left + width;
    // SAFETY: LibRaw guarantees raw_height rows of raw_pitch bytes, and the visible area
    // lies inside them. The caller has checked raw_image is non-null.
    let raw = unsafe { std::slice::from_raw_parts(data.rawdata.raw_image, visible) };
    let mut samples = vec![0u16; width * height];
    samples.par_chunks_mut(width).enumerate().for_each(|(row, out)| {
        let start = (top + row) * pitch + left;
        out.copy_from_slice(&raw[start..start + width]);
    });
    samples
}

/// Only these makes store corrections we can read, so nothing else pays to re-read the file.
fn read_embedded(path: &Path, make: &str) -> EmbeddedData {
    const MAKES: [&str; 4] = ["sony", "fujifilm", "panasonic", "nikon"];
    if !MAKES.iter().any(|known| make.eq_ignore_ascii_case(known)) {
        return EmbeddedData::default();
    }
    std::fs::read(path).map(|bytes| parse_embedded(&bytes)).unwrap_or_default()
}

/// Reads metadata only. This opens the file without unpacking it, so it is cheap enough to
/// run over a whole library at import.
pub fn read_metadata(path: &Path) -> Result<PhotoInfo> {
    if merged::is_merged(path) {
        return merged::read_info(path);
    }
    let handle = Handle::open(path)?;
    let data = handle.data();
    Ok(PhotoInfo {
        metadata: metadata(data, path),
        width: data.sizes.width as u32,
        height: data.sizes.height as u32,
    })
}

/// Decodes a raw file to its visible mosaic and metadata.
pub fn load(path: &Path) -> Result<RawImage> {
    if merged::is_merged(path) {
        return merged::read(path);
    }
    let handle = Handle::open(path)?;
    let mut meta = metadata(handle.data(), path);
    let camera = meta.camera();
    let embedded = read_embedded(path, &meta.make);
    if let (true, Some(name)) = (meta.lens.is_empty(), embedded.lens_name) {
        meta.lens = name;
    }

    // SAFETY: the handle holds an opened file.
    let unpacked = check("libraw_unpack", unsafe { sys::libraw_unpack(handle.0) });
    match unpacked {
        Err(Error::LibRaw { message, .. }) if message.to_lowercase().contains("unsupported") => {
            return Err(Error::UnsupportedCompression { camera });
        }
        other => other?,
    }

    let data = handle.data();
    if data.rawdata.raw_image.is_null() {
        return Err(Error::UnsupportedLayout {
            camera,
            detail: "decoded to full-colour data rather than a mosaic".into(),
        });
    }
    let cfa = read_cfa(data, &camera)?;
    let samples = copy_visible(data);
    let black = read_black(data, &cfa);
    let white = read_white(data, &samples, &black);
    Ok(RawImage {
        width: data.sizes.width as u32,
        height: data.sizes.height as u32,
        samples,
        cfa,
        black,
        white,
        color: read_color(data),
        opcodes: read_opcodes(data),
        metadata: meta,
        embedded_lens: embedded.corrections,
    })
}

/// Extracts the largest embedded preview without decoding the sensor data.
pub fn load_preview(path: &Path) -> Result<Preview> {
    if merged::is_merged(path) {
        return merged::read_preview(path);
    }
    let handle = Handle::open(path)?;
    unpack_largest_preview(&handle)?;
    make_preview(&handle)
}

/// Extracts the smallest embedded JPEG whose long edge reaches `min_long_edge`, so a small
/// target doesn't pay to decode a full-resolution preview. Uses the largest preview when
/// none is big enough.
pub fn load_preview_sized(path: &Path, min_long_edge: u32) -> Result<Preview> {
    if merged::is_merged(path) {
        return merged::read_preview(path);
    }
    let handle = Handle::open(path)?;
    let list = &handle.data().thumbs_list;
    let count = (list.thumbcount.max(0) as usize).min(list.thumblist.len());
    match smallest_jpeg_reaching(&list.thumblist[..count], min_long_edge) {
        // SAFETY: the handle holds an opened file and the index is inside its thumbnail list.
        Some(index) if unsafe { sys::libraw_unpack_thumb_ex(handle.0, index as i32) } == 0 => {}
        _ => unpack_largest_preview(&handle)?,
    }
    make_preview(&handle)
}

fn smallest_jpeg_reaching(
    thumbnails: &[sys::libraw_thumbnail_item_t],
    min_long_edge: u32,
) -> Option<usize> {
    thumbnails
        .iter()
        .enumerate()
        .filter(|(_, item)| item.tformat == sys::LIBRAW_INTERNAL_THUMBNAIL_JPEG)
        .map(|(index, item)| (index, item.twidth.max(item.theight) as u32))
        .filter(|&(_, long_edge)| long_edge >= min_long_edge)
        .min_by_key(|&(_, long_edge)| long_edge)
        .map(|(index, _)| index)
}

fn unpack_largest_preview(handle: &Handle) -> Result<()> {
    // SAFETY: the handle holds an opened file.
    check("libraw_unpack_thumb", unsafe { sys::libraw_unpack_thumb(handle.0) })
        .map_err(|_| Error::NoPreview)
}

fn make_preview(handle: &Handle) -> Result<Preview> {
    let mut code = 0;
    // SAFETY: the thumbnail was unpacked by the caller; `code` outlives the call.
    let image = unsafe { sys::libraw_dcraw_make_mem_thumb(handle.0, &mut code) };
    if image.is_null() {
        check("libraw_dcraw_make_mem_thumb", code)?;
        return Err(Error::NoPreview);
    }
    // SAFETY: a non-null result holds `data_size` bytes after its header and is freed once.
    let preview = unsafe {
        let bytes = std::slice::from_raw_parts((*image).data.as_ptr(), (*image).data_size as usize);
        let preview = match (*image).type_ {
            sys::LIBRAW_IMAGE_JPEG => Preview::Jpeg(bytes.to_vec()),
            _ => Preview::Rgb8 {
                width: (*image).width as u32,
                height: (*image).height as u32,
                data: bytes.to_vec(),
            },
        };
        sys::libraw_dcraw_clear_mem(image);
        preview
    };
    Ok(preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bayer_site_matches_libraw_rggb() {
        const RGGB: u32 = 0xb4b4b4b4;
        assert_eq!(bayer_site(RGGB, 0, 0), 0);
        assert_eq!(bayer_site(RGGB, 0, 1), 1);
        assert_eq!(bayer_site(RGGB, 1, 0), 3);
        assert_eq!(bayer_site(RGGB, 1, 1), 2);
    }

    fn samples_with(body: &[(u16, usize)]) -> Vec<u16> {
        body.iter().flat_map(|&(level, count)| std::iter::repeat_n(level, count)).collect()
    }

    #[test]
    fn clip_spike_finds_saturation_above_a_smooth_tail() {
        let mut samples = samples_with(&[(16372, 5000)]);
        for level in 9000..16000 {
            samples.extend(std::iter::repeat_n(level, 3));
        }
        assert_eq!(find_clip_spike(&samples, 8000), Some(16372));
    }

    #[test]
    fn clip_spike_ignores_a_few_hot_pixels() {
        let samples = samples_with(&[(16383, 13), (9000, 100_000)]);
        assert_eq!(find_clip_spike(&samples, 10_000), None);
    }

    #[test]
    fn clip_spike_ignores_stuck_pixels_in_a_large_frame() {
        let samples = samples_with(&[(16383, 178), (2000, 30_000_000)]);
        assert_eq!(find_clip_spike(&samples, 8000), None);
    }

    #[test]
    fn clip_spike_never_matches_below_the_floor() {
        let samples = samples_with(&[(150, 1_000_000)]);
        assert_eq!(find_clip_spike(&samples, 2000), None);
    }

    #[test]
    fn clip_spike_handles_a_spike_spread_over_two_levels() {
        let samples = samples_with(&[(65534, 11548), (65535, 12288), (40000, 500)]);
        assert_eq!(find_clip_spike(&samples, 30_000), Some(65535));
    }

    #[test]
    fn clip_spike_sees_past_stragglers_on_a_quantised_curve() {
        let mut levels: Vec<(u16, usize)> = (0..8).map(|step| (16404 + step * 32, 400)).collect();
        levels.push((16372, 3_700_000));
        levels.extend((1..40).map(|step| (16372 - step * 32, 50_000)));
        assert_eq!(find_clip_spike(&samples_with(&levels), 8000), Some(16372));
    }

    #[test]
    fn multipliers_normalise_to_green() {
        assert_eq!(normalized_multipliers(&[2.0, 1.0, 1.5, 1.0]), Some([2.0, 1.0, 1.5]));
        assert_eq!(normalized_multipliers(&[4.0, 2.0, 3.0, 2.0]), Some([2.0, 1.0, 1.5]));
        assert_eq!(normalized_multipliers(&[2.0, 0.0, 1.0, 2.0]), Some([1.0, 1.0, 0.5]));
        assert_eq!(normalized_multipliers(&[0.0, 0.0, 0.0, 0.0]), None);
    }

    #[test]
    fn missing_file_is_an_error() {
        let error = load(Path::new("/nonexistent/file.ARW")).unwrap_err();
        assert!(matches!(error, Error::LibRaw { .. }));
    }

    fn thumbnail(
        format: sys::LibRaw_internal_thumbnail_formats,
        width: u16,
        height: u16,
    ) -> sys::libraw_thumbnail_item_t {
        sys::libraw_thumbnail_item_t {
            tformat: format,
            twidth: width,
            theight: height,
            tflip: 0,
            tlength: 0,
            tmisc: 0,
            toffset: 0,
        }
    }

    #[test]
    fn preview_choice_takes_the_smallest_jpeg_big_enough() {
        let thumbnails = [
            thumbnail(sys::LIBRAW_INTERNAL_THUMBNAIL_JPEG, 160, 120),
            thumbnail(sys::LIBRAW_INTERNAL_THUMBNAIL_JPEG, 6048, 4024),
            thumbnail(sys::LIBRAW_INTERNAL_THUMBNAIL_JPEG, 1620, 1080),
            thumbnail(sys::LIBRAW_INTERNAL_THUMBNAIL_PPM, 640, 424),
        ];
        assert_eq!(smallest_jpeg_reaching(&thumbnails, 512), Some(2));
        assert_eq!(smallest_jpeg_reaching(&thumbnails, 2048), Some(1));
        assert_eq!(smallest_jpeg_reaching(&thumbnails, 8000), None);
    }

    #[test]
    fn preview_choice_skips_a_thumbnail_of_unknown_size() {
        let thumbnails = [thumbnail(sys::LIBRAW_INTERNAL_THUMBNAIL_JPEG, 0, 0)];
        assert_eq!(smallest_jpeg_reaching(&thumbnails, 512), None);
    }

    #[test]
    fn metadata_of_a_missing_file_is_an_error() {
        let error = read_metadata(Path::new("/nonexistent/file.ARW")).unwrap_err();
        assert!(matches!(error, Error::LibRaw { .. }));
    }
}
