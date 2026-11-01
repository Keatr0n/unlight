use crate::{
    error::Result,
    gpu::GpuContext,
    masks::MaskPipelines,
    params::{MAX_TILE_EDGE, TileParams},
    proof::ProofPipeline,
    repair::RepairPipelines,
    scheduler::{SchedulerConfig, Tile, TileScheduler},
    timing::GpuProfiler,
};
use std::{sync::{Arc, OnceLock}, time::{Duration, Instant}};

pub(crate) const COMMON: &str = include_str!("shaders/common.wgsl");
const XTRANS_COMMON: &str = include_str!("shaders/xtrans_common.wgsl");
pub(crate) const FINISH_COMMON: &str = include_str!("shaders/finish_common.wgsl");
pub(crate) const FINISH: &str = include_str!("shaders/finish.wgsl");
const LOCAL_EDITS: &str = include_str!("shaders/local_edits.wgsl");
const WORKGROUP_EDGE: u32 = 16;

/// Storage precision for full-size RGB data. Interactive work uses `F16`, export `F32`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    F16,
    F32,
}

impl Precision {
    pub fn format(self) -> wgpu::TextureFormat {
        match self {
            Self::F16 => wgpu::TextureFormat::Rgba16Float,
            Self::F32 => wgpu::TextureFormat::Rgba32Float,
        }
    }

    fn wgsl(self) -> &'static str {
        match self {
            Self::F16 => "rgba16float",
            Self::F32 => "rgba32float",
        }
    }
}

pub(crate) struct Pipelines {
    pub linearize: wgpu::ComputePipeline,
    pub hot_pixels: wgpu::ComputePipeline,
    pub highlight_seed: wgpu::ComputePipeline,
    pub highlight_reduce: wgpu::ComputePipeline,
    pub highlight_expand: wgpu::ComputePipeline,
    pub highlight_fill: wgpu::ComputePipeline,
    pub rcd_high_pass: wgpu::ComputePipeline,
    pub rcd_direction: wgpu::ComputePipeline,
    pub rcd_green: wgpu::ComputePipeline,
    pub rcd_diagonal: wgpu::ComputePipeline,
    pub rcd_final: wgpu::ComputePipeline,
    pub full_colour: wgpu::ComputePipeline,
    pub xtrans_green: wgpu::ComputePipeline,
    pub xtrans_solitary: wgpu::ComputePipeline,
    pub xtrans_cross: wgpu::ComputePipeline,
    pub xtrans_blocks: wgpu::ComputePipeline,
    pub xtrans_derivative: wgpu::ComputePipeline,
    pub xtrans_homogeneity: wgpu::ComputePipeline,
    pub xtrans_final: wgpu::ComputePipeline,
    pub denoise_level: wgpu::ComputePipeline,
    pub denoise_full: wgpu::ComputePipeline,
    pub denoise_patch: wgpu::ComputePipeline,
    pub denoise_nlm: wgpu::ComputePipeline,
    pub bin: wgpu::ComputePipeline,
    pub downsample: wgpu::ComputePipeline,
    pub guide_seed: wgpu::ComputePipeline,
    pub guide_reduce: wgpu::ComputePipeline,
    pub guide_solve: wgpu::ComputePipeline,
    pub develop: wgpu::ComputePipeline,
    pub sharpen_rows: wgpu::ComputePipeline,
    pub finish_display: wgpu::ComputePipeline,
    pub finish_export: wgpu::ComputePipeline,
    pub histogram: wgpu::ComputePipeline,
    pub masks: MaskPipelines,
    pub repair: RepairPipelines,
}

pub(crate) fn compile(
    device: &wgpu::Device,
    label: &str,
    parts: &[&str],
    substitutions: &[(&str, &str)],
) -> wgpu::ComputePipeline {
    let mut source = parts.concat();
    for (name, value) in substitutions {
        source = source.replace(name, value);
    }
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    })
}

impl Pipelines {
    fn new(device: &wgpu::Device, precision: Precision) -> Self {
        let rgb = [("{{RGB_FORMAT}}", precision.wgsl())];
        let plain = |label, source| compile(device, label, &[COMMON, source], &[]);
        let xtrans = |label, source| compile(device, label, &[COMMON, XTRANS_COMMON, source], &[]);
        let denoise_level = include_str!("shaders/denoise_level.wgsl");
        let standalone = |label, source| compile(device, label, &[source], &[]);
        Self {
            linearize: plain("linearize", include_str!("shaders/linearize.wgsl")),
            hot_pixels: plain("hot pixels", include_str!("shaders/hot_pixels.wgsl")),
            highlight_seed: plain("highlight seed", include_str!("shaders/highlight_seed.wgsl")),
            highlight_reduce: plain("highlight reduce", include_str!("shaders/highlight_reduce.wgsl")),
            highlight_expand: plain("highlight expand", include_str!("shaders/highlight_expand.wgsl")),
            highlight_fill: plain("highlight fill", include_str!("shaders/highlight_fill.wgsl")),
            rcd_high_pass: plain("rcd high pass", include_str!("shaders/rcd_hp.wgsl")),
            rcd_direction: plain("rcd direction", include_str!("shaders/rcd_dir.wgsl")),
            rcd_green: plain("rcd green", include_str!("shaders/rcd_green.wgsl")),
            rcd_diagonal: plain("rcd diagonal", include_str!("shaders/rcd_diag.wgsl")),
            full_colour: compile(
                device,
                "full colour",
                &[COMMON, include_str!("shaders/full_colour.wgsl")],
                &rgb,
            ),
            rcd_final: compile(
                device,
                "rcd final",
                &[COMMON, include_str!("shaders/rcd_final.wgsl")],
                &rgb,
            ),
            xtrans_green: xtrans("xtrans green", include_str!("shaders/xtrans_green.wgsl")),
            xtrans_solitary: xtrans("xtrans solitary", include_str!("shaders/xtrans_solitary.wgsl")),
            xtrans_cross: xtrans("xtrans cross", include_str!("shaders/xtrans_cross.wgsl")),
            xtrans_blocks: xtrans("xtrans blocks", include_str!("shaders/xtrans_blocks.wgsl")),
            xtrans_derivative: plain(
                "xtrans derivative",
                include_str!("shaders/xtrans_derivative.wgsl"),
            ),
            xtrans_homogeneity: plain(
                "xtrans homogeneity",
                include_str!("shaders/xtrans_homogeneity.wgsl"),
            ),
            xtrans_final: compile(
                device,
                "xtrans final",
                &[COMMON, include_str!("shaders/xtrans_final.wgsl")],
                &rgb,
            ),
            denoise_level: compile(
                device,
                "denoise level",
                &[COMMON, denoise_level],
                &[("{{OUTPUT_FORMAT}}", "rgba16float")],
            ),
            denoise_full: compile(
                device,
                "denoise full",
                &[COMMON, denoise_level],
                &[("{{OUTPUT_FORMAT}}", precision.wgsl())],
            ),
            denoise_patch: plain("denoise patch", include_str!("shaders/denoise_patch.wgsl")),
            denoise_nlm: compile(
                device,
                "denoise means",
                &[COMMON, include_str!("shaders/denoise_nlm.wgsl")],
                &rgb,
            ),
            bin: plain("bin", include_str!("shaders/bin.wgsl")),
            downsample: plain("downsample", include_str!("shaders/downsample.wgsl")),
            guide_seed: standalone("guide seed", include_str!("shaders/guide_seed.wgsl")),
            guide_reduce: standalone("guide reduce", include_str!("shaders/guide_reduce.wgsl")),
            guide_solve: standalone("guide solve", include_str!("shaders/guide_solve.wgsl")),
            develop: compile(
                device,
                "develop",
                &[COMMON, LOCAL_EDITS, include_str!("shaders/develop.wgsl")],
                &rgb,
            ),
            sharpen_rows: compile(
                device,
                "sharpen rows",
                &[COMMON, FINISH_COMMON, include_str!("shaders/sharpen_rows.wgsl")],
                &rgb,
            ),
            finish_display: compile(
                device,
                "finish display",
                &[COMMON, FINISH_COMMON, FINISH],
                &[("{{OUTPUT_FORMAT}}", "rgba8unorm")],
            ),
            finish_export: compile(
                device,
                "finish export",
                &[COMMON, FINISH_COMMON, FINISH],
                &[("{{OUTPUT_FORMAT}}", "rgba32float")],
            ),
            histogram: standalone("histogram", include_str!("shaders/histogram.wgsl")),
            masks: MaskPipelines::new(device),
            repair: RepairPipelines::new(device, precision.wgsl()),
        }
    }
}

/// Compiled pipelines and shared resources for one device and precision.
pub struct Engine {
    pub ctx: Arc<GpuContext>,
    pub precision: Precision,
    pub(crate) pipelines: Pipelines,
    pub(crate) tile_buffer: wgpu::Buffer,
    pub(crate) develop_buffer: wgpu::Buffer,
    pub(crate) finish_buffer: wgpu::Buffer,
    pub(crate) sampler: wgpu::Sampler,
    pub(crate) table_sampler: wgpu::Sampler,
    pub(crate) proof: OnceLock<ProofPipeline>,
}

impl Engine {
    pub fn new(ctx: Arc<GpuContext>, precision: Precision) -> Result<Self> {
        let device = &ctx.device;
        let pipelines = Pipelines::new(device, precision);
        let uniform = |label, size| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let tile_buffer = uniform("tile params", size_of::<TileParams>() as u64);
        let develop_buffer = uniform("develop params", size_of::<crate::params::DevelopParams>() as u64);
        let finish_buffer = uniform("finish params", size_of::<crate::params::FinishParams>() as u64);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        // Hue wraps around, and hue is the second axis of a profile table.
        let table_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("profile table"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        ctx.check()?;
        Ok(Self {
            pipelines,
            tile_buffer,
            develop_buffer,
            finish_buffer,
            sampler,
            table_sampler,
            proof: OnceLock::new(),
            precision,
            ctx,
        })
    }

    pub(crate) fn encoder(&self) -> wgpu::CommandEncoder {
        self.ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default())
    }

    pub(crate) fn write_tile(&self, params: &TileParams) {
        self.ctx.queue.write_buffer(&self.tile_buffer, 0, bytemuck::bytes_of(params));
    }

    pub(crate) fn bind(
        &self,
        pipeline: &wgpu::ComputePipeline,
        entries: &[wgpu::BindGroupEntry<'_>],
    ) -> wgpu::BindGroup {
        self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries,
        })
    }

    pub(crate) fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::ComputePipeline,
        group: &wgpu::BindGroup,
        size: [u32; 2],
        label: &'static str,
        profiler: Option<&mut GpuProfiler>,
    ) {
        let timestamp_writes = profiler.and_then(|profiler| profiler.scope(label));
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(label),
            timestamp_writes,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, group, &[]);
        pass.dispatch_workgroups(size[0].div_ceil(WORKGROUP_EDGE), size[1].div_ceil(WORKGROUP_EDGE), 1);
    }

    pub(crate) fn submit_and_wait(&self, encoder: wgpu::CommandEncoder) -> Result<Duration> {
        let start = Instant::now();
        self.ctx.queue.submit([encoder.finish()]);
        self.ctx.wait()?;
        Ok(start.elapsed())
    }

    /// Runs `encode` over a region one submit at a time, adapting tile size as it goes.
    pub(crate) fn run_tiles(
        &self,
        width: u32,
        height: u32,
        config: SchedulerConfig,
        mut encode: impl FnMut(&mut wgpu::CommandEncoder, Tile),
    ) -> Result<()> {
        let mut scheduler = TileScheduler::new(width, height, config);
        while let Some(tile) = scheduler.next_tile() {
            let mut encoder = self.encoder();
            encode(&mut encoder, tile);
            scheduler.record(tile, self.submit_and_wait(encoder)?);
        }
        Ok(())
    }

    pub(crate) fn storage_texture(
        &self,
        label: &str,
        size: [u32; 3],
        mips: u32,
        format: wgpu::TextureFormat,
    ) -> wgpu::Texture {
        self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: size[2] },
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    pub(crate) fn max_tile_extent(&self) -> u32 {
        MAX_TILE_EDGE + 2 * crate::params::TILE_PAD
    }

    pub(crate) fn map_read(&self, buffer: &wgpu::Buffer) -> Result<Vec<u8>> {
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.ctx.wait()?;
        let gpu_error = |message: String| crate::EngineError::Gpu(message);
        receiver
            .recv()
            .map_err(|error| gpu_error(error.to_string()))?
            .map_err(|error| gpu_error(error.to_string()))?;
        let view = buffer.slice(..).get_mapped_range();
        let bytes = view.map_err(|error| gpu_error(error.to_string()))?.to_vec();
        buffer.unmap();
        Ok(bytes)
    }

    /// Copies an RGBA8 texture back to the CPU, tightly packed.
    pub(crate) fn read_rgba8(&self, texture: &wgpu::Texture, width: u32, height: u32) -> Result<Vec<u8>> {
        let row = width * 4;
        let aligned = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("view readback"),
            size: u64::from(aligned) * u64::from(height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.encoder();
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
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
        let mut pixels = Vec::with_capacity((row * height) as usize);
        for line in bytes.chunks_exact(aligned as usize) {
            pixels.extend_from_slice(&line[..row as usize]);
        }
        Ok(pixels)
    }
}
