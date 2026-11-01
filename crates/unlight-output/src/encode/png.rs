use super::{Input, failure};
use crate::Result;
use crate::image::{quantize_u8, quantize_u16_bytes};
use crate::settings::{BitDepth, PngSettings};
use png::{ColorType, Compression, Encoder, Info};
use std::borrow::Cow;
use std::io::Write;

const FORMAT: &str = "PNG";
const XMP_KEYWORD: &str = "XML:com.adobe.xmp";

pub(super) fn encode(input: &Input<'_>, settings: &PngSettings, out: &mut dyn Write) -> Result<()> {
    let error = failure(FORMAT);
    let (width, height) = (input.image.width(), input.image.height());

    let mut info = Info::with_size(width, height);
    info.color_type = ColorType::Rgb;
    info.icc_profile = Some(Cow::Borrowed(input.icc));
    info.exif_metadata = input.metadata.exif_bytes().map(Cow::Owned);
    let samples = match settings.depth {
        BitDepth::Eight => {
            info.bit_depth = png::BitDepth::Eight;
            quantize_u8(input.image.pixels())
        }
        BitDepth::Sixteen => {
            info.bit_depth = png::BitDepth::Sixteen;
            quantize_u16_bytes(input.image.pixels(), u16::to_be_bytes)
        }
    };

    let mut encoder = Encoder::with_info(out, info).map_err(|e| error(&e))?;
    encoder.set_compression(Compression::Fast);
    if let Some(xmp) = &input.metadata.xmp {
        encoder
            .add_itxt_chunk(XMP_KEYWORD.to_owned(), xmp.clone())
            .map_err(|e| error(&e))?;
    }
    let mut writer = encoder.write_header().map_err(|e| error(&e))?;
    writer.write_image_data(&samples).map_err(|e| error(&e))?;
    writer.finish().map_err(|e| error(&e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::testing::{all_u16_codes, embedded, test_image};
    use crate::icc::icc_profile;
    use crate::image::{Image, quantize_u16};
    use crate::metadata::Embedded;
    use std::io::Cursor;
    use unlight_color::output::OutputSpace;

    fn encoded(image: &Image<'_>, depth: BitDepth, metadata: &Embedded) -> Vec<u8> {
        let icc = icc_profile(image.space()).unwrap();
        let input = Input {
            image,
            icc: &icc,
            metadata,
        };
        let mut bytes = Vec::new();
        encode(&input, &PngSettings { depth }, &mut bytes).unwrap();
        bytes
    }

    fn reader(bytes: &[u8]) -> png::Reader<Cursor<&[u8]>> {
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::IDENTITY);
        decoder.read_info().unwrap()
    }

    fn decoded(bytes: &[u8]) -> (png::OutputInfo, Vec<u8>) {
        let mut reader = reader(bytes);
        let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buffer).unwrap();
        buffer.truncate(info.buffer_size());
        (info, buffer)
    }

    #[test]
    fn eight_bit_decodes_to_the_exact_samples() {
        let image = test_image(97, 64, OutputSpace::Srgb);
        let (info, samples) = decoded(&encoded(&image, BitDepth::Eight, &Embedded::default()));
        assert_eq!((info.width, info.height), (97, 64));
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        assert_eq!(info.color_type, ColorType::Rgb);
        assert_eq!(samples, quantize_u8(image.pixels()));
    }

    #[test]
    fn every_16_bit_code_survives() {
        let image = all_u16_codes(OutputSpace::DisplayP3);
        let (info, samples) = decoded(&encoded(&image, BitDepth::Sixteen, &Embedded::default()));
        assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
        let (pairs, _) = samples.as_chunks::<2>();
        let codes: Vec<u16> = pairs.iter().map(|pair| u16::from_be_bytes(*pair)).collect();
        assert_eq!(codes, quantize_u16(image.pixels(), u16::MAX));
    }

    #[test]
    fn embeds_the_exact_icc_profile() {
        for space in OutputSpace::ALL {
            let image = test_image(16, 16, space);
            let bytes = encoded(&image, BitDepth::Eight, &Embedded::default());
            let reader = reader(&bytes);
            let profile = reader.info().icc_profile.as_deref().expect("no iCCP chunk");
            assert_eq!(profile, icc_profile(space).unwrap());
            assert!(lcms2::Profile::new_icc(profile).is_ok());
        }
    }

    #[test]
    fn embeds_exif_and_xmp() {
        let image = test_image(16, 16, OutputSpace::Srgb);
        let bytes = encoded(&image, BitDepth::Eight, &embedded(&image));

        let fields = exif::Reader::new()
            .read_from_container(&mut Cursor::new(&bytes))
            .unwrap();
        let iso = fields
            .get_field(exif::Tag::PhotographicSensitivity, exif::In::PRIMARY)
            .unwrap();
        assert_eq!(iso.value.get_uint(0), Some(400));

        let mut reader = reader(&bytes);
        let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buffer).unwrap();
        reader.finish().unwrap();
        let chunk = reader
            .info()
            .utf8_text
            .iter()
            .find(|chunk| chunk.keyword == XMP_KEYWORD)
            .unwrap();
        assert!(chunk.get_text().unwrap().contains("© 2026 Jane Doe"));
        assert!(
            !chunk.compressed,
            "XMP readers scan for uncompressed packets"
        );
    }

    #[test]
    fn single_pixel_images_work() {
        let image = test_image(1, 1, OutputSpace::Srgb);
        for depth in [BitDepth::Eight, BitDepth::Sixteen] {
            let (info, _) = decoded(&encoded(&image, depth, &Embedded::default()));
            assert_eq!((info.width, info.height), (1, 1));
        }
    }
}
