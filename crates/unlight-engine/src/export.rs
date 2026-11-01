//! Full-resolution rendering on a dedicated device. A lost device only costs the tile in
//! flight: a fresh device takes over and already-finished tiles are kept.

use crate::{
    error::{EngineError, Result},
    gpu::GpuContext,
    params::MAX_TILE_EDGE,
    pipelines::{Engine, Precision},
    scheduler::{SchedulerConfig, Tile, TileScheduler},
    masks::MaskRasterSource,
    session::{SceneViews, Session},
    tools,
};
use rayon::prelude::*;
use unlight_color::output::OutputSpace;
use unlight_raw::RawImage;
use unlight_recipe::Recipe;

const RGBA32F_BYTES: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FaultInjection {
    None,
    /// Lose the device once, after this many develop tiles.
    LoseDeviceOnceAfter(u32),
    /// Lose the device on every attempt, after this many develop tiles.
    LoseDeviceAlwaysAfter(u32),
}

#[derive(Clone, Copy, Debug)]
pub struct ExportOptions {
    pub precision: Precision,
    pub output_space: OutputSpace,
    pub schedule: SchedulerConfig,
    pub max_recoveries: u32,
    pub fault: FaultInjection,
    /// Where `MaskShape::Raster` finds its data.
    pub mask_rasters: Option<&'static dyn MaskRasterSource>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            precision: Precision::F32,
            output_space: OutputSpace::Srgb,
            schedule: SchedulerConfig::BACKGROUND,
            max_recoveries: 3,
            fault: FaultInjection::None,
            mask_rasters: None,
        }
    }
}

/// Interleaved RGB, encoded for the requested output space.
#[derive(Clone, Debug)]
pub struct ExportedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<f32>,
    pub device_losses: u32,
    pub tiles: u32,
}

struct Attempt<'a> {
    raw: &'a RawImage,
    recipe: &'a Recipe,
    options: &'a ExportOptions,
    lose_after: Option<u32>,
    denoised_scene: Option<&'a [[f32; 4]]>,
}

fn copy_tile(pixels: &mut [f32], image_width: u32, tile: Tile, stride: u32, data: &[u8]) {
    let image_width = image_width as usize;
    let tile_width = tile.width as usize;
    let rows = pixels.par_chunks_mut(image_width * 3).skip(tile.y as usize).take(tile.height as usize);
    rows.enumerate().for_each(|(row, line)| {
        let start = row * stride as usize;
        let source = &data[start..start + tile_width * RGBA32F_BYTES as usize];
        let target = &mut line[tile.x as usize * 3..(tile.x as usize + tile_width) * 3];
        for (pixel, texel) in target.as_chunks_mut::<3>().0.iter_mut().zip(source.as_chunks::<16>().0) {
            let values: &[f32] = bytemuck::cast_slice(texel.as_slice());
            pixel.copy_from_slice(&values[..3]);
        }
    });
}

fn run_attempt(
    attempt: &Attempt,
    scheduler: &mut TileScheduler,
    pixels: &mut [f32],
    completed: &mut u32,
) -> Result<()> {
    let (raw, recipe) = (attempt.raw, attempt.recipe);
    let ctx = GpuContext::new()?;
    let engine = Engine::new(ctx, attempt.options.precision)?;
    let schedule = attempt.options.schedule;
    let mut session = Session::open(engine, raw, recipe)?;
    session.set_mask_rasters(attempt.options.mask_rasters);
    session.sync(recipe)?;
    let [output_width, output_height] = session.output_size(recipe);

    let image = session.engine.storage_texture(
        "scene image",
        [raw.width, raw.height, 1],
        1,
        attempt.options.precision.format(),
    );
    let image_view = image.create_view(&Default::default());
    session.engine.demosaic(&session.source, &image_view, schedule)?;
    let repaired = session.repaired_image(&image_view, schedule)?;
    let rawer = repaired.as_ref().unwrap_or(&image_view);
    let denoised = match attempt.denoised_scene {
        Some(scene) => Some(session.engine.upload_scene(scene, [raw.width, raw.height])),
        None => session.denoise_image(&recipe.detail.noise_reduction, rawer, schedule)?,
    };
    let cleaner = session.cleaner_image(recipe, rawer, schedule)?;
    let scene_view = denoised.as_ref().unwrap_or(rawer);
    let scene = SceneViews { image: scene_view, cleaner: cleaner.as_ref().unwrap_or(scene_view), rawer };

    let aspect = raw.width as f32 / raw.height as f32;
    let step = [1.0 / output_width as f32, 1.0 / output_height as f32];
    let mut develop = tools::develop_params(&session.source, recipe, &session.stats, &session.optics);
    develop.view_step = step;
    let mut finish = tools::finish_params(recipe, aspect, 1.0);
    tools::apply_output_space(&mut finish, attempt.options.output_space);
    finish.view_step = step;
    finish.dither = 0.0;
    session.apply_masks(recipe, &mut develop, &mut finish, 1.0);
    session.write_develop(&develop, &finish);

    let developed = session.develop_tile_view();
    let blurred_rows = session.develop_tile_view();
    let local_results = session.local_tile_view();
    let tile_output = session.engine.storage_texture(
        "export tile",
        [MAX_TILE_EDGE, MAX_TILE_EDGE, 1],
        1,
        wgpu::TextureFormat::Rgba32Float,
    );
    let tile_view = tile_output.create_view(&Default::default());
    let develop_group = session.develop_group(&scene, &developed, &local_results);
    let sharpen_group = session.sharpen_group(&developed, &blurred_rows);
    let finish_group = session.finish_group(
        &session.engine.pipelines.finish_export,
        &developed,
        &blurred_rows,
        &local_results,
        &tile_view,
    );

    let engine = &session.engine;
    let stride = MAX_TILE_EDGE * RGBA32F_BYTES;
    let readback = engine.ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("export readback"),
        size: u64::from(stride) * u64::from(MAX_TILE_EDGE),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let pad = Session::pad();
    let mut tiles_this_attempt = 0;
    while let Some(tile) = scheduler.next_tile() {
        if attempt.lose_after == Some(tiles_this_attempt) {
            engine.ctx.simulate_device_loss();
        }
        let result = (|| {
            engine.write_tile(&session.tile_params(tile, pad));
            let mut encoder = engine.encoder();
            let padded = [tile.width + 2 * pad, tile.height + 2 * pad];
            let inner = [tile.width, tile.height];
            let pipelines = &engine.pipelines;
            engine.dispatch(&mut encoder, &pipelines.develop, &develop_group, padded, "develop", None);
            if finish.sharpen_amount > 0.0 || finish.local_detail > 0.0 {
                engine.dispatch(&mut encoder, &pipelines.sharpen_rows, &sharpen_group, padded, "sharpen rows", None);
            }
            engine.dispatch(&mut encoder, &pipelines.finish_export, &finish_group, inner, "finish", None);
            encoder.copy_texture_to_buffer(
                tile_output.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(stride),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d { width: tile.width, height: tile.height, depth_or_array_layers: 1 },
            );
            let elapsed = engine.submit_and_wait(encoder)?;
            let bytes = engine.map_read(&readback)?;
            Ok::<_, EngineError>((elapsed, bytes))
        })();
        match result {
            Ok((elapsed, bytes)) => {
                copy_tile(pixels, output_width, tile, stride, &bytes);
                scheduler.record(tile, elapsed);
                tiles_this_attempt += 1;
                *completed += 1;
            }
            Err(error) => {
                scheduler.requeue(tile);
                return Err(error);
            }
        }
    }
    Ok(())
}

/// The photo after demosaicing and repairs but before noise reduction, as linear RGBA, for a
/// denoiser that runs outside the engine.
pub fn read_scene(raw: &RawImage, recipe: &Recipe) -> Result<Vec<[f32; 4]>> {
    let schedule = SchedulerConfig::BACKGROUND;
    let engine = Engine::new(GpuContext::new()?, Precision::F32)?;
    let mut session = Session::open(engine, raw, recipe)?;
    session.sync(recipe)?;
    let size = [raw.width, raw.height];
    let image = session.engine.storage_texture("scene image", [size[0], size[1], 1], 1, Precision::F32.format());
    let view = image.create_view(&Default::default());
    session.engine.demosaic(&session.source, &view, schedule)?;
    let repaired = session.repaired_image(&view, schedule)?;
    session.engine.read_scene(repaired.as_ref().unwrap_or(&view).texture(), size)
}

/// A small copy of a photo, from its preview pyramid.
#[derive(Clone, Debug)]
pub struct ReducedScene {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[f32; 3]>,
}

/// The photo at a long edge of `max_edge` or less, or the pyramid's smallest level if that's
/// bigger, on the same scale and in the same colour as `read_scene` but without paying for a
/// demosaic. It comes from a device of its own, so a merge can't disturb the editor.
pub fn read_reduced(raw: &RawImage, max_edge: u32) -> Result<ReducedScene> {
    let schedule = SchedulerConfig::BACKGROUND;
    let engine = Engine::new(GpuContext::new()?, Precision::F32)?;
    let source = engine.load(raw, schedule)?;
    let preview = engine.build_preview(&source, schedule)?;
    let last = preview.levels - 1;
    let level = (0..last).find(|&level| preview.width.max(preview.height) >> level <= max_edge).unwrap_or(last);
    let (width, height) = ((preview.width >> level).max(1), (preview.height >> level).max(1));
    let pixels = engine.read_preview_region(&preview, level, [0, 0, width, height])?;
    Ok(ReducedScene { width, height, pixels })
}

/// Renders a photo at full resolution, recovering from lost devices along the way.
pub fn export(raw: &RawImage, recipe: &Recipe, options: &ExportOptions) -> Result<ExportedImage> {
    render(raw, recipe, options, None)
}

/// Like `export`, with `denoised_scene` taking the place of the engine's noise reduction. It is
/// a result of processing what `read_scene` returned.
pub fn export_with_scene(
    raw: &RawImage,
    recipe: &Recipe,
    options: &ExportOptions,
    denoised_scene: &[[f32; 4]],
) -> Result<ExportedImage> {
    render(raw, recipe, options, Some(denoised_scene))
}

fn render(
    raw: &RawImage,
    recipe: &Recipe,
    options: &ExportOptions,
    denoised_scene: Option<&[[f32; 4]]>,
) -> Result<ExportedImage> {
    let schedule = SchedulerConfig {
        max_edge: options.schedule.max_edge.min(MAX_TILE_EDGE),
        ..options.schedule
    };
    let crop = &recipe.crop;
    let width = (((crop.right - crop.left) * raw.width as f32).round() as u32).max(1);
    let height = (((crop.bottom - crop.top) * raw.height as f32).round() as u32).max(1);
    let mut scheduler = TileScheduler::new(width, height, schedule);
    let mut pixels = vec![0.0f32; width as usize * height as usize * 3];
    let mut tiles = 0;
    let mut device_losses = 0;

    loop {
        let lose_after = match options.fault {
            FaultInjection::None => None,
            FaultInjection::LoseDeviceOnceAfter(count) => (device_losses == 0).then_some(count),
            FaultInjection::LoseDeviceAlwaysAfter(count) => Some(count),
        };
        let attempt = Attempt { raw, recipe, options, lose_after, denoised_scene };
        match run_attempt(&attempt, &mut scheduler, &mut pixels, &mut tiles) {
            Ok(()) => {
                return Ok(ExportedImage {
                    width,
                    height,
                    pixels,
                    device_losses,
                    tiles,
                });
            }
            Err(EngineError::DeviceLost(_)) if device_losses < options.max_recoveries => {
                device_losses += 1;
            }
            Err(EngineError::DeviceLost(_)) => {
                return Err(EngineError::RecoveryExhausted(device_losses + 1));
            }
            Err(error) => return Err(error),
        }
    }
}
