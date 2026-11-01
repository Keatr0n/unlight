//! Moves the demosaiced photo to and from the CPU, for processing that can't run in a shader.

use crate::{error::Result, pipelines::Engine};
use wgpu::util::DeviceExt;

const TEXEL_BYTES: u32 = 16;
/// Read in bands, since a whole 100 MP frame would be over a gigabyte in one buffer.
const BAND_ROWS: u32 = 256;

impl Engine {
    pub(crate) fn read_scene(&self, texture: &wgpu::Texture, [width, height]: [u32; 2]) -> Result<Vec<[f32; 4]>> {
        let row_bytes = (width * TEXEL_BYTES) as usize;
        let stride = (width * TEXEL_BYTES).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene readback"),
            size: u64::from(stride) * u64::from(BAND_ROWS),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut pixels = Vec::with_capacity(width as usize * height as usize);
        for top in (0..height).step_by(BAND_ROWS as usize) {
            let rows = BAND_ROWS.min(height - top);
            let mut encoder = self.encoder();
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: top, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(stride),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d { width, height: rows, depth_or_array_layers: 1 },
            );
            self.submit_and_wait(encoder)?;
            let bytes = self.map_read(&buffer)?;
            for line in bytes.chunks_exact(stride as usize).take(rows as usize) {
                let (texels, _) = line[..row_bytes].as_chunks::<{ TEXEL_BYTES as usize }>();
                pixels.extend(texels.iter().map(|texel| bytemuck::pod_read_unaligned::<[f32; 4]>(texel)));
            }
        }
        Ok(pixels)
    }

    pub(crate) fn upload_scene(&self, pixels: &[[f32; 4]], [width, height]: [u32; 2]) -> wgpu::TextureView {
        let texture = self.ctx.device.create_texture_with_data(
            &self.ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("uploaded scene"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(pixels),
        );
        texture.create_view(&Default::default())
    }
}
