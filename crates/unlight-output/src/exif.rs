//! TIFF directories, used both for EXIF blocks and for the TIFF encoder's own image
//! directory. Everything is little-endian.

use crate::metadata::{CaptureTime, GpsPosition, ImageMetadata, MetadataPolicy, MetadataSettings};
use std::collections::BTreeMap;
use unlight_color::output::OutputSpace;

const TIFF_MAGIC: [u8; 4] = *b"II*\0";
const HEADER_LEN: u32 = 8;
const DIRECTORY_OVERHEAD: u32 = 2 + 4;
const ENTRY_LEN: u32 = 12;

const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
pub(crate) const TAG_ORIENTATION: u16 = 0x0112;
const TAG_ARTIST: u16 = 0x013B;
const TAG_COPYRIGHT: u16 = 0x8298;
const TAG_EXIF_DIRECTORY: u16 = 0x8769;
const TAG_GPS_DIRECTORY: u16 = 0x8825;

const TAG_EXPOSURE_TIME: u16 = 0x829A;
const TAG_F_NUMBER: u16 = 0x829D;
const TAG_ISO: u16 = 0x8827;
const TAG_EXIF_VERSION: u16 = 0x9000;
const TAG_DATE_TIME_ORIGINAL: u16 = 0x9003;
const TAG_OFFSET_TIME_ORIGINAL: u16 = 0x9011;
const TAG_FOCAL_LENGTH: u16 = 0x920A;
const TAG_COLOR_SPACE: u16 = 0xA001;
const TAG_PIXEL_X_DIMENSION: u16 = 0xA002;
const TAG_PIXEL_Y_DIMENSION: u16 = 0xA003;
const TAG_LENS_MODEL: u16 = 0xA434;

const TAG_GPS_VERSION: u16 = 0;
const TAG_GPS_LATITUDE_REF: u16 = 1;
const TAG_GPS_LATITUDE: u16 = 2;
const TAG_GPS_LONGITUDE_REF: u16 = 3;
const TAG_GPS_LONGITUDE: u16 = 4;
const TAG_GPS_ALTITUDE_REF: u16 = 5;
const TAG_GPS_ALTITUDE: u16 = 6;

const ORIENTATION_NORMAL: u16 = 1;
const COLOR_SPACE_SRGB: u16 = 1;
const COLOR_SPACE_UNCALIBRATED: u16 = 0xFFFF;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Byte(Vec<u8>),
    Ascii(String),
    Short(Vec<u16>),
    Long(Vec<u32>),
    Rational(Vec<[u32; 2]>),
    Undefined(Vec<u8>),
}

impl Value {
    fn type_code(&self) -> u16 {
        match self {
            Self::Byte(_) => 1,
            Self::Ascii(_) => 2,
            Self::Short(_) => 3,
            Self::Long(_) => 4,
            Self::Rational(_) => 5,
            Self::Undefined(_) => 7,
        }
    }

    fn count(&self) -> u32 {
        let count = match self {
            Self::Byte(values) | Self::Undefined(values) => values.len(),
            Self::Ascii(text) => text.len() + 1,
            Self::Short(values) => values.len(),
            Self::Long(values) => values.len(),
            Self::Rational(values) => values.len(),
        };
        count as u32
    }

    fn bytes(&self) -> Vec<u8> {
        match self {
            Self::Byte(values) | Self::Undefined(values) => values.clone(),
            Self::Ascii(text) => text.bytes().chain([0]).collect(),
            Self::Short(values) => values.iter().flat_map(|v| v.to_le_bytes()).collect(),
            Self::Long(values) => values.iter().flat_map(|v| v.to_le_bytes()).collect(),
            Self::Rational(values) => values
                .iter()
                .flatten()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        }
    }

    fn out_of_line_len(&self) -> u32 {
        match self.bytes().len() as u32 {
            len if len <= 4 => 0,
            len => len + len % 2,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Directory {
    entries: BTreeMap<u16, Value>,
}

impl Directory {
    pub fn set(&mut self, tag: u16, value: Value) {
        self.entries.insert(tag, value);
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn table_len(&self) -> u32 {
        DIRECTORY_OVERHEAD + ENTRY_LEN * self.entries.len() as u32
    }

    fn len(&self) -> u32 {
        self.table_len()
            + self
                .entries
                .values()
                .map(Value::out_of_line_len)
                .sum::<u32>()
    }

    fn encode(&self, offset: u32, out: &mut Vec<u8>) {
        let mut data_offset = offset + self.table_len();
        let mut data = Vec::new();
        out.extend((self.entries.len() as u16).to_le_bytes());
        for (tag, value) in &self.entries {
            out.extend(tag.to_le_bytes());
            out.extend(value.type_code().to_le_bytes());
            out.extend(value.count().to_le_bytes());
            let mut bytes = value.bytes();
            if bytes.len() <= 4 {
                bytes.resize(4, 0);
                out.extend(bytes);
            } else {
                out.extend(data_offset.to_le_bytes());
                bytes.resize(value.out_of_line_len() as usize, 0);
                data_offset += bytes.len() as u32;
                data.extend(bytes);
            }
        }
        out.extend(0u32.to_le_bytes());
        out.extend(data);
    }
}

/// An image directory with its optional EXIF and GPS sub-directories.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TiffTree {
    pub main: Directory,
    pub exif: Directory,
    pub gps: Directory,
}

impl TiffTree {
    /// The directories as they sit at `offset` in a file.
    pub fn encode(&self, offset: u32) -> Vec<u8> {
        let mut main = self.main.clone();
        let sub_directories = [
            (TAG_EXIF_DIRECTORY, &self.exif),
            (TAG_GPS_DIRECTORY, &self.gps),
        ]
        .map(|(tag, directory)| (!directory.is_empty()).then_some((tag, directory)));
        for (tag, _) in sub_directories.iter().flatten() {
            main.set(*tag, Value::Long(vec![0]));
        }

        let mut next = offset + main.len();
        let mut placed = Vec::new();
        for (tag, directory) in sub_directories.iter().flatten() {
            main.set(*tag, Value::Long(vec![next]));
            placed.push((next, *directory));
            next += directory.len();
        }

        let mut out = Vec::with_capacity((next - offset) as usize);
        main.encode(offset, &mut out);
        for (position, directory) in placed {
            directory.encode(position, &mut out);
        }
        out
    }

    pub fn to_standalone_bytes(&self) -> Vec<u8> {
        let mut out = file_header(HEADER_LEN).to_vec();
        out.extend(self.encode(HEADER_LEN));
        out
    }
}

pub(crate) fn file_header(first_directory: u32) -> [u8; HEADER_LEN as usize] {
    let [a, b, c, d] = first_directory.to_le_bytes();
    let [e, f, g, h] = TIFF_MAGIC;
    [e, f, g, h, a, b, c, d]
}

/// EXIF's ASCII fields in practice hold UTF-8, but a NUL or control byte would cut them short.
fn clean_text(text: &str) -> Option<String> {
    let cleaned: String = text.chars().filter(|c| !c.is_control()).collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

fn set_text(directory: &mut Directory, tag: u16, text: Option<&String>) {
    if let Some(text) = text.and_then(|text| clean_text(text)) {
        directory.set(tag, Value::Ascii(text));
    }
}

fn positive(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value > 0.0)
}

fn decimal(value: f64, denominator: u32) -> [u32; 2] {
    [(value * f64::from(denominator)).round() as u32, denominator]
}

/// Cameras label fast speeds as fractions of a second, so they stay that way.
fn exposure_rational(seconds: f64) -> [u32; 2] {
    if seconds < 0.25 {
        [1, (1.0 / seconds).round() as u32]
    } else {
        decimal(seconds, 10)
    }
}

fn copyright_tags(main: &mut Directory, metadata: &ImageMetadata) {
    set_text(main, TAG_ARTIST, metadata.creator.as_ref());
    set_text(main, TAG_COPYRIGHT, metadata.copyright.as_ref());
}

fn camera_tags(main: &mut Directory, metadata: &ImageMetadata) {
    set_text(main, TAG_MAKE, metadata.camera_make.as_ref());
    set_text(main, TAG_MODEL, metadata.camera_model.as_ref());
    main.set(TAG_ORIENTATION, Value::Short(vec![ORIENTATION_NORMAL]));
}

fn capture_tags(exif: &mut Directory, time: &CaptureTime) {
    exif.set(TAG_DATE_TIME_ORIGINAL, Value::Ascii(time.exif_date_time()));
    if let Some(offset) = time.exif_offset() {
        exif.set(TAG_OFFSET_TIME_ORIGINAL, Value::Ascii(offset));
    }
}

fn exposure_tags(exif: &mut Directory, metadata: &ImageMetadata) {
    if let Some(seconds) = positive(metadata.exposure_seconds) {
        exif.set(
            TAG_EXPOSURE_TIME,
            Value::Rational(vec![exposure_rational(seconds)]),
        );
    }
    if let Some(f_number) = positive(metadata.f_number) {
        exif.set(TAG_F_NUMBER, Value::Rational(vec![decimal(f_number, 100)]));
    }
    if let Some(iso) = metadata.iso {
        exif.set(
            TAG_ISO,
            Value::Short(vec![iso.min(u32::from(u16::MAX)) as u16]),
        );
    }
    if let Some(focal_length) = positive(metadata.focal_length_mm) {
        exif.set(
            TAG_FOCAL_LENGTH,
            Value::Rational(vec![decimal(focal_length, 100)]),
        );
    }
    set_text(exif, TAG_LENS_MODEL, metadata.lens.as_ref());
}

fn exif_directory(
    metadata: &ImageMetadata,
    [width, height]: [u32; 2],
    space: OutputSpace,
) -> Directory {
    let mut exif = Directory::default();
    exif.set(TAG_EXIF_VERSION, Value::Undefined(b"0232".to_vec()));
    let color_space = if space == OutputSpace::Srgb {
        COLOR_SPACE_SRGB
    } else {
        COLOR_SPACE_UNCALIBRATED
    };
    exif.set(TAG_COLOR_SPACE, Value::Short(vec![color_space]));
    exif.set(TAG_PIXEL_X_DIMENSION, Value::Long(vec![width]));
    exif.set(TAG_PIXEL_Y_DIMENSION, Value::Long(vec![height]));
    if let Some(time) = &metadata.capture_time {
        capture_tags(&mut exif, time);
    }
    exposure_tags(&mut exif, metadata);
    exif
}

fn degrees_minutes_seconds(degrees: f64) -> Vec<[u32; 2]> {
    const SECONDS_DENOMINATOR: u32 = 10_000;
    let total_seconds = (degrees.abs() * 3600.0 * f64::from(SECONDS_DENOMINATOR)).round() as u64;
    let scale = u64::from(SECONDS_DENOMINATOR);
    let whole_seconds = total_seconds / scale;
    vec![
        [(whole_seconds / 3600) as u32, 1],
        [(whole_seconds / 60 % 60) as u32, 1],
        [(total_seconds % (60 * scale)) as u32, SECONDS_DENOMINATOR],
    ]
}

fn hemisphere(value: f64, positive: &str, negative: &str) -> Value {
    Value::Ascii(if value < 0.0 { negative } else { positive }.to_owned())
}

fn gps_directory(position: &GpsPosition) -> Directory {
    let mut gps = Directory::default();
    if !(position.latitude.is_finite() && position.longitude.is_finite()) {
        return gps;
    }
    gps.set(TAG_GPS_VERSION, Value::Byte(vec![2, 3, 0, 0]));
    gps.set(
        TAG_GPS_LATITUDE_REF,
        hemisphere(position.latitude, "N", "S"),
    );
    gps.set(
        TAG_GPS_LATITUDE,
        Value::Rational(degrees_minutes_seconds(position.latitude)),
    );
    gps.set(
        TAG_GPS_LONGITUDE_REF,
        hemisphere(position.longitude, "E", "W"),
    );
    gps.set(
        TAG_GPS_LONGITUDE,
        Value::Rational(degrees_minutes_seconds(position.longitude)),
    );
    if let Some(altitude) = position.altitude.filter(|altitude| altitude.is_finite()) {
        gps.set(
            TAG_GPS_ALTITUDE_REF,
            Value::Byte(vec![u8::from(altitude < 0.0)]),
        );
        gps.set(
            TAG_GPS_ALTITUDE,
            Value::Rational(vec![decimal(altitude.abs(), 100)]),
        );
    }
    gps
}

pub(crate) fn build(
    metadata: &ImageMetadata,
    settings: MetadataSettings,
    size: [u32; 2],
    space: OutputSpace,
) -> Option<TiffTree> {
    let mut tree = TiffTree::default();
    match settings.policy {
        MetadataPolicy::None => return None,
        MetadataPolicy::CopyrightOnly => copyright_tags(&mut tree.main, metadata),
        MetadataPolicy::All => {
            camera_tags(&mut tree.main, metadata);
            copyright_tags(&mut tree.main, metadata);
            tree.exif = exif_directory(metadata, size, space);
            if let Some(position) = metadata.gps.as_ref().filter(|_| !settings.strip_location) {
                tree.gps = gps_directory(position);
            }
        }
    }
    (!tree.main.is_empty()).then_some(tree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use exif::{In, Reader, Tag, Value as ExifValue};

    fn sample() -> ImageMetadata {
        ImageMetadata {
            source_name: "DSC0001".into(),
            camera_make: Some("SONY".into()),
            camera_model: Some("ILCE-7RM5".into()),
            lens: Some("FE 24-70mm F2.8 GM II".into()),
            capture_time: Some(CaptureTime {
                year: 2026,
                month: 3,
                day: 14,
                hour: 9,
                minute: 26,
                second: 53,
                utc_offset_minutes: Some(-300),
            }),
            exposure_seconds: Some(1.0 / 250.0),
            f_number: Some(2.8),
            iso: Some(400),
            focal_length_mm: Some(35.0),
            creator: Some("Jane Döe".into()),
            copyright: Some("© 2026 Jane Döe".into()),
            gps: Some(GpsPosition {
                latitude: -33.8568,
                longitude: 151.2153,
                altitude: Some(58.5),
            }),
            ..Default::default()
        }
    }

    fn settings(policy: MetadataPolicy) -> MetadataSettings {
        MetadataSettings {
            policy,
            strip_location: false,
        }
    }

    fn read_back(metadata: &ImageMetadata, settings: MetadataSettings) -> exif::Exif {
        let tree = build(metadata, settings, [6000, 4000], OutputSpace::AdobeRgb).unwrap();
        Reader::new().read_raw(tree.to_standalone_bytes()).unwrap()
    }

    fn text(fields: &exif::Exif, tag: Tag) -> String {
        match &fields.get_field(tag, In::PRIMARY).unwrap().value {
            ExifValue::Ascii(lines) => String::from_utf8(lines[0].clone()).unwrap(),
            other => panic!("{tag} is {other:?}"),
        }
    }

    fn rationals(fields: &exif::Exif, tag: Tag) -> Vec<f64> {
        match &fields.get_field(tag, In::PRIMARY).unwrap().value {
            ExifValue::Rational(values) => values.iter().map(|v| v.to_f64()).collect(),
            other => panic!("{tag} is {other:?}"),
        }
    }

    fn short(fields: &exif::Exif, tag: Tag) -> u32 {
        fields
            .get_field(tag, In::PRIMARY)
            .unwrap()
            .value
            .get_uint(0)
            .unwrap()
    }

    #[test]
    fn camera_and_exposure_read_back() {
        let fields = read_back(&sample(), settings(MetadataPolicy::All));
        assert_eq!(text(&fields, Tag::Make), "SONY");
        assert_eq!(text(&fields, Tag::Model), "ILCE-7RM5");
        assert_eq!(text(&fields, Tag::LensModel), "FE 24-70mm F2.8 GM II");
        assert_eq!(short(&fields, Tag::Orientation), 1);
        assert_eq!(short(&fields, Tag::PhotographicSensitivity), 400);
        assert_eq!(rationals(&fields, Tag::ExposureTime), [1.0 / 250.0]);
        assert_eq!(rationals(&fields, Tag::FNumber), [2.8]);
        assert_eq!(rationals(&fields, Tag::FocalLength), [35.0]);
        assert_eq!(short(&fields, Tag::PixelXDimension), 6000);
        assert_eq!(short(&fields, Tag::PixelYDimension), 4000);
    }

    #[test]
    fn capture_date_reads_back_with_its_offset() {
        let fields = read_back(&sample(), settings(MetadataPolicy::All));
        assert_eq!(text(&fields, Tag::DateTimeOriginal), "2026:03:14 09:26:53");
        assert_eq!(text(&fields, Tag::OffsetTimeOriginal), "-05:00");
    }

    #[test]
    fn copyright_is_utf8() {
        let fields = read_back(&sample(), settings(MetadataPolicy::All));
        assert_eq!(text(&fields, Tag::Copyright), "© 2026 Jane Döe");
        assert_eq!(text(&fields, Tag::Artist), "Jane Döe");
    }

    #[test]
    fn colour_space_tag_is_uncalibrated_unless_srgb() {
        let fields = read_back(&sample(), settings(MetadataPolicy::All));
        assert_eq!(short(&fields, Tag::ColorSpace), 0xFFFF);
        let srgb = build(
            &sample(),
            settings(MetadataPolicy::All),
            [1, 1],
            OutputSpace::Srgb,
        )
        .unwrap();
        let fields = Reader::new().read_raw(srgb.to_standalone_bytes()).unwrap();
        assert_eq!(short(&fields, Tag::ColorSpace), 1);
    }

    #[test]
    fn gps_reads_back() {
        let fields = read_back(&sample(), settings(MetadataPolicy::All));
        assert_eq!(text(&fields, Tag::GPSLatitudeRef), "S");
        assert_eq!(text(&fields, Tag::GPSLongitudeRef), "E");
        let latitude = rationals(&fields, Tag::GPSLatitude);
        let decimal = latitude[0] + latitude[1] / 60.0 + latitude[2] / 3600.0;
        assert!((decimal - 33.8568).abs() < 1e-6, "{latitude:?}");
        let longitude = rationals(&fields, Tag::GPSLongitude);
        let decimal = longitude[0] + longitude[1] / 60.0 + longitude[2] / 3600.0;
        assert!((decimal - 151.2153).abs() < 1e-6, "{longitude:?}");
        assert_eq!(rationals(&fields, Tag::GPSAltitude), [58.5]);
    }

    #[test]
    fn strip_location_removes_the_whole_gps_directory() {
        let stripped = MetadataSettings {
            policy: MetadataPolicy::All,
            strip_location: true,
        };
        let fields = read_back(&sample(), stripped);
        assert!(fields.get_field(Tag::GPSLatitude, In::PRIMARY).is_none());
        assert!(fields.get_field(Tag::GPSVersionID, In::PRIMARY).is_none());
        assert_eq!(text(&fields, Tag::Make), "SONY");
    }

    #[test]
    fn copyright_only_drops_everything_else() {
        let fields = read_back(&sample(), settings(MetadataPolicy::CopyrightOnly));
        assert_eq!(text(&fields, Tag::Copyright), "© 2026 Jane Döe");
        for tag in [
            Tag::Make,
            Tag::Model,
            Tag::ExposureTime,
            Tag::DateTimeOriginal,
            Tag::GPSLatitude,
        ] {
            assert!(fields.get_field(tag, In::PRIMARY).is_none(), "{tag} leaked");
        }
    }

    #[test]
    fn copyright_only_without_a_copyright_writes_nothing() {
        let metadata = ImageMetadata {
            creator: None,
            copyright: None,
            ..sample()
        };
        let built = build(
            &metadata,
            settings(MetadataPolicy::CopyrightOnly),
            [1, 1],
            OutputSpace::Srgb,
        );
        assert!(built.is_none());
    }

    #[test]
    fn policy_none_writes_nothing() {
        assert!(
            build(
                &sample(),
                settings(MetadataPolicy::None),
                [1, 1],
                OutputSpace::Srgb
            )
            .is_none()
        );
    }

    #[test]
    fn hostile_text_cannot_truncate_or_break_a_field() {
        let metadata = ImageMetadata {
            copyright: Some("a\0b\nc".into()),
            ..sample()
        };
        let fields = read_back(&metadata, settings(MetadataPolicy::All));
        assert_eq!(text(&fields, Tag::Copyright), "abc");
    }

    #[test]
    fn nonsense_exposure_values_are_left_out() {
        let metadata = ImageMetadata {
            exposure_seconds: Some(-1.0),
            f_number: Some(f64::NAN),
            focal_length_mm: Some(0.0),
            ..sample()
        };
        let fields = read_back(&metadata, settings(MetadataPolicy::All));
        for tag in [Tag::ExposureTime, Tag::FNumber, Tag::FocalLength] {
            assert!(
                fields.get_field(tag, In::PRIMARY).is_none(),
                "{tag} was written"
            );
        }
    }

    #[test]
    fn exposures_pick_the_rational_a_camera_would() {
        assert_eq!(exposure_rational(1.0 / 8000.0), [1, 8000]);
        assert_eq!(exposure_rational(0.3), [3, 10]);
        assert_eq!(exposure_rational(30.0), [300, 10]);
    }

    #[test]
    fn seconds_never_round_up_to_sixty() {
        let parts = degrees_minutes_seconds(59.999_999_999);
        assert_eq!(parts[0], [60, 1]);
        assert_eq!(parts[1], [0, 1]);
        assert_eq!(parts[2][0], 0);
    }

    #[test]
    fn large_iso_saturates() {
        let metadata = ImageMetadata {
            iso: Some(409_600),
            ..sample()
        };
        let fields = read_back(&metadata, settings(MetadataPolicy::All));
        assert_eq!(short(&fields, Tag::PhotographicSensitivity), 65535);
    }

    #[test]
    fn directories_are_sorted_and_word_aligned() {
        let mut directory = Directory::default();
        directory.set(0x0200, Value::Ascii("abcde".into()));
        directory.set(0x0100, Value::Ascii("abcdefgh".into()));
        let mut bytes = Vec::new();
        directory.encode(8, &mut bytes);
        assert_eq!(bytes.len() as u32, directory.len());
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), 0x0100);
        assert_eq!(bytes.len() % 2, 0);
    }
}
