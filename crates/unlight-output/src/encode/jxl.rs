use super::{Input, failure};
use crate::image::{quantize_u8, quantize_u16};
use crate::metadata::Embedded;
use crate::settings::{BitDepth, JxlMode, JxlSettings};
use crate::{Error, Image, Result, ffi};
use bytemuck::NoUninit;
use rayon::prelude::*;
use std::ffi::{c_char, c_void};
use std::io::Write;
use std::ptr::{NonNull, null};
use unlight_color::output::{OutputSpace, TransferFunction};

const FORMAT: &str = "JPEG XL";
const INITIAL_OUTPUT_BYTES: usize = 1 << 20;
const EXIF_BOX: &[u8; 4] = b"Exif";
const XMP_BOX: &[u8; 4] = b"xml ";
/// Exif boxes start with the offset of the TIFF header within the payload.
const EXIF_TIFF_OFFSET: [u8; 4] = [0; 4];
const LOSSY_BITS_PER_SAMPLE: u32 = 16;

fn step_failed(step: &str, code: ffi::JxlEncoderError) -> Error {
    failure(FORMAT)(&format!("{step} failed with libjxl error {code}"))
}

struct Runner(NonNull<c_void>);

impl Runner {
    fn new() -> Result<Self> {
        let threads = rayon::current_num_threads();
        // SAFETY: a null memory manager asks for libjxl's default allocator.
        let runner = unsafe { ffi::JxlThreadParallelRunnerCreate(null(), threads) };
        NonNull::new(runner)
            .map(Self)
            .ok_or_else(|| failure(FORMAT)(&"could not start threads"))
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        // SAFETY: the pointer came from JxlThreadParallelRunnerCreate and is destroyed once.
        unsafe { ffi::JxlThreadParallelRunnerDestroy(self.0.as_ptr()) };
    }
}

struct Encoder {
    raw: NonNull<ffi::JxlEncoder>,
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: the pointer came from JxlEncoderCreate and is destroyed once.
        unsafe { ffi::JxlEncoderDestroy(self.raw.as_ptr()) };
    }
}

impl Encoder {
    fn new() -> Result<Self> {
        // SAFETY: a null memory manager asks for libjxl's default allocator.
        let raw = unsafe { ffi::JxlEncoderCreate(null()) };
        let raw =
            NonNull::new(raw).ok_or_else(|| failure(FORMAT)(&"could not create an encoder"))?;
        Ok(Self { raw })
    }

    fn ptr(&self) -> *mut ffi::JxlEncoder {
        self.raw.as_ptr()
    }

    fn check(&self, status: ffi::JxlEncoderStatus, step: &str) -> Result<()> {
        if status == ffi::JXL_ENC_SUCCESS {
            return Ok(());
        }
        // SAFETY: the encoder is alive for as long as `self`.
        Err(step_failed(step, unsafe {
            ffi::JxlEncoderGetError(self.ptr())
        }))
    }

    fn use_runner(&self, runner: &Runner) -> Result<()> {
        // SAFETY: both are alive until after the last encoder call in `encode`.
        let status = unsafe {
            ffi::JxlEncoderSetParallelRunner(
                self.ptr(),
                Some(ffi::JxlThreadParallelRunner),
                runner.0.as_ptr(),
            )
        };
        self.check(status, "setting the thread runner")
    }

    fn set_basic_info(
        &self,
        image: &Image<'_>,
        bits_per_sample: u32,
        lossless: bool,
    ) -> Result<()> {
        let mut info = ffi::JxlBasicInfo::default();
        // SAFETY: `info` is a valid struct for the library to fill in.
        unsafe { ffi::JxlEncoderInitBasicInfo(&mut info) };
        info.xsize = image.width();
        info.ysize = image.height();
        info.bits_per_sample = bits_per_sample;
        info.num_color_channels = 3;
        info.uses_original_profile = i32::from(lossless);
        // SAFETY: libjxl copies `info` during the call.
        let status = unsafe { ffi::JxlEncoderSetBasicInfo(self.ptr(), &info) };
        self.check(status, "setting the image header")
    }

    fn set_color(&self, space: OutputSpace) -> Result<()> {
        let encoding = color_encoding(space);
        // SAFETY: libjxl copies `encoding` during the call.
        let status = unsafe { ffi::JxlEncoderSetColorEncoding(self.ptr(), &encoding) };
        self.check(status, "setting the colour encoding")
    }

    fn add_box(&self, kind: &[u8; 4], contents: &[u8]) -> Result<()> {
        // SAFETY: libjxl copies the contents, and `kind` has the four bytes it reads.
        let status = unsafe {
            ffi::JxlEncoderAddBox(
                self.ptr(),
                kind.as_ptr().cast::<c_char>(),
                contents.as_ptr(),
                contents.len(),
                ffi::JXL_FALSE as i32,
            )
        };
        self.check(status, "adding a metadata box")
    }

    fn add_metadata(&self, metadata: &Embedded) -> Result<()> {
        let exif = metadata.exif_bytes();
        if exif.is_none() && metadata.xmp.is_none() {
            return Ok(());
        }
        // SAFETY: the encoder is alive for as long as `self`.
        self.check(
            unsafe { ffi::JxlEncoderUseBoxes(self.ptr()) },
            "enabling metadata boxes",
        )?;
        if let Some(exif) = exif {
            self.add_box(EXIF_BOX, &[&EXIF_TIFF_OFFSET[..], &exif].concat())?;
        }
        if let Some(xmp) = &metadata.xmp {
            self.add_box(XMP_BOX, xmp.as_bytes())?;
        }
        // SAFETY: the encoder is alive for as long as `self`.
        unsafe { ffi::JxlEncoderCloseBoxes(self.ptr()) };
        Ok(())
    }

    fn frame_settings(&self, settings: &JxlSettings) -> Result<*mut ffi::JxlEncoderFrameSettings> {
        // SAFETY: the returned settings belong to the encoder and live as long as it does.
        let frame = unsafe { ffi::JxlEncoderFrameSettingsCreate(self.ptr(), null()) };
        if frame.is_null() {
            return Err(failure(FORMAT)(&"could not create frame settings"));
        }
        // SAFETY: `frame` was checked above.
        let status = unsafe {
            ffi::JxlEncoderFrameSettingsSetOption(
                frame,
                ffi::JXL_ENC_FRAME_SETTING_EFFORT,
                i64::from(settings.effort),
            )
        };
        self.check(status, "setting the effort")?;
        // SAFETY: `frame` was checked above.
        let status = match settings.mode {
            JxlMode::Lossless { .. } => unsafe {
                ffi::JxlEncoderSetFrameLossless(frame, ffi::JXL_TRUE as i32)
            },
            JxlMode::Distance(distance) => unsafe {
                ffi::JxlEncoderSetFrameDistance(frame, distance)
            },
            JxlMode::Quality(quality) => unsafe {
                let distance = ffi::JxlEncoderDistanceFromQuality(f32::from(quality));
                ffi::JxlEncoderSetFrameDistance(frame, distance)
            },
        };
        self.check(status, "setting the distance")?;
        Ok(frame)
    }

    fn add_frame<T: NoUninit>(
        &self,
        frame: *mut ffi::JxlEncoderFrameSettings,
        samples: &[T],
        data_type: ffi::JxlDataType,
    ) -> Result<()> {
        let format = ffi::JxlPixelFormat {
            num_channels: 3,
            data_type,
            endianness: ffi::JXL_NATIVE_ENDIAN,
            align: 0,
        };
        // SAFETY: the pointer and length describe `samples`, and libjxl copies them.
        let status = unsafe {
            ffi::JxlEncoderAddImageFrame(
                frame,
                &format,
                samples.as_ptr().cast::<c_void>(),
                size_of_val(samples),
            )
        };
        self.check(status, "adding the image")
    }

    fn finish(&self) -> Result<Vec<u8>> {
        // SAFETY: the encoder is alive for as long as `self`.
        unsafe { ffi::JxlEncoderCloseInput(self.ptr()) };
        let mut output = vec![0u8; INITIAL_OUTPUT_BYTES];
        let mut written = 0;
        loop {
            let mut available = output.len() - written;
            // SAFETY: `next` and `available` describe the unwritten tail of `output`.
            let mut next = unsafe { output.as_mut_ptr().add(written) };
            let status =
                unsafe { ffi::JxlEncoderProcessOutput(self.ptr(), &mut next, &mut available) };
            written = output.len() - available;
            match status {
                ffi::JXL_ENC_SUCCESS => {
                    output.truncate(written);
                    return Ok(output);
                }
                ffi::JXL_ENC_NEED_MORE_OUTPUT => output.resize(output.len() * 2, 0),
                other => self.check(other, "encoding")?,
            }
        }
    }
}

/// Colour goes in as signalling rather than ICC, because lossy JPEG XL stores XYB and
/// needs to know the real primaries and curve to get back to them.
fn color_encoding(space: OutputSpace) -> ffi::JxlColorEncoding {
    let primaries = space.primaries();
    let (white_point, primaries_id) = match space {
        OutputSpace::Srgb => (ffi::JXL_WHITE_POINT_D65, ffi::JXL_PRIMARIES_SRGB),
        OutputSpace::DisplayP3 => (ffi::JXL_WHITE_POINT_D65, ffi::JXL_PRIMARIES_P3),
        OutputSpace::AdobeRgb => (ffi::JXL_WHITE_POINT_D65, ffi::JXL_PRIMARIES_CUSTOM),
        OutputSpace::ProPhoto => (ffi::JXL_WHITE_POINT_CUSTOM, ffi::JXL_PRIMARIES_CUSTOM),
    };
    let (transfer_function, gamma) = match space.transfer() {
        TransferFunction::Srgb => (ffi::JXL_TRANSFER_FUNCTION_SRGB, 0.0),
        TransferFunction::Gamma(gamma) => (ffi::JXL_TRANSFER_FUNCTION_GAMMA, 1.0 / gamma),
    };
    ffi::JxlColorEncoding {
        color_space: ffi::JXL_COLOR_SPACE_RGB,
        white_point,
        white_point_xy: space.white_xy(),
        primaries: primaries_id,
        primaries_red_xy: primaries.red,
        primaries_green_xy: primaries.green,
        primaries_blue_xy: primaries.blue,
        transfer_function,
        gamma,
        rendering_intent: ffi::JXL_RENDERING_INTENT_RELATIVE,
    }
}

fn clipped(pixels: &[f32]) -> Vec<f32> {
    pixels
        .par_iter()
        .map(|value| {
            if value.is_nan() {
                0.0
            } else {
                value.clamp(0.0, 1.0)
            }
        })
        .collect()
}

pub(super) fn encode(input: &Input<'_>, settings: &JxlSettings, out: &mut dyn Write) -> Result<()> {
    let image = input.image;
    let (bits_per_sample, lossless) = match settings.mode {
        JxlMode::Lossless {
            depth: BitDepth::Eight,
        } => (8, true),
        JxlMode::Lossless {
            depth: BitDepth::Sixteen,
        } => (16, true),
        JxlMode::Distance(_) | JxlMode::Quality(_) => (LOSSY_BITS_PER_SAMPLE, false),
    };

    let runner = Runner::new()?;
    let encoder = Encoder::new()?;
    encoder.use_runner(&runner)?;
    encoder.set_basic_info(image, bits_per_sample, lossless)?;
    encoder.set_color(image.space())?;
    encoder.add_metadata(input.metadata)?;
    let frame = encoder.frame_settings(settings)?;
    match settings.mode {
        JxlMode::Lossless {
            depth: BitDepth::Eight,
        } => {
            encoder.add_frame(frame, &quantize_u8(image.pixels()), ffi::JXL_TYPE_UINT8)?;
        }
        JxlMode::Lossless {
            depth: BitDepth::Sixteen,
        } => {
            encoder.add_frame(
                frame,
                &quantize_u16(image.pixels(), u16::MAX),
                ffi::JXL_TYPE_UINT16,
            )?;
        }
        JxlMode::Distance(_) | JxlMode::Quality(_) => {
            encoder.add_frame(frame, &clipped(image.pixels()), ffi::JXL_TYPE_FLOAT)?;
        }
    }
    let bytes = encoder.finish()?;
    drop(encoder);
    drop(runner);
    out.write_all(&bytes)
        .map_err(|error| failure(FORMAT)(&error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::testing::{all_u16_codes, embedded, psnr, test_image};
    use crate::icc::icc_profile;
    use jxl_oxide::JxlImage;
    use std::io::Cursor;

    fn encoded(image: &Image<'_>, settings: &JxlSettings, metadata: &Embedded) -> Vec<u8> {
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

    fn lossless(depth: BitDepth) -> JxlSettings {
        JxlSettings {
            mode: JxlMode::Lossless { depth },
            effort: 3,
        }
    }

    fn decoded(bytes: &[u8]) -> (JxlImage, Vec<f32>) {
        let image = JxlImage::builder().read(Cursor::new(bytes)).unwrap();
        let render = image.render_frame(0).unwrap();
        let pixels = render.image_all_channels().buf().to_vec();
        (image, pixels)
    }

    /// Through libjxl, the reference decoder. jxl-oxide only renders XYB correctly into sRGB
    /// unless it's given a colour engine, so lossy checks in wide gamuts need this one.
    fn reference_decoded(bytes: &[u8]) -> Vec<f32> {
        let format = ffi::JxlPixelFormat {
            num_channels: 3,
            data_type: ffi::JXL_TYPE_FLOAT,
            endianness: ffi::JXL_NATIVE_ENDIAN,
            align: 0,
        };
        let mut pixels = Vec::new();
        // SAFETY: test-only decoding; `bytes` and `pixels` outlive the decoder.
        unsafe {
            let decoder = ffi::JxlDecoderCreate(null());
            let events = ffi::JXL_DEC_FULL_IMAGE as i32;
            assert_eq!(
                ffi::JxlDecoderSubscribeEvents(decoder, events),
                ffi::JXL_DEC_SUCCESS
            );
            assert_eq!(
                ffi::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len()),
                ffi::JXL_DEC_SUCCESS
            );
            ffi::JxlDecoderCloseInput(decoder);
            loop {
                match ffi::JxlDecoderProcessInput(decoder) {
                    ffi::JXL_DEC_NEED_IMAGE_OUT_BUFFER => {
                        let mut size = 0;
                        ffi::JxlDecoderImageOutBufferSize(decoder, &format, &mut size);
                        pixels.resize(size / size_of::<f32>(), 0.0f32);
                        let target = pixels.as_mut_ptr().cast::<c_void>();
                        let status =
                            ffi::JxlDecoderSetImageOutBuffer(decoder, &format, target, size);
                        assert_eq!(status, ffi::JXL_DEC_SUCCESS);
                    }
                    ffi::JXL_DEC_FULL_IMAGE => {}
                    ffi::JXL_DEC_SUCCESS => break,
                    other => panic!("libjxl could not decode the file: status {other}"),
                }
            }
            ffi::JxlDecoderDestroy(decoder);
        }
        pixels
    }

    /// Lossy JPEG XL spends its bits where the eye looks, so the noisy band along the bottom
    /// of the test image says little. The smooth part shows whether colour came back right.
    fn smooth_psnr(image: &Image<'_>, decoded: &[f32]) -> f64 {
        let smooth_rows = image.height() as usize * 7 / 10;
        let samples = smooth_rows * image.row_samples();
        psnr(&image.pixels()[..samples], &decoded[..samples])
    }

    fn boxes(bytes: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        let mut found = Vec::new();
        let mut position = 0;
        while position + 8 <= bytes.len() {
            let size =
                u32::from_be_bytes(bytes[position..position + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = bytes[position + 4..position + 8].try_into().unwrap();
            let end = if size == 0 {
                bytes.len()
            } else {
                position + size
            };
            found.push((kind, bytes[position + 8..end].to_vec()));
            position = end;
        }
        found
    }

    #[test]
    fn lossless_keeps_every_8_bit_sample() {
        let image = test_image(97, 64, OutputSpace::Srgb);
        let bytes = encoded(&image, &lossless(BitDepth::Eight), &Embedded::default());
        let (_, pixels) = decoded(&bytes);
        let codes: Vec<u8> = pixels
            .iter()
            .map(|value| (value * 255.0).round() as u8)
            .collect();
        assert_eq!(codes, quantize_u8(image.pixels()));
    }

    #[test]
    fn lossless_keeps_every_16_bit_code() {
        let image = all_u16_codes(OutputSpace::ProPhoto);
        let bytes = encoded(&image, &lossless(BitDepth::Sixteen), &Embedded::default());
        let (decoded_image, pixels) = decoded(&bytes);
        assert_eq!(
            decoded_image
                .image_header()
                .metadata
                .bit_depth
                .bits_per_sample(),
            16
        );
        let codes: Vec<u16> = pixels
            .iter()
            .map(|value| (value * 65535.0).round() as u16)
            .collect();
        assert_eq!(codes, quantize_u16(image.pixels(), u16::MAX));
    }

    #[test]
    fn distance_controls_quality_and_size() {
        let image = test_image(256, 192, OutputSpace::Srgb);
        let near = encoded(
            &image,
            &JxlSettings {
                mode: JxlMode::Distance(0.5),
                effort: 5,
            },
            &Embedded::default(),
        );
        let far = encoded(
            &image,
            &JxlSettings {
                mode: JxlMode::Distance(6.0),
                effort: 5,
            },
            &Embedded::default(),
        );
        assert!(far.len() < near.len());
        let near_quality = smooth_psnr(&image, &reference_decoded(&near));
        let far_quality = smooth_psnr(&image, &reference_decoded(&far));
        assert!(near_quality > 48.0, "{near_quality:.1} dB");
        assert!(near_quality > far_quality);
    }

    #[test]
    fn quality_maps_onto_distance() {
        let image = test_image(256, 192, OutputSpace::Srgb);
        let high = encoded(
            &image,
            &JxlSettings {
                mode: JxlMode::Quality(95),
                effort: 5,
            },
            &Embedded::default(),
        );
        let low = encoded(
            &image,
            &JxlSettings {
                mode: JxlMode::Quality(40),
                effort: 5,
            },
            &Embedded::default(),
        );
        assert!(low.len() < high.len());
        assert!(smooth_psnr(&image, &reference_decoded(&high)) > 42.0);
    }

    #[test]
    fn lossy_stays_accurate_in_every_colour_space() {
        for space in OutputSpace::ALL {
            let image = test_image(128, 96, space);
            let settings = JxlSettings {
                mode: JxlMode::Distance(0.5),
                effort: 5,
            };
            let pixels = reference_decoded(&encoded(&image, &settings, &Embedded::default()));
            let quality = smooth_psnr(&image, &pixels);
            assert!(quality > 42.0, "{space:?} reached only {quality:.1} dB");
        }
    }

    #[test]
    fn decoders_see_a_profile_with_the_right_primaries() {
        for space in OutputSpace::ALL {
            let image = test_image(16, 16, space);
            for settings in [
                lossless(BitDepth::Sixteen),
                JxlSettings {
                    mode: JxlMode::Distance(1.0),
                    effort: 3,
                },
            ] {
                let (decoded_image, _) = decoded(&encoded(&image, &settings, &Embedded::default()));
                let icc = decoded_image
                    .original_icc()
                    .map(<[u8]>::to_vec)
                    .unwrap_or_else(|| decoded_image.rendered_icc());
                let profile = lcms2::Profile::new_icc(&icc).unwrap();
                let reference = lcms2::Profile::new_icc(&icc_profile(space).unwrap()).unwrap();
                for tag in [
                    lcms2::TagSignature::RedColorantTag,
                    lcms2::TagSignature::GreenColorantTag,
                    lcms2::TagSignature::BlueColorantTag,
                ] {
                    let (lcms2::Tag::CIEXYZ(ours), lcms2::Tag::CIEXYZ(theirs)) =
                        (profile.read_tag(tag), reference.read_tag(tag))
                    else {
                        panic!("{space:?} has no {tag:?}");
                    };
                    assert!((ours.X - theirs.X).abs() < 1e-3, "{space:?} {tag:?}");
                    assert!((ours.Y - theirs.Y).abs() < 1e-3, "{space:?} {tag:?}");
                    assert!((ours.Z - theirs.Z).abs() < 1e-3, "{space:?} {tag:?}");
                }
            }
        }
    }

    #[test]
    fn embeds_exif_and_xmp_boxes() {
        let image = test_image(32, 32, OutputSpace::Srgb);
        let bytes = encoded(&image, &lossless(BitDepth::Eight), &embedded(&image));
        let found = boxes(&bytes);

        let (_, exif_box) = found
            .iter()
            .find(|(kind, _)| kind == EXIF_BOX)
            .expect("no Exif box");
        assert_eq!(&exif_box[..4], &EXIF_TIFF_OFFSET);
        let fields = exif::Reader::new()
            .read_raw(exif_box[4..].to_vec())
            .unwrap();
        let iso = fields
            .get_field(exif::Tag::PhotographicSensitivity, exif::In::PRIMARY)
            .unwrap();
        assert_eq!(iso.value.get_uint(0), Some(400));

        let (_, xmp_box) = found
            .iter()
            .find(|(kind, _)| kind == XMP_BOX)
            .expect("no xml box");
        assert!(String::from_utf8_lossy(xmp_box).contains("© 2026 Jane Doe"));
        assert!(decoded(&bytes).1.len() == 32 * 32 * 3);
    }

    #[test]
    fn no_metadata_means_no_boxes() {
        let image = test_image(32, 32, OutputSpace::Srgb);
        let bytes = encoded(&image, &lossless(BitDepth::Eight), &Embedded::default());
        assert!(!bytes.windows(4).any(|window| window == EXIF_BOX));
    }

    #[test]
    fn out_of_range_samples_clip_instead_of_failing() {
        let pixels = vec![
            -1.0,
            2.0,
            f32::NAN,
            0.5,
            0.5,
            0.5,
            0.2,
            0.2,
            0.2,
            0.8,
            0.8,
            0.8,
        ];
        let image = Image::new(2, 2, OutputSpace::Srgb, pixels).unwrap();
        let settings = JxlSettings {
            mode: JxlMode::Distance(1.0),
            effort: 3,
        };
        assert_eq!(
            decoded(&encoded(&image, &settings, &Embedded::default()))
                .1
                .len(),
            12
        );
    }

    #[test]
    fn single_pixel_images_work() {
        let image = test_image(1, 1, OutputSpace::Srgb);
        let bytes = encoded(&image, &lossless(BitDepth::Sixteen), &Embedded::default());
        assert_eq!(decoded(&bytes).1.len(), 3);
    }

    #[test]
    fn output_buffer_growth_keeps_every_byte() {
        let image = test_image(1500, 1000, OutputSpace::Srgb);
        let bytes = encoded(&image, &lossless(BitDepth::Sixteen), &Embedded::default());
        assert!(
            bytes.len() > INITIAL_OUTPUT_BYTES,
            "the test needs to outgrow the first buffer"
        );
        let (_, pixels) = decoded(&bytes);
        let codes: Vec<u16> = pixels
            .iter()
            .map(|value| (value * 65535.0).round() as u16)
            .collect();
        assert_eq!(codes, quantize_u16(image.pixels(), u16::MAX));
    }
}
