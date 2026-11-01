use super::{Input, failure};
use crate::Result;
use crate::exif::{Directory, TAG_ORIENTATION, TiffTree, Value, file_header};
use crate::image::{quantize_u8, quantize_u16_bytes};
use crate::settings::{BitDepth, TiffCompression, TiffSettings};
use flate2::{Compression, write::ZlibEncoder};
use rayon::prelude::*;
use std::borrow::Cow;
use std::io::Write;

const FORMAT: &str = "TIFF";
const ROWS_PER_STRIP: usize = 64;
const SAMPLES_PER_PIXEL: usize = 3;
const FILE_HEADER_LEN: u64 = 8;
const DEFLATE_LEVEL: u32 = 6;
/// Room for tags and metadata after the strips. Directories hold a few megabytes at most.
const MAX_DIRECTORY_BYTES: u64 = 1 << 24;

const TAG_IMAGE_WIDTH: u16 = 256;
const TAG_IMAGE_LENGTH: u16 = 257;
const TAG_BITS_PER_SAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_SAMPLES_PER_PIXEL: u16 = 277;
const TAG_ROWS_PER_STRIP: u16 = 278;
const TAG_STRIP_BYTE_COUNTS: u16 = 279;
const TAG_X_RESOLUTION: u16 = 282;
const TAG_Y_RESOLUTION: u16 = 283;
const TAG_PLANAR_CONFIGURATION: u16 = 284;
const TAG_RESOLUTION_UNIT: u16 = 296;
const TAG_PREDICTOR: u16 = 317;
const TAG_XMP: u16 = 700;
const TAG_ICC_PROFILE: u16 = 34675;

const COMPRESSION_NONE: u16 = 1;
const COMPRESSION_DEFLATE: u16 = 8;
const PHOTOMETRIC_RGB: u16 = 2;
const PLANAR_CHUNKY: u16 = 1;
const PREDICTOR_HORIZONTAL: u16 = 2;
const RESOLUTION_UNIT_NONE: u16 = 1;
const ORIENTATION_NORMAL: u16 = 1;

fn bits(depth: BitDepth) -> u16 {
    match depth {
        BitDepth::Eight => 8,
        BitDepth::Sixteen => 16,
    }
}

/// Differences between neighbours compress far better than the samples themselves.
fn apply_horizontal_predictor(strip: &mut [u8], row_bytes: usize, depth: BitDepth) {
    for row in strip.chunks_exact_mut(row_bytes) {
        match depth {
            BitDepth::Eight => {
                for i in (SAMPLES_PER_PIXEL..row.len()).rev() {
                    row[i] = row[i].wrapping_sub(row[i - SAMPLES_PER_PIXEL]);
                }
            }
            BitDepth::Sixteen => {
                let sample =
                    |row: &[u8], i: usize| u16::from_le_bytes([row[i * 2], row[i * 2 + 1]]);
                for i in (SAMPLES_PER_PIXEL..row.len() / 2).rev() {
                    let delta = sample(row, i).wrapping_sub(sample(row, i - SAMPLES_PER_PIXEL));
                    row[i * 2..i * 2 + 2].copy_from_slice(&delta.to_le_bytes());
                }
            }
        }
    }
}

fn deflate(strip: &[u8], row_bytes: usize, depth: BitDepth) -> std::io::Result<Vec<u8>> {
    let mut predicted = strip.to_vec();
    apply_horizontal_predictor(&mut predicted, row_bytes, depth);
    let mut encoder = ZlibEncoder::new(
        Vec::with_capacity(strip.len() / 2),
        Compression::new(DEFLATE_LEVEL),
    );
    encoder.write_all(&predicted)?;
    encoder.finish()
}

fn compress_strips<'a>(
    samples: &'a [u8],
    row_bytes: usize,
    settings: &TiffSettings,
) -> Result<Vec<Cow<'a, [u8]>>> {
    let strip_bytes = row_bytes * ROWS_PER_STRIP;
    match settings.compression {
        TiffCompression::None => Ok(samples.chunks(strip_bytes).map(Cow::Borrowed).collect()),
        TiffCompression::Deflate => samples
            .par_chunks(strip_bytes)
            .map(|strip| deflate(strip, row_bytes, settings.depth).map(Cow::Owned))
            .collect::<std::io::Result<_>>()
            .map_err(|error| failure(FORMAT)(&error)),
    }
}

fn structure_tags(directory: &mut Directory, input: &Input<'_>, settings: &TiffSettings) {
    let (width, height) = (input.image.width(), input.image.height());
    let (compression, predictor) = match settings.compression {
        TiffCompression::None => (COMPRESSION_NONE, None),
        TiffCompression::Deflate => (COMPRESSION_DEFLATE, Some(PREDICTOR_HORIZONTAL)),
    };
    directory.set(TAG_IMAGE_WIDTH, Value::Long(vec![width]));
    directory.set(TAG_IMAGE_LENGTH, Value::Long(vec![height]));
    directory.set(
        TAG_BITS_PER_SAMPLE,
        Value::Short(vec![bits(settings.depth); SAMPLES_PER_PIXEL]),
    );
    directory.set(TAG_COMPRESSION, Value::Short(vec![compression]));
    directory.set(TAG_PHOTOMETRIC, Value::Short(vec![PHOTOMETRIC_RGB]));
    directory.set(TAG_ORIENTATION, Value::Short(vec![ORIENTATION_NORMAL]));
    directory.set(
        TAG_SAMPLES_PER_PIXEL,
        Value::Short(vec![SAMPLES_PER_PIXEL as u16]),
    );
    directory.set(TAG_ROWS_PER_STRIP, Value::Long(vec![ROWS_PER_STRIP as u32]));
    directory.set(TAG_PLANAR_CONFIGURATION, Value::Short(vec![PLANAR_CHUNKY]));
    directory.set(TAG_X_RESOLUTION, Value::Rational(vec![[1, 1]]));
    directory.set(TAG_Y_RESOLUTION, Value::Rational(vec![[1, 1]]));
    directory.set(
        TAG_RESOLUTION_UNIT,
        Value::Short(vec![RESOLUTION_UNIT_NONE]),
    );
    if let Some(predictor) = predictor {
        directory.set(TAG_PREDICTOR, Value::Short(vec![predictor]));
    }
}

fn image_directories(
    input: &Input<'_>,
    settings: &TiffSettings,
    strips: &[Cow<'_, [u8]>],
) -> TiffTree {
    let mut tree = input.metadata.exif.clone().unwrap_or_default();
    structure_tags(&mut tree.main, input, settings);
    tree.main
        .set(TAG_ICC_PROFILE, Value::Undefined(input.icc.to_vec()));
    if let Some(xmp) = &input.metadata.xmp {
        tree.main
            .set(TAG_XMP, Value::Byte(xmp.clone().into_bytes()));
    }

    let mut offset = FILE_HEADER_LEN as u32;
    let mut offsets = Vec::with_capacity(strips.len());
    for strip in strips {
        offsets.push(offset);
        offset += strip.len() as u32;
    }
    let lengths = strips.iter().map(|strip| strip.len() as u32).collect();
    tree.main.set(TAG_STRIP_OFFSETS, Value::Long(offsets));
    tree.main.set(TAG_STRIP_BYTE_COUNTS, Value::Long(lengths));
    tree
}

/// Writes strips first and the directory last, which needs no seeking.
pub(super) fn encode(
    input: &Input<'_>,
    settings: &TiffSettings,
    out: &mut dyn Write,
) -> Result<()> {
    let pixels = input.image.pixels();
    let (samples, bytes_per_sample) = match settings.depth {
        BitDepth::Eight => (quantize_u8(pixels), 1),
        BitDepth::Sixteen => (quantize_u16_bytes(pixels, u16::to_le_bytes), 2),
    };
    let row_bytes = input.image.row_samples() * bytes_per_sample;
    let strips = compress_strips(&samples, row_bytes, settings)?;

    let data_len: u64 = strips.iter().map(|strip| strip.len() as u64).sum();
    let padding = data_len % 2;
    let directory_offset = FILE_HEADER_LEN + data_len + padding;
    if directory_offset + MAX_DIRECTORY_BYTES > u64::from(u32::MAX) {
        return Err(failure(FORMAT)(
            &"the image needs more than the 4 GiB classic TIFF allows",
        ));
    }
    let directory_offset = directory_offset as u32;
    let directory = image_directories(input, settings, &strips).encode(directory_offset);

    let write =
        |out: &mut dyn Write, bytes: &[u8]| out.write_all(bytes).map_err(|e| failure(FORMAT)(&e));
    write(out, &file_header(directory_offset))?;
    for strip in &strips {
        write(out, strip)?;
    }
    write(out, &[0; 1][..padding as usize])?;
    write(out, &directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use crate::encode::testing::{all_u16_codes, embedded, test_image};
    use crate::icc::icc_profile;
    use crate::image::{Image, quantize_u16};
    use crate::metadata::Embedded;
    use ::tiff::decoder::{Decoder, DecodingResult};
    use ::tiff::tags::{CompressionMethod, Tag};
    use std::io::Cursor;
    use unlight_color::output::OutputSpace;

    const DEPTHS: [BitDepth; 2] = [BitDepth::Eight, BitDepth::Sixteen];
    const COMPRESSIONS: [TiffCompression; 2] = [TiffCompression::None, TiffCompression::Deflate];

    fn encoded(image: &Image<'_>, settings: &TiffSettings, metadata: &Embedded) -> Vec<u8> {
        let icc = icc_profile(image.space()).unwrap();
        let input = Input {
            image,
            icc: &icc,
            metadata,
        };
        let mut bytes = Vec::new();
        encode(&input, settings, &mut bytes).unwrap();
        bytes
    }

    fn decoder(bytes: &[u8]) -> Decoder<Cursor<&[u8]>> {
        Decoder::new(Cursor::new(bytes)).unwrap()
    }

    fn assert_exact(image: &Image<'_>, settings: &TiffSettings) {
        let bytes = encoded(image, settings, &Embedded::default());
        let mut decoder = decoder(&bytes);
        assert_eq!(
            decoder.dimensions().unwrap(),
            (image.width(), image.height())
        );
        match (decoder.read_image().unwrap(), settings.depth) {
            (DecodingResult::U8(decoded), BitDepth::Eight) => {
                assert_eq!(decoded, quantize_u8(image.pixels()));
            }
            (DecodingResult::U16(decoded), BitDepth::Sixteen) => {
                assert_eq!(decoded, quantize_u16(image.pixels(), u16::MAX));
            }
            _ => panic!("decoder returned the wrong depth for {settings:?}"),
        }
    }

    #[test]
    fn every_combination_decodes_to_the_exact_samples() {
        let image = test_image(97, 211, OutputSpace::Srgb);
        for depth in DEPTHS {
            for compression in COMPRESSIONS {
                assert_exact(&image, &TiffSettings { depth, compression });
            }
        }
    }

    #[test]
    fn every_16_bit_code_survives() {
        let image = all_u16_codes(OutputSpace::ProPhoto);
        for compression in COMPRESSIONS {
            assert_exact(
                &image,
                &TiffSettings {
                    depth: BitDepth::Sixteen,
                    compression,
                },
            );
        }
    }

    #[test]
    fn small_images_work() {
        for (width, height) in [(1, 1), (2, 1), (1, 2), (3, 65)] {
            let image = test_image(width, height, OutputSpace::Srgb);
            for depth in DEPTHS {
                for compression in COMPRESSIONS {
                    assert_exact(&image, &TiffSettings { depth, compression });
                }
            }
        }
    }

    #[test]
    fn deflate_is_declared_and_saves_space() {
        let image = test_image(256, 256, OutputSpace::Srgb);
        let plain = TiffSettings {
            depth: BitDepth::Sixteen,
            compression: TiffCompression::None,
        };
        let deflated = TiffSettings {
            compression: TiffCompression::Deflate,
            ..plain
        };
        let none = encoded(&image, &plain, &Embedded::default());
        let small = encoded(&image, &deflated, &Embedded::default());
        assert!(
            small.len() < none.len() / 2,
            "{} vs {}",
            small.len(),
            none.len()
        );
        let method = decoder(&small).get_tag_u32(Tag::Compression).unwrap();
        assert_eq!(method as u16, CompressionMethod::Deflate.to_u16());
    }

    #[test]
    fn embeds_the_exact_icc_profile() {
        for space in OutputSpace::ALL {
            let image = test_image(16, 16, space);
            let settings = TiffSettings {
                depth: BitDepth::Eight,
                compression: TiffCompression::Deflate,
            };
            let bytes = encoded(&image, &settings, &Embedded::default());
            let profile = decoder(&bytes).get_tag_u8_vec(Tag::IccProfile).unwrap();
            assert_eq!(profile, icc_profile(space).unwrap());
            assert!(lcms2::Profile::new_icc(&profile).is_ok());
        }
    }

    #[test]
    fn embeds_exif_in_a_sub_directory_and_xmp_in_its_tag() {
        let image = test_image(16, 16, OutputSpace::AdobeRgb);
        let metadata = embedded(&image);
        let settings = TiffSettings {
            depth: BitDepth::Sixteen,
            compression: TiffCompression::None,
        };
        let bytes = encoded(&image, &settings, &metadata);

        let fields = exif::Reader::new()
            .read_from_container(&mut Cursor::new(&bytes))
            .unwrap();
        let iso = fields
            .get_field(exif::Tag::PhotographicSensitivity, exif::In::PRIMARY)
            .unwrap();
        assert_eq!(iso.value.get_uint(0), Some(400));
        let model = fields
            .get_field(exif::Tag::Model, exif::In::PRIMARY)
            .unwrap();
        assert_eq!(model.display_value().to_string(), "\"ILCE-7RM5\"");

        let xmp = decoder(&bytes)
            .get_tag_u8_vec(Tag::Unknown(TAG_XMP))
            .unwrap();
        assert!(String::from_utf8(xmp).unwrap().contains("© 2026 Jane Doe"));
    }

    #[test]
    fn orientation_is_normal_even_without_metadata() {
        let image = test_image(4, 4, OutputSpace::Srgb);
        let settings = TiffSettings {
            depth: BitDepth::Eight,
            compression: TiffCompression::None,
        };
        let bytes = encoded(&image, &settings, &Embedded::default());
        assert_eq!(decoder(&bytes).get_tag_u32(Tag::Orientation).unwrap(), 1);
    }

    #[test]
    fn predictor_round_trips_by_hand() {
        let original: Vec<u8> = (0..12).map(|i| i * 20).collect();
        let mut strip = original.clone();
        apply_horizontal_predictor(&mut strip, 12, BitDepth::Eight);
        assert_eq!(&strip[..3], &original[..3]);
        assert_eq!(&strip[3..6], &[60, 60, 60]);
    }

    #[test]
    fn a_failing_writer_is_reported() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::StorageFull.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let image = test_image(8, 8, OutputSpace::Srgb);
        let icc = icc_profile(OutputSpace::Srgb).unwrap();
        let metadata = Embedded::default();
        let input = Input {
            image: &image,
            icc: &icc,
            metadata: &metadata,
        };
        let settings = TiffSettings {
            depth: BitDepth::Eight,
            compression: TiffCompression::None,
        };
        assert!(matches!(
            encode(&input, &settings, &mut Broken),
            Err(Error::Encode { .. })
        ));
    }
}
