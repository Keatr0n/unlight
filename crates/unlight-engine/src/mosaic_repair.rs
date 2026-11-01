//! Repairs the linearized mosaic before demosaicing: hot pixels first, then clipped highlights.
//! Clipped sites arrive negated, so one texture carries both their value and the clip mask.

use crate::{
    demosaic::entry,
    error::Result,
    params::{ExpandParams, HighlightParams, MOSAIC_BORDER, TileParams, cfa_params},
    pipelines::Engine,
    scheduler::{SchedulerConfig, Tile},
};
use rayon::prelude::*;
use unlight_raw::RawImage;
use wgpu::util::DeviceExt;

const FIELD_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The colour field stops one level short of the seed. It's smooth, so it loses nothing, and
/// that finest level alone would cost as much as all the others together.
const FIELD_FINEST: u32 = 1;
/// How much support the coarser estimate counts for, in units of a fully supported cell.
const PRIOR_WEIGHT: f32 = 0.5;
/// At the top there is no coarser estimate, so a cell's own colour stands however weak it is.
const TOP_PRIOR_WEIGHT: f32 = 1e-3;

/// Runs down to a single cell, the photo's overall highlight colour, which is the fallback for
/// any area with no support of its own.
fn field_levels(width: u32, height: u32) -> u32 {
    (width.max(height).max(1).ilog2() + 1).max(FIELD_FINEST + 1)
}

pub(crate) fn level_view(texture: &wgpu::Texture, level: u32) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        base_mip_level: level,
        mip_level_count: Some(1),
        ..Default::default()
    })
}

/// Whether any site reaches `clip`, on the same 0 to 1 scale the mosaic is normalised to.
fn has_clipped_sites(raw: &RawImage, clip: f32) -> bool {
    let black = raw.black.iter().flatten().copied().fold(f32::MAX, f32::min);
    let threshold = black + (raw.white - black) * clip;
    let brightest = raw.samples.par_chunks(1 << 16).map(|chunk| chunk.iter().copied().max().unwrap_or(0)).max();
    brightest.is_some_and(|sample| f32::from(sample) >= threshold)
}

impl Engine {
    /// Returns the finished mosaic, with hot pixels gone and clipped sites rebuilt.
    pub(crate) fn repair_mosaic(
        &self,
        raw: &RawImage,
        (balance, clip): ([f32; 4], f32),
        linear: &wgpu::TextureView,
        schedule: SchedulerConfig,
    ) -> Result<wgpu::TextureView> {
        // These passes are cheap per pixel, so starting small would only add round trips.
        let schedule = SchedulerConfig { initial_edge: schedule.max_edge, ..schedule };
        let bordered = [raw.width + 2 * MOSAIC_BORDER, raw.height + 2 * MOSAIC_BORDER];
        let mosaic_view = |label| {
            self.storage_texture(label, [bordered[0], bordered[1], 1], 1, wgpu::TextureFormat::R32Float)
                .create_view(&Default::default())
        };
        let cfa = cfa_params(&raw.cfa);
        let params = |tile: Tile, origin: [i32; 2], image: [u32; 2]| TileParams {
            origin: [tile.x as i32 + origin[0], tile.y as i32 + origin[1]],
            size: [tile.width as i32, tile.height as i32],
            image: [image[0] as i32, image[1] as i32],
            pad: 0,
            period: raw.cfa.period as i32,
            cfa,
        };
        let image = [raw.width, raw.height];
        let around = [-(MOSAIC_BORDER as i32); 2];

        let despeckled = mosaic_view("despeckled mosaic");
        let p = &self.pipelines;
        let group = self.bind(&p.hot_pixels, &[self.tile_entry(), entry(1, linear), entry(2, &despeckled)]);
        self.run_tiles(bordered[0], bordered[1], schedule, |encoder, tile| {
            self.write_tile(&params(tile, around, image));
            self.dispatch(encoder, &p.hot_pixels, &group, [tile.width, tile.height], "hot pixels", None);
        })?;
        if !has_clipped_sites(raw, clip) {
            return Ok(despeckled);
        }

        let factor = if raw.cfa.is_xtrans() { 3 } else { 2 };
        let field_size = [(raw.width / factor).max(1), (raw.height / factor).max(1)];
        let levels = field_levels(field_size[0], field_size[1]);
        let level_size = |level: u32| [(field_size[0] >> level).max(1), (field_size[1] >> level).max(1)];
        let pushed = self.storage_texture("highlight pushed", [field_size[0], field_size[1], 1], levels, FIELD_FORMAT);
        let filled_size = level_size(FIELD_FINEST);
        let filled = self.storage_texture(
            "highlight filled",
            [filled_size[0], filled_size[1], 1],
            levels - FIELD_FINEST,
            FIELD_FORMAT,
        );

        let seed_buffer = self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("highlight params"),
            contents: bytemuck::bytes_of(&HighlightParams { balance }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let seed_target = level_view(&pushed, 0);
        let group = self.bind(
            &p.highlight_seed,
            &[
                self.tile_entry(),
                wgpu::BindGroupEntry { binding: 1, resource: seed_buffer.as_entire_binding() },
                entry(2, &despeckled),
                entry(3, &seed_target),
            ],
        );
        self.run_tiles(field_size[0], field_size[1], schedule, |encoder, tile| {
            self.write_tile(&params(tile, [0, 0], image));
            self.dispatch(encoder, &p.highlight_seed, &group, [tile.width, tile.height], "highlight seed", None);
        })?;

        for level in 1..levels {
            let (from, to) = (level_view(&pushed, level - 1), level_view(&pushed, level));
            let group = self.bind(&p.highlight_reduce, &[self.tile_entry(), entry(1, &from), entry(2, &to)]);
            let [width, height] = level_size(level);
            self.run_tiles(width, height, schedule, |encoder, tile| {
                self.write_tile(&params(tile, [0, 0], image));
                self.dispatch(encoder, &p.highlight_reduce, &group, [tile.width, tile.height], "highlight reduce", None);
            })?;
        }

        let expand_buffer = |prior_weight| {
            self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("highlight expand params"),
                contents: bytemuck::bytes_of(&ExpandParams { prior_weight, padding: [0.0; 3] }),
                usage: wgpu::BufferUsages::UNIFORM,
            })
        };
        let (inner_prior, top_prior) = (expand_buffer(PRIOR_WEIGHT), expand_buffer(TOP_PRIOR_WEIGHT));
        for level in (FIELD_FINEST..levels).rev() {
            let is_top = level + 1 == levels;
            let coarse = if is_top { level_view(&pushed, level) } else { level_view(&filled, level + 1 - FIELD_FINEST) };
            let (here, to) = (level_view(&pushed, level), level_view(&filled, level - FIELD_FINEST));
            let prior = if is_top { &top_prior } else { &inner_prior };
            let group = self.bind(
                &p.highlight_expand,
                &[
                    self.tile_entry(),
                    entry(1, &here),
                    entry(2, &coarse),
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    wgpu::BindGroupEntry { binding: 4, resource: prior.as_entire_binding() },
                    entry(5, &to),
                ],
            );
            let [width, height] = level_size(level);
            self.run_tiles(width, height, schedule, |encoder, tile| {
                self.write_tile(&params(tile, [0, 0], image));
                self.dispatch(encoder, &p.highlight_expand, &group, [tile.width, tile.height], "highlight expand", None);
            })?;
        }

        let mosaic = mosaic_view("mosaic");
        let color_field = filled.create_view(&wgpu::TextureViewDescriptor {
            mip_level_count: Some(1),
            ..Default::default()
        });
        let group = self.bind(
            &p.highlight_fill,
            &[
                self.tile_entry(),
                entry(1, &despeckled),
                entry(2, &color_field),
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                entry(4, &mosaic),
            ],
        );
        self.run_tiles(bordered[0], bordered[1], schedule, |encoder, tile| {
            self.write_tile(&params(tile, around, image));
            self.dispatch(encoder, &p.highlight_fill, &group, [tile.width, tile.height], "highlight fill", None);
        })?;
        Ok(mosaic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_colour_field_keeps_a_usable_top_level() {
        assert_eq!(field_levels(4784, 3188), 13);
        assert_eq!(field_levels(16, 16), 5);
        assert_eq!(field_levels(1, 1), 2);
    }
}
