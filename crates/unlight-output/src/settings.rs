use crate::atomic::CollisionPolicy;
use crate::filename;
use crate::metadata::{MetadataPolicy, MetadataSettings};
use crate::resize::{ResizeMode, ResizeSettings};
use crate::sharpen::{OutputSharpening, SharpenAmount, SharpenMedia};
use crate::watermark::WatermarkSettings;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use unlight_color::output::OutputSpace;

const MAX_QUALITY: u8 = 100;
const MAX_JXL_DISTANCE: f32 = 25.0;
const JXL_EFFORT_RANGE: std::ops::RangeInclusive<u8> = 1..=9;
const MAX_AVIF_SPEED: u8 = 10;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromaSubsampling {
    #[default]
    Yuv444,
    Yuv420,
}

/// How hard mozjpeg works. `Fast` writes baseline files. `Smallest` writes progressive ones
/// 10 to 20% smaller, and takes about ten times as long.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JpegEffort {
    #[default]
    Fast,
    Smallest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JpegSettings {
    pub quality: u8,
    pub subsampling: ChromaSubsampling,
    pub effort: JpegEffort,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BitDepth {
    Eight,
    #[default]
    Sixteen,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TiffCompression {
    None,
    #[default]
    Deflate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TiffSettings {
    pub depth: BitDepth,
    pub compression: TiffCompression,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PngSettings {
    pub depth: BitDepth,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JxlMode {
    Lossless {
        depth: BitDepth,
    },
    /// Butteraugli distance: 1 is visually lossless and lower is better.
    Distance(f32),
    /// 0..=100, mapped to a distance the way libjxl does.
    Quality(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct JxlSettings {
    pub mode: JxlMode,
    /// 1 is fastest, 9 is smallest.
    pub effort: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvifDepth {
    Eight,
    #[default]
    Ten,
    Twelve,
}

impl AvifDepth {
    pub(crate) fn bits(self) -> u32 {
        match self {
            Self::Eight => 8,
            Self::Ten => 10,
            Self::Twelve => 12,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvifSettings {
    pub quality: u8,
    pub depth: AvifDepth,
    /// 0 is slowest and smallest, 10 is fastest.
    pub speed: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "format", rename_all = "snake_case")]
pub enum FormatSettings {
    Jpeg(JpegSettings),
    Tiff(TiffSettings),
    Png(PngSettings),
    JpegXl(JxlSettings),
    Avif(AvifSettings),
}

impl FormatSettings {
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Jpeg(_) => "jpg",
            Self::Tiff(_) => "tif",
            Self::Png(_) => "png",
            Self::JpegXl(_) => "jxl",
            Self::Avif(_) => "avif",
        }
    }

    fn validate(&self) -> Result<()> {
        let valid = match self {
            Self::Jpeg(settings) => settings.quality <= MAX_QUALITY,
            Self::Tiff(_) | Self::Png(_) => true,
            Self::JpegXl(settings) => {
                JXL_EFFORT_RANGE.contains(&settings.effort)
                    && match settings.mode {
                        JxlMode::Lossless { .. } => true,
                        JxlMode::Distance(distance) => (0.0..=MAX_JXL_DISTANCE).contains(&distance),
                        JxlMode::Quality(quality) => quality <= MAX_QUALITY,
                    }
            }
            Self::Avif(settings) => {
                settings.quality <= MAX_QUALITY && settings.speed <= MAX_AVIF_SPEED
            }
        };
        valid
            .then_some(())
            .ok_or_else(|| Error::Settings(format!("{self:?} is out of range")))
    }
}

/// `OutputSpace` lives in a crate without serde.
#[derive(Serialize, Deserialize)]
#[serde(remote = "OutputSpace", rename_all = "snake_case")]
enum OutputSpaceDef {
    Srgb,
    DisplayP3,
    AdobeRgb,
    ProPhoto,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportSettings {
    pub format: FormatSettings,
    /// The space the engine rendered into. Files embed its profile.
    #[serde(with = "OutputSpaceDef")]
    pub color_space: OutputSpace,
    pub resize: Option<ResizeSettings>,
    pub sharpening: Option<OutputSharpening>,
    pub metadata: MetadataSettings,
    pub watermark: Option<WatermarkSettings>,
    /// Text with `{name}`, `{date}`, `{seq}`, `{camera}` and `{rating}` tokens.
    pub filename_template: String,
    pub collision: CollisionPolicy,
}

impl ExportSettings {
    pub fn validate(&self) -> Result<()> {
        self.format.validate()?;
        filename::validate(&self.filename_template)?;
        if let Some(resize) = &self.resize {
            resize.validate()?;
        }
        if let Some(watermark) = &self.watermark {
            watermark.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportPreset {
    pub name: String,
    pub settings: ExportSettings,
}

fn preset(name: &str, settings: ExportSettings) -> ExportPreset {
    ExportPreset {
        name: name.to_owned(),
        settings,
    }
}

fn base(format: FormatSettings, color_space: OutputSpace) -> ExportSettings {
    ExportSettings {
        format,
        color_space,
        resize: None,
        sharpening: None,
        metadata: MetadataSettings::default(),
        watermark: None,
        filename_template: "{name}".to_owned(),
        collision: CollisionPolicy::default(),
    }
}

impl ExportPreset {
    pub fn builtin() -> Vec<Self> {
        let full_jpeg = FormatSettings::Jpeg(JpegSettings {
            quality: 92,
            subsampling: ChromaSubsampling::Yuv444,
            effort: JpegEffort::Fast,
        });
        let web_jpeg = FormatSettings::Jpeg(JpegSettings {
            quality: 82,
            subsampling: ChromaSubsampling::Yuv420,
            effort: JpegEffort::Smallest,
        });
        let archive_tiff = FormatSettings::Tiff(TiffSettings {
            depth: BitDepth::Sixteen,
            compression: TiffCompression::Deflate,
        });
        let lossless_jxl = FormatSettings::JpegXl(JxlSettings {
            mode: JxlMode::Lossless {
                depth: BitDepth::Sixteen,
            },
            effort: 5,
        });
        let web_avif = FormatSettings::Avif(AvifSettings {
            quality: 70,
            depth: AvifDepth::Ten,
            speed: 6,
        });

        let web_size = ResizeSettings {
            mode: ResizeMode::LongEdge(2048),
            allow_upscale: false,
        };
        let screen = OutputSharpening {
            media: SharpenMedia::Screen,
            amount: SharpenAmount::Standard,
        };
        let copyright_only = MetadataSettings {
            policy: MetadataPolicy::CopyrightOnly,
            strip_location: true,
        };
        vec![
            preset("Full-size JPEG", base(full_jpeg, OutputSpace::Srgb)),
            preset(
                "Web JPEG",
                ExportSettings {
                    resize: Some(web_size),
                    sharpening: Some(screen),
                    metadata: copyright_only,
                    ..base(web_jpeg, OutputSpace::Srgb)
                },
            ),
            preset(
                "Web AVIF",
                ExportSettings {
                    resize: Some(web_size),
                    sharpening: Some(screen),
                    metadata: copyright_only,
                    ..base(web_avif, OutputSpace::DisplayP3)
                },
            ),
            preset("Archive TIFF", base(archive_tiff, OutputSpace::ProPhoto)),
            preset(
                "Lossless JPEG XL",
                base(lossless_jxl, OutputSpace::ProPhoto),
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resize::Fit;
    use crate::watermark::{Anchor, WatermarkSource};
    use std::path::PathBuf;

    fn everything() -> ExportSettings {
        ExportSettings {
            format: FormatSettings::Tiff(TiffSettings {
                depth: BitDepth::Sixteen,
                compression: TiffCompression::Deflate,
            }),
            color_space: OutputSpace::AdobeRgb,
            resize: Some(ResizeSettings {
                mode: ResizeMode::Exact {
                    width: 1200,
                    height: 800,
                    fit: Fit::Cover,
                },
                allow_upscale: true,
            }),
            sharpening: Some(OutputSharpening {
                media: SharpenMedia::Matte,
                amount: SharpenAmount::High,
            }),
            metadata: MetadataSettings {
                policy: MetadataPolicy::CopyrightOnly,
                strip_location: true,
            },
            watermark: Some(WatermarkSettings {
                source: WatermarkSource::Text {
                    text: "© Jane".into(),
                    font: PathBuf::from("/fonts/a.ttf"),
                    color: [255, 255, 255],
                },
                anchor: Anchor::BottomLeft,
                opacity: 0.5,
                scale: 0.2,
                inset: 0.02,
            }),
            filename_template: "{date}_{name}_{seq}".into(),
            collision: CollisionPolicy::Skip,
        }
    }

    #[test]
    fn settings_survive_json() {
        let settings = everything();
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(
            serde_json::from_str::<ExportSettings>(&json).unwrap(),
            settings
        );
    }

    #[test]
    fn every_format_survives_json() {
        for preset in ExportPreset::builtin() {
            let json = serde_json::to_string(&preset).unwrap();
            assert_eq!(serde_json::from_str::<ExportPreset>(&json).unwrap(), preset);
        }
    }

    #[test]
    fn colour_spaces_have_readable_json_names() {
        let json = serde_json::to_value(everything()).unwrap();
        assert_eq!(json["color_space"], "adobe_rgb");
        assert_eq!(json["format"]["format"], "tiff");
    }

    #[test]
    fn builtin_presets_are_valid_and_uniquely_named() {
        let presets = ExportPreset::builtin();
        for preset in &presets {
            preset
                .settings
                .validate()
                .unwrap_or_else(|error| panic!("{}: {error}", preset.name));
        }
        let names: std::collections::BTreeSet<&str> =
            presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names.len(), presets.len());
    }

    #[test]
    fn out_of_range_formats_are_refused() {
        let jpeg = |quality| {
            FormatSettings::Jpeg(JpegSettings {
                quality,
                subsampling: ChromaSubsampling::Yuv444,
                effort: JpegEffort::Fast,
            })
        };
        assert!(jpeg(100).validate().is_ok());
        assert!(jpeg(101).validate().is_err());

        let jxl = |mode, effort| FormatSettings::JpegXl(JxlSettings { mode, effort });
        assert!(jxl(JxlMode::Distance(1.0), 7).validate().is_ok());
        assert!(jxl(JxlMode::Distance(26.0), 7).validate().is_err());
        assert!(jxl(JxlMode::Distance(f32::NAN), 7).validate().is_err());
        assert!(jxl(JxlMode::Quality(101), 7).validate().is_err());
        assert!(jxl(JxlMode::Distance(1.0), 0).validate().is_err());
        assert!(jxl(JxlMode::Distance(1.0), 10).validate().is_err());

        let avif = |quality, speed| {
            FormatSettings::Avif(AvifSettings {
                quality,
                depth: AvifDepth::Ten,
                speed,
            })
        };
        assert!(avif(100, 10).validate().is_ok());
        assert!(avif(101, 6).validate().is_err());
        assert!(avif(50, 11).validate().is_err());
    }

    #[test]
    fn a_bad_template_fails_validation() {
        let settings = ExportSettings {
            filename_template: "{nope}".into(),
            ..everything()
        };
        assert!(matches!(settings.validate(), Err(Error::Template(_))));
    }

    #[test]
    fn extensions_match_the_format() {
        let extensions: Vec<&str> = ExportPreset::builtin()
            .iter()
            .map(|p| p.settings.format.extension())
            .collect();
        assert_eq!(extensions, ["jpg", "jpg", "avif", "tif", "jxl"]);
    }
}
