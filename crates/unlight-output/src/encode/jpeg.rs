use super::{Input, failure};
use crate::image::quantize_u8;
use crate::metadata::Embedded;
use crate::settings::{ChromaSubsampling, JpegEffort, JpegSettings};
use crate::{Error, Result};
use mozjpeg::{ColorSpace, Compress, Marker};
use std::any::Any;
use std::io::Write;
use std::panic::{self, AssertUnwindSafe};

const FORMAT: &str = "JPEG";
const EXIF_PREFIX: &[u8] = b"Exif\0\0";
const XMP_PREFIX: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
/// Followed by the 1-based chunk number and the chunk count. Our profiles need one chunk.
const ICC_PREFIX: &[u8] = b"ICC_PROFILE\0\x01\x01";
/// A marker's 16-bit length field counts its own two bytes.
const MAX_SEGMENT_PAYLOAD: usize = 65533;

fn segment(kind: &'static str, prefix: &[u8], body: &[u8]) -> Result<Vec<u8>> {
    let size = prefix.len() + body.len();
    if size > MAX_SEGMENT_PAYLOAD {
        return Err(Error::MetadataTooLarge {
            kind,
            format: FORMAT,
            size,
        });
    }
    Ok([prefix, body].concat())
}

fn metadata_segments(metadata: &Embedded) -> Result<Vec<Vec<u8>>> {
    let mut segments = Vec::new();
    if let Some(exif) = metadata.exif_bytes() {
        segments.push(segment("EXIF", EXIF_PREFIX, &exif)?);
    }
    if let Some(xmp) = &metadata.xmp {
        segments.push(segment("XMP", XMP_PREFIX, xmp.as_bytes())?);
    }
    Ok(segments)
}

fn chroma_pixel_sizes(subsampling: ChromaSubsampling) -> ((u8, u8), (u8, u8)) {
    match subsampling {
        ChromaSubsampling::Yuv444 => ((1, 1), (1, 1)),
        ChromaSubsampling::Yuv420 => ((2, 2), (2, 2)),
    }
}

fn compress(
    input: &Input<'_>,
    settings: &JpegSettings,
    rgb: &[u8],
    segments: &[Vec<u8>],
    icc_segment: &[u8],
    out: &mut dyn Write,
) -> std::io::Result<()> {
    let mut compress = Compress::new(ColorSpace::JCS_EXT_RGB);
    if settings.effort == JpegEffort::Fast {
        compress.set_fastest_defaults();
        compress.set_optimize_coding(true);
    }
    compress.set_size(input.image.width() as usize, input.image.height() as usize);
    compress.set_quality(f32::from(settings.quality));
    let (cb, cr) = chroma_pixel_sizes(settings.subsampling);
    compress.set_chroma_sampling_pixel_sizes(cb, cr);

    let mut started = compress.start_compress(out)?;
    for payload in segments {
        started.write_marker(Marker::APP(1), payload);
    }
    started.write_marker(Marker::APP(2), icc_segment);
    started.write_scanlines(rgb)?;
    started.finish().map(drop)
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("mozjpeg failed")
}

pub(super) fn encode(
    input: &Input<'_>,
    settings: &JpegSettings,
    out: &mut dyn Write,
) -> Result<()> {
    let segments = metadata_segments(input.metadata)?;
    // Not `write_icc_profile`: it numbers chunks from 0, and readers like libjpeg's own
    // `jpeg_read_icc_profile` then treat the profile as corrupt and ignore it.
    let icc_segment = segment("ICC", ICC_PREFIX, input.icc)?;
    let rgb = quantize_u8(input.image.pixels());
    // mozjpeg reports errors by unwinding through its C frames.
    let run = || compress(input, settings, &rgb, &segments, &icc_segment, out);
    panic::catch_unwind(AssertUnwindSafe(run))
        .map_err(|payload| failure(FORMAT)(&panic_message(payload.as_ref())))?
        .map_err(|error| failure(FORMAT)(&error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Image;
    use crate::encode::testing::{embedded, psnr, test_image};
    use crate::icc::icc_profile;
    use std::io::Cursor;
    use unlight_color::output::OutputSpace;
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace as ZuneColorSpace;
    use zune_core::options::DecoderOptions;
    use zune_jpeg::JpegDecoder;

    const fn settings(quality: u8, subsampling: ChromaSubsampling) -> JpegSettings {
        JpegSettings {
            quality,
            subsampling,
            effort: JpegEffort::Fast,
        }
    }

    fn encoded(space: OutputSpace, settings: &JpegSettings) -> (Vec<u8>, Vec<f32>) {
        let image = test_image(96, 64, space);
        let icc = icc_profile(space).unwrap();
        let metadata = embedded(&image);
        let input = Input {
            image: &image,
            icc: &icc,
            metadata: &metadata,
        };
        let mut bytes = Vec::new();
        encode(&input, settings, &mut bytes).unwrap();
        (bytes, image.pixels().to_vec())
    }

    fn decoder(bytes: &[u8]) -> JpegDecoder<ZCursor<&[u8]>> {
        let options = DecoderOptions::default().jpeg_set_out_colorspace(ZuneColorSpace::RGB);
        JpegDecoder::new_with_options(ZCursor::new(bytes), options)
    }

    fn decoded(bytes: &[u8]) -> Vec<f32> {
        let rgb = decoder(bytes).decode().unwrap();
        rgb.iter().map(|&value| f32::from(value) / 255.0).collect()
    }

    fn chroma_sampling(bytes: &[u8]) -> u8 {
        let start = bytes
            .windows(2)
            .position(|pair| pair == [0xFF, 0xC0] || pair == [0xFF, 0xC2])
            .expect("no frame header");
        bytes[start + 11]
    }

    #[test]
    fn round_trips_close_to_the_source() {
        for subsampling in [ChromaSubsampling::Yuv444, ChromaSubsampling::Yuv420] {
            let (bytes, source) = encoded(OutputSpace::Srgb, &settings(95, subsampling));
            let quality = psnr(&source, &decoded(&bytes));
            assert!(
                quality > 38.0,
                "{subsampling:?} reached only {quality:.1} dB"
            );
        }
    }

    #[test]
    fn smallest_effort_also_round_trips_and_is_smaller() {
        let fast = JpegSettings {
            quality: 85,
            ..settings(85, ChromaSubsampling::Yuv444)
        };
        let smallest = JpegSettings {
            effort: JpegEffort::Smallest,
            ..fast
        };
        let (fast_bytes, source) = encoded(OutputSpace::Srgb, &fast);
        let (small_bytes, _) = encoded(OutputSpace::Srgb, &smallest);
        assert!(small_bytes.len() < fast_bytes.len());
        assert!(psnr(&source, &decoded(&small_bytes)) > 33.0);
    }

    #[test]
    fn subsampling_lands_in_the_frame_header() {
        let (full, _) = encoded(OutputSpace::Srgb, &settings(90, ChromaSubsampling::Yuv444));
        let (half, _) = encoded(OutputSpace::Srgb, &settings(90, ChromaSubsampling::Yuv420));
        assert_eq!(chroma_sampling(&full), 0x11);
        assert_eq!(chroma_sampling(&half), 0x22);
        assert!(half.len() < full.len());
    }

    #[test]
    fn quality_changes_size_and_accuracy() {
        let (low, source) = encoded(OutputSpace::Srgb, &settings(30, ChromaSubsampling::Yuv444));
        let (high, _) = encoded(OutputSpace::Srgb, &settings(98, ChromaSubsampling::Yuv444));
        assert!(low.len() < high.len());
        assert!(psnr(&source, &decoded(&low)) < psnr(&source, &decoded(&high)));
    }

    #[test]
    fn the_extremes_of_quality_work() {
        for quality in [0, 1, 100] {
            let (bytes, _) = encoded(
                OutputSpace::Srgb,
                &settings(quality, ChromaSubsampling::Yuv444),
            );
            assert!(!decoded(&bytes).is_empty());
        }
    }

    #[test]
    fn embeds_the_exact_icc_profile_for_every_space() {
        for space in OutputSpace::ALL {
            let (bytes, _) = encoded(space, &settings(80, ChromaSubsampling::Yuv444));
            let mut decoder = decoder(&bytes);
            decoder.decode_headers().unwrap();
            let embedded = decoder.icc_profile().expect("no ICC profile");
            assert_eq!(embedded, icc_profile(space).unwrap(), "{space:?}");
            assert!(lcms2::Profile::new_icc(&embedded).is_ok());
        }
    }

    #[test]
    fn icc_chunks_are_numbered_from_one() {
        let (bytes, _) = encoded(OutputSpace::Srgb, &settings(80, ChromaSubsampling::Yuv444));
        let start = bytes
            .windows(12)
            .position(|window| window == b"ICC_PROFILE\0")
            .unwrap();
        assert_eq!(&bytes[start + 12..start + 14], [1, 1]);
    }

    #[test]
    fn embeds_exif_and_xmp() {
        let (bytes, _) = encoded(OutputSpace::Srgb, &settings(80, ChromaSubsampling::Yuv444));
        let fields = exif::Reader::new()
            .read_from_container(&mut Cursor::new(&bytes))
            .unwrap();
        let model = fields
            .get_field(exif::Tag::Model, exif::In::PRIMARY)
            .unwrap();
        assert_eq!(model.display_value().to_string(), "\"ILCE-7RM5\"");

        let mut decoder = decoder(&bytes);
        decoder.decode_headers().unwrap();
        let xmp = decoder.xmp().expect("no XMP");
        assert!(String::from_utf8_lossy(xmp).contains("© 2026 Jane Doe"));
    }

    #[test]
    fn writes_no_metadata_segments_when_there_is_none() {
        let image = test_image(16, 16, OutputSpace::Srgb);
        let icc = icc_profile(OutputSpace::Srgb).unwrap();
        let metadata = Embedded::default();
        let input = Input {
            image: &image,
            icc: &icc,
            metadata: &metadata,
        };
        let mut bytes = Vec::new();
        encode(&input, &settings(80, ChromaSubsampling::Yuv444), &mut bytes).unwrap();
        assert!(!bytes.windows(4).any(|window| window == b"Exif"));
        assert!(!bytes.windows(9).any(|window| window == b"ns.adobe."));
    }

    #[test]
    fn oversized_metadata_is_an_error_rather_than_a_corrupt_file() {
        let image = test_image(16, 16, OutputSpace::Srgb);
        let icc = icc_profile(OutputSpace::Srgb).unwrap();
        let metadata = Embedded {
            exif: None,
            xmp: Some("x".repeat(70_000)),
        };
        let input = Input {
            image: &image,
            icc: &icc,
            metadata: &metadata,
        };
        let error = encode(
            &input,
            &settings(80, ChromaSubsampling::Yuv444),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::MetadataTooLarge { kind: "XMP", .. }),
            "{error}"
        );
    }

    #[test]
    fn dimensions_past_the_format_limit_are_an_error_rather_than_a_crash() {
        let image = Image::new(70_000, 1, OutputSpace::Srgb, vec![0.5; 210_000]).unwrap();
        let icc = icc_profile(OutputSpace::Srgb).unwrap();
        let metadata = Embedded::default();
        let input = Input {
            image: &image,
            icc: &icc,
            metadata: &metadata,
        };
        let result = encode(
            &input,
            &settings(80, ChromaSubsampling::Yuv444),
            &mut Vec::new(),
        );
        assert!(
            matches!(result, Err(Error::Encode { format: "JPEG", .. })),
            "{result:?}"
        );
    }

    #[test]
    fn a_full_disk_is_an_error_rather_than_a_crash() {
        struct FullDisk;
        impl Write for FullDisk {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::StorageFull.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let image = test_image(256, 256, OutputSpace::Srgb);
        let icc = icc_profile(OutputSpace::Srgb).unwrap();
        let metadata = Embedded::default();
        let input = Input {
            image: &image,
            icc: &icc,
            metadata: &metadata,
        };
        let result = encode(
            &input,
            &settings(100, ChromaSubsampling::Yuv444),
            &mut FullDisk,
        );
        assert!(
            matches!(result, Err(Error::Encode { format: "JPEG", .. })),
            "{result:?}"
        );
    }
}
