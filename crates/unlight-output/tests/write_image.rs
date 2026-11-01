//! The whole export flow through the public API, read back with independent decoders.

use exif::{In, Reader, Tag};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use tempfile::tempdir;
use unlight_output::*;

const JPEG: FormatSettings = FormatSettings::Jpeg(JpegSettings {
    quality: 90,
    subsampling: ChromaSubsampling::Yuv444,
    effort: JpegEffort::Fast,
});
const TIFF: FormatSettings = FormatSettings::Tiff(TiffSettings {
    depth: BitDepth::Sixteen,
    compression: TiffCompression::Deflate,
});
const PNG: FormatSettings = FormatSettings::Png(PngSettings {
    depth: BitDepth::Sixteen,
});
const JXL: FormatSettings = FormatSettings::JpegXl(JxlSettings {
    mode: JxlMode::Lossless {
        depth: BitDepth::Sixteen,
    },
    effort: 3,
});
const AVIF: FormatSettings = FormatSettings::Avif(AvifSettings {
    quality: 80,
    depth: AvifDepth::Ten,
    speed: 10,
});
const ALL_FORMATS: [FormatSettings; 5] = [JPEG, TIFF, PNG, JXL, AVIF];

fn pixels(width: u32, height: u32) -> Vec<f32> {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
    for y in 0..height {
        for x in 0..width {
            let u = x as f32 / (width - 1) as f32;
            let v = y as f32 / (height - 1) as f32;
            pixels.extend([u, v, 1.0 - u]);
        }
    }
    pixels
}

fn image(width: u32, height: u32, space: OutputSpace) -> Image<'static> {
    Image::new(width, height, space, pixels(width, height)).unwrap()
}

fn settings(format: FormatSettings) -> ExportSettings {
    ExportSettings {
        format,
        color_space: OutputSpace::Srgb,
        resize: None,
        sharpening: None,
        metadata: MetadataSettings::default(),
        watermark: None,
        filename_template: "{name}".into(),
        collision: CollisionPolicy::Unique,
    }
}

fn metadata() -> ImageMetadata {
    ImageMetadata {
        source_name: "DSC0001".into(),
        sequence: 7,
        rating: Some(5),
        camera_make: Some("FUJIFILM".into()),
        camera_model: Some("X-T5".into()),
        lens: Some("XF23mmF1.4 R LM WR".into()),
        capture_time: Some(CaptureTime {
            year: 2026,
            month: 10,
            day: 30,
            hour: 12,
            minute: 34,
            second: 56,
            utc_offset_minutes: Some(60),
        }),
        exposure_seconds: Some(1.0 / 125.0),
        f_number: Some(1.4),
        iso: Some(640),
        focal_length_mm: Some(23.0),
        creator: Some("Jane Doe".into()),
        copyright: Some("© 2026 Jane Doe".into()),
        gps: Some(GpsPosition {
            latitude: 48.8584,
            longitude: 2.2945,
            altitude: Some(35.0),
        }),
    }
}

fn files_in(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn text(fields: &exif::Exif, tag: Tag) -> Option<String> {
    let field = fields.get_field(tag, In::PRIMARY)?;
    Some(
        field
            .display_value()
            .to_string()
            .trim_matches('"')
            .to_owned(),
    )
}

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

/// Pulls the EXIF block out of any of the five containers.
fn exif_of(path: &Path) -> Option<exif::Exif> {
    let bytes = fs::read(path).unwrap();
    match path.extension()?.to_str()? {
        "jpg" | "tif" | "png" => Reader::new()
            .read_from_container(&mut Cursor::new(&bytes))
            .ok(),
        "jxl" | "avif" => {
            let start = bytes.windows(8).position(|w| w == b"II*\0\x08\0\0\0")?;
            Reader::new().read_raw(bytes[start..].to_vec()).ok()
        }
        other => panic!("unexpected extension {other}"),
    }
}

#[test]
fn every_format_writes_a_named_file_and_nothing_else() {
    let directory = tempdir().unwrap();
    for format in ALL_FORMATS {
        let path = write_image(
            &image(64, 48, OutputSpace::Srgb),
            &settings(format),
            &metadata(),
            directory.path(),
        )
        .unwrap();
        assert!(fs::metadata(&path).unwrap().len() > 0, "{path:?}");
    }
    assert_eq!(
        files_in(directory.path()),
        [
            "DSC0001.avif",
            "DSC0001.jpg",
            "DSC0001.jxl",
            "DSC0001.png",
            "DSC0001.tif"
        ]
    );
}

#[test]
fn every_format_carries_exif_and_copyright() {
    let directory = tempdir().unwrap();
    for format in ALL_FORMATS {
        let path = write_image(
            &image(64, 48, OutputSpace::Srgb),
            &settings(format),
            &metadata(),
            directory.path(),
        )
        .unwrap();
        let fields = exif_of(&path).unwrap_or_else(|| panic!("no EXIF in {path:?}"));
        assert_eq!(
            text(&fields, Tag::Model).as_deref(),
            Some("X-T5"),
            "{path:?}"
        );
        assert_eq!(
            text(&fields, Tag::Make).as_deref(),
            Some("FUJIFILM"),
            "{path:?}"
        );
        assert_eq!(
            text(&fields, Tag::Orientation).as_deref(),
            Some("row 0 at top and column 0 at left"),
            "{path:?}"
        );
        let date = text(&fields, Tag::DateTimeOriginal).unwrap();
        assert_eq!(date, "2026-10-30 12:34:56", "{path:?}");
        assert!(
            fields.get_field(Tag::GPSLatitude, In::PRIMARY).is_some(),
            "{path:?}"
        );
        assert!(
            contains(&fs::read(&path).unwrap(), "© 2026 Jane Doe".as_bytes()),
            "{path:?}"
        );
    }
}

#[test]
fn metadata_none_leaves_no_trace_in_any_format() {
    let directory = tempdir().unwrap();
    let mut export = settings(JPEG);
    export.metadata.policy = MetadataPolicy::None;
    for format in ALL_FORMATS {
        export.format = format;
        let path = write_image(
            &image(64, 48, OutputSpace::Srgb),
            &export,
            &metadata(),
            directory.path(),
        )
        .unwrap();
        let bytes = fs::read(&path).unwrap();
        assert!(
            exif_of(&path).is_none_or(|fields| text(&fields, Tag::Model).is_none()),
            "{path:?}"
        );
        assert!(!contains(&bytes, b"X-T5"), "{path:?} leaked the camera");
        assert!(!contains(&bytes, b"Jane Doe"), "{path:?} leaked the owner");
    }
}

#[test]
fn copyright_only_keeps_the_owner_and_drops_the_camera() {
    let directory = tempdir().unwrap();
    let mut export = settings(JPEG);
    export.metadata.policy = MetadataPolicy::CopyrightOnly;
    for format in ALL_FORMATS {
        export.format = format;
        let path = write_image(
            &image(64, 48, OutputSpace::Srgb),
            &export,
            &metadata(),
            directory.path(),
        )
        .unwrap();
        let bytes = fs::read(&path).unwrap();
        assert!(contains(&bytes, "© 2026 Jane Doe".as_bytes()), "{path:?}");
        assert!(!contains(&bytes, b"X-T5"), "{path:?} leaked the camera");
        assert!(!contains(&bytes, b"FUJIFILM"), "{path:?} leaked the make");
    }
}

#[test]
fn strip_location_removes_gps_from_every_format() {
    let directory = tempdir().unwrap();
    let mut export = settings(JPEG);
    export.metadata.strip_location = true;
    for format in ALL_FORMATS {
        export.format = format;
        let path = write_image(
            &image(64, 48, OutputSpace::Srgb),
            &export,
            &metadata(),
            directory.path(),
        )
        .unwrap();
        let fields = exif_of(&path).unwrap();
        assert!(
            fields.get_field(Tag::GPSLatitude, In::PRIMARY).is_none(),
            "{path:?}"
        );
        assert_eq!(
            text(&fields, Tag::Model).as_deref(),
            Some("X-T5"),
            "{path:?}"
        );
    }
}

#[test]
fn a_colour_space_mismatch_is_refused_before_writing() {
    let directory = tempdir().unwrap();
    let export = ExportSettings {
        color_space: OutputSpace::AdobeRgb,
        ..settings(JPEG)
    };
    let error = write_image(
        &image(8, 8, OutputSpace::Srgb),
        &export,
        &metadata(),
        directory.path(),
    )
    .unwrap_err();
    assert!(matches!(error, Error::ColorSpaceMismatch { .. }), "{error}");
    assert!(files_in(directory.path()).is_empty());
}

#[test]
fn bad_settings_are_refused_before_writing() {
    let directory = tempdir().unwrap();
    let export = ExportSettings {
        filename_template: "{bogus}".into(),
        ..settings(JPEG)
    };
    assert!(
        write_image(
            &image(8, 8, OutputSpace::Srgb),
            &export,
            &metadata(),
            directory.path()
        )
        .is_err()
    );
    assert!(files_in(directory.path()).is_empty());
}

#[test]
fn the_template_names_the_file() {
    let directory = tempdir().unwrap();
    let export = ExportSettings {
        filename_template: "{date}_{camera}_{name}_{seq}_{rating}".into(),
        ..settings(JPEG)
    };
    let path = write_image(
        &image(8, 8, OutputSpace::Srgb),
        &export,
        &metadata(),
        directory.path(),
    )
    .unwrap();
    assert_eq!(
        path.file_name().unwrap(),
        "2026-10-30_X-T5_DSC0001_0007_5.jpg"
    );
}

#[test]
fn hostile_names_cannot_leave_the_destination() {
    let outer = tempdir().unwrap();
    let directory = outer.path().join("exports");
    fs::create_dir(&directory).unwrap();
    let hostile = ImageMetadata {
        source_name: "../../escape".into(),
        ..metadata()
    };
    let path = write_image(
        &image(8, 8, OutputSpace::Srgb),
        &settings(JPEG),
        &hostile,
        &directory,
    )
    .unwrap();
    assert_eq!(path.parent().unwrap(), directory);
    assert_eq!(files_in(outer.path()), ["exports"]);
    assert_eq!(files_in(&directory).len(), 1);
}

#[test]
fn skip_returns_an_error_and_keeps_the_original() {
    let directory = tempdir().unwrap();
    let export = ExportSettings {
        collision: CollisionPolicy::Skip,
        ..settings(PNG)
    };
    let source = image(8, 8, OutputSpace::Srgb);
    let first = write_image(&source, &export, &metadata(), directory.path()).unwrap();
    let before = fs::read(&first).unwrap();
    let different = image(16, 16, OutputSpace::Srgb);
    let error = write_image(&different, &export, &metadata(), directory.path()).unwrap_err();
    assert!(
        matches!(&error, Error::Skipped { path } if *path == first),
        "{error}"
    );
    assert_eq!(fs::read(&first).unwrap(), before);
}

#[test]
fn overwrite_replaces_and_unique_adds_a_suffix() {
    let directory = tempdir().unwrap();
    let source = image(8, 8, OutputSpace::Srgb);
    let overwrite = ExportSettings {
        collision: CollisionPolicy::Overwrite,
        ..settings(PNG)
    };
    write_image(&source, &overwrite, &metadata(), directory.path()).unwrap();
    write_image(&source, &overwrite, &metadata(), directory.path()).unwrap();
    assert_eq!(files_in(directory.path()), ["DSC0001.png"]);

    let path = write_image(&source, &settings(PNG), &metadata(), directory.path()).unwrap();
    assert_eq!(path.file_name().unwrap(), "DSC0001-1.png");
}

#[test]
fn a_failed_export_leaves_no_temporary_files() {
    let directory = tempdir().unwrap();
    fs::create_dir(directory.path().join("DSC0001.jpg")).unwrap();
    let export = ExportSettings {
        collision: CollisionPolicy::Overwrite,
        ..settings(JPEG)
    };
    let result = write_image(
        &image(8, 8, OutputSpace::Srgb),
        &export,
        &metadata(),
        directory.path(),
    );
    assert!(result.is_err());
    assert_eq!(files_in(directory.path()), ["DSC0001.jpg"]);
}

#[test]
fn an_encoder_failure_after_writing_began_leaves_no_temporary_files() {
    let directory = tempdir().unwrap();
    let oversized = ImageMetadata {
        copyright: Some("x".repeat(70_000)),
        ..metadata()
    };
    let result = write_image(
        &image(8, 8, OutputSpace::Srgb),
        &settings(JPEG),
        &oversized,
        directory.path(),
    );
    assert!(
        matches!(result, Err(Error::MetadataTooLarge { .. })),
        "{result:?}"
    );
    assert!(files_in(directory.path()).is_empty());
}

#[test]
fn a_missing_watermark_fails_cleanly() {
    let directory = tempdir().unwrap();
    let export = ExportSettings {
        watermark: Some(WatermarkSettings {
            source: WatermarkSource::Image {
                path: PathBuf::from("/nonexistent.png"),
            },
            anchor: Anchor::Center,
            opacity: 1.0,
            scale: 0.5,
            inset: 0.0,
        }),
        ..settings(JPEG)
    };
    let error = write_image(
        &image(32, 32, OutputSpace::Srgb),
        &export,
        &metadata(),
        directory.path(),
    )
    .unwrap_err();
    assert!(matches!(error, Error::Watermark(_)), "{error}");
    assert!(files_in(directory.path()).is_empty());
}

#[test]
fn resizing_changes_the_written_size_and_the_exif_dimensions() {
    let directory = tempdir().unwrap();
    let export = ExportSettings {
        resize: Some(ResizeSettings {
            mode: ResizeMode::LongEdge(50),
            allow_upscale: false,
        }),
        ..settings(TIFF)
    };
    let path = write_image(
        &image(200, 100, OutputSpace::Srgb),
        &export,
        &metadata(),
        directory.path(),
    )
    .unwrap();
    let mut decoder = tiff::decoder::Decoder::new(fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(decoder.dimensions().unwrap(), (50, 25));
    let fields = exif_of(&path).unwrap();
    let width = fields.get_field(Tag::PixelXDimension, In::PRIMARY).unwrap();
    assert_eq!(width.value.get_uint(0), Some(50));
}

#[test]
fn every_step_runs_together() {
    let directory = tempdir().unwrap();
    let logo = directory.path().join("logo.png");
    {
        let file = std::io::BufWriter::new(fs::File::create(&logo).unwrap());
        let mut encoder = png::Encoder::new(file, 4, 4);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[255; 64])
            .unwrap();
    }
    let export = ExportSettings {
        color_space: OutputSpace::DisplayP3,
        resize: Some(ResizeSettings {
            mode: ResizeMode::Percent(50.0),
            allow_upscale: false,
        }),
        sharpening: Some(OutputSharpening {
            media: SharpenMedia::Glossy,
            amount: SharpenAmount::Standard,
        }),
        watermark: Some(WatermarkSettings {
            source: WatermarkSource::Image { path: logo },
            anchor: Anchor::TopLeft,
            opacity: 1.0,
            scale: 0.25,
            inset: 0.0,
        }),
        ..settings(TIFF)
    };
    let output = directory.path().join("out");
    fs::create_dir(&output).unwrap();
    let source = image(200, 100, OutputSpace::DisplayP3);
    let path = write_image(&source, &export, &metadata(), &output).unwrap();

    let mut decoder = tiff::decoder::Decoder::new(fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(decoder.dimensions().unwrap(), (100, 50));
    let tiff::decoder::DecodingResult::U16(samples) = decoder.read_image().unwrap() else {
        panic!("expected 16-bit samples");
    };
    assert!(
        samples[..3].iter().all(|&sample| sample > 65000),
        "{:?}",
        &samples[..3]
    );
    let bottom_right = samples[samples.len() - 3];
    assert!(
        bottom_right > 30000,
        "the image itself is missing: {bottom_right}"
    );
}

#[test]
fn the_source_image_is_left_untouched() {
    let directory = tempdir().unwrap();
    let source = pixels(64, 64);
    let image = Image::new(64, 64, OutputSpace::Srgb, source.as_slice()).unwrap();
    let export = ExportSettings {
        sharpening: Some(OutputSharpening {
            media: SharpenMedia::Matte,
            amount: SharpenAmount::High,
        }),
        ..settings(PNG)
    };
    write_image(&image, &export, &metadata(), directory.path()).unwrap();
    assert_eq!(image.pixels(), source);
}

#[test]
fn builtin_presets_all_work_end_to_end() {
    let directory = tempdir().unwrap();
    for preset in ExportPreset::builtin() {
        let source = image(96, 64, preset.settings.color_space);
        let path = write_image(&source, &preset.settings, &metadata(), directory.path())
            .unwrap_or_else(|error| panic!("{}: {error}", preset.name));
        assert!(path.exists(), "{}", preset.name);
    }
}

/// Sample colours as a colour-managed viewer would see them: decoded through the profile
/// embedded in the file, then shown on an sRGB screen.
fn seen_through_embedded_profile(path: &Path) -> Vec<[f32; 3]> {
    let decoder = png::Decoder::new(std::io::BufReader::new(fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let profile = reader
        .info()
        .icc_profile
        .as_deref()
        .expect("no iCCP chunk")
        .to_vec();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buffer).unwrap();
    let samples: Vec<[f32; 3]> = buffer[..info.buffer_size()]
        .as_chunks::<6>()
        .0
        .iter()
        .map(|pixel| {
            [0, 2, 4].map(|i| f32::from(u16::from_be_bytes([pixel[i], pixel[i + 1]])) / 65535.0)
        })
        .collect();

    let transform: lcms2::Transform<[f32; 3], [f32; 3]> = lcms2::Transform::new(
        &lcms2::Profile::new_icc(&profile).unwrap(),
        lcms2::PixelFormat::RGB_FLT,
        &lcms2::Profile::new_srgb(),
        lcms2::PixelFormat::RGB_FLT,
        lcms2::Intent::RelativeColorimetric,
    )
    .unwrap();
    let mut shown = vec![[0.0; 3]; samples.len()];
    transform.transform_pixels(&samples, &mut shown);
    shown
}

#[test]
fn the_same_scene_looks_the_same_in_every_colour_space() {
    let directory = tempdir().unwrap();
    let srgb_to_prophoto =
        unlight_color::matrix::inverse(&OutputSpace::Srgb.matrix_from_prophoto()).unwrap();
    let scene: Vec<[f64; 3]> = (0..16 * 16)
        .map(|i| {
            let (x, y) = (f64::from(i % 16), f64::from(i / 16));
            let srgb_linear = [0.05 + x * 0.05, 0.05 + y * 0.05, 0.8 - x * 0.03 - y * 0.015];
            unlight_color::matrix::apply(&srgb_to_prophoto, &srgb_linear)
        })
        .collect();
    let render = |space: OutputSpace| -> Vec<f32> {
        let matrix = space.matrix_from_prophoto();
        let transfer = space.transfer();
        scene
            .iter()
            .flat_map(|color| {
                unlight_color::matrix::apply(&matrix, color)
                    .map(|value| transfer.encode(value as f32))
            })
            .collect()
    };
    let srgb = render(OutputSpace::Srgb);

    for space in OutputSpace::ALL {
        let source = Image::new(16, 16, space, render(space)).unwrap();
        let export = ExportSettings {
            color_space: space,
            ..settings(PNG)
        };
        let output = directory
            .path()
            .join(space.name().replace(['(', ')', ' '], ""));
        fs::create_dir(&output).unwrap();
        let path = write_image(&source, &export, &metadata(), &output).unwrap();
        for (shown, expected) in seen_through_embedded_profile(&path)
            .iter()
            .zip(srgb.as_chunks::<3>().0)
        {
            for channel in 0..3 {
                let difference = (shown[channel] - expected[channel]).abs();
                assert!(
                    difference < 2e-3,
                    "{space:?}: saw {shown:?}, expected {expected:?}"
                );
            }
        }
    }
}

#[test]
fn icc_profile_is_public_and_parses() {
    for space in OutputSpace::ALL {
        assert!(lcms2::Profile::new_icc(&icc_profile(space).unwrap()).is_ok());
    }
}
