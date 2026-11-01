//! Lens correction data cameras store in their raw files. LibRaw skips these tags, so they are
//! read from the file here. This module only knows where the numbers live; `unlight-lens`
//! decides what they mean.

use serde::{Deserialize, Serialize};

const TAG_SUB_IFDS: u16 = 0x014a;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_MAKER_NOTE: u16 = 0x927c;

const SONY_CHROMATIC_STATE: u16 = 0x7034;
const SONY_CHROMATIC: u16 = 0x7035;
const SONY_DISTORTION_STATE: u16 = 0x7036;
const SONY_DISTORTION: u16 = 0x7037;
const SONY_NO_PARAMETERS: i32 = 255;

const FUJI_MAGIC: &[u8] = b"FUJIFILMCCD-RAW ";
const FUJI_CFA_OFFSET: usize = 100;
const FUJI_IFD: u16 = 0xf000;
const FUJI_DISTORTION: u16 = 0xf00b;
const FUJI_CHROMATIC: u16 = 0xf00f;
const FUJI_VIGNETTING: u16 = 0xf010;
/// Knots shared by the three tables, after the leading reference radius.
const FUJI_KNOTS: usize = 9;

const PANASONIC_MAGIC: &[u8] = b"IIU\0";
const PANASONIC_DISTORTION: u16 = 0x0119;
const PANASONIC_PREVIEW: u16 = 0x002e;
const PANASONIC_LENS: u16 = 0x0051;
const PANASONIC_NOTE_HEADER: usize = 12;
const PANASONIC_FIXED_POINT: f64 = 32768.0;

const NIKON_INFO: u16 = 0xc7d5;
const NIKON_INFO_TIFF: usize = 10;
const NIKON_DISTORTION: u16 = 0x0005;
const NIKON_VIGNETTE: u16 = 0x0006;
const NIKON_DISTORTION_TERMS: [usize; 3] = [0x14, 0x1c, 0x24];
const NIKON_VIGNETTE_TERMS: [usize; 3] = [0x24, 0x34, 0x44];

/// Sony's tables, stored as a count followed by fixed-point values at even radial steps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SonyLens {
    pub distortion: Vec<i16>,
    /// Red values followed by blue values.
    pub chromatic: Vec<i16>,
}

/// Fujifilm's tables, each with one entry per knot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FujifilmLens {
    pub knots: Vec<f64>,
    pub distortion: Vec<f64>,
    pub chromatic_red: Vec<f64>,
    pub chromatic_blue: Vec<f64>,
    pub vignetting: Vec<f64>,
}

/// `Ru = scale·(Rd + a·Rd³ + b·Rd⁵ + c·Rd⁷)`, with radii divided by `radius` pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanasonicLens {
    pub scale: f64,
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub radius: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NikonLens {
    pub distortion: [f64; 3],
    pub vignette: [f64; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EmbeddedLens {
    Sony(SonyLens),
    Fujifilm(FujifilmLens),
    Panasonic(PanasonicLens),
    Nikon(NikonLens),
}

/// A little- or big-endian TIFF structure whose offsets count from `base`.
#[derive(Clone, Copy)]
struct Tiff<'a> {
    bytes: &'a [u8],
    base: usize,
    little: bool,
}

#[derive(Clone, Copy)]
struct Entry<'a> {
    tag: u16,
    kind: u16,
    count: usize,
    data: &'a [u8],
    /// Where `data` begins, as an offset from the TIFF base.
    offset: usize,
    little: bool,
}

fn element_size(kind: u16) -> usize {
    match kind {
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => 1,
    }
}

impl<'a> Tiff<'a> {
    fn at(bytes: &'a [u8], base: usize) -> Option<Self> {
        let little = match bytes.get(base..base + 2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        Some(Self { bytes, base, little })
    }

    fn u16_at(&self, offset: usize) -> Option<u16> {
        let raw = self.bytes.get(offset..offset + 2)?.try_into().ok()?;
        Some(if self.little { u16::from_le_bytes(raw) } else { u16::from_be_bytes(raw) })
    }

    fn u32_at(&self, offset: usize) -> Option<u32> {
        let raw = self.bytes.get(offset..offset + 4)?.try_into().ok()?;
        Some(if self.little { u32::from_le_bytes(raw) } else { u32::from_be_bytes(raw) })
    }

    fn first_directory(&self) -> Option<Vec<Entry<'a>>> {
        let offset = self.u32_at(self.base + 4)? as usize;
        self.directory(offset)
    }

    fn directory(&self, offset: usize) -> Option<Vec<Entry<'a>>> {
        let start = self.base.checked_add(offset)?;
        let count = self.u16_at(start)? as usize;
        (0..count).map(|index| self.entry(start + 2 + index * 12)).collect()
    }

    fn entry(&self, at: usize) -> Option<Entry<'a>> {
        let kind = self.u16_at(at + 2)?;
        let count = self.u32_at(at + 4)? as usize;
        let size = count.checked_mul(element_size(kind))?;
        let start = if size <= 4 {
            at + 8
        } else {
            self.base.checked_add(self.u32_at(at + 8)? as usize)?
        };
        let data = self.bytes.get(start..start.checked_add(size)?)?;
        let offset = start.checked_sub(self.base)?;
        Some(Entry { tag: self.u16_at(at)?, kind, count, data, offset, little: self.little })
    }
}

impl Entry<'_> {
    fn values(&self) -> Tiff<'_> {
        Tiff { bytes: self.data, base: 0, little: self.little }
    }

    fn integers(&self) -> Vec<i32> {
        let tiff = self.values();
        let read = |index: usize| match self.kind {
            3 => tiff.u16_at(index * 2).map(i32::from),
            8 => tiff.u16_at(index * 2).map(|value| i32::from(value as i16)),
            4 | 9 => tiff.u32_at(index * 4).map(|value| value as i32),
            _ => None,
        };
        (0..self.count).map_while(read).collect()
    }

    fn rationals(&self) -> Vec<f64> {
        let tiff = self.values();
        let read = |index: usize| {
            let numerator = tiff.u32_at(index * 8)? as i32;
            let denominator = tiff.u32_at(index * 8 + 4)? as i32;
            (denominator != 0).then(|| f64::from(numerator) / f64::from(denominator))
        };
        (0..self.count).map_while(read).collect()
    }

    fn offsets(&self) -> Vec<usize> {
        let tiff = self.values();
        (0..self.count).map_while(|index| tiff.u32_at(index * 4)).map(|value| value as usize).collect()
    }
}

fn find<'e, 'a>(entries: &'e [Entry<'a>], tag: u16) -> Option<&'e Entry<'a>> {
    entries.iter().find(|entry| entry.tag == tag)
}

fn sub_directories<'a>(tiff: &Tiff<'a>, root: &[Entry<'a>]) -> Vec<Vec<Entry<'a>>> {
    let offsets = find(root, TAG_SUB_IFDS).map(Entry::offsets).unwrap_or_default();
    offsets.into_iter().filter_map(|offset| tiff.directory(offset)).collect()
}

fn sony_table(directory: &[Entry], tag: u16, state: u16) -> Option<Vec<i16>> {
    let disabled = find(directory, state)
        .is_some_and(|entry| entry.integers().first() == Some(&SONY_NO_PARAMETERS));
    if disabled {
        return None;
    }
    let values = find(directory, tag)?.integers();
    let (&count, table) = values.split_first()?;
    (count > 0 && table.len() >= count as usize)
        .then(|| table.iter().map(|&value| value as i16).collect())
}

fn parse_sony(tiff: &Tiff, root: &[Entry]) -> Option<SonyLens> {
    sub_directories(tiff, root).iter().find_map(|directory| {
        let distortion = sony_table(directory, SONY_DISTORTION, SONY_DISTORTION_STATE)?;
        Some(SonyLens {
            chromatic: sony_table(directory, SONY_CHROMATIC, SONY_CHROMATIC_STATE)
                .unwrap_or_default(),
            distortion,
        })
    })
}

fn fuji_table(entries: &[Entry], tag: u16) -> Option<Vec<f64>> {
    let values = find(entries, tag)?.rationals();
    (values.len() > FUJI_KNOTS).then_some(values)
}

fn parse_fujifilm(bytes: &[u8]) -> Option<EmbeddedLens> {
    let offset = u32::from_be_bytes(bytes.get(FUJI_CFA_OFFSET..FUJI_CFA_OFFSET + 4)?.try_into().ok()?);
    let tiff = Tiff::at(bytes, offset as usize)?;
    let root = tiff.first_directory()?;
    let entry = find(&root, FUJI_IFD)?;
    let directory = tiff.directory(*entry.offsets().first()?)?;

    let geometry = fuji_table(&directory, FUJI_DISTORTION)?;
    let knots = geometry.get(1..=FUJI_KNOTS)?.to_vec();
    let values = |table: &[f64], first: usize| table.get(first..first + FUJI_KNOTS).map(<[f64]>::to_vec);
    let distortion = values(&geometry, FUJI_KNOTS + 1)?;
    let chromatic = fuji_table(&directory, FUJI_CHROMATIC).unwrap_or_default();
    let vignette = fuji_table(&directory, FUJI_VIGNETTING).unwrap_or_default();
    Some(EmbeddedLens::Fujifilm(FujifilmLens {
        chromatic_red: values(&chromatic, FUJI_KNOTS + 1).unwrap_or_default(),
        chromatic_blue: values(&chromatic, 2 * FUJI_KNOTS + 1).unwrap_or_default(),
        vignetting: values(&vignette, FUJI_KNOTS + 1).unwrap_or_default(),
        knots,
        distortion,
    }))
}

fn panasonic_checksum(bytes: impl Iterator<Item = u8>) -> u16 {
    bytes.fold(0u32, |sum, byte| (73 * sum + u32::from(byte)) % 0xffef) as u16
}

/// The four checksums XOR to zero against the words they are stored in.
fn panasonic_checksums_hold(data: &[u8; 32]) -> bool {
    let word = |index: usize| u16::from_le_bytes([data[index * 2], data[index * 2 + 1]]);
    let stepped = |start: usize| (0..14).map(move |index| data[start + index * 2]);
    let first = panasonic_checksum(data[4..16].iter().copied());
    let second = panasonic_checksum(data[16..28].iter().copied());
    let third = panasonic_checksum(stepped(2));
    let fourth = panasonic_checksum(stepped(3));
    first ^ word(1) ^ second ^ word(14) ^ third ^ word(0) ^ fourth ^ word(15) == 0
}

fn parse_panasonic(root: &[Entry]) -> Option<PanasonicLens> {
    let data: &[u8; 32] = find(root, PANASONIC_DISTORTION)?.data.try_into().ok()?;
    if !panasonic_checksums_hold(data) {
        return None;
    }
    let word = |index: usize| i16::from_le_bytes([data[index * 2], data[index * 2 + 1]]);
    let fixed = |index: usize| f64::from(word(index)) / PANASONIC_FIXED_POINT;
    let enabled = word(7) & 0x0f == 1;
    let radius = f64::from(word(12));
    (enabled && radius > 0.0).then(|| PanasonicLens {
        scale: 1.0 / (1.0 + fixed(5)),
        a: fixed(8),
        b: fixed(4),
        c: fixed(11),
        radius,
    })
}

/// The lens name sits in the makernote of the embedded JPEG rather than in the raw's own tags.
fn panasonic_lens_name(root: &[Entry]) -> Option<String> {
    let jpeg = find(root, PANASONIC_PREVIEW)?;
    let exif = jpeg.data.windows(6).position(|window| window == b"Exif\0\0")? + 6;
    let tiff = Tiff::at(jpeg.data, exif)?;
    let top = tiff.first_directory()?;
    let exif_ifd = tiff.directory(*find(&top, TAG_EXIF_IFD)?.offsets().first()?)?;
    let note = find(&exif_ifd, TAG_MAKER_NOTE)?;
    let makernote = tiff.directory(note.offset + PANASONIC_NOTE_HEADER)?;
    let name = find(&makernote, PANASONIC_LENS)?.data;
    let end = name.iter().position(|&byte| byte == 0).unwrap_or(name.len());
    let name = String::from_utf8_lossy(&name[..end]).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

fn nikon_terms<const N: usize>(data: &[u8], tiff: &Tiff, offsets: [usize; N]) -> Option<[f64; N]> {
    let terms = offsets.map(|offset| {
        let rational = data.get(offset..offset + 8)?;
        let view = Tiff { bytes: rational, base: 0, little: tiff.little };
        let numerator = view.u32_at(0)? as i32;
        let denominator = view.u32_at(4)? as i32;
        (denominator != 0).then(|| f64::from(numerator) / f64::from(denominator))
    });
    terms.iter().all(Option::is_some).then(|| terms.map(|term| term.unwrap_or_default()))
}

fn parse_nikon(tiff: &Tiff, root: &[Entry]) -> Option<NikonLens> {
    sub_directories(tiff, root).iter().find_map(|directory| {
        let info = find(directory, NIKON_INFO)?.data;
        if !info.starts_with(b"Nikon\0") {
            return None;
        }
        let inner = Tiff::at(info, NIKON_INFO_TIFF)?;
        let entries = inner.first_directory()?;
        let distortion = find(&entries, NIKON_DISTORTION)?.data;
        let vignette = find(&entries, NIKON_VIGNETTE)?.data;
        Some(NikonLens {
            distortion: nikon_terms(distortion, &inner, NIKON_DISTORTION_TERMS)?,
            vignette: nikon_terms(vignette, &inner, NIKON_VIGNETTE_TERMS)?,
        })
    })
}

/// What a raw file itself says about its lens.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EmbeddedData {
    pub corrections: Option<EmbeddedLens>,
    /// A lens name the makernote holds and LibRaw doesn't report.
    pub lens_name: Option<String>,
}

/// Reads embedded corrections from a raw file's bytes. Anything unrecognised yields nothing,
/// which leaves the photo to the lens database.
pub fn parse_embedded(bytes: &[u8]) -> EmbeddedData {
    if bytes.starts_with(FUJI_MAGIC) {
        return EmbeddedData { corrections: parse_fujifilm(bytes), lens_name: None };
    }
    let Some(tiff) = Tiff::at(bytes, 0) else {
        return EmbeddedData::default();
    };
    let Some(root) = tiff.first_directory() else {
        return EmbeddedData::default();
    };
    if bytes.starts_with(PANASONIC_MAGIC) {
        return EmbeddedData {
            corrections: parse_panasonic(&root).map(EmbeddedLens::Panasonic),
            lens_name: panasonic_lens_name(&root),
        };
    }
    let corrections = parse_sony(&tiff, &root)
        .map(EmbeddedLens::Sony)
        .or_else(|| parse_nikon(&tiff, &root).map(EmbeddedLens::Nikon));
    EmbeddedData { corrections, lens_name: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ifd(entries: &[(u16, u16, u32, u32)]) -> Vec<u8> {
        let mut out = (entries.len() as u16).to_le_bytes().to_vec();
        for &(tag, kind, count, value) in entries {
            out.extend(tag.to_le_bytes());
            out.extend(kind.to_le_bytes());
            out.extend(count.to_le_bytes());
            out.extend(value.to_le_bytes());
        }
        out.extend(0u32.to_le_bytes());
        out
    }

    fn sony_file(chromatic_state: i32) -> Vec<u8> {
        let table = |values: &[i16]| {
            let mut out = (values.len() as i16).to_le_bytes().to_vec();
            values.iter().for_each(|value| out.extend(value.to_le_bytes()));
            out
        };
        let distortion = table(&[7, 0, -20]);
        let chromatic = table(&[10, 20, 30, -1, -2, -3]);

        let mut file = b"II*\0".to_vec();
        file.extend(8u32.to_le_bytes());
        file.extend(ifd(&[(TAG_SUB_IFDS, 4, 1, 26)]));
        let sub_start = file.len();
        assert_eq!(sub_start, 26);
        let data_start = sub_start as u32 + 2 + 4 * 12 + 4;
        file.extend(ifd(&[
            (SONY_CHROMATIC_STATE, 3, 1, chromatic_state as u32),
            (SONY_CHROMATIC, 8, 7, data_start),
            (SONY_DISTORTION_STATE, 3, 1, 1),
            (SONY_DISTORTION, 8, 4, data_start + 14),
        ]));
        file.extend(chromatic);
        file.extend(distortion);
        file
    }

    #[test]
    fn reads_sony_tables_from_a_sub_ifd() {
        let data = parse_embedded(&sony_file(1));
        let Some(EmbeddedLens::Sony(sony)) = data.corrections else { panic!("{data:?}") };
        assert_eq!(sony.distortion, vec![7, 0, -20]);
        assert_eq!(sony.chromatic, vec![10, 20, 30, -1, -2, -3]);
    }

    #[test]
    fn respects_sony_saying_it_has_no_parameters() {
        let data = parse_embedded(&sony_file(SONY_NO_PARAMETERS));
        let Some(EmbeddedLens::Sony(sony)) = data.corrections else { panic!("{data:?}") };
        assert!(sony.chromatic.is_empty());
        assert_eq!(sony.distortion.len(), 3);
    }

    fn panasonic_block() -> [u8; 32] {
        let mut words = [0i16; 16];
        words[4] = -312;
        words[5] = -160;
        words[7] = 1;
        words[8] = 86;
        words[11] = 107;
        words[12] = 3240;
        let mut data = [0u8; 32];
        for (index, word) in words.iter().enumerate() {
            data[index * 2..index * 2 + 2].copy_from_slice(&word.to_le_bytes());
        }
        let first = panasonic_checksum(data[4..16].iter().copied());
        let second = panasonic_checksum(data[16..28].iter().copied());
        data[2..4].copy_from_slice(&first.to_le_bytes());
        data[28..30].copy_from_slice(&second.to_le_bytes());
        let third = panasonic_checksum((0..14).map(|index| data[2 + index * 2]));
        let fourth = panasonic_checksum((0..14).map(|index| data[3 + index * 2]));
        data[0..2].copy_from_slice(&third.to_le_bytes());
        data[30..32].copy_from_slice(&fourth.to_le_bytes());
        data
    }

    fn panasonic_file(block: &[u8; 32]) -> Vec<u8> {
        let mut file = b"IIU\0".to_vec();
        file.extend(8u32.to_le_bytes());
        file.extend(ifd(&[(PANASONIC_DISTORTION, 7, 32, 26)]));
        file.extend(block);
        file
    }

    #[test]
    fn decodes_panasonic_fixed_point_terms() {
        let data = parse_embedded(&panasonic_file(&panasonic_block()));
        let Some(EmbeddedLens::Panasonic(lens)) = data.corrections else { panic!("{data:?}") };
        assert_eq!(lens.radius, 3240.0);
        assert!((lens.a - 86.0 / 32768.0).abs() < 1e-9);
        assert!((lens.b + 312.0 / 32768.0).abs() < 1e-9);
        assert!((lens.c - 107.0 / 32768.0).abs() < 1e-9);
        assert!((lens.scale - 1.0 / (1.0 - 160.0 / 32768.0)).abs() < 1e-9);
    }

    #[test]
    fn rejects_a_panasonic_block_with_a_bad_checksum() {
        let mut block = panasonic_block();
        block[16] ^= 1;
        assert_eq!(parse_embedded(&panasonic_file(&block)).corrections, None);
    }

    #[test]
    fn unknown_bytes_yield_nothing() {
        assert_eq!(parse_embedded(b"not a raw file"), EmbeddedData::default());
        assert_eq!(parse_embedded(b"II*\0\xff\xff\xff\xff"), EmbeddedData::default());
        assert_eq!(parse_embedded(FUJI_MAGIC), EmbeddedData::default());
    }
}
