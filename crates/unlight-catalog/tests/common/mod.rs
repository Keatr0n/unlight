//! Fixtures shared by the integration tests. Nothing from the samples directory or the
//! user's cache is touched: raws are synthetic DNGs that LibRaw really opens, so these tests
//! run under `nix flake check`.
#![allow(dead_code)]

use jpeg_encoder::{ColorType, Encoder};
use std::{
    env, fs,
    path::{Path, PathBuf},
};
use unlight_catalog::{CancelToken, Catalog, ScanReport};

const BYTE: u16 = 1;
const ASCII: u16 = 2;
const SHORT: u16 = 3;
const LONG: u16 = 4;
const RATIONAL: u16 = 5;
const SRATIONAL: u16 = 10;

const SENSOR_WIDTH: u32 = 64;
const SENSOR_HEIGHT: u32 = 48;

struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    value: Vec<u8>,
}

fn entry(tag: u16, kind: u16, count: u32, value: Vec<u8>) -> Entry {
    Entry {
        tag,
        kind,
        count,
        value,
    }
}

fn long(tag: u16, value: u32) -> Entry {
    entry(tag, LONG, 1, value.to_le_bytes().to_vec())
}

fn short(tag: u16, value: u16) -> Entry {
    entry(tag, SHORT, 1, value.to_le_bytes().to_vec())
}

fn ascii(tag: u16, text: &str) -> Entry {
    let mut value = text.as_bytes().to_vec();
    value.push(0);
    entry(tag, ASCII, value.len() as u32, value)
}

fn rational(tag: u16, numerator: u32, denominator: u32) -> Entry {
    let value = [numerator.to_le_bytes(), denominator.to_le_bytes()].concat();
    entry(tag, RATIONAL, 1, value)
}

/// Appends an IFD at the end of `file` and returns its offset. Values too big for an entry
/// go after the table. Tags must be ascending.
fn write_ifd(file: &mut Vec<u8>, entries: &[Entry]) -> u32 {
    if !file.len().is_multiple_of(2) {
        file.push(0);
    }
    let start = file.len() as u32;
    let table_size = 2 + entries.len() as u32 * 12 + 4;
    let mut overflow_offset = start + table_size;
    let mut overflow = Vec::new();
    file.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for entry in entries {
        file.extend_from_slice(&entry.tag.to_le_bytes());
        file.extend_from_slice(&entry.kind.to_le_bytes());
        file.extend_from_slice(&entry.count.to_le_bytes());
        if entry.value.len() <= 4 {
            let mut inline = entry.value.clone();
            inline.resize(4, 0);
            file.extend_from_slice(&inline);
        } else {
            file.extend_from_slice(&overflow_offset.to_le_bytes());
            overflow.extend_from_slice(&entry.value);
            if !overflow.len().is_multiple_of(2) {
                overflow.push(0);
            }
            overflow_offset = start + table_size + overflow.len() as u32;
        }
    }
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&overflow);
    start
}

/// A minimal but well-formed DNG. `seed` changes its content and so its fingerprint.
#[derive(Clone, Debug)]
pub struct SyntheticRaw {
    pub make: String,
    pub model: String,
    pub lens: String,
    pub captured: String,
    pub iso: u16,
    pub orientation: u16,
    pub seed: u32,
    /// Embedded JPEG size. Its top-left quadrant is red, the rest blue.
    pub preview: Option<(u32, u32)>,
    /// Width, height and sites of a particular mosaic, in place of noise from the seed.
    pub sensor: Option<(u32, u32, Vec<u16>)>,
}

impl Default for SyntheticRaw {
    fn default() -> Self {
        Self {
            make: "Acme".into(),
            model: "Ranger".into(),
            lens: "Acme 50mm".into(),
            captured: "2024:03:15 10:20:30".into(),
            iso: 400,
            orientation: 1,
            seed: 0,
            preview: None,
            sensor: None,
        }
    }
}

pub fn quadrant_jpeg(width: u32, height: u32) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            let red = x < width / 2 && y < height / 2;
            rgb.extend_from_slice(if red { &[220, 30, 30] } else { &[30, 30, 220] });
        }
    }
    let mut jpeg = Vec::new();
    Encoder::new(&mut jpeg, 92)
        .encode(&rgb, width as u16, height as u16, ColorType::Rgb)
        .unwrap();
    jpeg
}

impl SyntheticRaw {
    pub fn seeded(seed: u32) -> Self {
        Self {
            seed,
            ..Self::default()
        }
    }

    fn sensor_size(&self) -> (u32, u32) {
        self.sensor.as_ref().map_or((SENSOR_WIDTH, SENSOR_HEIGHT), |sensor| (sensor.0, sensor.1))
    }

    fn sensor_data(&self) -> Vec<u8> {
        if let Some((_, _, sites)) = &self.sensor {
            return sites.iter().flat_map(|site| site.to_le_bytes()).collect();
        }
        let mut state = self.seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..SENSOR_WIDTH * SENSOR_HEIGHT)
            .flat_map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                ((state % 4000) as u16 + 200).to_le_bytes()
            })
            .collect()
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut file = b"II*\0\0\0\0\0".to_vec();
        let sensor = self.sensor_data();
        let sensor_offset = file.len() as u32;
        file.extend_from_slice(&sensor);
        let jpeg = self.preview.map(|(width, height)| quadrant_jpeg(width, height));
        let jpeg_offset = file.len() as u32;
        if let Some(jpeg) = &jpeg {
            file.extend_from_slice(jpeg);
        }

        let exif = write_ifd(
            &mut file,
            &[
                rational(0x829a, 1, 250),
                rational(0x829d, 28, 10),
                short(0x8827, self.iso),
                ascii(0x9003, &self.captured),
                rational(0x920a, 50, 1),
                ascii(0xa434, &self.lens),
            ],
        );
        let raw_ifd = write_ifd(
            &mut file,
            &[
                long(254, 0),
                long(256, self.sensor_size().0),
                long(257, self.sensor_size().1),
                short(258, 16),
                short(259, 1),
                short(262, 32803),
                long(273, sensor_offset),
                short(277, 1),
                long(278, self.sensor_size().1),
                long(279, sensor.len() as u32),
                entry(33421, SHORT, 2, [2u16.to_le_bytes(), 2u16.to_le_bytes()].concat()),
                entry(33422, BYTE, 4, vec![0, 1, 1, 2]),
                short(50717, 4200),
            ],
        );
        let root = self.write_root_ifd(
            &mut file,
            exif,
            raw_ifd,
            jpeg.as_ref().map(|j| (jpeg_offset, j.len())),
        );
        file[4..8].copy_from_slice(&root.to_le_bytes());
        file
    }

    fn write_root_ifd(&self, file: &mut Vec<u8>, exif: u32, raw_ifd: u32, jpeg: Option<(u32, usize)>) -> u32 {
        let matrix: Vec<u8> = [1i32, 0, 0, 0, 1, 0, 0, 0, 1]
            .iter()
            .flat_map(|&value| [(value * 10000).to_le_bytes(), 10000i32.to_le_bytes()].concat())
            .collect();
        let (preview_width, preview_height) = self.preview.unwrap_or((SENSOR_WIDTH, SENSOR_HEIGHT));
        let mut entries = vec![long(254, 1), long(256, preview_width), long(257, preview_height)];
        entries.push(entry(
            258,
            SHORT,
            3,
            [8u16, 8, 8].iter().flat_map(|bits| bits.to_le_bytes()).collect(),
        ));
        entries.push(short(259, if jpeg.is_some() { 7 } else { 1 }));
        entries.push(short(262, if jpeg.is_some() { 6 } else { 2 }));
        entries.push(ascii(271, &self.make));
        entries.push(ascii(272, &self.model));
        entries.push(long(273, jpeg.map_or(0, |(offset, _)| offset)));
        entries.push(short(274, self.orientation));
        entries.push(short(277, 3));
        entries.push(long(278, preview_height));
        entries.push(long(279, jpeg.map_or(0, |(_, length)| length as u32)));
        entries.push(long(330, raw_ifd));
        entries.push(long(34665, exif));
        entries.push(entry(50706, BYTE, 4, vec![1, 4, 0, 0]));
        entries.push(entry(50707, BYTE, 4, vec![1, 1, 0, 0]));
        entries.push(ascii(50708, &format!("{} {}", self.make, self.model)));
        entries.push(entry(50721, SRATIONAL, 9, matrix));
        entries.push(short(50778, 21));
        write_ifd(file, &entries)
    }

    pub fn write(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, self.bytes()).unwrap();
    }
}

/// A catalog in a temp dir, kept alive with it.
pub struct TestCatalog {
    pub dir: tempfile::TempDir,
    pub catalog: Catalog,
}

impl TestCatalog {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog.db")).unwrap();
        Self { dir, catalog }
    }

    pub fn root(&self) -> PathBuf {
        self.dir.path().join("photos")
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root().join(relative)
    }

    pub fn scan(&self) -> ScanReport {
        self.catalog
            .scan(&self.root(), &CancelToken::default(), |_| {})
            .unwrap()
    }
}

/// The samples directory, or `None` to skip. Nothing may write into it.
pub fn samples_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(env::var_os("UNLIGHT_SAMPLES")?);
    dir.is_dir().then_some(dir)
}

pub fn sample_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    files
}
