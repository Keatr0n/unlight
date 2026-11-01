//! DNG camera profiles, from a `.dcp` file or a DNG's first IFD. Both are TIFF, and a `.dcp`
//! only swaps the magic number for `IIRC` or `MMCR`. Profiles come from anywhere, so every
//! offset and count is checked, and any value that could reach a pixel as NaN or infinity
//! is refused rather than clamped.

use crate::matrix::Mat3;
use std::{fs, io, path::Path};

const TAG_UNIQUE_CAMERA_MODEL: u16 = 50708;
const TAG_COLOR_MATRIX_1: u16 = 50721;
const TAG_COLOR_MATRIX_2: u16 = 50722;
const TAG_CALIBRATION_ILLUMINANT_1: u16 = 50778;
const TAG_CALIBRATION_ILLUMINANT_2: u16 = 50779;
const TAG_EXTRA_CAMERA_PROFILES: u16 = 50933;
const TAG_PROFILE_NAME: u16 = 50936;
const TAG_HUE_SAT_DIMS: u16 = 50937;
const TAG_HUE_SAT_DATA_1: u16 = 50938;
const TAG_HUE_SAT_DATA_2: u16 = 50939;
const TAG_TONE_CURVE: u16 = 50940;
const TAG_FORWARD_MATRIX_1: u16 = 50964;
const TAG_FORWARD_MATRIX_2: u16 = 50965;
const TAG_LOOK_DIMS: u16 = 50981;
const TAG_LOOK_DATA: u16 = 50982;
const TAG_HUE_SAT_ENCODING: u16 = 51107;
const TAG_LOOK_ENCODING: u16 = 51108;
const TAG_BASELINE_EXPOSURE_OFFSET: u16 = 51109;
const TAG_DEFAULT_BLACK_RENDER: u16 = 51110;

const KIND_BYTE: u16 = 1;
const KIND_ASCII: u16 = 2;
const KIND_SHORT: u16 = 3;
const KIND_LONG: u16 = 4;
const KIND_RATIONAL: u16 = 5;
const KIND_UNDEFINED: u16 = 7;
const KIND_SLONG: u16 = 9;
const KIND_SRATIONAL: u16 = 10;
const KIND_FLOAT: u16 = 11;
const KIND_DOUBLE: u16 = 12;

const TIFF_MAGIC: u16 = 42;
const HEADER_SIZE: usize = 8;
const ENTRY_SIZE: usize = 12;
const INLINE_SIZE: usize = 4;
const ENCODING_SRGB: f64 = 1.0;
const BLACK_RENDER_NONE: f64 = 1.0;

/// More than any converter writes, and a limit on what a hostile file can make us parse.
const MAX_EXTRA_PROFILES: usize = 64;
/// Far above any real table, and small enough that a 3D texture of it always fits.
const MAX_DIVISIONS: u32 = 512;
/// The bounds keep a hostile file's values finite through the whole colour pipeline.
const MAX_MATRIX_ENTRY: f64 = 64.0;
const MAX_TABLE_SCALE: f32 = 64.0;
const MAX_HUE_SHIFT: f32 = 360.0;
const MAX_EXPOSURE_OFFSET: f64 = 10.0;

#[derive(Debug, thiserror::Error)]
pub enum DcpError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("not a TIFF or DCP file")]
    NotAProfile,
    #[error("the file ends inside its tag directory")]
    Truncated,
    #[error("tag {0} is malformed")]
    Malformed(u16),
}

/// A DNG hue, saturation and value lookup table.
#[derive(Clone, Debug, PartialEq)]
pub struct HueSatTable {
    /// Grid points along hue, saturation and value.
    pub divisions: [u32; 3],
    /// Whether the value axis is sRGB-encoded rather than linear.
    pub srgb_values: bool,
    /// Hue shift in degrees, saturation scale and value scale. Value changes slowest, then
    /// hue, then saturation.
    pub entries: Vec<[f32; 3]>,
}

/// The tags that go with one calibration illuminant.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProfileCalibration {
    /// EXIF LightSource code.
    pub illuminant: u16,
    pub color_matrix: Option<Mat3>,
    pub forward_matrix: Option<Mat3>,
    pub hue_sat_map: Option<HueSatTable>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dcp {
    pub unique_camera_model: String,
    pub name: String,
    pub calibrations: [ProfileCalibration; 2],
    pub look_table: Option<HueSatTable>,
    /// Input and output pairs on 0..=1, with input strictly increasing.
    pub tone_curve: Option<Vec<[f32; 2]>>,
    pub baseline_exposure_offset: f32,
    /// False when DefaultBlackRender asks for no automatic black subtraction.
    pub auto_black: bool,
}

#[derive(Clone, Copy)]
struct Entry {
    tag: u16,
    kind: u16,
    count: usize,
    /// Where the four-byte value field sits in the file.
    field: usize,
}

struct Directory<'a> {
    bytes: &'a [u8],
    big_endian: bool,
    entries: Vec<Entry>,
}

fn kind_size(kind: u16) -> Option<usize> {
    match kind {
        KIND_BYTE | KIND_ASCII | KIND_UNDEFINED => Some(1),
        KIND_SHORT => Some(2),
        KIND_LONG | KIND_SLONG | KIND_FLOAT => Some(4),
        KIND_RATIONAL | KIND_SRATIONAL | KIND_DOUBLE => Some(8),
        _ => None,
    }
}

fn u16_from(bytes: &[u8], big_endian: bool) -> Option<u16> {
    let raw = *bytes.first_chunk()?;
    Some(if big_endian { u16::from_be_bytes(raw) } else { u16::from_le_bytes(raw) })
}

fn u32_from(bytes: &[u8], big_endian: bool) -> Option<u32> {
    let raw = *bytes.first_chunk()?;
    Some(if big_endian { u32::from_be_bytes(raw) } else { u32::from_le_bytes(raw) })
}

fn u64_from(bytes: &[u8], big_endian: bool) -> Option<u64> {
    let raw = *bytes.first_chunk()?;
    Some(if big_endian { u64::from_be_bytes(raw) } else { u64::from_le_bytes(raw) })
}

fn byte_order(magic: [u8; 4]) -> Option<bool> {
    let tiff = |big_endian| {
        (u16_from(&magic[2..], big_endian) == Some(TIFF_MAGIC)).then_some(big_endian)
    };
    match &magic {
        b"IIRC" => Some(false),
        b"MMCR" => Some(true),
        [b'I', b'I', ..] => tiff(false),
        [b'M', b'M', ..] => tiff(true),
        _ => None,
    }
}

/// The SDK reads a zero denominator as zero, and real files rely on that.
fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator == 0.0 { 0.0 } else { numerator / denominator }
}

impl<'a> Directory<'a> {
    fn read(bytes: &'a [u8]) -> Result<Self, DcpError> {
        let magic = *bytes.first_chunk().ok_or(DcpError::NotAProfile)?;
        let big_endian = byte_order(magic).ok_or(DcpError::NotAProfile)?;
        let offset = bytes.get(4..HEADER_SIZE).and_then(|field| u32_from(field, big_endian));
        let offset = offset.ok_or(DcpError::Truncated)? as usize;
        let count = bytes
            .get(offset..)
            .and_then(|tail| u16_from(tail, big_endian))
            .ok_or(DcpError::Truncated)?;
        let first = offset + 2;
        let entries = (0..usize::from(count))
            .map(|index| {
                let start = first + index * ENTRY_SIZE;
                let entry = bytes.get(start..start + ENTRY_SIZE).ok_or(DcpError::Truncated)?;
                let word = |at: usize| u16_from(&entry[at..], big_endian);
                let count = u32_from(&entry[4..], big_endian);
                match (word(0), word(2), count) {
                    (Some(tag), Some(kind), Some(count)) => {
                        Ok(Entry { tag, kind, count: count as usize, field: start + 8 })
                    }
                    _ => Err(DcpError::Truncated),
                }
            })
            .collect::<Result<_, DcpError>>()?;
        Ok(Self { bytes, big_endian, entries })
    }

    fn find(&self, tag: u16) -> Option<Entry> {
        self.entries.iter().find(|entry| entry.tag == tag).copied()
    }

    fn payload(&self, entry: Entry) -> Option<&'a [u8]> {
        let size = kind_size(entry.kind)?.checked_mul(entry.count)?;
        let start = if size <= INLINE_SIZE {
            entry.field
        } else {
            u32_from(self.bytes.get(entry.field..)?, self.big_endian)? as usize
        };
        self.bytes.get(start..start.checked_add(size)?)
    }

    fn number(&self, kind: u16, bytes: &[u8]) -> Option<f64> {
        let order = self.big_endian;
        let long = |at: usize| u32_from(bytes.get(at..)?, order);
        let signed = |at: usize| long(at).map(|value| value as i32);
        Some(match kind {
            KIND_BYTE => f64::from(*bytes.first()?),
            KIND_SHORT => f64::from(u16_from(bytes, order)?),
            KIND_LONG => f64::from(long(0)?),
            KIND_SLONG => f64::from(signed(0)?),
            KIND_FLOAT => f64::from(f32::from_bits(long(0)?)),
            KIND_DOUBLE => f64::from_bits(u64_from(bytes, order)?),
            KIND_RATIONAL => ratio(f64::from(long(0)?), f64::from(long(4)?)),
            KIND_SRATIONAL => ratio(f64::from(signed(0)?), f64::from(signed(4)?)),
            _ => return None,
        })
    }

    /// A tag's values, or `None` when it's absent. Anything non-finite is malformed.
    fn numbers(&self, tag: u16) -> Result<Option<Vec<f64>>, DcpError> {
        let Some(entry) = self.find(tag) else {
            return Ok(None);
        };
        let malformed = DcpError::Malformed(tag);
        let bytes = self.payload(entry).ok_or(DcpError::Malformed(tag))?;
        let size = kind_size(entry.kind).ok_or(DcpError::Malformed(tag))?;
        let values: Option<Vec<f64>> =
            bytes.chunks_exact(size).map(|chunk| self.number(entry.kind, chunk)).collect();
        match values {
            Some(values) if values.iter().all(|value| value.is_finite()) => Ok(Some(values)),
            _ => Err(malformed),
        }
    }

    fn number_at(&self, tag: u16) -> Result<Option<f64>, DcpError> {
        match self.numbers(tag)?.as_deref() {
            None => Ok(None),
            Some(&[value]) => Ok(Some(value)),
            Some(_) => Err(DcpError::Malformed(tag)),
        }
    }

    fn text(&self, tag: u16) -> Result<String, DcpError> {
        let Some(entry) = self.find(tag) else {
            return Ok(String::new());
        };
        if !matches!(entry.kind, KIND_ASCII | KIND_BYTE | KIND_UNDEFINED) {
            return Err(DcpError::Malformed(tag));
        }
        let bytes = self.payload(entry).ok_or(DcpError::Malformed(tag))?;
        let end = bytes.iter().position(|&byte| byte == 0).unwrap_or(bytes.len());
        Ok(String::from_utf8_lossy(&bytes[..end]).trim().to_owned())
    }

    fn matrix(&self, tag: u16) -> Result<Option<Mat3>, DcpError> {
        let Some(values) = self.numbers(tag)? else {
            return Ok(None);
        };
        let [a, b, c, d, e, f, g, h, i] = values.as_slice() else {
            return Err(DcpError::Malformed(tag));
        };
        if values.iter().any(|value| value.abs() > MAX_MATRIX_ENTRY) {
            return Err(DcpError::Malformed(tag));
        }
        Ok(Some([[*a, *b, *c], [*d, *e, *f], [*g, *h, *i]]))
    }

    fn divisions(&self, tag: u16) -> Result<Option<[u32; 3]>, DcpError> {
        let Some(values) = self.numbers(tag)? else {
            return Ok(None);
        };
        let &[hue, saturation, value] = values.as_slice() else {
            return Err(DcpError::Malformed(tag));
        };
        let divisions = [hue, saturation, value].map(|count| count as u32);
        let in_range = (1..=MAX_DIVISIONS).contains(&divisions[0])
            && (2..=MAX_DIVISIONS).contains(&divisions[1])
            && (1..=MAX_DIVISIONS).contains(&divisions[2]);
        in_range.then_some(Some(divisions)).ok_or(DcpError::Malformed(tag))
    }

    fn table(
        &self,
        divisions: Option<[u32; 3]>,
        data_tag: u16,
        encoding_tag: u16,
    ) -> Result<Option<HueSatTable>, DcpError> {
        let Some(values) = self.numbers(data_tag)? else {
            return Ok(None);
        };
        let malformed = || DcpError::Malformed(data_tag);
        let divisions = divisions.ok_or_else(malformed)?;
        let count = divisions.iter().map(|&count| count as usize).product::<usize>();
        if values.len() != count * 3 {
            return Err(malformed());
        }
        let entries: Vec<[f32; 3]> =
            values.as_chunks::<3>().0.iter().map(|entry| entry.map(|value| value as f32)).collect();
        let usable = entries.iter().all(|&[hue, saturation, value]| {
            hue.abs() <= MAX_HUE_SHIFT
                && (0.0..=MAX_TABLE_SCALE).contains(&saturation)
                && (0.0..=MAX_TABLE_SCALE).contains(&value)
        });
        if !usable {
            return Err(malformed());
        }
        let srgb_values = self.number_at(encoding_tag)? == Some(ENCODING_SRGB);
        Ok(Some(HueSatTable { divisions, srgb_values, entries }))
    }

    fn tone_curve(&self) -> Result<Option<Vec<[f32; 2]>>, DcpError> {
        let Some(values) = self.numbers(TAG_TONE_CURVE)? else {
            return Ok(None);
        };
        let malformed = || DcpError::Malformed(TAG_TONE_CURVE);
        if values.len() < 4 || values.len() % 2 != 0 {
            return Err(malformed());
        }
        let points: Vec<[f32; 2]> =
            values.as_chunks::<2>().0.iter().map(|point| point.map(|value| value as f32)).collect();
        let on_unit_square =
            points.iter().flatten().all(|value| (0.0..=1.0).contains(value));
        let climbing = points.windows(2).all(|pair| pair[1][0] > pair[0][0] && pair[1][1] >= pair[0][1]);
        (on_unit_square && climbing).then_some(Some(points)).ok_or_else(malformed)
    }

    fn calibration(
        &self,
        illuminant: u16,
        color: u16,
        forward: u16,
        map: Option<HueSatTable>,
    ) -> Result<ProfileCalibration, DcpError> {
        let illuminant = self.number_at(illuminant)?.map_or(0, |code| code as u16);
        Ok(ProfileCalibration {
            illuminant,
            color_matrix: self.matrix(color)?,
            forward_matrix: self.matrix(forward)?,
            hue_sat_map: map,
        })
    }
}

impl Dcp {
    pub fn parse(bytes: &[u8]) -> Result<Self, DcpError> {
        let tags = Directory::read(bytes)?;
        let hue_sat = tags.divisions(TAG_HUE_SAT_DIMS)?;
        let map = |data| tags.table(hue_sat, data, TAG_HUE_SAT_ENCODING);
        let first = tags.calibration(
            TAG_CALIBRATION_ILLUMINANT_1,
            TAG_COLOR_MATRIX_1,
            TAG_FORWARD_MATRIX_1,
            map(TAG_HUE_SAT_DATA_1)?,
        )?;
        let second = tags.calibration(
            TAG_CALIBRATION_ILLUMINANT_2,
            TAG_COLOR_MATRIX_2,
            TAG_FORWARD_MATRIX_2,
            map(TAG_HUE_SAT_DATA_2)?,
        )?;
        let offset = tags.number_at(TAG_BASELINE_EXPOSURE_OFFSET)?.unwrap_or(0.0);
        if offset.abs() > MAX_EXPOSURE_OFFSET {
            return Err(DcpError::Malformed(TAG_BASELINE_EXPOSURE_OFFSET));
        }
        let look_dims = tags.divisions(TAG_LOOK_DIMS)?;
        Ok(Self {
            unique_camera_model: tags.text(TAG_UNIQUE_CAMERA_MODEL)?,
            name: tags.text(TAG_PROFILE_NAME)?,
            calibrations: [first, second],
            look_table: tags.table(look_dims, TAG_LOOK_DATA, TAG_LOOK_ENCODING)?,
            tone_curve: tags.tone_curve()?,
            baseline_exposure_offset: offset as f32,
            auto_black: tags.number_at(TAG_DEFAULT_BLACK_RENDER)? != Some(BLACK_RENDER_NONE),
        })
    }

    pub fn read(path: &Path) -> Result<Self, DcpError> {
        Self::parse(&fs::read(path)?)
    }

    /// The profiles a DNG's ExtraCameraProfiles tag points at. Each is laid out like a `.dcp`
    /// file with its offsets counted from its own start. A broken one is left out, since the
    /// photo and its other profiles are still good.
    pub fn parse_extras(bytes: &[u8]) -> Vec<Self> {
        let offsets = Directory::read(bytes).ok().and_then(|tags| tags.numbers(TAG_EXTRA_CAMERA_PROFILES).ok().flatten());
        offsets
            .unwrap_or_default()
            .iter()
            .take(MAX_EXTRA_PROFILES)
            .filter_map(|offset| bytes.get(*offset as usize..))
            .filter_map(|profile| Self::parse(profile).ok())
            .collect()
    }

    /// Whether this says anything beyond the matrices a raw file already has.
    pub fn has_rendering(&self) -> bool {
        self.look_table.is_some()
            || self.tone_curve.is_some()
            || self.calibrations.iter().any(|calibration| calibration.hue_sat_map.is_some())
    }
}

#[cfg(test)]
pub(crate) mod testing {
    /// Writes a TIFF-style file, so tests can make any profile, including broken ones.
    pub struct Builder {
        big_endian: bool,
        magic: [u8; 4],
        tags: Vec<(u16, u16, u32, Vec<u8>)>,
    }

    impl Builder {
        pub fn new(magic: &[u8; 4]) -> Self {
            Self { big_endian: magic[0] == b'M', magic: *magic, tags: Vec::new() }
        }

        fn long(&self, value: u32) -> [u8; 4] {
            if self.big_endian { value.to_be_bytes() } else { value.to_le_bytes() }
        }

        pub fn raw(mut self, tag: u16, kind: u16, count: u32, payload: Vec<u8>) -> Self {
            self.tags.push((tag, kind, count, payload));
            self
        }

        pub fn text(self, tag: u16, value: &str) -> Self {
            let mut bytes = value.as_bytes().to_vec();
            bytes.push(0);
            let count = bytes.len() as u32;
            self.raw(tag, super::KIND_ASCII, count, bytes)
        }

        pub fn longs(self, tag: u16, values: &[u32]) -> Self {
            let payload = values.iter().flat_map(|&value| self.long(value)).collect();
            self.raw(tag, super::KIND_LONG, values.len() as u32, payload)
        }

        pub fn short(self, tag: u16, value: u16) -> Self {
            let bytes =
                if self.big_endian { value.to_be_bytes() } else { value.to_le_bytes() };
            self.raw(tag, super::KIND_SHORT, 1, bytes.to_vec())
        }

        pub fn floats(self, tag: u16, values: &[f32]) -> Self {
            let payload = values.iter().flat_map(|value| self.long(value.to_bits())).collect();
            self.raw(tag, super::KIND_FLOAT, values.len() as u32, payload)
        }

        pub fn rationals(self, tag: u16, values: &[f32]) -> Self {
            let payload = values
                .iter()
                .flat_map(|&value| {
                    let top = self.long((value * 10000.0).round() as i32 as u32);
                    let bottom = self.long(10000);
                    [top, bottom].concat()
                })
                .collect();
            self.raw(tag, super::KIND_SRATIONAL, values.len() as u32, payload)
        }

        pub fn build(mut self) -> Vec<u8> {
            self.tags.sort_by_key(|tag| tag.0);
            let mut out = self.magic.to_vec();
            out.extend(self.long(super::HEADER_SIZE as u32));
            let count = self.tags.len() as u16;
            out.extend(if self.big_endian { count.to_be_bytes() } else { count.to_le_bytes() });
            let directory_end = out.len() + self.tags.len() * super::ENTRY_SIZE + 4;
            let mut data: Vec<u8> = Vec::new();
            for (tag, kind, count, payload) in &self.tags {
                let word = |value: u16| {
                    if self.big_endian { value.to_be_bytes() } else { value.to_le_bytes() }
                };
                out.extend(word(*tag));
                out.extend(word(*kind));
                out.extend(self.long(*count));
                if payload.len() <= super::INLINE_SIZE {
                    let mut field = payload.clone();
                    field.resize(super::INLINE_SIZE, 0);
                    out.extend(field);
                } else {
                    out.extend(self.long((directory_end + data.len()) as u32));
                    data.extend(payload);
                }
            }
            out.extend([0; 4]);
            out.extend(data);
            out
        }
    }

    pub const MATRIX: [f32; 9] = [0.8, -0.2, -0.1, -0.5, 1.3, 0.2, -0.1, 0.2, 0.6];
    pub const WARM_MATRIX: [f32; 9] = [1.0, -0.2, -0.1, -0.4, 1.2, 0.2, -0.05, 0.1, 0.5];
    pub const FORWARD: [f32; 9] = [0.436, 0.3851, 0.1431, 0.2225, 0.7169, 0.0606, 0.0139, 0.0971, 0.7139];

    /// A 2×2×2 table where each grid point is distinct, so ordering mistakes show up.
    pub fn table_values() -> Vec<f32> {
        (0..8).flat_map(|index| [index as f32, 1.0 + index as f32 * 0.1, 1.0]).collect()
    }

    pub fn complete(magic: &[u8; 4]) -> Vec<u8> {
        Builder::new(magic)
            .text(super::TAG_UNIQUE_CAMERA_MODEL, "Test Camera")
            .text(super::TAG_PROFILE_NAME, "Test Profile")
            .short(super::TAG_CALIBRATION_ILLUMINANT_1, 17)
            .short(super::TAG_CALIBRATION_ILLUMINANT_2, 21)
            .rationals(super::TAG_COLOR_MATRIX_1, &WARM_MATRIX)
            .rationals(super::TAG_COLOR_MATRIX_2, &MATRIX)
            .rationals(super::TAG_FORWARD_MATRIX_1, &FORWARD)
            .rationals(super::TAG_FORWARD_MATRIX_2, &FORWARD)
            .longs(super::TAG_HUE_SAT_DIMS, &[2, 2, 2])
            .floats(super::TAG_HUE_SAT_DATA_1, &table_values())
            .floats(super::TAG_HUE_SAT_DATA_2, &table_values())
            .longs(super::TAG_HUE_SAT_ENCODING, &[1])
            .longs(super::TAG_LOOK_DIMS, &[2, 2, 2])
            .floats(super::TAG_LOOK_DATA, &table_values())
            .floats(super::TAG_TONE_CURVE, &[0.0, 0.0, 0.5, 0.6, 1.0, 1.0])
            .rationals(super::TAG_BASELINE_EXPOSURE_OFFSET, &[-0.5])
            .longs(super::TAG_DEFAULT_BLACK_RENDER, &[1])
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::{testing::*, *};

    #[test]
    fn reads_every_tag_in_both_byte_orders() {
        for magic in [b"IIRC", b"MMCR", b"II*\0", b"MM\0*"] {
            let profile = Dcp::parse(&complete(magic)).unwrap();
            assert_eq!(profile.unique_camera_model, "Test Camera");
            assert_eq!(profile.name, "Test Profile");
            assert_eq!(profile.calibrations[0].illuminant, 17);
            assert_eq!(profile.calibrations[1].illuminant, 21);
            let matrix = profile.calibrations[1].color_matrix.unwrap();
            assert!((matrix[1][1] - 1.3).abs() < 1e-9, "{matrix:?}");
            assert!(profile.calibrations[0].forward_matrix.is_some());
            let map = profile.calibrations[1].hue_sat_map.as_ref().unwrap();
            assert_eq!(map.divisions, [2, 2, 2]);
            assert!(map.srgb_values);
            assert_eq!(map.entries[3], [3.0, 1.3, 1.0]);
            let look = profile.look_table.as_ref().unwrap();
            assert!(!look.srgb_values, "the look table has its own encoding tag");
            assert_eq!(profile.tone_curve.as_deref().unwrap()[1], [0.5, 0.6]);
            assert!((profile.baseline_exposure_offset + 0.5).abs() < 1e-6);
            assert!(!profile.auto_black);
            assert!(profile.has_rendering());
        }
    }

    fn with_extras(extras: &[Vec<u8>]) -> Vec<u8> {
        const PLACEHOLDER: u32 = 0xfeed_beef;
        let slots = vec![PLACEHOLDER; extras.len()];
        let mut file = Builder::new(b"II*\0")
            .text(TAG_PROFILE_NAME, "Main")
            .longs(TAG_EXTRA_CAMERA_PROFILES, &slots)
            .build();
        for extra in extras {
            let start = (file.len() as u32).to_le_bytes();
            let slot = file.windows(4).position(|field| field == PLACEHOLDER.to_le_bytes()).unwrap();
            file[slot..slot + 4].copy_from_slice(&start);
            file.extend(extra);
        }
        file
    }

    #[test]
    fn extra_profiles_are_found_after_the_main_one() {
        let named = |name: &str| Builder::new(b"IIRC").text(TAG_PROFILE_NAME, name).build();
        let file = with_extras(&[named("Camera Faithful"), named("Camera Vivid")]);
        assert_eq!(Dcp::parse(&file).unwrap().name, "Main");
        let names: Vec<String> = Dcp::parse_extras(&file).into_iter().map(|profile| profile.name).collect();
        assert_eq!(names, ["Camera Faithful", "Camera Vivid"]);
    }

    #[test]
    fn a_broken_extra_profile_is_left_out() {
        let good = Builder::new(b"MMCR").text(TAG_PROFILE_NAME, "Good").build();
        let file = with_extras(&[b"nonsense".to_vec(), good]);
        assert_eq!(Dcp::parse_extras(&file).len(), 1);
    }

    #[test]
    fn a_file_with_no_extra_profiles_has_none() {
        assert!(Dcp::parse_extras(&complete(b"II*\0")).is_empty());
        assert!(Dcp::parse_extras(b"not a tiff").is_empty());
    }

    #[test]
    fn a_matrix_only_profile_has_no_rendering_of_its_own() {
        let bytes = Builder::new(b"IIRC").rationals(TAG_COLOR_MATRIX_1, &MATRIX).build();
        let profile = Dcp::parse(&bytes).unwrap();
        assert!(!profile.has_rendering());
        assert!(profile.auto_black);
        assert_eq!(profile.baseline_exposure_offset, 0.0);
    }

    #[test]
    fn refuses_other_files() {
        for bytes in [&b""[..], b"II", b"IIRC", b"\x89PNG\r\n\x1a\n", b"II\x2b\0\x08\0\0\0"] {
            assert!(Dcp::parse(bytes).is_err(), "{bytes:?}");
        }
    }

    #[test]
    fn a_zero_denominator_reads_as_zero() {
        let mut payload = Vec::new();
        for _ in 0..9 {
            payload.extend(0i32.to_le_bytes());
            payload.extend(0i32.to_le_bytes());
        }
        let bytes = Builder::new(b"IIRC")
            .raw(TAG_COLOR_MATRIX_1, KIND_SRATIONAL, 9, payload)
            .build();
        let profile = Dcp::parse(&bytes).unwrap();
        assert_eq!(profile.calibrations[0].color_matrix, Some([[0.0; 3]; 3]));
    }

    #[test]
    fn refuses_a_table_whose_size_disagrees_with_its_dims() {
        let bytes = Builder::new(b"IIRC")
            .longs(TAG_HUE_SAT_DIMS, &[3, 2, 2])
            .floats(TAG_HUE_SAT_DATA_1, &table_values())
            .build();
        assert!(matches!(Dcp::parse(&bytes), Err(DcpError::Malformed(TAG_HUE_SAT_DATA_1))));
    }

    #[test]
    fn refuses_a_table_with_no_dims() {
        let bytes = Builder::new(b"IIRC").floats(TAG_LOOK_DATA, &table_values()).build();
        assert!(matches!(Dcp::parse(&bytes), Err(DcpError::Malformed(TAG_LOOK_DATA))));
    }

    #[test]
    fn refuses_dims_that_would_overflow_or_exhaust_memory() {
        for dims in [[u32::MAX, u32::MAX, u32::MAX], [0, 2, 1], [90, 1, 1], [100_000, 2, 1]] {
            let bytes = Builder::new(b"IIRC")
                .longs(TAG_LOOK_DIMS, &dims)
                .floats(TAG_LOOK_DATA, &table_values())
                .build();
            assert!(matches!(Dcp::parse(&bytes), Err(DcpError::Malformed(TAG_LOOK_DIMS))), "{dims:?}");
        }
    }

    #[test]
    fn refuses_values_that_could_poison_a_pixel() {
        for bad in [f32::NAN, f32::INFINITY, -1.0, 1e30] {
            let mut values = table_values();
            values[4] = bad;
            let bytes = Builder::new(b"IIRC")
                .longs(TAG_HUE_SAT_DIMS, &[2, 2, 2])
                .floats(TAG_HUE_SAT_DATA_1, &values)
                .build();
            assert!(Dcp::parse(&bytes).is_err(), "{bad}");
        }
        let bytes = Builder::new(b"IIRC").floats(TAG_BASELINE_EXPOSURE_OFFSET, &[1e9]).build();
        assert!(Dcp::parse(&bytes).is_err());
        let bytes = Builder::new(b"IIRC").floats(TAG_COLOR_MATRIX_1, &[1e6; 9]).build();
        assert!(Dcp::parse(&bytes).is_err());
    }

    #[test]
    fn refuses_a_tone_curve_that_is_not_a_function_on_the_unit_square() {
        let cases: [&[f32]; 5] = [
            &[0.0, 0.0],
            &[0.0, 0.0, 1.0],
            &[0.0, 0.0, 0.5, 0.5, 0.5, 0.6, 1.0, 1.0],
            &[0.0, 0.0, 0.5, 0.8, 0.8, 0.4, 1.0, 1.0],
            &[0.0, 0.0, 2.0, 1.0],
        ];
        for values in cases {
            let bytes = Builder::new(b"IIRC").floats(TAG_TONE_CURVE, values).build();
            assert!(matches!(Dcp::parse(&bytes), Err(DcpError::Malformed(TAG_TONE_CURVE))), "{values:?}");
        }
    }

    #[test]
    fn refuses_offsets_and_counts_past_the_end() {
        let huge_offset = Builder::new(b"IIRC")
            .raw(TAG_LOOK_DATA, KIND_FLOAT, 2, u32::MAX.to_le_bytes().repeat(2))
            .build();
        let mut pointed_away = huge_offset.clone();
        pointed_away[HEADER_SIZE + 2 + 8..HEADER_SIZE + 2 + 12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Dcp::parse(&pointed_away).is_err());

        let mut counted_away = complete(b"IIRC");
        counted_away[HEADER_SIZE + 2 + 4..HEADER_SIZE + 2 + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Dcp::parse(&counted_away).is_err());

        let mut directory_away = complete(b"IIRC");
        directory_away[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(Dcp::parse(&directory_away), Err(DcpError::Truncated)));
    }

    #[test]
    fn unknown_kinds_on_tags_nobody_reads_are_ignored() {
        let bytes = Builder::new(b"IIRC")
            .raw(1, 99, u32::MAX, vec![0; 4])
            .rationals(TAG_COLOR_MATRIX_1, &MATRIX)
            .build();
        assert!(Dcp::parse(&bytes).is_ok());
    }

    fn assert_sound(profile: &Dcp) {
        let tables = profile
            .calibrations
            .iter()
            .filter_map(|calibration| calibration.hue_sat_map.as_ref())
            .chain(&profile.look_table);
        for table in tables {
            let expected: usize = table.divisions.iter().map(|&count| count as usize).product();
            assert_eq!(table.entries.len(), expected);
            assert!(table.entries.iter().flatten().all(|value| value.is_finite()));
        }
        if let Some(curve) = &profile.tone_curve {
            assert!(curve.windows(2).all(|pair| pair[1][0] > pair[0][0]));
        }
        assert!(profile.baseline_exposure_offset.is_finite());
    }

    #[test]
    fn every_truncation_is_handled() {
        for magic in [b"IIRC", b"MM\0*"] {
            let bytes = complete(magic);
            for length in 0..bytes.len() {
                if let Ok(profile) = Dcp::parse(&bytes[..length]) {
                    assert_sound(&profile);
                }
            }
        }
    }

    #[test]
    fn corruption_never_panics_or_lets_a_bad_table_through() {
        let original = complete(b"IIRC");
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as usize
        };
        for _ in 0..50_000 {
            let mut bytes = original.clone();
            for _ in 0..1 + next() % 8 {
                let at = next() % bytes.len();
                bytes[at] = next() as u8;
            }
            if let Ok(profile) = Dcp::parse(&bytes) {
                assert_sound(&profile);
            }
        }
    }

    #[test]
    fn corrupting_each_directory_byte_is_handled() {
        let original = complete(b"MMCR");
        let directory_end = HEADER_SIZE + 2 + 17 * ENTRY_SIZE;
        for at in 0..directory_end {
            for value in [0x00, 0x7f, 0x80, 0xff] {
                let mut bytes = original.clone();
                bytes[at] = value;
                if let Ok(profile) = Dcp::parse(&bytes) {
                    assert_sound(&profile);
                }
            }
        }
    }
}
