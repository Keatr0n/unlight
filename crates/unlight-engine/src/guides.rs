//! The fixed-size guide image behind Highlights, Shadows, Clarity and Dehaze. Solving the
//! filters at one size for every zoom and for export is what keeps preview and export in
//! agreement, because the coarse layers are literally the same data.

use crate::{
    error::Result,
    params::{GuideParams, SolveParams, wgsl_mat3},
    pipelines::Engine,
    source::Preview,
    tools::{ImageStats, LUMA_WEIGHTS},
};
use unlight_color::Mat3;
use wgpu::util::DeviceExt;

pub const GUIDE_EDGE: u32 = 1024;
/// Mip of the guide the base layer is solved at, about 64 px. Coarser means wider smoothing.
const BASE_LEVEL: u32 = 4;
const DETAIL_LEVEL: u32 = 3;
/// About 512 px, which puts Texture between sharpening's few pixels and Clarity's wide band.
const FINE_LEVEL: u32 = 1;
const LEVELS: u32 = BASE_LEVEL + 1;
const BASE_EPSILON: f32 = 0.5;
const DETAIL_EPSILON: f32 = 0.08;
const FINE_EPSILON: f32 = 0.05;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
const STATS_LOW_PERCENTILE: f64 = 0.002;
const STATS_HIGH_PERCENTILE: f64 = 0.998;

pub struct Guides {
    width: u32,
    height: u32,
    texture: wgpu::Texture,
    pub base: wgpu::TextureView,
    pub detail: wgpu::TextureView,
    pub fine: wgpu::TextureView,
}

/// A copy of the guide's top level for the CPU.
pub(crate) struct GuideImage {
    pub width: usize,
    pub height: usize,
    pub texels: Vec<[f32; 2]>,
}

fn level_size(width: u32, height: u32, level: u32) -> [u32; 2] {
    [(width >> level).max(1), (height >> level).max(1)]
}

impl Engine {
    pub(crate) fn create_guides(&self, image_width: u32, image_height: u32) -> Guides {
        let long_edge = image_width.max(image_height) as f32;
        let scale = GUIDE_EDGE as f32 / long_edge;
        let width = ((image_width as f32 * scale).round() as u32).max(1 << BASE_LEVEL);
        let height = ((image_height as f32 * scale).round() as u32).max(1 << BASE_LEVEL);
        let texture = self.storage_texture("guide", [width, height, 1], LEVELS, FORMAT);
        let solved = |label, level| {
            let [w, h] = level_size(width, height, level);
            self.storage_texture(label, [w, h, 1], 1, FORMAT).create_view(&Default::default())
        };
        Guides {
            base: solved("guide base", BASE_LEVEL),
            detail: solved("guide detail", DETAIL_LEVEL),
            fine: solved("guide fine", FINE_LEVEL),
            width,
            height,
            texture,
        }
    }

    /// Rebuilds the guide stack from the preview. Runs when white balance changes.
    pub(crate) fn update_guides(
        &self,
        guides: &Guides,
        preview: &Preview,
        camera_to_working: &Mat3,
    ) -> Result<()> {
        let device = &self.ctx.device;
        let level_view = |level: u32| {
            guides.texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        let size = [guides.width, guides.height];
        let scale = preview.width as f32 / guides.width as f32;
        let params = GuideParams {
            camera_to_working: wgsl_mat3(camera_to_working),
            luma: LUMA_WEIGHTS,
            size: [size[0] as f32, size[1] as f32],
            lod: scale.max(1.0).log2().min(preview.levels as f32 - 1.0),
            padding: 0.0,
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("guide params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let mut encoder = self.encoder();
        let seed_view = level_view(0);
        let seed = self.bind(
            &self.pipelines.guide_seed,
            &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&preview.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&seed_view) },
            ],
        );
        self.dispatch(&mut encoder, &self.pipelines.guide_seed, &seed, size, "guide seed", None);

        for level in 1..LEVELS {
            let (from, to) = (level_view(level - 1), level_view(level));
            let group = self.bind(
                &self.pipelines.guide_reduce,
                &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&from) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&to) },
                ],
            );
            let size = level_size(guides.width, guides.height, level);
            self.dispatch(&mut encoder, &self.pipelines.guide_reduce, &group, size, "guide reduce", None);
        }

        for (level, epsilon, target) in [
            (BASE_LEVEL, BASE_EPSILON, &guides.base),
            (DETAIL_LEVEL, DETAIL_EPSILON, &guides.detail),
            (FINE_LEVEL, FINE_EPSILON, &guides.fine),
        ] {
            let solve = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("guide solve params"),
                contents: bytemuck::bytes_of(&SolveParams { epsilon, padding: [0.0; 3] }),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let moments = level_view(level);
            let group = self.bind(
                &self.pipelines.guide_solve,
                &[
                    wgpu::BindGroupEntry { binding: 0, resource: solve.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&moments) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(target) },
                ],
            );
            let size = level_size(guides.width, guides.height, level);
            self.dispatch(&mut encoder, &self.pipelines.guide_solve, &group, size, "guide solve", None);
        }
        self.submit_and_wait(encoder)?;
        Ok(())
    }

    /// Every texel of one level of the guide stack, row by row.
    fn read_level(&self, guides: &Guides, level: u32) -> Result<Vec<[f32; 4]>> {
        let [width, height] = level_size(guides.width, guides.height, level);
        let row = width * 16;
        let aligned = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("guide readback"),
            size: u64::from(aligned) * u64::from(height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.encoder();
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &guides.texture,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(aligned),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.submit_and_wait(encoder)?;
        let bytes = self.map_read(&buffer)?;
        let mut texels = Vec::with_capacity((width * height) as usize);
        for line in bytes.chunks_exact(aligned as usize) {
            let cells = line[..row as usize].as_chunks::<16>().0;
            texels.extend(cells.iter().map(|cell| bytemuck::pod_read_unaligned::<[f32; 4]>(cell)));
        }
        Ok(texels)
    }

    /// Reads global statistics from the guide's base level.
    pub(crate) fn measure_stats(&self, guides: &Guides) -> Result<ImageStats> {
        let texels = self.read_level(guides, BASE_LEVEL)?;
        let brightest_dark = texels.iter().fold(0.0f32, |brightest, texel| brightest.max(texel[2]));
        Ok(stats_from(texels.iter().map(|texel| texel[0]).collect(), brightest_dark))
    }

    /// The guide at its full size, as log luminance and darkest channel at each pixel.
    pub(crate) fn read_guide(&self, guides: &Guides) -> Result<GuideImage> {
        let texels = self.read_level(guides, 0)?;
        Ok(GuideImage {
            width: guides.width as usize,
            height: guides.height as usize,
            texels: texels.iter().map(|texel| [texel[0], texel[2]]).collect(),
        })
    }
}

fn stats_from(mut log_luma: Vec<f32>, brightest_dark: f32) -> ImageStats {
    if log_luma.is_empty() {
        return ImageStats::default();
    }
    log_luma.sort_by(f32::total_cmp);
    let at = |fraction: f64| log_luma[((log_luma.len() - 1) as f64 * fraction) as usize];
    ImageStats {
        white_ev: at(STATS_HIGH_PERCENTILE),
        black_ev: at(STATS_LOW_PERCENTILE),
        atmosphere: brightest_dark.clamp(0.05, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_read_the_ends_of_the_distribution() {
        let values: Vec<f32> = (0..1000).map(|i| -10.0 + i as f32 * 0.01).collect();
        let stats = stats_from(values, 0.6);
        assert!((stats.black_ev + 9.98).abs() < 0.03, "{stats:?}");
        assert!((stats.white_ev + 0.02).abs() < 0.03, "{stats:?}");
        assert_eq!(stats.atmosphere, 0.6);
    }

    #[test]
    fn stats_survive_an_empty_image() {
        assert_eq!(stats_from(Vec::new(), 0.0), ImageStats::default());
    }

    #[test]
    fn level_sizes_never_reach_zero() {
        assert_eq!(level_size(1, 1, 4), [1, 1]);
        assert_eq!(level_size(1024, 683, 4), [64, 42]);
    }
}
