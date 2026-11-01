//! Noise reduction, stage C2. A pyramid built on the preview mips handles everything coarser
//! than a couple of source pixels, including all of the blotchy chroma noise. It is whole-image
//! and shared, so a fit-to-screen preview and an export use the very same coarse result, and a
//! slider only has to re-solve down to the level on screen. Full resolution adds the fine band
//! in tiles, with non-local means for luminance. Everything here is cached against the noise
//! settings, so no other slider re-runs it.

use crate::{
    demosaic::entry,
    error::Result,
    mosaic_repair::level_view,
    noise::NoiseModel,
    params::{DenoiseParams, MAX_TILE_EDGE, MeansParams, StabiliseParams, TILE_PAD, TileParams, cfa_params},
    pipelines::Engine,
    scheduler::{SchedulerConfig, Tile},
    source::{Preview, SourceImage},
};
use unlight_recipe::NoiseReduction;
use wgpu::util::DeviceExt;

const SOLVED_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Sites of each colour in a preview pixel, which sets how much binning quietened it.
const BAYER_SITES: [f32; 3] = [1.0, 2.0, 1.0];
const XTRANS_SITES: [f32; 3] = [2.0, 5.0, 2.0];
/// A merged photo's preview pixel averages four whole pixels, each with every colour.
const FULL_COLOUR_SITES: [f32; 3] = [4.0; 3];
/// Luma's variance over what independent channels would give. Demosaicing correlates them,
/// and correlated noise doesn't average away in the luma mix.
const LUMA_CORRELATION: f32 = 2.5;
/// The opponent luma mix, the same as `LUMA` in the shaders.
const LUMA_MIX: [f32; 3] = [0.25, 0.5, 0.25];

const CHROMA_THRESHOLD: f32 = 4.0;
const LUMA_THRESHOLD: f32 = 2.5;
/// Share of the patches' variance that non-local means treats as noise and forgives.
const NOISE_ALLOWANCE: f32 = 0.6;
/// How far the patch distance may go past that allowance, as a multiple of it, at full strength.
const MAX_SIMILARITY: f32 = 2.2;
const FEATURES: f32 = 7.0;
/// Non-local means takes out most noise as soon as it has any reach at all, so the low end of
/// the slider fades the result in. Past this strength, the slider only widens what matches.
const FULL_EFFECT: f32 = 0.4;

pub(crate) struct NoiseFilter {
    solved: wgpu::Texture,
    pub view: wgpu::TextureView,
    levels: u32,
    settings: Option<NoiseReduction>,
    /// Finest level solved in full.
    solved_to: u32,
    /// Finest level with a usable result, which may be a quick one.
    usable_to: u32,
    /// How many levels have been solved, which only changing a noise setting should add to.
    pub solves: u64,
}

/// Everything a full-resolution denoise reads besides the image itself.
pub(crate) struct DenoiseJob<'a> {
    pub filter: &'a NoiseFilter,
    pub source: &'a SourceImage,
    pub preview: &'a Preview,
    pub settings: &'a NoiseReduction,
}

/// Whether these settings do anything to this photo.
pub(crate) fn is_active(settings: &NoiseReduction, noise: &NoiseModel) -> bool {
    (settings.luminance > 0.0 || settings.color > 0.0) && noise.variance(0.5) > 0.0
}

/// The chroma threshold. It reaches full effect by the default of 25, since colour noise is
/// meant to be gone without touching the slider, then climbs gently from there.
fn chroma_strength(settings: &NoiseReduction) -> f32 {
    let amount = settings.color / 25.0;
    CHROMA_THRESHOLD * amount.min(1.0).powi(2) * (1.0 + (amount - 1.0).max(0.0) * 0.5)
}

/// The noise expected in a level of the pyramid, as `shot · signal + read` per colour. `sites`
/// is how many raw sites of each colour were averaged into one of its pixels.
fn level_noise(source: &SourceImage, sites: [f32; 3]) -> ([f32; 4], [f32; 4]) {
    let balance = source.reference_neutral.map(|value| (1.0 / value) as f32);
    let model = source.noise;
    let shot = std::array::from_fn(|c| if c < 3 { model.shot * balance[c] / sites[c] } else { 0.0 });
    let read = std::array::from_fn(|c| if c < 3 { model.read * balance[c] * balance[c] / sites[c] } else { 0.0 });
    (shot, read)
}

/// How much quieter Bayer red and blue are at a level than their site count says. bin.wgsl gives
/// each site's share to neighbouring pixels at 3:1 along each axis, so a run of `n` pixels holds
/// `n - 0.375` sites' worth of variance rather than `n`. The difference fades up the pyramid.
/// The finest level is left out on purpose: counting it there leaves the preview's colour
/// noise above what a scaled-down export shows.
fn bayer_spread_gain(level: u32) -> f32 {
    if level == 0 {
        return 1.0;
    }
    let spread = 1.0 - 0.375 / 2f32.powi(level as i32);
    1.0 / (spread * spread)
}

fn preview_params(source: &SourceImage, settings: &NoiseReduction, level: u32, quick: bool) -> DenoiseParams {
    let base = if source.cfa.is_full_colour() {
        FULL_COLOUR_SITES
    } else if source.cfa.is_xtrans() {
        XTRANS_SITES
    } else {
        BAYER_SITES
    };
    let binned = 4f32.powi(level as i32);
    let mut sites = base.map(|sites| sites * binned);
    if !source.cfa.is_full_colour() && !source.cfa.is_xtrans() {
        sites[0] *= bayer_spread_gain(level);
        sites[2] *= bayer_spread_gain(level);
    }
    let (shot, read) = level_noise(source, sites);
    let luma = settings.luminance / 100.0;
    let protected_contrast = 1.0 - 0.8 * settings.luminance_contrast / 100.0;
    let fine_level = if level == 0 { 0.5 } else { 1.0 };
    // Coarse levels hold the broad colour mottling Smoothness is for. At 0 their threshold sits
    // below the noise, which leaves it in. The default clears the noise comfortably.
    let smoothing = 0.1 + 1.2 * settings.color_smoothness / 100.0;
    let chroma_shape = if level >= 1 { smoothing } else { 1.0 };
    DenoiseParams {
        shot,
        read,
        luma_threshold: LUMA_THRESHOLD * luma * luma * protected_contrast * fine_level,
        chroma_threshold: chroma_strength(settings) * chroma_shape,
        edge_color: 0.0,
        in_image: 1,
        quick: u32::from(quick),
        padding: [0; 3],
    }
}

fn full_params(source: &SourceImage, settings: &NoiseReduction, in_image: bool) -> DenoiseParams {
    let (shot, read) = level_noise(source, [1.0; 3]);
    DenoiseParams {
        shot,
        read,
        luma_threshold: 0.0,
        chroma_threshold: chroma_strength(settings),
        edge_color: settings.color_detail / 100.0,
        in_image: u32::from(in_image),
        quick: 0,
        padding: [0; 3],
    }
}

fn stabilise_params(source: &SourceImage) -> StabiliseParams {
    let (shot, read) = level_noise(source, [1.0; 3]);
    let weigh = |values: &[f32; 4]| -> f32 { (0..3).map(|c| values[c] * LUMA_MIX[c] * LUMA_MIX[c]).sum() };
    StabiliseParams {
        shot: weigh(&shot) * LUMA_CORRELATION,
        read: weigh(&read) * LUMA_CORRELATION,
        padding: [0.0; 2],
    }
}

fn means_params(settings: &NoiseReduction) -> MeansParams {
    let strength = settings.luminance / 100.0;
    let detail = settings.luminance_detail / 100.0;
    let allowance = 2.0 * FEATURES * NOISE_ALLOWANCE;
    let reach = MAX_SIMILARITY * strength * (1.4 - 0.8 * detail);
    MeansParams {
        scale: 1.0 / (allowance * reach.max(1e-3)),
        allowance: 1.0 / reach.max(1e-3),
        keep: (1.0 - strength / FULL_EFFECT).max(0.0) + 0.1 * detail,
        padding: 0.0,
    }
}

impl NoiseFilter {
    /// Drops the pyramid, for when the image under it changed but the settings didn't.
    pub(crate) fn invalidate(&mut self) {
        self.settings = None;
    }
}

impl Engine {
    pub(crate) fn create_noise_filter(&self, preview: &Preview) -> NoiseFilter {
        let solved = self.storage_texture(
            "denoised preview",
            [preview.width, preview.height, 1],
            preview.levels,
            SOLVED_FORMAT,
        );
        let view = solved.create_view(&Default::default());
        NoiseFilter {
            solved,
            view,
            levels: preview.levels,
            settings: None,
            solved_to: preview.levels,
            usable_to: preview.levels,
            solves: 0,
        }
    }

    /// Makes the pyramid current for `settings`, in full from the top down to `full`, then
    /// quickly, which leaves out luminance and colour detail, down to `quick`.
    pub(crate) fn solve_noise(
        &self,
        filter: &mut NoiseFilter,
        source: &SourceImage,
        preview: &Preview,
        settings: &NoiseReduction,
        [full, quick]: [u32; 2],
    ) -> Result<()> {
        if filter.settings.as_ref() != Some(settings) {
            filter.settings = Some(*settings);
            filter.solved_to = filter.levels;
            filter.usable_to = filter.levels;
        }
        let last = filter.levels - 1;
        let (full, quick) = (full.min(last), quick.min(full).min(last));
        if full < filter.solved_to {
            for level in (full..filter.solved_to).rev() {
                self.solve_level(filter, source, preview, settings, level, false)?;
            }
            // Any quick level below here was built on the result that just got replaced.
            filter.solved_to = full;
            filter.usable_to = full;
        }
        for level in (quick..full.min(filter.usable_to)).rev() {
            self.solve_level(filter, source, preview, settings, level, true)?;
        }
        filter.usable_to = filter.usable_to.min(quick);
        Ok(())
    }

    fn solve_level(
        &self,
        filter: &mut NoiseFilter,
        source: &SourceImage,
        preview: &Preview,
        settings: &NoiseReduction,
        level: u32,
        quick: bool,
    ) -> Result<()> {
        filter.solves += 1;
        let p = &self.pipelines;
        let cfa = cfa_params(&source.cfa);
        let period = source.cfa.period as i32;
        let top = level + 1 == filter.levels;
        let fine = level_view(preview.texture(), level);
        let coarse = level_view(preview.texture(), (level + 1).min(filter.levels - 1));
        let solved_coarse = if top { coarse.clone() } else { level_view(&filter.solved, level + 1) };
        let output = level_view(&filter.solved, level);
        let uniform = self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("denoise level params"),
            contents: bytemuck::bytes_of(&preview_params(source, settings, level, quick)),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let group = self.bind(
            &p.denoise_level,
            &[
                self.tile_entry(),
                wgpu::BindGroupEntry { binding: 1, resource: uniform.as_entire_binding() },
                entry(2, &fine),
                entry(3, &coarse),
                entry(4, &solved_coarse),
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                entry(6, &output),
            ],
        );
        let size = [(preview.width >> level).max(1), (preview.height >> level).max(1)];
        // Cheap per pixel, so starting small would only add round trips while a slider moves.
        let schedule = SchedulerConfig { initial_edge: MAX_TILE_EDGE, ..SchedulerConfig::BACKGROUND };
        self.run_tiles(size[0], size[1], schedule, |encoder, tile| {
            self.write_tile(&TileParams {
                origin: [tile.x as i32, tile.y as i32],
                size: [tile.width as i32, tile.height as i32],
                image: [size[0] as i32, size[1] as i32],
                pad: 0,
                period,
                cfa,
            });
            self.dispatch(encoder, &p.denoise_level, &group, [tile.width, tile.height], "denoise level", None);
        })
    }

    /// Denoises one region of a demosaiced image at full resolution. The pyramid must already
    /// be solved to level 0, and `demosaiced` must be valid for `TILE_PAD` around the region.
    pub(crate) fn denoise_region(
        &self,
        job: &DenoiseJob,
        demosaiced: &wgpu::TextureView,
        output: &wgpu::TextureView,
        region: Tile,
        schedule: SchedulerConfig,
    ) -> Result<()> {
        let DenoiseJob { filter, source, preview, settings } = *job;
        let p = &self.pipelines;
        let schedule = SchedulerConfig { max_edge: schedule.max_edge.min(MAX_TILE_EDGE), ..schedule };
        let low = level_view(preview.texture(), 0);
        let solved = level_view(&filter.solved, 0);
        let with_means = settings.luminance > 0.0;
        let cfa = cfa_params(&source.cfa);
        let tile_at = |tile: Tile, pad: u32| TileParams {
            origin: [(tile.x + region.x) as i32, (tile.y + region.y) as i32],
            size: [tile.width as i32, tile.height as i32],
            image: [source.width as i32, source.height as i32],
            pad: pad as i32,
            period: source.cfa.period as i32,
            cfa,
        };
        let buffer = |contents: &[u8]| {
            self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("denoise params"),
                contents,
                usage: wgpu::BufferUsages::UNIFORM,
            })
        };
        let level_uniform = buffer(bytemuck::bytes_of(&full_params(source, settings, !with_means)));
        let level_group = |written: &wgpu::TextureView| {
            self.bind(
                &p.denoise_full,
                &[
                    self.tile_entry(),
                    wgpu::BindGroupEntry { binding: 1, resource: level_uniform.as_entire_binding() },
                    entry(2, demosaiced),
                    entry(3, &low),
                    entry(4, &solved),
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    entry(6, written),
                ],
            )
        };

        if !with_means {
            let group = level_group(output);
            return self.run_tiles(region.width, region.height, schedule, |encoder, tile| {
                self.write_tile(&tile_at(tile, 0));
                self.dispatch(encoder, &p.denoise_full, &group, [tile.width, tile.height], "denoise fine", None);
            });
        }

        let extent = region.width.max(region.height).min(schedule.max_edge) + 2 * TILE_PAD;
        let tile_view = |label, format| {
            self.storage_texture(label, [extent, extent, 1], 1, format).create_view(&Default::default())
        };
        let corrected = tile_view("denoise corrected", self.precision.format());
        let first = tile_view("patch features a", wgpu::TextureFormat::Rgba32Float);
        let second = tile_view("patch features b", wgpu::TextureFormat::Rgba32Float);
        let stabilise = buffer(bytemuck::bytes_of(&stabilise_params(source)));
        let means = buffer(bytemuck::bytes_of(&means_params(settings)));
        let correction = level_group(&corrected);
        let patches = self.bind(
            &p.denoise_patch,
            &[
                self.tile_entry(),
                wgpu::BindGroupEntry { binding: 1, resource: stabilise.as_entire_binding() },
                entry(2, &corrected),
                entry(3, &first),
                entry(4, &second),
            ],
        );
        let average = self.bind(
            &p.denoise_nlm,
            &[
                self.tile_entry(),
                wgpu::BindGroupEntry { binding: 1, resource: means.as_entire_binding() },
                entry(2, &corrected),
                entry(3, &first),
                entry(4, &second),
                entry(5, output),
            ],
        );
        self.run_tiles(region.width, region.height, schedule, |encoder, tile| {
            self.write_tile(&tile_at(tile, TILE_PAD));
            let padded = [tile.width + 2 * TILE_PAD, tile.height + 2 * TILE_PAD];
            self.dispatch(encoder, &p.denoise_full, &correction, padded, "denoise fine", None);
            self.dispatch(encoder, &p.denoise_patch, &patches, padded, "denoise patches", None);
            self.dispatch(encoder, &p.denoise_nlm, &average, [tile.width, tile.height], "denoise means", None);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_active_without_noise_or_settings() {
        let noisy = NoiseModel { shot: 1e-4, read: 1e-6 };
        assert!(is_active(&NoiseReduction::default(), &noisy));
        assert!(!is_active(&NoiseReduction::default(), &NoiseModel::default()));
        let off = NoiseReduction { luminance: 0.0, color: 0.0, ..NoiseReduction::default() };
        assert!(!is_active(&off, &noisy));
    }

    #[test]
    fn chroma_strength_grows_with_the_slider() {
        let at = |color| chroma_strength(&NoiseReduction { color, ..NoiseReduction::default() });
        assert_eq!(at(0.0), 0.0);
        assert!(at(10.0) < at(25.0) && at(25.0) < at(100.0));
    }
}
