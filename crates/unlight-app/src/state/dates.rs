//! Typed dates, for the filter bar and smart collections. `2024`, `2024-03` and `2024-03-15`
//! all work, each meaning the whole of that span in local time.

use chrono::{DateTime, Local, NaiveDate, TimeZone};

fn local_midnight(date: NaiveDate) -> Option<i64> {
    let time = date.and_hms_opt(0, 0, 0)?;
    Some(Local.from_local_datetime(&time).earliest()?.timestamp())
}

/// The start of a date and the start of what follows it, in Unix seconds.
pub fn date_span(text: &str) -> Option<(i64, i64)> {
    let mut parts = text.trim().splitn(3, '-').map(|part| part.parse::<u32>().ok());
    let year = parts.next()?? as i32;
    let month = parts.next().unwrap_or(Some(0))?;
    let day = parts.next().unwrap_or(Some(0))?;
    let (start, end) = match (month, day) {
        (0, 0) => (
            NaiveDate::from_ymd_opt(year, 1, 1)?,
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?,
        ),
        (month, 0) => {
            let next = if month == 12 {
                (year + 1, 1)
            } else {
                (year, month + 1)
            };
            (
                NaiveDate::from_ymd_opt(year, month, 1)?,
                NaiveDate::from_ymd_opt(next.0, next.1, 1)?,
            )
        }
        (month, day) => {
            let start = NaiveDate::from_ymd_opt(year, month, day)?;
            (start, start.succ_opt()?)
        }
    };
    Some((local_midnight(start)?, local_midnight(end)?))
}

/// Capture time as the camera's clock showed it.
pub fn format_capture(seconds: i64) -> String {
    DateTime::from_timestamp(seconds, 0)
        .map(|time| time.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_year_spans_the_year() {
        let (start, end) = date_span("2024").unwrap();
        assert!((end - start - 366 * 86_400).abs() <= 3600);
    }

    #[test]
    fn december_rolls_into_the_next_year() {
        let (start, end) = date_span("2023-12").unwrap();
        assert!((end - start - 31 * 86_400).abs() <= 3600);
    }

    #[test]
    fn a_day_is_one_day() {
        let (start, end) = date_span(" 2024-03-15 ").unwrap();
        assert!((end - start - 86_400).abs() <= 3600);
    }

    #[test]
    fn nonsense_is_rejected() {
        for text in ["", "soon", "2024-13", "2024-02-31", "2024-xx"] {
            assert_eq!(date_span(text), None, "{text}");
        }
    }
}
