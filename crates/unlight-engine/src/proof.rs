//! Soft proofing in the view path: what the photo will look like in another colour space, and
//! which pixels won't make it there. A proof only ever swaps the last pass of a view for a copy
//! with the lookup compiled in, so the recipe, the histogram of the photo itself and every
//! export are out of its reach, and a view with proofing off runs the ordinary shader.

use crate::{
    params::wgsl_mat3,
    pipelines::{COMMON, FINISH, FINISH_COMMON, Engine, compile},
};
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;
use unlight_color::{
    Mat3, ProofLut,
    matrix::{IDENTITY, inverse, mul, prophoto_to_srgb},
    output::OutputSpace,
};
use wgpu::util::DeviceExt;

const PROOF_SHADER: &str = include_str!("shaders/proof.wgsl");
/// Magenta, which a photo rarely holds and the clipping overlays don't use.
const WARNING_COLOR: [f32; 4] = [1.0, 0.0, 1.0, 1.0];

/// What to proof against.
#[derive(Clone, Debug, PartialEq)]
pub enum ProofTarget {
    /// A matrix space, worked out exactly per pixel.
    Space(OutputSpace),
    /// A baked ICC profile.
    Table(Arc<ProofLut>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Proof {
    pub target: ProofTarget,
    /// Paints what the target can't show in the warning colour.
    pub gamut_warning: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ProofParams {
    working_to_target: [[f32; 4]; 3],
    target_to_display: [[f32; 4]; 3],
    warning: [f32; 4],
    uses_table: f32,
    show_warning: f32,
    table_cells: f32,
    padding: f32,
}

/// Compiled the first time it's asked for, so that proofing costs an editor nothing until then.
pub(crate) struct ProofPipeline {
    pipeline: wgpu::ComputePipeline,
    sampler: wgpu::Sampler,
    empty_table: wgpu::TextureView,
}

impl ProofPipeline {
    pub(crate) fn new(engine: &Engine) -> Self {
        let device = &engine.ctx.device;
        let pipeline = compile(
            device,
            "finish proof",
            &[COMMON, FINISH_COMMON, PROOF_SHADER, FINISH],
            &[("{{OUTPUT_FORMAT}}", "rgba8unorm"), ("// {{PROOF}}", "color = proofed(rgb, color, dither);")],
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("proof table"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let empty_table = table_texture(engine, 1, &[[0.0; 4]]);
        Self { pipeline, sampler, empty_table }
    }
}

fn table_texture(engine: &Engine, size: u32, entries: &[[f32; 4]]) -> wgpu::TextureView {
    engine
        .ctx
        .device
        .create_texture_with_data(
            &engine.ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("proof table"),
                size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: size },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D3,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(entries),
        )
        .create_view(&Default::default())
}

/// A session's copy of the table in use.
#[derive(Default)]
pub(crate) struct ProofTable {
    uploaded: Option<(u64, wgpu::TextureView)>,
}

impl ProofTable {
    fn view(&mut self, engine: &Engine, table: &ProofLut) -> &wgpu::TextureView {
        if self.uploaded.as_ref().is_some_and(|(id, _)| *id != table.id) {
            self.uploaded = None;
        }
        &self.uploaded.get_or_insert_with(|| (table.id, table_texture(engine, table.size, &table.entries))).1
    }
}

fn params(proof: &Proof) -> ProofParams {
    let (target, uses_table, cells): (Mat3, _, _) = match &proof.target {
        ProofTarget::Space(space) => (space.matrix_from_prophoto(), 0.0, 1.0),
        ProofTarget::Table(table) => (IDENTITY, 1.0, table.size as f32),
    };
    let back = inverse(&target).unwrap_or(IDENTITY);
    ProofParams {
        working_to_target: wgsl_mat3(&target),
        target_to_display: wgsl_mat3(&mul(&prophoto_to_srgb(), &back)),
        warning: WARNING_COLOR,
        uses_table,
        show_warning: f32::from(u8::from(proof.gamut_warning)),
        table_cells: cells,
        padding: 0.0,
    }
}

/// The textures a finish pass reads and writes.
pub(crate) struct FinishViews<'a> {
    pub developed: &'a wgpu::TextureView,
    pub blurred_rows: &'a wgpu::TextureView,
    pub local_results: &'a wgpu::TextureView,
    pub output: &'a wgpu::TextureView,
}

impl Engine {
    fn proof_pipeline(&self) -> &ProofPipeline {
        self.proof.get_or_init(|| ProofPipeline::new(self))
    }

    pub(crate) fn proof_finish(&self) -> &wgpu::ComputePipeline {
        &self.proof_pipeline().pipeline
    }

    pub(crate) fn proof_group(
        &self,
        proof: &Proof,
        tables: &mut ProofTable,
        views: &FinishViews<'_>,
    ) -> wgpu::BindGroup {
        let pipeline = self.proof_pipeline();
        let uniform = self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("proof params"),
            contents: bytemuck::bytes_of(&params(proof)),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let table = match &proof.target {
            ProofTarget::Table(table) => tables.view(self, table),
            ProofTarget::Space(_) => &pipeline.empty_table,
        };
        let view = |binding, view| wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::TextureView(view) };
        self.bind(
            &pipeline.pipeline,
            &[
                wgpu::BindGroupEntry { binding: 0, resource: self.tile_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.finish_buffer.as_entire_binding() },
                view(2, views.developed),
                view(3, views.blurred_rows),
                view(4, views.output),
                view(5, views.local_results),
                wgpu::BindGroupEntry { binding: 6, resource: uniform.as_entire_binding() },
                view(7, table),
                wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::Sampler(&pipeline.sampler) },
            ],
        )
    }
}
