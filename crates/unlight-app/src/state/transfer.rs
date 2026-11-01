//! The import and export dialogs' state. The work itself happens on workers; this is what the
//! dialogs show and what their buttons ask for.

use std::path::PathBuf;
use unlight_catalog::{ScanProgress, ScanReport};
use unlight_output::{
    AvifDepth, AvifSettings, BitDepth, ChromaSubsampling, ExportPreset, ExportSettings, FormatSettings,
    JpegEffort, JpegSettings, JxlMode, JxlSettings, PngSettings, TiffCompression, TiffSettings,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ImportPhase {
    #[default]
    Choosing,
    Running(Option<ScanProgress>),
    Finished(ScanReport),
    Failed(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportState {
    pub folder: String,
    pub phase: ImportPhase,
    /// A file to land on when the scan finishes, for a path opened from outside.
    pub focus: Option<PathBuf>,
}

impl ImportState {
    pub fn is_running(&self) -> bool {
        matches!(self.phase, ImportPhase::Running(_))
    }
}

/// What the dialog shows while a scan runs.
pub fn describe_progress(progress: Option<ScanProgress>) -> (String, Option<f32>) {
    use unlight_catalog::ScanPhase;
    match progress {
        None => ("Starting…".to_owned(), None),
        Some(ScanProgress {
            phase: ScanPhase::Discovering,
            done,
            ..
        }) => (format!("Looking for raw files: {done} found"), None),
        Some(ScanProgress {
            phase: ScanPhase::Reading,
            done,
            total,
        }) => (
            format!("Reading {done} of {total}"),
            Some(done as f32 / total.max(1) as f32),
        ),
    }
}

/// "1 photo" or "2 photos".
pub fn count_of(number: usize, singular: &str, plural: &str) -> String {
    format!("{number} {}", if number == 1 { singular } else { plural })
}

/// One line per thing worth knowing about a finished scan.
pub fn describe_report(report: &ScanReport) -> Vec<String> {
    let line = |number: usize, singular: &str, plural: &str| {
        (number > 0).then(|| count_of(number, singular, plural))
    };
    let added = report.added.saturating_sub(report.unsupported);
    let lines = [
        line(added, "new photo added", "new photos added"),
        line(report.changed, "changed file re-read", "changed files re-read"),
        line(report.unchanged, "already up to date", "already up to date"),
        line(
            report.moved,
            "moved file found, edits kept",
            "moved files found, edits kept",
        ),
        line(
            report.restored,
            "missing photo found again",
            "missing photos found again",
        ),
        line(report.missing, "photo now missing", "photos now missing"),
        line(report.unsupported, "unsupported file", "unsupported files"),
        line(report.sidecars_read, "sidecar read", "sidecars read"),
        line(report.sidecar_conflicts, "sidecar conflict", "sidecar conflicts"),
    ];
    let mut lines: Vec<String> = lines.into_iter().flatten().collect();
    if report.cancelled {
        lines.insert(0, "Cancelled. What was read so far is kept.".to_owned());
    }
    if lines.is_empty() {
        lines.push("No raw files found there".to_owned());
    }
    lines
}

/// The formats the export dialog offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Jpeg,
    Tiff,
    Png,
    JpegXl,
    Avif,
}

impl ExportFormat {
    pub const ALL: [(Self, &'static str); 5] = [
        (Self::Jpeg, "JPEG"),
        (Self::Tiff, "TIFF"),
        (Self::Png, "PNG"),
        (Self::JpegXl, "JPEG XL"),
        (Self::Avif, "AVIF"),
    ];

    pub fn of(format: &FormatSettings) -> Self {
        match format {
            FormatSettings::Jpeg(_) => Self::Jpeg,
            FormatSettings::Tiff(_) => Self::Tiff,
            FormatSettings::Png(_) => Self::Png,
            FormatSettings::JpegXl(_) => Self::JpegXl,
            FormatSettings::Avif(_) => Self::Avif,
        }
    }

    /// Settings to start from when the user switches to this format.
    pub fn default_settings(self) -> FormatSettings {
        match self {
            Self::Jpeg => FormatSettings::Jpeg(JpegSettings {
                quality: 92,
                subsampling: ChromaSubsampling::Yuv444,
                effort: JpegEffort::Fast,
            }),
            Self::Tiff => FormatSettings::Tiff(TiffSettings {
                depth: BitDepth::Sixteen,
                compression: TiffCompression::Deflate,
            }),
            Self::Png => FormatSettings::Png(PngSettings {
                depth: BitDepth::Sixteen,
            }),
            Self::JpegXl => FormatSettings::JpegXl(JxlSettings {
                mode: JxlMode::Lossless {
                    depth: BitDepth::Sixteen,
                },
                effort: 5,
            }),
            Self::Avif => FormatSettings::Avif(AvifSettings {
                quality: 70,
                depth: AvifDepth::Ten,
                speed: 6,
            }),
        }
    }
}

/// The export dialog's choices.
pub struct ExportForm {
    pub presets: Vec<ExportPreset>,
    pub preset: usize,
    pub settings: ExportSettings,
    pub directory: String,
}

impl ExportForm {
    pub fn new(directory: String) -> Option<Self> {
        let presets = ExportPreset::builtin();
        let settings = presets.first()?.settings.clone();
        Some(Self {
            presets,
            preset: 0,
            settings,
            directory,
        })
    }

    pub fn choose_preset(&mut self, index: usize) {
        if let Some(preset) = self.presets.get(index) {
            self.preset = index;
            self.settings = preset.settings.clone();
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportFailure {
    pub name: String,
    pub message: String,
}

/// A batch export under way or just finished.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportRun {
    pub total: usize,
    pub done: usize,
    pub skipped: usize,
    pub current: String,
    pub failures: Vec<ExportFailure>,
    pub finished: bool,
    pub cancelled: bool,
}

impl ExportRun {
    pub fn summary(&self) -> String {
        let written = self.done.saturating_sub(self.skipped + self.failures.len());
        let mut text = format!("{written} of {} exported", self.total);
        if self.skipped > 0 {
            text.push_str(&format!(", {} skipped", self.skipped));
        }
        if !self.failures.is_empty() {
            text.push_str(&format!(", {} failed", self.failures.len()));
        }
        if self.cancelled {
            text.push_str(", cancelled");
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_catalog::ScanPhase;

    #[test]
    fn progress_has_a_fraction_only_once_the_total_is_known() {
        let finding = ScanProgress {
            phase: ScanPhase::Discovering,
            done: 7,
            total: 0,
        };
        assert_eq!(describe_progress(Some(finding)).1, None);
        let reading = ScanProgress {
            phase: ScanPhase::Reading,
            done: 5,
            total: 20,
        };
        assert_eq!(describe_progress(Some(reading)).1, Some(0.25));
    }

    #[test]
    fn a_report_mentions_only_what_happened() {
        let report = ScanReport {
            added: 4,
            unsupported: 1,
            ..ScanReport::default()
        };
        assert_eq!(
            describe_report(&report),
            ["3 new photos added", "1 unsupported file"]
        );
    }

    #[test]
    fn an_empty_scan_says_so() {
        assert_eq!(
            describe_report(&ScanReport::default()),
            ["No raw files found there"]
        );
    }

    #[test]
    fn a_cancelled_scan_leads_with_that() {
        let report = ScanReport {
            added: 1,
            cancelled: true,
            ..ScanReport::default()
        };
        assert!(describe_report(&report)[0].starts_with("Cancelled"));
    }

    #[test]
    fn the_summary_separates_failures_from_files_written() {
        let run = ExportRun {
            total: 4,
            done: 4,
            skipped: 1,
            failures: vec![ExportFailure::default()],
            ..ExportRun::default()
        };
        assert_eq!(run.summary(), "2 of 4 exported, 1 skipped, 1 failed");
    }

    #[test]
    fn every_format_starts_from_valid_settings() {
        let mut form = ExportForm::new(String::new()).unwrap();
        for (format, _) in ExportFormat::ALL {
            form.settings.format = format.default_settings();
            assert!(form.settings.validate().is_ok(), "{format:?}");
            assert_eq!(ExportFormat::of(&form.settings.format), format);
        }
    }

    #[test]
    fn counts_agree_with_their_noun() {
        assert_eq!(count_of(1, "photo", "photos"), "1 photo");
        assert_eq!(count_of(2, "photo", "photos"), "2 photos");
    }

    #[test]
    fn choosing_a_preset_takes_its_settings() {
        let mut form = ExportForm::new(String::new()).unwrap();
        form.choose_preset(1);
        assert_eq!(form.preset, 1);
        assert_eq!(form.settings, form.presets[1].settings);
    }
}
