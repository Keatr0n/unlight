//! Turns a finished image into a file: resize, output sharpening, watermark, colour profile,
//! metadata and encoding, ending in an atomic write.

mod atomic;
mod encode;
mod error;
mod exif;
mod ffi;
mod filename;
mod icc;
mod image;
mod metadata;
mod proof;
mod resize;
mod settings;
mod sharpen;
mod watermark;
mod xmp;

pub use atomic::CollisionPolicy;
pub use error::{Error, Result};
pub use icc::icc_profile;
pub use image::Image;
pub use metadata::{CaptureTime, GpsPosition, ImageMetadata, MetadataPolicy, MetadataSettings};
pub use proof::{bake_proof, proof_profile_name, read_proof_profile};
pub use resize::{Fit, ResizeMode, ResizeSettings};
pub use settings::{
    AvifDepth, AvifSettings, BitDepth, ChromaSubsampling, ExportPreset, ExportSettings,
    FormatSettings, JpegEffort, JpegSettings, JxlMode, JxlSettings, PngSettings, TiffCompression,
    TiffSettings,
};
pub use sharpen::{OutputSharpening, SharpenAmount, SharpenMedia};
pub use unlight_color::output::OutputSpace;
pub use watermark::{Anchor, WatermarkSettings, WatermarkSource};

use metadata::Embedded;
use std::path::{Path, PathBuf};

fn render<'a>(image: &'a Image<'_>, settings: &ExportSettings) -> Result<Image<'a>> {
    let plan = settings
        .resize
        .as_ref()
        .and_then(|resize| resize::plan(image.width(), image.height(), resize));
    let mut output = match plan {
        Some(plan) => resize::resize(image, &plan)?,
        None => image.view(),
    };
    if let Some(sharpening) = settings.sharpening {
        sharpen::sharpen(&mut output, sharpening);
    }
    if let Some(watermark) = &settings.watermark {
        watermark::apply(&mut output, watermark)?;
    }
    Ok(output)
}

/// Runs the whole export for one image and returns where the file landed.
///
/// With [`CollisionPolicy::Skip`], an existing file gives [`Error::Skipped`] and no work is
/// done. Nothing appears under the final name until the file is complete.
pub fn write_image(
    image: &Image<'_>,
    settings: &ExportSettings,
    metadata: &ImageMetadata,
    destination_dir: &Path,
) -> Result<PathBuf> {
    settings.validate()?;
    if image.space() != settings.color_space {
        return Err(Error::ColorSpaceMismatch {
            image: image.space().name(),
            settings: settings.color_space.name(),
        });
    }
    let stem = filename::render(&settings.filename_template, metadata)?;
    let destination = atomic::Destination {
        directory: destination_dir,
        stem: &stem,
        extension: settings.format.extension(),
        collision: settings.collision,
    };
    destination.check_skip()?;

    let rendered = render(image, settings)?;
    let icc = icc_profile(rendered.space())?;
    let embedded = Embedded::new(
        metadata,
        settings.metadata,
        [rendered.width(), rendered.height()],
        rendered.space(),
    );
    let input = encode::Input {
        image: &rendered,
        icc: &icc,
        metadata: &embedded,
    };
    destination.write(|out| encode::encode(&input, &settings.format, out))
}
