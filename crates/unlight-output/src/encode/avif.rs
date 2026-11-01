use super::{Input, failure};
use crate::image::{quantize_u8, quantize_u16};
use crate::settings::{AvifDepth, AvifSettings};
use crate::{Error, Result, ffi};
use std::ffi::CStr;
use std::io::Write;
use std::ptr::NonNull;
use unlight_color::output::OutputSpace;

const FORMAT: &str = "AVIF";

fn step_failed(step: &str, result: ffi::avifResult, detail: &str) -> Error {
    // SAFETY: avifResultToString returns a static string for every value.
    let name = unsafe { CStr::from_ptr(ffi::avifResultToString(result)) };
    failure(FORMAT)(&format!("{step}: {} {detail}", name.to_string_lossy()))
}

fn check(result: ffi::avifResult, step: &str) -> Result<()> {
    if result == ffi::AVIF_RESULT_OK {
        Ok(())
    } else {
        Err(step_failed(step, result, ""))
    }
}

struct AvifImage(NonNull<ffi::avifImage>);

impl Drop for AvifImage {
    fn drop(&mut self) {
        // SAFETY: the pointer came from avifImageCreate and is destroyed once.
        unsafe { ffi::avifImageDestroy(self.0.as_ptr()) };
    }
}

/// `(colour primaries, transfer characteristics)` as CICP code points. Only sRGB and
/// Display P3 have them, so the other two rely on the ICC profile alone.
fn cicp(space: OutputSpace) -> (u16, u16) {
    match space {
        OutputSpace::Srgb => (
            ffi::AVIF_COLOR_PRIMARIES_BT709 as u16,
            ffi::AVIF_TRANSFER_CHARACTERISTICS_SRGB as u16,
        ),
        OutputSpace::DisplayP3 => (
            ffi::AVIF_COLOR_PRIMARIES_SMPTE432 as u16,
            ffi::AVIF_TRANSFER_CHARACTERISTICS_SRGB as u16,
        ),
        OutputSpace::AdobeRgb | OutputSpace::ProPhoto => (
            ffi::AVIF_COLOR_PRIMARIES_UNSPECIFIED as u16,
            ffi::AVIF_TRANSFER_CHARACTERISTICS_UNSPECIFIED as u16,
        ),
    }
}

impl AvifImage {
    fn new(input: &Input<'_>, depth: AvifDepth) -> Result<Self> {
        let image = input.image;
        // SAFETY: plain values; the result is checked below.
        let raw = unsafe {
            ffi::avifImageCreate(
                image.width(),
                image.height(),
                depth.bits(),
                ffi::AVIF_PIXEL_FORMAT_YUV444,
            )
        };
        let raw = NonNull::new(raw).ok_or_else(|| failure(FORMAT)(&"could not create an image"))?;
        let (primaries, transfer) = cicp(image.space());
        // SAFETY: `raw` is a valid image we own exclusively until it's wrapped.
        unsafe {
            let target = raw.as_ptr();
            (*target).yuvRange = ffi::AVIF_RANGE_FULL;
            (*target).colorPrimaries = primaries;
            (*target).transferCharacteristics = transfer;
            (*target).matrixCoefficients = ffi::AVIF_MATRIX_COEFFICIENTS_BT601 as u16;
        }
        Ok(Self(raw))
    }

    fn ptr(&self) -> *mut ffi::avifImage {
        self.0.as_ptr()
    }

    fn attach_metadata(&self, input: &Input<'_>) -> Result<()> {
        let image = self.ptr();
        // SAFETY: libavif copies each buffer during the call.
        unsafe {
            check(
                ffi::avifImageSetProfileICC(image, input.icc.as_ptr(), input.icc.len()),
                "setting ICC",
            )?;
            if let Some(exif) = input.metadata.exif_bytes() {
                check(
                    ffi::avifImageSetMetadataExif(image, exif.as_ptr(), exif.len()),
                    "setting EXIF",
                )?;
            }
            if let Some(xmp) = &input.metadata.xmp {
                check(
                    ffi::avifImageSetMetadataXMP(image, xmp.as_ptr(), xmp.len()),
                    "setting XMP",
                )?;
            }
        }
        Ok(())
    }

    fn convert_from_rgb(&self, pixels: &[f32], depth: AvifDepth, width: u32) -> Result<()> {
        let bits = depth.bits();
        let eight_bit;
        let high_bit;
        let (samples, bytes_per_sample): (*const u8, u32) = if depth == AvifDepth::Eight {
            eight_bit = quantize_u8(pixels);
            (eight_bit.as_ptr(), 1)
        } else {
            high_bit = quantize_u16(pixels, ((1u32 << bits) - 1) as u16);
            (high_bit.as_ptr().cast(), 2)
        };

        let mut rgb = ffi::avifRGBImage::default();
        // SAFETY: `rgb` is a valid struct, and libavif only reads from `samples`, which
        // outlives the conversion.
        unsafe {
            ffi::avifRGBImageSetDefaults(&mut rgb, self.ptr());
            rgb.format = ffi::AVIF_RGB_FORMAT_RGB;
            rgb.depth = bits;
            rgb.pixels = samples.cast_mut();
            rgb.rowBytes = width * 3 * bytes_per_sample;
            check(
                ffi::avifImageRGBToYUV(self.ptr(), &rgb),
                "converting to YUV",
            )
        }
    }
}

struct Output(ffi::avifRWData);

impl Drop for Output {
    fn drop(&mut self) {
        // SAFETY: the buffer was allocated by libavif, or is empty.
        unsafe { ffi::avifRWDataFree(&mut self.0) };
    }
}

struct AvifEncoder(NonNull<ffi::avifEncoder>);

impl Drop for AvifEncoder {
    fn drop(&mut self) {
        // SAFETY: the pointer came from avifEncoderCreate and is destroyed once.
        unsafe { ffi::avifEncoderDestroy(self.0.as_ptr()) };
    }
}

impl AvifEncoder {
    fn new(settings: &AvifSettings) -> Result<Self> {
        // SAFETY: the result is checked below.
        let raw = NonNull::new(unsafe { ffi::avifEncoderCreate() })
            .ok_or_else(|| failure(FORMAT)(&"could not create an encoder"))?;
        // SAFETY: `raw` is a valid encoder we own exclusively until it's wrapped.
        unsafe {
            let encoder = raw.as_ptr();
            (*encoder).maxThreads = rayon::current_num_threads() as i32;
            (*encoder).speed = i32::from(settings.speed);
            (*encoder).quality = i32::from(settings.quality);
            (*encoder).autoTiling = ffi::AVIF_TRUE as i32;
        }
        Ok(Self(raw))
    }

    fn write(&self, image: &AvifImage) -> Result<Output> {
        let mut output = Output(ffi::avifRWData {
            data: std::ptr::null_mut(),
            size: 0,
        });
        // SAFETY: both are valid, and `output` owns whatever libavif allocates.
        let result = unsafe { ffi::avifEncoderWrite(self.0.as_ptr(), image.ptr(), &mut output.0) };
        if result == ffi::AVIF_RESULT_OK {
            return Ok(output);
        }
        // SAFETY: `diag.error` is a NUL-terminated buffer owned by the encoder.
        let detail = unsafe { CStr::from_ptr((*self.0.as_ptr()).diag.error.as_ptr()) };
        Err(step_failed("encoding", result, &detail.to_string_lossy()))
    }
}

pub(super) fn encode(
    input: &Input<'_>,
    settings: &AvifSettings,
    out: &mut dyn Write,
) -> Result<()> {
    let image = AvifImage::new(input, settings.depth)?;
    image.attach_metadata(input)?;
    image.convert_from_rgb(input.image.pixels(), settings.depth, input.image.width())?;
    let output = AvifEncoder::new(settings)?.write(&image)?;
    // SAFETY: libavif reported success, so `data` points at `size` initialised bytes.
    let bytes = unsafe { std::slice::from_raw_parts(output.0.data, output.0.size) };
    out.write_all(bytes)
        .map_err(|error| failure(FORMAT)(&error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Image;
    use crate::encode::testing::{embedded, psnr, test_image};
    use crate::icc::icc_profile;
    use crate::metadata::Embedded;

    struct Decoded {
        depth: u32,
        pixels: Vec<f32>,
        icc: Vec<u8>,
        exif: Vec<u8>,
        xmp: Vec<u8>,
        primaries: u16,
        transfer: u16,
    }

    fn encoded(image: &Image<'_>, settings: &AvifSettings, metadata: &Embedded) -> Vec<u8> {
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

    fn copy_of(data: ffi::avifRWData) -> Vec<u8> {
        if data.size == 0 {
            return Vec::new();
        }
        // SAFETY: libavif keeps `size` bytes alive at `data` until the image is destroyed.
        unsafe { std::slice::from_raw_parts(data.data, data.size) }.to_vec()
    }

    fn decoded(bytes: &[u8]) -> Decoded {
        // SAFETY: test-only decoding through libavif, with every result asserted.
        unsafe {
            let decoder = ffi::avifDecoderCreate();
            let image = ffi::avifImageCreateEmpty();
            let result = ffi::avifDecoderReadMemory(decoder, image, bytes.as_ptr(), bytes.len());
            assert_eq!(
                result,
                ffi::AVIF_RESULT_OK,
                "libavif could not read the file"
            );

            let mut rgb = ffi::avifRGBImage::default();
            ffi::avifRGBImageSetDefaults(&mut rgb, image);
            rgb.format = ffi::AVIF_RGB_FORMAT_RGB;
            rgb.depth = 16;
            assert_eq!(
                ffi::avifRGBImageAllocatePixels(&mut rgb),
                ffi::AVIF_RESULT_OK
            );
            assert_eq!(ffi::avifImageYUVToRGB(image, &mut rgb), ffi::AVIF_RESULT_OK);
            let (width, height) = ((*image).width as usize, (*image).height as usize);
            let mut pixels = Vec::with_capacity(width * height * 3);
            for y in 0..height {
                let row = rgb.pixels.add(y * rgb.rowBytes as usize).cast::<u16>();
                let row = std::slice::from_raw_parts(row, width * 3);
                pixels.extend(row.iter().map(|&value| f32::from(value) / 65535.0));
            }
            let output = Decoded {
                depth: (*image).depth,
                pixels,
                icc: copy_of((*image).icc),
                exif: copy_of((*image).exif),
                xmp: copy_of((*image).xmp),
                primaries: (*image).colorPrimaries,
                transfer: (*image).transferCharacteristics,
            };
            ffi::avifRGBImageFreePixels(&mut rgb);
            ffi::avifImageDestroy(image);
            ffi::avifDecoderDestroy(decoder);
            output
        }
    }

    fn settings(quality: u8, depth: AvifDepth) -> AvifSettings {
        AvifSettings {
            quality,
            depth,
            speed: 10,
        }
    }

    #[test]
    fn every_depth_round_trips() {
        let image = test_image(128, 96, OutputSpace::Srgb);
        for depth in [AvifDepth::Eight, AvifDepth::Ten, AvifDepth::Twelve] {
            let result = decoded(&encoded(&image, &settings(90, depth), &Embedded::default()));
            assert_eq!(result.depth, depth.bits());
            let quality = psnr(image.pixels(), &result.pixels);
            assert!(quality > 34.0, "{depth:?} reached only {quality:.1} dB");
        }
    }

    #[test]
    fn high_bit_depths_keep_more_precision_on_a_shallow_ramp() {
        let (width, height) = (512, 64);
        let pixels: Vec<f32> = (0..height)
            .flat_map(|_| (0..width).flat_map(|x| [0.4 + x as f32 / (width - 1) as f32 * 0.02; 3]))
            .collect();
        let image = Image::new(width, height, OutputSpace::Srgb, pixels).unwrap();
        let levels = |depth| {
            let result = decoded(&encoded(
                &image,
                &settings(100, depth),
                &Embedded::default(),
            ));
            let row = &result.pixels[..width as usize * 3];
            row.as_chunks::<3>()
                .0
                .iter()
                .map(|pixel| (pixel[1] * 65535.0).round() as u32)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        };
        let (eight, twelve) = (levels(AvifDepth::Eight), levels(AvifDepth::Twelve));
        assert!(eight <= 8, "{eight} levels at 8 bits");
        assert!(
            twelve > 3 * eight,
            "{twelve} levels at 12 bits vs {eight} at 8"
        );
    }

    #[test]
    fn quality_changes_size_and_accuracy() {
        let image = test_image(256, 192, OutputSpace::Srgb);
        let low = encoded(&image, &settings(20, AvifDepth::Ten), &Embedded::default());
        let high = encoded(&image, &settings(95, AvifDepth::Ten), &Embedded::default());
        assert!(low.len() < high.len());
        let low_quality = psnr(image.pixels(), &decoded(&low).pixels);
        let high_quality = psnr(image.pixels(), &decoded(&high).pixels);
        assert!(low_quality < high_quality);
    }

    #[test]
    fn embeds_the_exact_icc_profile_for_every_space() {
        for space in OutputSpace::ALL {
            let image = test_image(32, 32, space);
            let result = decoded(&encoded(
                &image,
                &settings(60, AvifDepth::Ten),
                &Embedded::default(),
            ));
            assert_eq!(result.icc, icc_profile(space).unwrap(), "{space:?}");
            assert!(lcms2::Profile::new_icc(&result.icc).is_ok());
        }
    }

    #[test]
    fn signals_cicp_where_it_exists() {
        for (space, expected) in [
            (OutputSpace::Srgb, (1, 13)),
            (OutputSpace::DisplayP3, (12, 13)),
            (OutputSpace::AdobeRgb, (2, 2)),
            (OutputSpace::ProPhoto, (2, 2)),
        ] {
            let image = test_image(32, 32, space);
            let result = decoded(&encoded(
                &image,
                &settings(60, AvifDepth::Ten),
                &Embedded::default(),
            ));
            assert_eq!((result.primaries, result.transfer), expected, "{space:?}");
        }
    }

    #[test]
    fn embeds_exif_and_xmp() {
        let image = test_image(32, 32, OutputSpace::Srgb);
        let result = decoded(&encoded(
            &image,
            &settings(60, AvifDepth::Eight),
            &embedded(&image),
        ));
        let fields = exif::Reader::new().read_raw(result.exif).unwrap();
        let iso = fields
            .get_field(exif::Tag::PhotographicSensitivity, exif::In::PRIMARY)
            .unwrap();
        assert_eq!(iso.value.get_uint(0), Some(400));
        assert!(String::from_utf8_lossy(&result.xmp).contains("© 2026 Jane Doe"));
    }

    #[test]
    fn no_metadata_means_no_exif_item() {
        let image = test_image(32, 32, OutputSpace::Srgb);
        let result = decoded(&encoded(
            &image,
            &settings(60, AvifDepth::Eight),
            &Embedded::default(),
        ));
        assert!(result.exif.is_empty() && result.xmp.is_empty());
    }

    #[test]
    fn small_images_work() {
        for (width, height) in [(1, 1), (2, 3), (17, 1)] {
            let image = test_image(width, height, OutputSpace::Srgb);
            let result = decoded(&encoded(
                &image,
                &settings(80, AvifDepth::Ten),
                &Embedded::default(),
            ));
            assert_eq!(result.pixels.len(), width as usize * height as usize * 3);
        }
    }

    #[test]
    fn step_failures_name_the_libavif_error() {
        let error = step_failed("encoding", ffi::AVIF_RESULT_INVALID_ARGUMENT, "");
        assert!(error.to_string().contains("nvalid"), "{error}");
    }
}
