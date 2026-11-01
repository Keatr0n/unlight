//! Measures Photo Merge against a known answer. There are no real brackets or panoramas in the
//! sample set, so this makes sets from a real raw: the demosaiced photo is the truth, each
//! simulated frame goes back to a noisy, clipped mosaic, and the frames then take the same road
//! as a real file, through the engine's demosaic and highlight repair. Montages go to PNG, for
//! looking at, because a number alone doesn't show a ghost.
//!
//! ```text
//! evaluate hdr <raw> <output directory>
//! evaluate pano <raw> <output directory> [--lens]
//! evaluate memory <raw> <frames>
//! evaluate real-pano <directory of JPEGs> <output directory>
//! evaluate real-hdr <directory holding list.txt> <output directory>
//! ```
//!
//! The two `real-` modes run on real photographs that aren't raw. They're 8-bit and display
//! referred, so they say nothing about demosaicing or true linearity, but the camera motion,
//! parallax and scenes are real.

use rayon::prelude::*;
use std::{
    path::{Path, PathBuf},
    time::Instant,
};
use unlight_color::{
    CameraProfile,
    difference::{delta_e_2000, srgb_to_lab},
    matrix::{Mat3, Vec3, apply, inverse, mul},
};
use unlight_engine::{ExportOptions, ExportedImage, export, read_reduced, read_scene};
use unlight_merge::{
    Deghost, Frame, FrameSource, HdrOptions, Image, MergeError, NO_FRAME, PanoramaOptions, Plane, Progress, Projection,
    Result, merge_hdr, merge_panorama, write_merged,
};
use unlight_raw::{FixVignetteRadial, Opcode, Opcodes, RawImage, WarpPlane, WarpRectilinear};
use unlight_recipe::Recipe;

/// Full-well and read noise of a clean sensor at base ISO, as a share of white.
const SHOT_NOISE: f32 = 2e-5;
const READ_NOISE: f32 = 1e-7;
/// Seam energy is measured in log light, floored here so sensor noise near black doesn't swamp it.
const SEAM_FLOOR: f32 = 0.02;

struct Random(u64);

impl Random {
    fn uniform(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    fn gaussian(&mut self) -> f32 {
        let radius = (-2.0 * self.uniform().max(1e-7).ln()).sqrt();
        radius * (self.uniform() * std::f32::consts::TAU).cos()
    }
}

/// Frames held as raws in memory.
struct RawSet {
    raws: Vec<RawImage>,
    frames: Vec<Frame>,
}

impl RawSet {
    fn new(raws: Vec<RawImage>) -> Self {
        let frames = raws
            .iter()
            .enumerate()
            .map(|(index, raw)| {
                let copy = read_reduced(raw, unlight_merge::ANALYSIS_EDGE as u32).expect("reduced copy");
                let neutral = CameraProfile::new(&raw.color).as_shot_neutral();
                Frame {
                    path: PathBuf::from(format!("frame-{index}")),
                    width: raw.width as usize,
                    height: raw.height as usize,
                    metadata: raw.metadata.clone(),
                    color: raw.color.clone(),
                    opcodes: raw.opcodes.clone(),
                    embedded_lens: raw.embedded_lens.clone(),
                    balance: neutral.map(|value| (1.0 / value) as f32),
                    reduced: Image { width: copy.width as usize, height: copy.height as usize, pixels: copy.pixels },
                }
            })
            .collect();
        Self { raws, frames }
    }
}

impl FrameSource for RawSet {
    fn frames(&self) -> &[Frame] {
        &self.frames
    }

    fn full(&self, index: usize) -> Result<Image> {
        let raw = &self.raws[index];
        let pixels = read_scene(raw, &Recipe::default()).map_err(|error| MergeError::Source(error.to_string()))?;
        Ok(Image::from_rgba(raw.width as usize, raw.height as usize, &pixels))
    }
}

/// Turns a linear balanced image back into a raw of the same camera.
fn to_mosaic(like: &RawImage, image: &Image, balance: [f32; 3], seed: u64) -> RawImage {
    let (black, white) = (like.black[0][0], like.white);
    let cfa = like.cfa;
    let width = image.width;
    let mut samples = vec![0u16; image.width * image.height];
    samples.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let mut random = Random(seed ^ ((y as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)));
        for (x, sample) in row.iter_mut().enumerate() {
            let channel = cfa.color_at(x as u32, y as u32) as usize;
            let level = (image.at(x, y)[channel] / balance[channel]).max(0.0);
            let noisy = level + random.gaussian() * (SHOT_NOISE * level + READ_NOISE).sqrt();
            *sample = (black + noisy.clamp(0.0, 1.0) * (white - black)).round() as u16;
        }
    });
    RawImage {
        width: image.width as u32,
        height: image.height as u32,
        samples,
        black: [[black; 6]; 6],
        metadata: unlight_raw::Metadata {
            make: like.metadata.make.clone(),
            model: like.metadata.model.clone(),
            ..unlight_raw::Metadata::default()
        },
        opcodes: Opcodes::default(),
        embedded_lens: None,
        ..like.clone()
    }
}

fn sample_lanczos(image: &Image, x: f32, y: f32) -> [f32; 3] {
    fn weight(value: f32) -> f32 {
        if value.abs() < 1e-5 {
            return 1.0;
        }
        if value.abs() >= 3.0 {
            return 0.0;
        }
        let scaled = value * std::f32::consts::PI;
        3.0 * scaled.sin() * (scaled / 3.0).sin() / (scaled * scaled)
    }
    let (left, top) = (x.floor() as i32, y.floor() as i32);
    let mut sum = [0.0f32; 3];
    let mut total = 0.0;
    for row in top - 2..=top + 3 {
        let weight_y = weight(y - row as f32);
        let at_y = row.clamp(0, image.height as i32 - 1) as usize;
        for column in left - 2..=left + 3 {
            let tap = weight_y * weight(x - column as f32);
            let at_x = column.clamp(0, image.width as i32 - 1) as usize;
            let value = image.at(at_x, at_y);
            (0..3).for_each(|c| sum[c] += value[c] * tap);
            total += tap;
        }
    }
    sum.map(|value| (value / total).max(0.0))
}

fn percentile(values: &mut [f32], fraction: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let at = ((values.len() - 1) as f32 * fraction) as usize;
    *values.select_nth_unstable_by(at, f32::total_cmp).1
}

fn luma(pixel: [f32; 3]) -> f32 {
    pixel[0] * 0.25 + pixel[1] * 0.5 + pixel[2] * 0.25
}

fn render_at(raw: &RawImage, recipe: &Recipe) -> ExportedImage {
    export(raw, recipe, &ExportOptions::default()).expect("render")
}

fn develop(change: impl FnOnce(&mut Recipe)) -> Recipe {
    let mut recipe = Recipe::default();
    change(&mut recipe);
    recipe
}

/// Colour difference between two renders, as its mean and 95th percentile, skipping the border.
fn delta_e(a: &ExportedImage, b: &ExportedImage, region: Option<[usize; 4]>) -> (f32, f32) {
    let (width, height) = (a.width as usize, a.height as usize);
    let [left, top, right, bottom] = region.unwrap_or([16, 16, width - 16, height - 16]);
    let mut values: Vec<f32> = (top..bottom)
        .into_par_iter()
        .flat_map_iter(|y| {
            (left..right).step_by(2).map(move |x| {
                let at = (y * width + x) * 3;
                let pick = |image: &ExportedImage| [image.pixels[at], image.pixels[at + 1], image.pixels[at + 2]];
                delta_e_2000(srgb_to_lab(pick(a)), srgb_to_lab(pick(b))) as f32
            })
        })
        .collect();
    let mean = values.iter().sum::<f32>() / values.len().max(1) as f32;
    (mean, percentile(&mut values, 0.95))
}

fn tile_of(image: &ExportedImage, factor: usize) -> (usize, usize, Vec<u8>) {
    let (width, height) = (image.width as usize / factor, image.height as usize / factor);
    let mut out = vec![0u8; width * height * 3];
    for y in 0..height {
        for x in 0..width {
            for channel in 0..3 {
                let mut sum = 0.0;
                for dy in 0..factor {
                    for dx in 0..factor {
                        sum += image.pixels[((y * factor + dy) * image.width as usize + x * factor + dx) * 3 + channel];
                    }
                }
                out[(y * width + x) * 3 + channel] = ((sum / (factor * factor) as f32).clamp(0.0, 1.0) * 255.0) as u8;
            }
        }
    }
    (width, height, out)
}

fn save_montage(path: &Path, tiles: &[(usize, usize, Vec<u8>)], columns: usize) {
    const GAP: usize = 6;
    let cell_width = tiles.iter().map(|tile| tile.0).max().unwrap_or(1);
    let cell_height = tiles.iter().map(|tile| tile.1).max().unwrap_or(1);
    let rows = tiles.len().div_ceil(columns);
    let (width, height) = (columns * (cell_width + GAP), rows * (cell_height + GAP));
    let mut pixels = vec![32u8; width * height * 3];
    for (index, (tile_width, tile_height, data)) in tiles.iter().enumerate() {
        let (left, top) = ((index % columns) * (cell_width + GAP), (index / columns) * (cell_height + GAP));
        for y in 0..*tile_height {
            let target = ((top + y) * width + left) * 3;
            let source = y * tile_width * 3;
            pixels[target..target + tile_width * 3].copy_from_slice(&data[source..source + tile_width * 3]);
        }
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path).expect("montage file"));
    let mut encoder = png::Encoder::new(file, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().expect("png header").write_image_data(&pixels).expect("png data");
    println!("montage: {}", path.display());
}

fn error_tile(a: &ExportedImage, b: &ExportedImage, factor: usize, gain: f32) -> (usize, usize, Vec<u8>) {
    let difference = ExportedImage {
        pixels: a.pixels.iter().zip(&b.pixels).map(|(x, y)| (x - y).abs() * gain).collect(),
        ..a.clone()
    };
    tile_of(&difference, factor)
}

fn truth_of(path: &Path) -> (RawImage, Image) {
    let raw = unlight_raw::load(path).expect("decoding");
    let start = Instant::now();
    let pixels = read_scene(&raw, &Recipe::default()).expect("demosaic");
    println!("truth: {} {}×{} read in {:.1} s", raw.metadata.camera(), raw.width, raw.height, start.elapsed().as_secs_f32());
    let full = Image::from_rgba(raw.width as usize, raw.height as usize, &pixels);
    (raw, full)
}

fn balance_of(raw: &RawImage) -> [f32; 3] {
    CameraProfile::new(&raw.color).as_shot_neutral().map(|value| (1.0 / value) as f32)
}

fn merged_raw(merged: &unlight_merge::Merged, directory: &Path, name: &str) -> RawImage {
    let path = directory.join(name);
    write_merged(&path, merged).expect("writing");
    let size = std::fs::metadata(&path).expect("file").len();
    let raw_bytes = merged.image.width * merged.image.height * 6;
    println!("file: {:.1} MiB, {:.2}:1 against raw half floats", size as f64 / (1 << 20) as f64, raw_bytes as f64 / size as f64);
    unlight_raw::load(&path).expect("reading the merge back")
}

fn truth_raw(truth: &Image, like: &RawImage, directory: &Path, name: &str) -> RawImage {
    let path = directory.join(name);
    let photo = unlight_raw::merged::MergedPhoto {
        width: truth.width as u32,
        height: truth.height as u32,
        pixels: &truth.pixels,
        color: &like.color,
        metadata: &like.metadata,
        opcodes: &Opcodes::default(),
        embedded_lens: None,
        preview_jpeg: b"",
    };
    unlight_raw::merged::write(&path, &photo).expect("writing truth");
    unlight_raw::load(&path).expect("reading truth")
}

fn rigid(dx: f64, dy: f64, degrees: f64) -> Mat3 {
    let (sin, cos) = degrees.to_radians().sin_cos();
    [[cos, -sin, dx], [sin, cos, dy], [0.0, 0.0, 1.0]]
}

fn warp_point(matrix: &Mat3, x: f64, y: f64) -> [f64; 2] {
    let [px, py, w] = apply(matrix, &[x, y, 1.0]);
    [px / w, py / w]
}

/// One bracketed frame: the truth seen by a camera moved by `matrix` and exposed at `stops`.
fn bracket_frame(truth: &Image, matrix: &Mat3, stops: f32, object: Option<[usize; 4]>) -> Image {
    let back = inverse(matrix).expect("invertible");
    let (half_x, half_y) = (truth.width as f64 * 0.5, truth.height as f64 * 0.5);
    let gain = stops.exp2();
    Image::from_fn(truth.width, truth.height, |x, y| {
        let [sx, sy] = warp_point(&back, x as f64 + 0.5 - half_x, y as f64 + 0.5 - half_y);
        let (sx, sy) = (sx + half_x - 0.5, sy + half_y - 0.5);
        let (mut sx, mut sy) = (sx as f32, sy as f32);
        if let Some([left, top, right, bottom]) = object
            && (left..right).contains(&x)
            && (top..bottom).contains(&y)
        {
            // A patch of the scene from elsewhere, which is what a passer-by looks like.
            sx -= (right - left) as f32 * 1.7;
            sy += (bottom - top) as f32 * 0.4;
        }
        sample_lanczos(truth, sx, sy).map(|value| value * gain)
    })
}

fn corner_error(found: &Mat3, truth: &Mat3, width: usize, height: usize) -> f64 {
    let (half_x, half_y) = (width as f64 * 0.5, height as f64 * 0.5);
    [[-half_x, -half_y], [half_x, -half_y], [-half_x, half_y], [half_x, half_y]]
        .iter()
        .map(|corner| {
            let (a, b) = (warp_point(found, corner[0], corner[1]), warp_point(truth, corner[0], corner[1]));
            (a[0] - b[0]).hypot(a[1] - b[1])
        })
        .fold(0.0, f64::max)
}

struct BandError {
    name: &'static str,
    rms_stops: f32,
    median_stops: f32,
    pixels: usize,
}

/// Errors in stops against the truth, by how bright the truth is.
fn radiance_bands(found: &Image, truth: &Image) -> Vec<BandError> {
    let bands: [(&'static str, f32, f32); 4] =
        [("shadows", 0.002, 0.02), ("midtones", 0.02, 0.5), ("bright", 0.5, 1.0), ("above white", 1.0, f32::MAX)];
    let margin = 24;
    bands
        .iter()
        .map(|&(name, low, high)| {
            let mut errors: Vec<f32> = Vec::new();
            for y in margin..truth.height - margin {
                for x in margin..truth.width - margin {
                    let level = luma(truth.at(x, y));
                    if level >= low && level < high {
                        errors.push((luma(found.at(x, y)).max(1e-6) / level).log2());
                    }
                }
            }
            let pixels = errors.len();
            let sum: f32 = errors.iter().map(|value| value * value).sum();
            let mut absolute: Vec<f32> = errors.iter().map(|value| value.abs()).collect();
            BandError {
                name,
                rms_stops: (sum / pixels.max(1) as f32).sqrt(),
                median_stops: percentile(&mut absolute, 0.5),
                pixels,
            }
        })
        .collect()
}

fn report_bands(label: &str, found: &Image, truth: &Image) {
    println!("{label}");
    for band in radiance_bands(found, truth) {
        println!(
            "  {:<12} {:>10} px  rms {:.4} EV  median {:.4} EV",
            band.name, band.pixels, band.rms_stops, band.median_stops
        );
    }
}

fn evaluate_hdr(file: &Path, directory: &Path) {
    let (raw, full) = truth_of(file);
    let quiet = full.reduced(2);
    drop(full);
    let balance = balance_of(&raw);
    let mut levels: Vec<f32> = quiet.pixels.iter().step_by(13).map(|pixel| luma(*pixel)).collect();
    let bright = percentile(&mut levels, 0.995).max(1e-4);
    let gain = 3.5 / bright;
    let truth = Image { pixels: quiet.pixels.iter().map(|pixel| pixel.map(|value| value * gain)).collect(), ..quiet };
    println!("truth scaled so 99.5% sits {:.2} EV over the white level", 3.5f32.log2());
    let (width, height) = (truth.width, truth.height);

    let stops = [-2.0f32, 0.0, 2.0];
    let motion = [rigid(-3.37, 2.11, -0.31), rigid(0.0, 0.0, 0.0), rigid(4.62, -2.83, 0.27)];
    let object = [width * 3 / 5, height / 3, width * 3 / 5 + width / 8, height / 3 + height / 4];

    for (label, moved) in [("still scene", false), ("with a moved object", true)] {
        println!("\n=== {label} ===");
        let raws: Vec<RawImage> = stops
            .iter()
            .zip(&motion)
            .enumerate()
            .map(|(index, (stop, matrix))| {
                let patch = (moved && index == 2).then_some(object);
                let frame = bracket_frame(&truth, matrix, *stop, patch);
                let mut mosaic = to_mosaic(&raw, &frame, balance, 0x1234_5678 + index as u64);
                mosaic.metadata.shutter = 0.01 * stop.exp2();
                mosaic.metadata.iso = 100.0;
                mosaic.metadata.aperture = 8.0;
                mosaic
            })
            .collect();
        let set = RawSet::new(raws);
        for (name, deghost) in [("no deghosting", Deghost::None), ("medium deghosting", Deghost::Medium)] {
            if deghost != Deghost::None && !moved {
                continue;
            }
            let start = Instant::now();
            let options = HdrOptions { auto_align: true, deghost };
            let (merged, report) = merge_hdr(&set, &options, &Progress::default()).expect("merge");
            println!("\n{name}: merged in {:.1} s", start.elapsed().as_secs_f32());
            for (index, estimate) in report.exposures.iter().enumerate() {
                println!(
                    "  frame {index}: used {:+.3} EV ({:?}), stated {:?}, true {:+.3} EV",
                    estimate.used, estimate.source, estimate.stated, stops[index]
                );
            }
            for index in [0, 2] {
                let error = corner_error(&report.alignments[index], &motion[index], width, height);
                println!("  frame {index} corner alignment error {error:.3} px");
            }
            println!(
                "  headroom {:.2} EV, ghost share {:.2}%, clipped in every frame {:.2}%",
                report.headroom,
                report.ghost_share * 100.0,
                report.clipped_share * 100.0
            );
            report_bands("  merged against truth:", &merged.image, &truth);
            if name.starts_with("medium") || !moved {
                let tag = if moved { "moved" } else { "still" };
                let merged_file = merged_raw(&merged, directory, &format!("hdr-{tag}.ulr"));
                let truth_file = truth_raw(&truth, &merged_file, directory, "hdr-truth.ulr");
                let graded = develop(|r| {
                    r.basic.exposure = -2.0;
                    r.basic.shadows = 60.0;
                });
                let (ours, ideal) = (render_at(&merged_file, &graded), render_at(&truth_file, &graded));
                let single = render_at(&set.raws[1], &graded);
                let (mean, p95) = delta_e(&ours, &ideal, None);
                let (single_mean, single_p95) = delta_e(&single, &ideal, None);
                println!("  ΔE2000 at -2 EV: merged {mean:.2} mean {p95:.2} p95, 0 EV frame alone {single_mean:.2} mean {single_p95:.2} p95");
                if moved {
                    let region = [object[0], object[1], object[2], object[3]];
                    let (mean, p95) = delta_e(&ours, &ideal, Some(region));
                    println!("  ΔE2000 inside the moved box: {mean:.2} mean {p95:.2} p95");
                }
                let factor = (width.max(height) / 900).max(1);
                let plain = |image: &RawImage| render_at(image, &Recipe::default());
                let tiles = vec![
                    tile_of(&plain(&set.raws[0]), factor),
                    tile_of(&plain(&set.raws[1]), factor),
                    tile_of(&plain(&set.raws[2]), factor),
                    tile_of(&plain(&merged_file), factor),
                    tile_of(&ours, factor),
                    tile_of(&ideal, factor),
                    tile_of(&single, factor),
                    error_tile(&ours, &ideal, factor, 8.0),
                ];
                save_montage(&directory.join(format!("hdr-{tag}.png")), &tiles, 4);
            }
        }
    }
}

/// Where a pixel of a view lands on the source photo, treated as a plane `focal` away.
fn view_to_source(ray: &Vec3, focal: f64, half: [f64; 2]) -> Option<[f32; 2]> {
    (ray[2] > 0.05).then(|| {
        [(ray[0] / ray[2] * focal + half[0] - 0.5) as f32, (ray[1] / ray[2] * focal + half[1] - 0.5) as f32]
    })
}

fn yaw_pitch(yaw: f64, pitch: f64) -> Mat3 {
    let (sin, cos) = yaw.sin_cos();
    let yawed = [[cos, 0.0, -sin], [0.0, 1.0, 0.0], [sin, 0.0, cos]];
    let (sin, cos) = pitch.sin_cos();
    let pitched = [[1.0, 0.0, 0.0], [0.0, cos, -sin], [0.0, sin, cos]];
    mul(&pitched, &yawed)
}

fn transpose(m: &Mat3) -> Mat3 {
    std::array::from_fn(|row| std::array::from_fn(|column| m[column][row]))
}

/// A lens to inflict on the views, and to tell the stitcher about through DNG opcodes.
#[derive(Clone, Copy)]
struct SimulatedLens {
    /// Radial term of the source radius over the output radius, at the far corner.
    barrel: f64,
    /// Light lost at the corner, as a share.
    falloff: f32,
    announced: bool,
}

impl SimulatedLens {
    fn opcodes(&self) -> Opcodes {
        let warp = WarpRectilinear {
            planes: vec![WarpPlane { radial: [1.0, self.barrel, 0.0, 0.0], tangential: [0.0; 2] }],
            center_x: 0.5,
            center_y: 0.5,
        };
        let gain_at_corner = 1.0 / (1.0 - f64::from(self.falloff));
        let vignette = FixVignetteRadial { k: [gain_at_corner - 1.0, 0.0, 0.0, 0.0, 0.0], center_x: 0.5, center_y: 0.5 };
        Opcodes { list3: vec![Opcode::WarpRectilinear(warp), Opcode::FixVignetteRadial(vignette)], ..Opcodes::default() }
    }
}

struct View {
    camera: Mat3,
    stops: f32,
}

struct Scene<'a> {
    truth: &'a Image,
    source_focal: f64,
    view_focal: f64,
    size: [usize; 2],
    lens: SimulatedLens,
}

/// What a camera with a lens on it would have recorded.
fn render_view(scene: &Scene<'_>, view: &View) -> Image {
    let [width, height] = scene.size;
    let (half_x, half_y) = (width as f64 * 0.5, height as f64 * 0.5);
    let truth_half = [scene.truth.width as f64 * 0.5, scene.truth.height as f64 * 0.5];
    let back = transpose(&view.camera);
    let gain = view.stops.exp2();
    let corner = half_x.hypot(half_y);
    let SimulatedLens { barrel, falloff, .. } = scene.lens;
    Image::from_fn(width, height, |x, y| {
        let (px, py) = (x as f64 + 0.5 - half_x, y as f64 + 0.5 - half_y);
        let radius = px.hypot(py) / corner;
        // Stored pixel to where the scene point would be with no lens: the inverse of the
        // correction the stitcher will run.
        let mut ideal = [px, py];
        for _ in 0..8 {
            let scale = 1.0 + barrel * (ideal[0].hypot(ideal[1]) / corner).powi(2);
            ideal = [px / scale, py / scale];
        }
        let ray = apply(&back, &[ideal[0] / scene.view_focal, ideal[1] / scene.view_focal, 1.0]);
        let Some([sx, sy]) = view_to_source(&ray, scene.source_focal, truth_half) else {
            return [0.0; 3];
        };
        if sx < 0.0 || sy < 0.0 || sx > (scene.truth.width - 1) as f32 || sy > (scene.truth.height - 1) as f32 {
            return [0.0; 3];
        }
        let shade = 1.0 - falloff * (radius * radius) as f32;
        sample_lanczos(scene.truth, sx, sy).map(|value| value * gain * shade)
    })
}

fn evaluate_panorama(file: &Path, directory: &Path, announced: bool) {
    let (raw, truth) = truth_of(file);
    let balance = balance_of(&raw);
    let mut levels: Vec<f32> = truth.pixels.iter().step_by(13).map(|pixel| luma(*pixel)).collect();
    let bright = percentile(&mut levels, 0.99).max(1e-4);
    let scale = 0.8 / bright;
    let truth = Image { pixels: truth.pixels.iter().map(|pixel| pixel.map(|value| value * scale)).collect(), ..truth };

    let (source_width, source_height) = (truth.width as f64, truth.height as f64);
    let source_focal = source_width / (2.0 * 35f64.to_radians().tan());
    // Narrow enough that all five views stay on the photo, which is about 70° across.
    let view_width = (source_width * 0.25) as usize;
    let view_height = (source_height * 0.55) as usize;
    let view_focal = source_focal;
    let across = (view_width as f64 * 0.5 / view_focal).atan() * 2.0;
    let step = across * 0.62;
    let count = 5;
    let offsets = [0.0f32, 0.42, -0.35, 0.2, -0.28];
    let lens = SimulatedLens { barrel: if announced { 0.045 } else { 0.0 }, falloff: 0.45, announced };
    let views: Vec<View> = (0..count)
        .map(|index| {
            let along = index as f64 - (count - 1) as f64 * 0.5;
            let pitch = 0.012 * (index as f64 * 2.1).sin();
            View { camera: yaw_pitch(along * step, pitch), stops: offsets[index] }
        })
        .collect();
    println!(
        "{count} views of {view_width}×{view_height}, focal {view_focal:.1}, {:.1}° step, vignette {:.0}% at the corner, {}",
        step.to_degrees(),
        lens.falloff * 100.0,
        if announced { "lens known through opcodes, barrel distortion added" } else { "lens unknown to the stitcher" }
    );
    let scene = Scene { truth: &truth, source_focal, view_focal, size: [view_width, view_height], lens };
    let mut rendered = Vec::with_capacity(count);
    let raws: Vec<RawImage> = views
        .iter()
        .enumerate()
        .map(|(index, view)| {
            let image = render_view(&scene, view);
            let mut mosaic = to_mosaic(&raw, &image, balance, 0xabcd + index as u64);
            let diagonal_35 = 43.27 * view_focal / (view_width as f64).hypot(view_height as f64);
            mosaic.metadata.focal_length_35mm = (diagonal_35 * 1.08) as f32;
            mosaic.metadata.shutter = 0.01;
            mosaic.metadata.iso = 100.0;
            mosaic.metadata.aperture = 8.0;
            if lens.announced {
                mosaic.opcodes = lens.opcodes();
            }
            rendered.push(image);
            mosaic
        })
        .collect();
    let set = RawSet::new(raws);
    let start = Instant::now();
    let options = PanoramaOptions { projection: Projection::Spherical, auto_crop: true, ..PanoramaOptions::default() };
    let (merged, report) = merge_panorama(&set, &options, &Progress::default()).expect("stitch");
    println!("stitched in {:.1} s", start.elapsed().as_secs_f32());
    println!("focal {:.1} against true {view_focal:.1} ({:+.2}%), started 8% off", report.focal, (report.focal / view_focal - 1.0) * 100.0);
    println!("reported reprojection {:.3} px rms", report.rms);

    let found: Vec<(Mat3, usize)> = report.cameras.iter().copied().zip(report.used.iter().copied()).collect();
    let root = found[0].1;
    let mut worst = 0.0f64;
    for (camera, index) in &found[1..] {
        let found_relative = mul(camera, &transpose(&found[0].0));
        let true_relative = mul(&views[*index].camera, &transpose(&views[root].camera));
        let change = mul(&found_relative, &transpose(&true_relative));
        let trace = change[0][0] + change[1][1] + change[2][2];
        let degrees = ((trace - 1.0) * 0.5).clamp(-1.0, 1.0).acos().to_degrees();
        worst = worst.max(degrees);
    }
    println!("worst relative rotation error {worst:.4}°");

    let mut residuals = Vec::new();
    for (first_at, (first_camera, first)) in found.iter().enumerate() {
        for (second_camera, second) in &found[first_at + 1..] {
            for gy in 1..12 {
                for gx in 1..16 {
                    let point = [
                        (gx as f64 / 16.0 - 0.5) * view_width as f64,
                        (gy as f64 / 12.0 - 0.5) * view_height as f64,
                    ];
                    let world = apply(&transpose(&views[*first].camera), &[point[0] / view_focal, point[1] / view_focal, 1.0]);
                    let seen = apply(&views[*second].camera, &world);
                    if seen[2] <= 0.0 {
                        continue;
                    }
                    let truth_at = [view_focal * seen[0] / seen[2], view_focal * seen[1] / seen[2]];
                    if truth_at[0].abs() > view_width as f64 * 0.5 || truth_at[1].abs() > view_height as f64 * 0.5 {
                        continue;
                    }
                    let ours = apply(
                        &mul(second_camera, &transpose(first_camera)),
                        &[point[0] / report.focal, point[1] / report.focal, 1.0],
                    );
                    let ours = [report.focal * ours[0] / ours[2], report.focal * ours[1] / ours[2]];
                    residuals.push((ours[0] - truth_at[0]).hypot(ours[1] - truth_at[1]) as f32);
                }
            }
        }
    }
    let rms = (residuals.iter().map(|value| value * value).sum::<f32>() / residuals.len().max(1) as f32).sqrt();
    println!("reprojection against the known transforms: {rms:.3} px rms over {} grid points, worst {:.3} px", residuals.len(), residuals.iter().copied().fold(0.0, f32::max));

    for (position, index) in report.used.iter().enumerate() {
        let gain = report.gains[position][1];
        let expected = 2f32.powf(views[report.reference].stops - views[*index].stops);
        println!("  frame {index}: gain {gain:.4}, expected {expected:.4} ({:+.2}%)", (gain / expected - 1.0) * 100.0);
    }
    println!("vignette terms {:?}, empty {:.3}%", report.vignette, report.empty_share * 100.0);

    let image = &merged.image;
    let (width, height) = (image.width, image.height);
    let reference_gain = 2f32.powf(views[report.reference].stops);
    let truth_half = [truth.width as f64 * 0.5, truth.height as f64 * 0.5];
    // The truth is looked up through the frame each pixel came from, and that frame's true
    // camera. A set can be solved to an overall scale that's a hair off, which stretches the
    // whole panorama evenly and harms nothing, and this keeps that out of the comparison.
    let [label_width, label_height] = report.seam_size;
    let label_at = |x: usize, y: usize| {
        let lx = (x * label_width / width).min(label_width - 1);
        let ly = (y * label_height / height).min(label_height - 1);
        report.seams[ly * label_width + lx]
    };
    let ideal = Image::from_fn(width, height, |x, y| {
        let u = (x as f64 + 0.5) / width as f64;
        let v = (y as f64 + 0.5) / height as f64;
        let label = label_at(x, y);
        if label == NO_FRAME {
            return [0.0; 3];
        }
        let (camera, index) = found[label as usize];
        let seen = apply(&camera, &report.canvas.ray(u, v));
        if seen[2] <= 0.0 {
            return [0.0; 3];
        }
        let ratio = report.focal / view_focal;
        let ray = apply(&transpose(&views[index].camera), &[seen[0] * ratio, seen[1] * ratio, seen[2]]);
        match view_to_source(&ray, source_focal, truth_half) {
            Some([sx, sy]) if sx >= 0.0 && sy >= 0.0 && sx < (truth.width - 1) as f32 && sy < (truth.height - 1) as f32 => {
                sample_lanczos(&truth, sx, sy).map(|value| value * reference_gain)
            }
            _ => [0.0; 3],
        }
    });
    let mut log_errors: Vec<f32> = image
        .pixels
        .iter()
        .zip(&ideal.pixels)
        .filter(|(a, b)| luma(**b) > 0.01 && luma(**a) > 0.0)
        .map(|(a, b)| (luma(*a) / luma(*b)).log2().abs())
        .collect();
    let mean = log_errors.iter().sum::<f32>() / log_errors.len().max(1) as f32;
    println!("panorama against the truth: mean {mean:.4} EV, p95 {:.4} EV", percentile(&mut log_errors, 0.95));

    let plane = |source: &Image| Plane {
        width,
        height,
        values: source.pixels.iter().map(|pixel| (luma(*pixel).max(SEAM_FLOOR)).log2()).collect(),
    };
    let (ours_log, ideal_log) = (plane(image), plane(&ideal));
    let (mut across, mut across_truth, mut seam_pixels) = (0.0f64, 0.0f64, 0usize);
    let (mut flat, mut flat_truth, mut flat_pixels) = (0.0f64, 0.0f64, 0usize);
    for y in 8..height - 8 {
        for x in 8..width - 8 {
            let here = label_at(x, y);
            let near_seam = (here != label_at(x + 4, y) || here != label_at(x, y + 4)) && here != NO_FRAME;
            let (ours, truth) = (
                (ours_log.at(x + 1, y) - ours_log.at(x, y)).abs() + (ours_log.at(x, y + 1) - ours_log.at(x, y)).abs(),
                (ideal_log.at(x + 1, y) - ideal_log.at(x, y)).abs() + (ideal_log.at(x, y + 1) - ideal_log.at(x, y)).abs(),
            );
            if near_seam {
                across += f64::from(ours);
                across_truth += f64::from(truth);
                seam_pixels += 1;
            } else {
                flat += f64::from(ours);
                flat_truth += f64::from(truth);
                flat_pixels += 1;
            }
        }
    }
    println!(
        "gradient energy near seams {:.4}, truth there {:.4}, ratio {:.3} ({seam_pixels} px)",
        across / seam_pixels.max(1) as f64,
        across_truth / seam_pixels.max(1) as f64,
        across / across_truth.max(1e-9)
    );
    println!(
        "gradient energy away from seams {:.4}, truth there {:.4}, ratio {:.3}",
        flat / flat_pixels.max(1) as f64,
        flat_truth / flat_pixels.max(1) as f64,
        flat / flat_truth.max(1e-9)
    );

    let tag = if announced { "lens" } else { "plain" };
    let merged_file = merged_raw(&merged, directory, &format!("pano-{tag}.ulr"));
    let truth_file = truth_raw(&ideal, &merged_file, directory, "pano-truth.ulr");
    let graded = develop(|r| r.basic.shadows = 30.0);
    let (ours, reference) = (render_at(&merged_file, &graded), render_at(&truth_file, &graded));
    let (mean, p95) = delta_e(&ours, &reference, None);
    println!("ΔE2000 against the truth: {mean:.2} mean, {p95:.2} p95");
    let factor = (width / 1500).max(1);
    let frame_factor = (view_width / 500).max(1);
    let mut tiles: Vec<(usize, usize, Vec<u8>)> =
        set.raws.iter().take(3).map(|one| tile_of(&render_at(one, &Recipe::default()), frame_factor)).collect();
    tiles.truncate(3);
    save_montage(&directory.join(format!("pano-{tag}-frames.png")), &tiles, 3);
    let tiles = vec![tile_of(&ours, factor), tile_of(&reference, factor), error_tile(&ours, &reference, factor, 8.0)];
    save_montage(&directory.join(format!("pano-{tag}.png")), &tiles, 1);
}

/// Ten 61 MP frames generated on demand, so they cost what a real source costs: one at a time.
struct Generated<'a> {
    frames: Vec<Frame>,
    views: Vec<View>,
    truth: &'a Image,
    source_focal: f64,
    view_focal: f64,
    size: [usize; 2],
}

impl FrameSource for Generated<'_> {
    fn frames(&self) -> &[Frame] {
        &self.frames
    }

    fn full(&self, index: usize) -> Result<Image> {
        let scene = Scene {
            truth: self.truth,
            source_focal: self.source_focal,
            view_focal: self.view_focal,
            size: self.size,
            lens: SimulatedLens { barrel: 0.0, falloff: 0.0, announced: false },
        };
        // The engine reads a frame back as RGBA, which briefly costs a third more than this.
        Ok(render_view(&scene, &self.views[index]))
    }
}

fn peak_gib() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kib: f64 = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0.0);
    kib / (1024.0 * 1024.0)
}

fn evaluate_memory(file: &Path, count: usize) {
    let (raw, truth) = truth_of(file);
    let balance = balance_of(&raw);
    let (source_width, source_height) = (truth.width as f64, truth.height as f64);
    let size = [9504usize, 6336];
    let source_focal = source_width / (2.0 * 40f64.to_radians().tan());
    // A view a tenth of the photo's width, stretched up to 61 MP, overlapping its neighbours by
    // a third. It's soft, but memory doesn't care, and a soft frame costs what a sharp one does.
    let view_share = (0.85 / (1.0 + (count as f64 - 1.0) * 0.67)).min(0.6);
    let across = (view_share * source_width * 0.5 / source_focal).atan() * 2.0;
    let view_focal = size[0] as f64 * 0.5 / (across * 0.5).tan();
    let height_share = size[1] as f64 / view_focal * source_focal / source_height;
    assert!(height_share < 1.0, "views are taller than the photo: {height_share}");
    let step = across * 0.67;
    let views: Vec<View> = (0..count)
        .map(|index| View {
            camera: yaw_pitch((index as f64 - (count - 1) as f64 * 0.5) * step, 0.002 * index as f64),
            stops: 0.0,
        })
        .collect();
    let reduced_size = [3000usize, 2000];
    let reduced_focal = view_focal * 3000.0 / size[0] as f64;
    let frames: Vec<Frame> = views
        .iter()
        .enumerate()
        .map(|(index, view)| {
            let scene = Scene {
                truth: &truth,
                source_focal,
                view_focal: reduced_focal,
                size: reduced_size,
                lens: SimulatedLens { barrel: 0.0, falloff: 0.0, announced: false },
            };
            let metadata = unlight_raw::Metadata {
                focal_length_35mm: (43.27 * view_focal / (size[0] as f64).hypot(size[1] as f64)) as f32,
                ..unlight_raw::Metadata::default()
            };
            Frame {
                path: PathBuf::from(format!("generated-{index}")),
                width: size[0],
                height: size[1],
                metadata,
                color: raw.color.clone(),
                opcodes: Opcodes::default(),
                embedded_lens: None,
                balance,
                reduced: render_view(&scene, view),
            }
        })
        .collect();
    println!("{count} frames of {}×{} ({:.0} MP), peak so far {:.2} GiB", size[0], size[1], (size[0] * size[1]) as f64 / 1e6, peak_gib());
    let source = Generated { frames, views, truth: &truth, source_focal, view_focal, size };
    let start = Instant::now();
    let options = PanoramaOptions { auto_crop: true, ..PanoramaOptions::default() };
    let (merged, report) = merge_panorama(&source, &options, &Progress::default()).expect("stitch");
    println!(
        "stitched {}×{} ({:.0} MP) at {:.0}% scale in {:.1} s, used {}, rms {:.2} px",
        merged.image.width,
        merged.image.height,
        (merged.image.width * merged.image.height) as f64 / 1e6,
        report.scale * 100.0,
        start.elapsed().as_secs_f32(),
        report.used.len(),
        report.rms,
    );
    println!("peak memory {:.2} GiB, of which the source photo is {:.2} GiB", peak_gib(), (truth.pixels.len() * 12) as f64 / (1u64 << 30) as f64);
    drop(source);
    drop(truth);
    if let Some(directory) = std::env::args().nth(4) {
        let directory = PathBuf::from(directory);
        let file = merged_raw(&merged, &directory, &format!("big-{count}.ulr"));
        drop(merged);
        let start = Instant::now();
        let image = render_at(&file, &develop(|r| r.basic.shadows = 30.0));
        println!(
            "rendered {}×{} through the engine in {:.1} s, {} tiles, {} device losses, finite: {}",
            image.width,
            image.height,
            start.elapsed().as_secs_f32(),
            image.tiles,
            image.device_losses,
            image.pixels.iter().all(|value| value.is_finite())
        );
        println!("peak memory with the render {:.2} GiB", peak_gib());
    }
}

fn decode_srgb(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.040_45 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
}

fn encode_srgb(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let encoded = if value <= 0.003_130_8 { value * 12.92 } else { 1.055 * value.powf(1.0 / 2.4) - 0.055 };
    (encoded * 255.0 + 0.5) as u8
}

/// A photograph as linear light, undoing the sRGB curve, which is the nearest thing to what the
/// sensor saw that an 8-bit file can offer.
fn read_photograph(path: &Path) -> Image {
    let bytes = std::fs::read(path).expect("reading photograph");
    let extension = path.extension().unwrap_or_default().to_string_lossy().to_lowercase();
    if extension == "png" {
        let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
        let mut reader = decoder.read_info().expect("png header");
        let mut buffer = vec![0; reader.output_buffer_size().expect("png size")];
        let info = reader.next_frame(&mut buffer).expect("png data");
        let stride = info.color_type.samples();
        let (width, height) = (info.width as usize, info.height as usize);
        return Image::from_fn(width, height, |x, y| {
            let at = (y * width + x) * stride;
            if stride >= 3 {
                [buffer[at], buffer[at + 1], buffer[at + 2]].map(decode_srgb)
            } else {
                [decode_srgb(buffer[at]); 3]
            }
        });
    }
    let options = zune_core::options::DecoderOptions::default().jpeg_set_out_colorspace(zune_core::colorspace::ColorSpace::RGB);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(std::io::Cursor::new(&bytes), options);
    let pixels = decoder.decode().expect("jpeg data");
    let (width, height) = decoder.dimensions().expect("jpeg size");
    Image::from_fn(width, height, |x, y| {
        let at = (y * width + x) * 3;
        [pixels[at], pixels[at + 1], pixels[at + 2]].map(decode_srgb)
    })
}

fn photograph_frame(path: &Path, image: &Image, shutter: f32) -> Frame {
    Frame {
        path: path.to_owned(),
        width: image.width,
        height: image.height,
        metadata: unlight_raw::Metadata { shutter, ..unlight_raw::Metadata::default() },
        color: unlight_raw::ColorData {
            xyz_to_camera: [[3.2404542, -1.5371385, -0.4985314], [-0.969266, 1.8760108, 0.041556], [0.0556434, -0.2040259, 1.0572252]],
            as_shot_multipliers: [1.0; 3],
            daylight_multipliers: [1.0; 3],
            baseline_exposure: 0.0,
            dng: None,
        },
        opcodes: Opcodes::default(),
        embedded_lens: None,
        balance: [1.0; 3],
        reduced: image.fitted(unlight_merge::ANALYSIS_EDGE),
    }
}

/// Photographs in memory, one per frame.
struct Photographs {
    frames: Vec<Frame>,
    images: Vec<Image>,
}

impl FrameSource for Photographs {
    fn frames(&self) -> &[Frame] {
        &self.frames
    }

    fn full(&self, index: usize) -> Result<Image> {
        Ok(self.images[index].clone())
    }
}

fn save_linear(path: &Path, image: &Image, max_edge: usize, stops: f32) {
    let small = image.fitted(max_edge);
    let gain = stops.exp2();
    let data: Vec<u8> = small.pixels.iter().flat_map(|pixel| pixel.map(|value| encode_srgb(value * gain))).collect();
    save_montage(path, &[(small.width, small.height, data)], 1);
}

fn evaluate_real_panorama(folder: &Path, directory: &Path) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(folder).expect("folder").flatten().map(|entry| entry.path()).collect();
    paths.sort();
    let images: Vec<Image> = paths.iter().map(|path| read_photograph(path)).collect();
    // Some of these sets are cropped by hand to slightly different sizes, which a merge refuses.
    let width = images.iter().map(|image| image.width).min().unwrap_or(0);
    let height = images.iter().map(|image| image.height).min().unwrap_or(0);
    let images: Vec<Image> = images
        .iter()
        .map(|image| {
            let (left, top) = ((image.width - width) / 2, (image.height - height) / 2);
            Image::from_fn(width, height, |x, y| image.at(x + left, y + top))
        })
        .collect();
    let frames = paths.iter().zip(&images).map(|(path, image)| photograph_frame(path, image, 0.0)).collect();
    println!("{} frames of {}×{} from {}", paths.len(), images[0].width, images[0].height, folder.display());
    let set = Photographs { frames, images };
    let name = folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
    for (tag, projection) in [("spherical", Projection::Spherical), ("cylindrical", Projection::Cylindrical)] {
        let start = Instant::now();
        let options = PanoramaOptions { projection, auto_crop: false, ..PanoramaOptions::default() };
        match merge_panorama(&set, &options, &Progress::default()) {
            Ok((merged, report)) => {
                println!(
                    "{tag}: {}×{} in {:.1} s, used {}/{}, focal {:.0} px, rms {:.3} px, closed {}, vignette {:?}",
                    merged.image.width,
                    merged.image.height,
                    start.elapsed().as_secs_f32(),
                    report.used.len(),
                    paths.len(),
                    report.focal,
                    report.rms,
                    report.closed_loop,
                    report.vignette,
                );
                let spread: Vec<f32> = report.gains.iter().map(|gain| gain[1].log2()).collect();
                println!("  gains in stops {spread:.3?}");
                save_linear(&directory.join(format!("{name}-{tag}.png")), &merged.image, 1800, 0.0);
            }
            Err(error) => println!("{tag}: failed: {error}"),
        }
    }
    let options = PanoramaOptions { boundary_warp: 1.0, auto_crop: true, ..PanoramaOptions::default() };
    match merge_panorama(&set, &options, &Progress::default()) {
        Ok((merged, report)) => {
            println!("warped and cropped: {}×{}, empty {:.3}%", merged.image.width, merged.image.height, report.empty_share * 100.0);
            save_linear(&directory.join(format!("{name}-warped.png")), &merged.image, 1800, 0.0);
        }
        Err(error) => println!("warped: failed: {error}"),
    }
}

fn evaluate_real_hdr(folder: &Path, directory: &Path) {
    let list = std::fs::read_to_string(folder.join("list.txt")).expect("list of exposures");
    let entries: Vec<(PathBuf, f32)> = list
        .lines()
        .filter_map(|line| {
            let (name, inverse) = line.split_once(' ')?;
            Some((folder.join(name), 1.0 / inverse.trim().parse::<f32>().ok()?))
        })
        .collect();
    // Every other frame, so that neighbours are two stops apart, the way a real bracket is.
    let picked: Vec<&(PathBuf, f32)> = entries.iter().step_by(2).collect();
    let images: Vec<Image> = picked.iter().map(|(path, _)| read_photograph(path)).collect();
    let frames = picked.iter().zip(&images).map(|((path, shutter), image)| photograph_frame(path, image, *shutter)).collect();
    println!("{} frames of {}×{}", picked.len(), images[0].width, images[0].height);
    let set = Photographs { frames, images };
    let start = Instant::now();
    let (merged, report) = merge_hdr(&set, &HdrOptions::default(), &Progress::default()).expect("merge");
    println!("merged in {:.2} s, reference {}, headroom {:.2} EV, clipped everywhere {:.3}%", start.elapsed().as_secs_f32(), report.reference, report.headroom, report.clipped_share * 100.0);
    for (index, estimate) in report.exposures.iter().enumerate() {
        println!("  frame {index}: file {:+.2} EV, measured {:?}, used {:+.2} EV ({:?})", estimate.stated.unwrap_or(0.0), estimate.measured, estimate.used, estimate.source);
    }
    for (index, matrix) in report.alignments.iter().enumerate() {
        println!("  frame {index}: shift {:+.2}, {:+.2} px", matrix[0][2], matrix[1][2]);
    }
    for (tag, stops) in [("dark", -5.0), ("base", 0.0), ("bright", 4.0)] {
        save_linear(&directory.join(format!("memorial-{tag}.png")), &merged.image, 900, stops);
    }
    let reference = &set.images[report.reference];
    save_linear(&directory.join("memorial-reference.png"), reference, 900, 0.0);
}

/// Cuts a window out of a montage, enlarged, for looking closely at one spot.
fn crop_png(file: &Path, arguments: &[String]) {
    let number = |index: usize| arguments[index].parse::<usize>().expect("x y width height zoom out.png");
    let (left, top, width, height, zoom) = (number(0), number(1), number(2), number(3), number(4));
    let image = read_photograph(file);
    let tile = Image::from_fn(width * zoom, height * zoom, |x, y| {
        image.at((left + x / zoom).min(image.width - 1), (top + y / zoom).min(image.height - 1))
    });
    let data: Vec<u8> = tile.pixels.iter().flat_map(|pixel| pixel.map(encode_srgb)).collect();
    save_montage(Path::new(&arguments[5]), &[(tile.width, tile.height, data)], 1);
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let usage = "evaluate hdr <raw> <dir> | evaluate pano <raw> <dir> [--lens] | evaluate memory <raw> <frames>";
    let (mode, file) = (arguments.first().expect(usage), arguments.get(1).expect(usage));
    match mode.as_str() {
        "hdr" => evaluate_hdr(Path::new(file), Path::new(arguments.get(2).expect(usage))),
        "pano" => evaluate_panorama(
            Path::new(file),
            Path::new(arguments.get(2).expect(usage)),
            arguments.iter().any(|argument| argument == "--lens"),
        ),
        "memory" => evaluate_memory(Path::new(file), arguments.get(2).expect(usage).parse().expect(usage)),
        "real-pano" => evaluate_real_panorama(Path::new(file), Path::new(arguments.get(2).expect(usage))),
        "crop" => crop_png(Path::new(file), &arguments[2..]),
        "real-hdr" => evaluate_real_hdr(Path::new(file), Path::new(arguments.get(2).expect(usage))),
        _ => panic!("{usage}"),
    }
}
