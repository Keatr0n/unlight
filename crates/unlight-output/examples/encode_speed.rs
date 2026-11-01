//! Times every encoder on a real render, which `unlight-cli render` can produce:
//! `cargo run --release -p unlight-output --example encode_speed -- photo.tiff`

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::Instant;
use tiff::decoder::{Decoder, DecodingResult};
use unlight_output::*;

fn load_srgb_tiff(path: &Path) -> (u32, u32, Vec<f32>) {
    let file = File::open(path).expect("can't open the TIFF");
    let mut decoder = Decoder::new(BufReader::new(file)).expect("not a TIFF");
    decoder = decoder.with_limits(tiff::decoder::Limits::unlimited());
    let (width, height) = decoder.dimensions().expect("no dimensions");
    let DecodingResult::U16(samples) = decoder.read_image().expect("can't decode") else {
        panic!("expected a 16-bit TIFF");
    };
    (
        width,
        height,
        samples
            .iter()
            .map(|&sample| f32::from(sample) / 65535.0)
            .collect(),
    )
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
        collision: CollisionPolicy::Overwrite,
    }
}

fn measure(label: &str, image: &Image<'_>, export: &ExportSettings) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let metadata = ImageMetadata {
        source_name: "bench".into(),
        ..Default::default()
    };
    let start = Instant::now();
    let path = write_image(image, export, &metadata, directory.path()).expect("export failed");
    let elapsed = start.elapsed().as_secs_f64();
    let megabytes = std::fs::metadata(&path).expect("output file").len() as f64 / 1e6;
    println!("{label:<38} {elapsed:>7.2} s {megabytes:>8.1} MB");
}

fn jpeg(quality: u8, subsampling: ChromaSubsampling, effort: JpegEffort) -> FormatSettings {
    FormatSettings::Jpeg(JpegSettings {
        quality,
        subsampling,
        effort,
    })
}

fn jxl(mode: JxlMode, effort: u8) -> FormatSettings {
    FormatSettings::JpegXl(JxlSettings { mode, effort })
}

fn avif(depth: AvifDepth, speed: u8) -> FormatSettings {
    FormatSettings::Avif(AvifSettings {
        quality: 70,
        depth,
        speed,
    })
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: encode_speed photo.tiff");
    let (width, height, pixels) = load_srgb_tiff(Path::new(&path));
    let megapixels = f64::from(width) * f64::from(height) / 1e6;
    println!(
        "{width}×{height} ({megapixels:.1} MP), {} threads",
        rayon::current_num_threads()
    );
    let image = Image::new(width, height, OutputSpace::Srgb, pixels).expect("image");

    use ChromaSubsampling::{Yuv420, Yuv444};
    let tiff = |depth, compression| FormatSettings::Tiff(TiffSettings { depth, compression });
    let lossless = JxlMode::Lossless {
        depth: BitDepth::Sixteen,
    };
    let cases = [
        ("JPEG q92 4:4:4 fast", jpeg(92, Yuv444, JpegEffort::Fast)),
        ("JPEG q82 4:2:0 fast", jpeg(82, Yuv420, JpegEffort::Fast)),
        (
            "JPEG q92 4:4:4 smallest",
            jpeg(92, Yuv444, JpegEffort::Smallest),
        ),
        (
            "JPEG q82 4:2:0 smallest",
            jpeg(82, Yuv420, JpegEffort::Smallest),
        ),
        (
            "TIFF 16-bit uncompressed",
            tiff(BitDepth::Sixteen, TiffCompression::None),
        ),
        (
            "TIFF 16-bit deflate",
            tiff(BitDepth::Sixteen, TiffCompression::Deflate),
        ),
        (
            "TIFF 8-bit deflate",
            tiff(BitDepth::Eight, TiffCompression::Deflate),
        ),
        (
            "PNG 8-bit",
            FormatSettings::Png(PngSettings {
                depth: BitDepth::Eight,
            }),
        ),
        (
            "PNG 16-bit",
            FormatSettings::Png(PngSettings {
                depth: BitDepth::Sixteen,
            }),
        ),
        (
            "JPEG XL distance 1, effort 7",
            jxl(JxlMode::Distance(1.0), 7),
        ),
        (
            "JPEG XL distance 1, effort 3",
            jxl(JxlMode::Distance(1.0), 3),
        ),
        ("JPEG XL lossless 16-bit, effort 3", jxl(lossless, 3)),
        ("JPEG XL lossless 16-bit, effort 5", jxl(lossless, 5)),
        ("AVIF q70 10-bit, speed 6", avif(AvifDepth::Ten, 6)),
        ("AVIF q70 10-bit, speed 9", avif(AvifDepth::Ten, 9)),
        ("AVIF q70 12-bit, speed 9", avif(AvifDepth::Twelve, 9)),
    ];
    for (label, format) in cases {
        measure(label, &image, &settings(format));
    }

    let web = ExportSettings {
        resize: Some(ResizeSettings {
            mode: ResizeMode::LongEdge(2048),
            allow_upscale: false,
        }),
        ..settings(jpeg(82, Yuv420, JpegEffort::Fast))
    };
    measure("Resize to 2048 + JPEG", &image, &web);
    let sharpened = ExportSettings {
        sharpening: Some(OutputSharpening {
            media: SharpenMedia::Screen,
            amount: SharpenAmount::Standard,
        }),
        ..web
    };
    measure("Resize to 2048 + sharpen + JPEG", &image, &sharpened);
    let full_sharpened = ExportSettings {
        sharpening: Some(OutputSharpening {
            media: SharpenMedia::Matte,
            amount: SharpenAmount::High,
        }),
        ..settings(jpeg(92, Yuv444, JpegEffort::Fast))
    };
    measure("Full size + matte sharpen + JPEG", &image, &full_sharpened);
}
