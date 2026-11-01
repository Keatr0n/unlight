use crate::{
    error::{EngineError, Result},
    noise::{self, NoiseModel},
    params::{LinearizeParams, MOSAIC_BORDER, TileParams, cfa_params, pack_tile},
    pipelines::Engine,
    scheduler::SchedulerConfig,
};
use unlight_color::{CameraProfile, Vec3};
use unlight_raw::{Cfa, GainMap, Opcode, RawImage};
use wgpu::util::DeviceExt;

/// Mips stop once the long edge gets this small.
const PREVIEW_MIN_EDGE: u32 = 256;
/// Sites this close to the white level count as clipped. The last stretch below saturation
/// isn't reliably linear, and it also absorbs noise on the saturation level itself.
const CLIP_LEVEL: f32 = 0.985;
/// Rows of a merged photo uploaded at a time. A 200 MP panorama is over a gigabyte of texels.
const UPLOAD_ROWS: usize = 512;

/// A photo on the GPU: its normalised, white-balanced mosaic plus what's needed to colour it.
pub struct SourceImage {
    pub width: u32,
    pub height: u32,
    pub cfa: Cfa,
    pub profile: CameraProfile,
    /// The white the mosaic is balanced to. User white balance is a ratio on top of this,
    /// so changing it never invalidates the demosaic.
    pub reference_neutral: Vec3,
    pub baseline_exposure: f32,
    pub noise: NoiseModel,
    pub(crate) mosaic: wgpu::TextureView,
}

/// A mip chain of the photo for fit-to-screen viewing.
pub struct Preview {
    pub width: u32,
    pub height: u32,
    pub image_width: u32,
    pub image_height: u32,
    pub levels: u32,
    pub(crate) view: wgpu::TextureView,
    texture: wgpu::Texture,
}

impl Preview {
    pub(crate) fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}

struct GainGrid {
    points: [u32; 2],
    origin: [f32; 2],
    spacing: [f32; 2],
    pitch: [i32; 2],
    gains: Vec<[f32; 4]>,
}

/// Folds a set of per-site gain maps into one texture with a channel per CFA position.
/// Returns `None` for layouts that aren't a plain repeat of the same grid.
fn gain_grid(list: &[Opcode]) -> Option<GainGrid> {
    let maps: Vec<&GainMap> = list
        .iter()
        .filter_map(|opcode| match opcode {
            Opcode::GainMap(map) => Some(map),
            _ => None,
        })
        .collect();
    let first = *maps.first()?;
    let pitch = [first.col_pitch.max(1), first.row_pitch.max(1)];
    let shared_grid = maps.iter().all(|map| {
        map.points_h == first.points_h
            && map.points_v == first.points_v
            && map.col_pitch == first.col_pitch
            && map.row_pitch == first.row_pitch
    });
    if !shared_grid || pitch[0] > 2 || pitch[1] > 2 || maps.len() as u32 > pitch[0] * pitch[1] {
        return None;
    }

    let count = (first.points_h * first.points_v) as usize;
    let mut gains = vec![[1.0; 4]; count];
    for map in &maps {
        let channel = ((map.top % pitch[1]) * pitch[0] + map.left % pitch[0]) as usize;
        let stride = map.map_planes.max(1) as usize;
        for (index, gain) in gains.iter_mut().enumerate() {
            gain[channel] = *map.gains.get(index * stride)?;
        }
    }
    Some(GainGrid {
        points: [first.points_h, first.points_v],
        origin: [first.origin_h as f32, first.origin_v as f32],
        spacing: [first.spacing_h as f32, first.spacing_v as f32],
        pitch: [pitch[0] as i32, pitch[1] as i32],
        gains,
    })
}

/// IEEE half precision to single precision.
fn half_to_float(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exponent = u32::from(bits >> 10) & 0x1f;
    let fraction = u32::from(bits & 0x3ff);
    let magnitude = match exponent {
        0 => (fraction as f32 / (1u32 << 24) as f32).to_bits(),
        31 => 0x7f80_0000 | (fraction << 13),
        _ => ((exponent + 112) << 23) | (fraction << 13),
    };
    f32::from_bits(sign | magnitude)
}

impl Engine {
    /// An empty pyramid the same shape as `like`.
    pub(crate) fn blank_preview(&self, like: &Preview) -> Preview {
        let texture = self.storage_texture(
            "repaired preview",
            [like.width, like.height, 1],
            like.levels,
            wgpu::TextureFormat::Rgba16Float,
        );
        Preview {
            view: texture.create_view(&Default::default()),
            texture,
            ..*like
        }
    }

    fn upload_gain(&self, grid: Option<&GainGrid>) -> wgpu::TextureView {
        let identity = [[1.0f32; 4]];
        let (points, data): ([u32; 2], &[[f32; 4]]) =
            grid.map_or(([1, 1], &identity), |grid| (grid.points, &grid.gains));
        let texture = self.ctx.device.create_texture_with_data(
            &self.ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("gain map"),
                size: wgpu::Extent3d {
                    width: points[0],
                    height: points[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(data),
        );
        texture.create_view(&Default::default())
    }

    fn upload_samples(&self, raw: &RawImage) -> wgpu::TextureView {
        let texture = self.ctx.device.create_texture_with_data(
            &self.ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("raw samples"),
                size: wgpu::Extent3d {
                    width: raw.width,
                    height: raw.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R16Uint,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&raw.samples),
        );
        texture.create_view(&Default::default())
    }

    /// Copies a rectangle of one pyramid level to the CPU as RGB. `rect` is x, y, width, height
    /// in that level's pixels.
    pub(crate) fn read_preview_region(&self, preview: &Preview, level: u32, rect: [u32; 4]) -> Result<Vec<[f32; 3]>> {
        const TEXEL_BYTES: u32 = 8;
        let [x, y, width, height] = rect;
        let aligned = (width * TEXEL_BYTES).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("preview readback"),
            size: u64::from(aligned) * u64::from(height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.encoder();
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: preview.texture(),
                mip_level: level,
                origin: wgpu::Origin3d { x, y, z: 0 },
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
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for line in bytes.chunks_exact(aligned as usize) {
            let row = &line[..(width * TEXEL_BYTES) as usize];
            for texel in row.as_chunks::<8>().0 {
                let channel = |at: usize| half_to_float(u16::from_le_bytes([texel[at], texel[at + 1]]));
                pixels.push([channel(0), channel(2), channel(4)]);
            }
        }
        Ok(pixels)
    }

    /// Uploads a merged photo as it is. It's already linear, full colour and balanced to its
    /// reference neutral, so it joins the pipeline after everything a mosaic has to go through.
    /// `SourceImage::mosaic` holds these texels in place of a mosaic.
    fn load_full_colour(&self, raw: &RawImage) -> Result<SourceImage> {
        let limit = self.ctx.device.limits().max_texture_dimension_2d;
        if raw.width > limit || raw.height > limit {
            return Err(EngineError::ImageTooLarge { width: raw.width, height: raw.height, limit });
        }
        let texture = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("full colour source"),
            size: wgpu::Extent3d { width: raw.width, height: raw.height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let texels = raw.width as usize * 4;
        for (band, rows) in raw.samples.chunks(texels * UPLOAD_ROWS).enumerate() {
            let origin = wgpu::Origin3d { x: 0, y: (band * UPLOAD_ROWS) as u32, z: 0 };
            let height = (rows.len() / texels) as u32;
            self.ctx.queue.write_texture(
                wgpu::TexelCopyTextureInfo { origin, ..texture.as_image_copy() },
                bytemuck::cast_slice(rows),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(raw.width * 8), rows_per_image: None },
                wgpu::Extent3d { width: raw.width, height, depth_or_array_layers: 1 },
            );
            self.ctx.queue.submit([]);
            self.ctx.wait()?;
        }
        let profile = CameraProfile::new(&raw.color);
        Ok(SourceImage {
            width: raw.width,
            height: raw.height,
            cfa: raw.cfa,
            reference_neutral: profile.as_shot_neutral(),
            profile,
            baseline_exposure: raw.color.baseline_exposure,
            noise: noise::estimate(raw),
            mosaic: texture.create_view(&Default::default()),
        })
    }

    /// Uploads a decoded raw and normalises it into a bordered 32-bit mosaic.
    pub fn load(&self, raw: &RawImage, schedule: SchedulerConfig) -> Result<SourceImage> {
        if raw.cfa.is_full_colour() {
            return self.load_full_colour(raw);
        }
        let limit = self.ctx.device.limits().max_texture_dimension_2d;
        let bordered = [raw.width + 2 * MOSAIC_BORDER, raw.height + 2 * MOSAIC_BORDER];
        if bordered[0] > limit || bordered[1] > limit {
            return Err(EngineError::ImageTooLarge { width: raw.width, height: raw.height, limit });
        }

        let profile = CameraProfile::new(&raw.color);
        let reference_neutral = profile.as_shot_neutral();
        let balance = reference_neutral.map(|value| (1.0 / value) as f32);

        let grid = gain_grid(&raw.opcodes.list2);
        let params = LinearizeParams {
            black: pack_tile(&raw.black),
            balance: [balance[0], balance[1], balance[2], 0.0],
            gain_origin: grid.as_ref().map_or([0.0; 2], |grid| grid.origin),
            gain_spacing: grid.as_ref().map_or([1.0; 2], |grid| grid.spacing),
            gain_points: grid.as_ref().map_or([1; 2], |g| [g.points[0] as i32, g.points[1] as i32]),
            gain_pitch: grid.as_ref().map_or([1; 2], |grid| grid.pitch),
            white: raw.white,
            clip: CLIP_LEVEL,
            gain_enabled: u32::from(grid.is_some()),
            padding: 0,
        };
        let linearize_buffer = self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("linearize params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let samples = self.upload_samples(raw);
        let gain = self.upload_gain(grid.as_ref());
        let linear = self
            .storage_texture("linear mosaic", [bordered[0], bordered[1], 1], 1, wgpu::TextureFormat::R32Float)
            .create_view(&Default::default());

        let group = self.bind(
            &self.pipelines.linearize,
            &[
                wgpu::BindGroupEntry { binding: 0, resource: self.tile_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: linearize_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&samples) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&gain) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&linear) },
            ],
        );
        let cfa = cfa_params(&raw.cfa);
        self.run_tiles(bordered[0], bordered[1], schedule, |encoder, tile| {
            self.write_tile(&TileParams {
                origin: [
                    tile.x as i32 - MOSAIC_BORDER as i32,
                    tile.y as i32 - MOSAIC_BORDER as i32,
                ],
                size: [tile.width as i32, tile.height as i32],
                image: [raw.width as i32, raw.height as i32],
                pad: 0,
                period: raw.cfa.period as i32,
                cfa,
            });
            self.dispatch(encoder, &self.pipelines.linearize, &group, [tile.width, tile.height], "linearize", None);
        })?;
        let mosaic = self.repair_mosaic(raw, (params.balance, CLIP_LEVEL), &linear, schedule)?;

        Ok(SourceImage {
            width: raw.width,
            height: raw.height,
            cfa: raw.cfa,
            profile,
            reference_neutral,
            baseline_exposure: raw.color.baseline_exposure,
            noise: noise::estimate(raw),
            mosaic,
        })
    }

    /// Bins the mosaic to a small RGB image and builds its mip chain.
    pub fn build_preview(&self, source: &SourceImage, schedule: SchedulerConfig) -> Result<Preview> {
        let factor = if source.cfa.is_xtrans() { 3 } else { 2 };
        let (width, height) = (source.width / factor, source.height / factor);
        let long_edge = width.max(height);
        let levels = (long_edge / PREVIEW_MIN_EDGE).max(1).ilog2() + 1;
        let texture = self.storage_texture(
            "preview",
            [width, height, 1],
            levels,
            wgpu::TextureFormat::Rgba16Float,
        );
        let level_view = |level: u32| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };

        let cfa = cfa_params(&source.cfa);
        let tile_params = |tile: crate::Tile| TileParams {
            origin: [tile.x as i32, tile.y as i32],
            size: [tile.width as i32, tile.height as i32],
            image: [source.width as i32, source.height as i32],
            pad: 0,
            period: source.cfa.period as i32,
            cfa,
        };

        let base = level_view(0);
        let first_level = if source.cfa.is_full_colour() { &self.pipelines.downsample } else { &self.pipelines.bin };
        let group = self.bind(
            first_level,
            &[
                wgpu::BindGroupEntry { binding: 0, resource: self.tile_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&source.mosaic) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&base) },
            ],
        );
        self.run_tiles(width, height, schedule, |encoder, tile| {
            self.write_tile(&tile_params(tile));
            self.dispatch(encoder, first_level, &group, [tile.width, tile.height], "bin", None);
        })?;

        for level in 1..levels {
            let (source_view, target_view) = (level_view(level - 1), level_view(level));
            let group = self.bind(
                &self.pipelines.downsample,
                &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.tile_buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&source_view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&target_view) },
                ],
            );
            let (level_width, level_height) = ((width >> level).max(1), (height >> level).max(1));
            self.run_tiles(level_width, level_height, schedule, |encoder, tile| {
                self.write_tile(&tile_params(tile));
                let size = [tile.width, tile.height];
                self.dispatch(encoder, &self.pipelines.downsample, &group, size, "downsample", None);
            })?;
        }

        Ok(Preview {
            width,
            height,
            image_width: source.width,
            image_height: source.height,
            levels,
            view: texture.create_view(&Default::default()),
            texture,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(top: u32, left: u32, gains: Vec<f32>) -> Opcode {
        Opcode::GainMap(GainMap {
            top,
            left,
            bottom: 100,
            right: 100,
            plane: 0,
            planes: 1,
            row_pitch: 2,
            col_pitch: 2,
            points_v: 1,
            points_h: 2,
            spacing_v: 1.0,
            spacing_h: 1.0,
            origin_v: 0.0,
            origin_h: 0.0,
            map_planes: 1,
            gains,
        })
    }

    #[test]
    fn half_floats_decode_exactly() {
        assert_eq!(half_to_float(0x3c00), 1.0);
        assert_eq!(half_to_float(0xc000), -2.0);
        assert_eq!(half_to_float(0x3800), 0.5);
        assert_eq!(half_to_float(0x0000), 0.0);
        assert_eq!(half_to_float(0x0001), 2f32.powi(-24));
    }

    #[test]
    fn gain_grid_gives_each_bayer_site_its_own_channel() {
        let list = [
            map(0, 0, vec![1.0, 2.0]),
            map(0, 1, vec![3.0, 4.0]),
            map(1, 0, vec![5.0, 6.0]),
            map(1, 1, vec![7.0, 8.0]),
        ];
        let grid = gain_grid(&list).unwrap();
        assert_eq!(grid.gains, vec![[1.0, 3.0, 5.0, 7.0], [2.0, 4.0, 6.0, 8.0]]);
        assert_eq!(grid.pitch, [2, 2]);
    }

    #[test]
    fn gain_grid_rejects_mismatched_maps_and_ignores_other_opcodes() {
        assert!(gain_grid(&[Opcode::Unsupported { id: 1, optional: true }]).is_none());
        let mut second = map(0, 1, vec![1.0, 2.0]);
        if let Opcode::GainMap(map) = &mut second {
            map.points_h = 1;
        }
        assert!(gain_grid(&[map(0, 0, vec![1.0, 2.0]), second]).is_none());
    }

    #[test]
    fn gain_grid_rejects_a_truncated_map() {
        assert!(gain_grid(&[map(0, 0, vec![1.0])]).is_none());
    }
}
