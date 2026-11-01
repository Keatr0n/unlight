use crate::exif::{self, TiffTree};
use crate::xmp;
use serde::{Deserialize, Serialize};
use unlight_color::output::OutputSpace;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataPolicy {
    #[default]
    All,
    CopyrightOnly,
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataSettings {
    pub policy: MetadataPolicy,
    pub strip_location: bool,
}

/// Local capture time, as the camera's clock showed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub utc_offset_minutes: Option<i16>,
}

impl CaptureTime {
    /// Treats `seconds` since the Unix epoch as UTC.
    pub fn from_unix_seconds(seconds: i64) -> Self {
        let days = seconds.div_euclid(86_400);
        let time = seconds.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: (time / 3600) as u8,
            minute: (time / 60 % 60) as u8,
            second: (time % 60) as u8,
            utc_offset_minutes: None,
        }
    }

    pub(crate) fn date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    pub(crate) fn exif_date_time(&self) -> String {
        format!(
            "{:04}:{:02}:{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    pub(crate) fn exif_offset(&self) -> Option<String> {
        self.utc_offset_minutes.map(|offset| {
            let sign = if offset < 0 { '-' } else { '+' };
            let minutes = offset.unsigned_abs();
            format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
        })
    }

    pub(crate) fn iso_8601(&self) -> String {
        let date = self.date();
        let time = format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second);
        format!("{date}T{time}{}", self.exif_offset().unwrap_or_default())
    }
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year as i32, month as u8, day as u8)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpsPosition {
    /// Degrees, north positive.
    pub latitude: f64,
    /// Degrees, east positive.
    pub longitude: f64,
    /// Metres above sea level.
    pub altitude: Option<f64>,
}

/// Everything known about the photo. The file name uses `source_name`, `sequence`, and the
/// camera, date and rating below.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImageMetadata {
    /// The source file's name without its extension.
    pub source_name: String,
    pub sequence: u32,
    pub rating: Option<u8>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub capture_time: Option<CaptureTime>,
    pub exposure_seconds: Option<f64>,
    pub f_number: Option<f64>,
    pub iso: Option<u32>,
    pub focal_length_mm: Option<f64>,
    pub creator: Option<String>,
    pub copyright: Option<String>,
    pub gps: Option<GpsPosition>,
}

/// Metadata in the form the encoders embed it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Embedded {
    pub exif: Option<TiffTree>,
    pub xmp: Option<String>,
}

impl Embedded {
    pub fn new(
        metadata: &ImageMetadata,
        settings: MetadataSettings,
        size: [u32; 2],
        space: OutputSpace,
    ) -> Self {
        Self {
            exif: exif::build(metadata, settings, size, space),
            xmp: xmp::packet(metadata, settings.policy),
        }
    }

    /// The bare TIFF structure that PNG, JPEG XL and AVIF store.
    pub fn exif_bytes(&self) -> Option<Vec<u8>> {
        self.exif.as_ref().map(TiffTree::to_standalone_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_seconds_become_civil_dates() {
        let check = |seconds, expected: &str| {
            assert_eq!(CaptureTime::from_unix_seconds(seconds).iso_8601(), expected);
        };
        check(0, "1970-01-01T00:00:00");
        check(951_782_400, "2000-02-29T00:00:00");
        check(1_793_404_799, "2026-10-30T23:59:59");
        check(-1, "1969-12-31T23:59:59");
    }

    #[test]
    fn offsets_format_with_their_sign() {
        let mut time = CaptureTime::from_unix_seconds(0);
        time.utc_offset_minutes = Some(-330);
        assert_eq!(time.exif_offset().as_deref(), Some("-05:30"));
        time.utc_offset_minutes = Some(60);
        assert_eq!(time.exif_offset().as_deref(), Some("+01:00"));
    }
}
