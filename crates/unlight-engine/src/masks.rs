//! Local adjustments as parameter fields rather than layers. Each mask rasterises once to a
//! fixed-size coverage map in source coordinates, then every mask's offsets are summed into a
//! few textures: `param(x) = global + Σ mask_i(x)·Δ_i`. The develop and finish passes read only
//! those sums, so a frame costs about the same with one mask as with sixteen, and because the
//! maps live in source coordinates they follow every crop and warp without being redrawn.
//!
//! All of this runs when a mask changes and never on a global slider. The maps are the same
//! size for every view and for export, which is what keeps preview and export in agreement.

use crate::{
    error::Result,
    local::{FIELD_LAYERS, field_offsets},
    params::wgsl_mat3,
    pipelines::{Engine, compile},
    source::Preview,
    tools::{LUMA_WEIGHTS, working_to_lms},
};
use bytemuck::{Pod, Zeroable};
use std::fmt;
use unlight_color::Mat3;
use unlight_recipe::{BrushDab, Mask, MaskComponent, MaskShape};
use wgpu::util::DeviceExt;

/// Long edge of a coverage map. Fields are smooth by nature, and this is fine enough that a
/// brush's edge stays crisp to well past fit-to-screen.
const MASK_EDGE: u32 = 2048;
const COVERAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;
const FIELD_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const MIN_COVERAGE_LAYERS: u32 = 4;
const MAX_COLOUR_SAMPLES: usize = 8;
/// The same bound the shaders use for the smallest dab worth drawing, plus a pixel of margin.
const SMALLEST_DAB: f32 = 0.75;

const ERASE: u32 = 1;
const AUTO_MASK: u32 = 2;

const KIND_NOTHING: u32 = 0;
const KIND_PAINTED: u32 = 1;
const KIND_LINEAR: u32 = 2;
const KIND_RADIAL: u32 = 3;
const KIND_LUMINANCE: u32 = 4;
const KIND_COLOUR: u32 = 5;

/// A coverage map from outside the recipe, such as an AI model's. Values are 0..=1, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskRaster {
    pub width: u32,
    pub height: u32,
    pub coverage: Vec<f32>,
}

/// Looks up what `MaskShape::Raster` names. Without one, those components select nothing.
pub trait MaskRasterSource: fmt::Debug + Send + Sync {
    fn load(&self, id: &str) -> Option<MaskRaster>;
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SceneParams {
    camera_to_working: [[f32; 4]; 3],
    working_to_lms: [[f32; 4]; 3],
    luma: [f32; 4],
    raster_size: [f32; 2],
    long_edge: f32,
    lod: f32,
    gain: f32,
    padding: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ShapeParams {
    kind: u32,
    inverted: u32,
    sample_count: u32,
    padding: u32,
    first: [f32; 4],
    second: [f32; 4],
    samples: [[f32; 4]; MAX_COLOUR_SAMPLES],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DabInstance {
    position: [f32; 2],
    radius: f32,
    feather: f32,
    flow: f32,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CombineParams {
    origin: [i32; 2],
    size: [i32; 2],
    count: u32,
    overlay: i32,
    padding: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct FieldRow {
    offsets: [[f32; 4]; FIELD_LAYERS],
    layer: i32,
    padding: [i32; 3],
}

/// A half-open rectangle of coverage-map pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PixelRect {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

impl PixelRect {
    fn whole([width, height]: [u32; 2]) -> Self {
        Self { left: 0, top: 0, right: width, bottom: height }
    }

    fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    fn size(self) -> [u32; 2] {
        [self.right.saturating_sub(self.left), self.bottom.saturating_sub(self.top)]
    }

    fn is_empty(self) -> bool {
        self.size().contains(&0)
    }
}

fn dab_rect(dab: &BrushDab, size: [u32; 2]) -> PixelRect {
    let long_edge = size[0].max(size[1]) as f32;
    let reach = (dab.radius * long_edge).max(SMALLEST_DAB) + 1.0;
    let bound = |axis: usize, offset: f32| {
        ((dab.position[axis] * size[axis] as f32 + offset).clamp(0.0, size[axis] as f32)) as u32
    };
    PixelRect {
        left: bound(0, -reach),
        top: bound(1, -reach),
        right: bound(0, reach + 1.0),
        bottom: bound(1, reach + 1.0),
    }
}

fn union_of_dabs(dabs: &[BrushDab], size: [u32; 2]) -> Option<PixelRect> {
    dabs.iter().map(|dab| dab_rect(dab, size)).reduce(PixelRect::union)
}

fn reads_scene(shape: &MaskShape) -> bool {
    match shape {
        MaskShape::LuminanceRange { .. } | MaskShape::ColorRange { .. } => true,
        MaskShape::Brush { dabs } => dabs.iter().any(|dab| dab.auto_mask),
        MaskShape::Linear { .. } | MaskShape::Radial { .. } | MaskShape::Raster { .. } => false,
    }
}

/// What a new mask needs done to its coverage map, given what was drawn last.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Update {
    Keep,
    /// Stroke continued: dabs from these indices on, per component, are new.
    Grow(Vec<usize>),
    Redraw,
}

fn plan_update(drawn: &[&MaskComponent], mask: &Mask) -> Update {
    if drawn.len() != mask.components.len() {
        return Update::Redraw;
    }
    let mut new_dabs = Vec::with_capacity(drawn.len());
    for (old, new) in drawn.iter().zip(&mask.components) {
        if old.subtract != new.subtract {
            return Update::Redraw;
        }
        new_dabs.push(match (&old.shape, &new.shape) {
            (MaskShape::Brush { dabs: before }, MaskShape::Brush { dabs: after })
                if after.len() >= before.len() && after[..before.len()] == before[..] =>
            {
                before.len()
            }
            (before, after) if before == after => 0,
            _ => return Update::Redraw,
        });
    }
    let unchanged = drawn.iter().zip(&mask.components).all(|(old, new)| **old == *new);
    if unchanged { Update::Keep } else { Update::Grow(new_dabs) }
}

fn shape_params(shape: &MaskShape, has_raster: bool) -> ShapeParams {
    let mut params = ShapeParams::zeroed();
    match shape {
        MaskShape::Brush { .. } => params.kind = KIND_PAINTED,
        MaskShape::Linear { start, end } => {
            params.kind = KIND_LINEAR;
            params.first = [start[0], start[1], end[0], end[1]];
        }
        MaskShape::Radial { center, radii, angle, feather, inverted } => {
            let (sine, cosine) = angle.to_radians().sin_cos();
            params.kind = KIND_RADIAL;
            params.inverted = u32::from(*inverted);
            params.first = [center[0], center[1], radii[0], radii[1]];
            params.second = [cosine, sine, *feather, 0.0];
        }
        MaskShape::LuminanceRange { low, high, smoothness } => {
            params.kind = KIND_LUMINANCE;
            params.first = [*low, *high, *smoothness, 0.0];
        }
        MaskShape::ColorRange { samples, tolerance } => {
            params.kind = KIND_COLOUR;
            params.first = [*tolerance, 0.0, 0.0, 0.0];
            params.sample_count = samples.len().min(MAX_COLOUR_SAMPLES) as u32;
            for (slot, sample) in params.samples.iter_mut().zip(samples) {
                *slot = [sample[0], sample[1], sample[2], 0.0];
            }
        }
        MaskShape::Raster { inverted, .. } => {
            params.kind = if has_raster { KIND_PAINTED } else { KIND_NOTHING };
            params.inverted = u32::from(*inverted);
        }
    }
    params
}

fn dab_instance(dab: &BrushDab) -> DabInstance {
    DabInstance {
        position: dab.position,
        radius: dab.radius,
        feather: dab.feather,
        flow: dab.flow,
        flags: if dab.erase { ERASE } else { 0 } | if dab.auto_mask { AUTO_MASK } else { 0 },
    }
}

fn field_rows(masks: &[Mask]) -> Vec<FieldRow> {
    let applied = masks.iter().enumerate();
    applied
        .filter(|(_, mask)| mask.enabled && !mask.components.is_empty() && !mask.adjustments.is_neutral())
        .map(|(index, mask)| FieldRow {
            offsets: field_offsets(&mask.adjustments),
            layer: index as i32,
            padding: [0; 3],
        })
        .collect()
}

pub(crate) struct MaskPipelines {
    dab: wgpu::RenderPipeline,
    first_shape: wgpu::RenderPipeline,
    added_shape: wgpu::RenderPipeline,
    subtracted_shape: wgpu::RenderPipeline,
    combine: wgpu::ComputePipeline,
}

struct RasterPipeline<'a> {
    label: &'a str,
    module: &'a wgpu::ShaderModule,
    entry: [&'a str; 2],
    topology: wgpu::PrimitiveTopology,
    buffers: &'a [Option<wgpu::VertexBufferLayout<'a>>],
    blend: Option<wgpu::BlendState>,
}

fn raster_pipeline(device: &wgpu::Device, spec: RasterPipeline<'_>) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(spec.label),
        layout: None,
        vertex: wgpu::VertexState {
            module: spec.module,
            entry_point: Some(spec.entry[0]),
            compilation_options: Default::default(),
            buffers: spec.buffers,
        },
        primitive: wgpu::PrimitiveState { topology: spec.topology, ..Default::default() },
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: spec.module,
            entry_point: Some(spec.entry[1]),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: COVERAGE_FORMAT,
                blend: spec.blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn blend_of(color: wgpu::BlendComponent) -> Option<wgpu::BlendState> {
    Some(wgpu::BlendState { color, alpha: wgpu::BlendComponent::REPLACE })
}

impl MaskPipelines {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let common = include_str!("shaders/mask_common.wgsl");
        let module = |label: &str, body: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl([common, body].concat().into()),
            })
        };
        let dab_module = module("mask dab", include_str!("shaders/mask_dab.wgsl"));
        let shape_module = module("mask shape", include_str!("shaders/mask_shape.wgsl"));
        let instance = wgpu::VertexBufferLayout {
            array_stride: size_of::<DabInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![
                0 => Float32x2, 1 => Float32, 2 => Float32, 3 => Float32, 4 => Uint32,
            ],
        };
        let dab = raster_pipeline(
            device,
            RasterPipeline {
                label: "mask dab",
                module: &dab_module,
                entry: ["dab_vertex", "dab_fragment"],
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                buffers: &[Some(instance)],
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            },
        );
        let shape = |label, blend| {
            raster_pipeline(
                device,
                RasterPipeline {
                    label,
                    module: &shape_module,
                    entry: ["shape_vertex", "shape_fragment"],
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    buffers: &[],
                    blend,
                },
            )
        };
        let largest = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let remaining = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::OneMinusSrc,
            operation: wgpu::BlendOperation::Add,
        };
        Self {
            dab,
            first_shape: shape("mask first shape", None),
            added_shape: shape("mask added shape", blend_of(largest)),
            subtracted_shape: shape("mask subtracted shape", blend_of(remaining)),
            combine: compile(device, "mask combine", &[include_str!("shaders/mask_combine.wgsl")], &[]),
        }
    }
}

/// Something a mask component drew into, kept so a stroke only ever adds its newest dabs.
struct Paint {
    view: wgpu::TextureView,
}

struct DrawnComponent {
    component: MaskComponent,
    paint: Option<Paint>,
}

#[derive(Default)]
struct DrawnMask {
    components: Vec<DrawnComponent>,
    scene_version: u64,
}

impl DrawnMask {
    fn sources(&self) -> Vec<&MaskComponent> {
        self.components.iter().map(|drawn| &drawn.component).collect()
    }

    fn reads_scene(&self) -> bool {
        self.components.iter().any(|drawn| reads_scene(&drawn.component.shape))
    }
}

struct Stack {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    layers: u32,
}

impl Stack {
    fn layer_view(&self, layer: u32) -> wgpu::TextureView {
        self.texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: layer,
            array_layer_count: Some(1),
            ..Default::default()
        })
    }
}

fn array_view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    })
}

/// What masks read of the photo. `version` moves whenever `preview` shows different pixels.
pub(crate) struct MaskScene<'a> {
    pub preview: &'a Preview,
    pub version: u64,
}

pub(crate) struct MaskFields {
    size: [u32; 2],
    params: wgpu::Buffer,
    blank: wgpu::TextureView,
    neutral: wgpu::TextureView,
    coverage: Option<Stack>,
    field: Option<Stack>,
    drawn: Vec<DrawnMask>,
    combined: Option<(Vec<FieldRow>, i32)>,
    active: bool,
    overlay: Option<usize>,
    rebuilds: u64,
    pub rasters: Option<&'static dyn MaskRasterSource>,
}

fn coverage_size(image_width: u32, image_height: u32) -> [u32; 2] {
    let long_edge = image_width.max(image_height);
    let scale = f64::from(MASK_EDGE.min(long_edge)) / f64::from(long_edge);
    let side = |value: u32| ((f64::from(value) * scale).round() as u32).max(1);
    [side(image_width), side(image_height)]
}

impl MaskFields {
    pub(crate) fn new(engine: &Engine, preview: &Preview, camera_to_working: &Mat3, exposure: f32) -> Self {
        let device = &engine.ctx.device;
        let size = coverage_size(preview.image_width, preview.image_height);
        let long_edge = size[0].max(size[1]);
        let scale = preview.width.max(preview.height) as f32 / long_edge as f32;
        let params = SceneParams {
            camera_to_working: wgsl_mat3(camera_to_working),
            working_to_lms: wgsl_mat3(&working_to_lms()),
            luma: LUMA_WEIGHTS,
            raster_size: [size[0] as f32, size[1] as f32],
            long_edge: long_edge as f32,
            lod: scale.max(1.0).log2().min(preview.levels as f32 - 1.0),
            gain: exposure.exp2(),
            padding: [0.0; 3],
        };
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mask scene"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let tiny = |label, layers, format, data: &[u8]| {
            device.create_texture_with_data(
                &engine.ctx.queue,
                &wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: layers },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                data,
            )
        };
        let blank = tiny("no paint", 1, COVERAGE_FORMAT, &[0; 2]).create_view(&Default::default());
        let empty_field = [0u8; 8 * FIELD_LAYERS];
        let neutral = array_view(&tiny("neutral field", FIELD_LAYERS as u32, FIELD_FORMAT, &empty_field));
        Self {
            size,
            params,
            blank,
            neutral,
            coverage: None,
            field: None,
            drawn: Vec::new(),
            combined: None,
            active: false,
            overlay: None,
            rebuilds: 0,
            rasters: None,
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// The fields the shaders read. With no masks this is a single neutral texel.
    pub(crate) fn field_view(&self) -> &wgpu::TextureView {
        match (&self.field, self.active) {
            (Some(field), true) => &field.view,
            _ => &self.neutral,
        }
    }

    fn make_coverage(&self, engine: &Engine, layers: u32) -> Stack {
        let [width, height] = self.size;
        let texture = engine.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mask coverage"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: layers },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COVERAGE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        Stack { view: array_view(&texture), texture, layers }
    }

    /// Makes room for `count` masks. Outgrowing the stack starts it again, which is rare.
    fn reserve(&mut self, engine: &Engine, count: u32) {
        let fits = self.coverage.as_ref().is_some_and(|stack| stack.layers >= count);
        if !fits {
            let layers = count.next_power_of_two().max(MIN_COVERAGE_LAYERS);
            self.coverage = Some(self.make_coverage(engine, layers));
            self.drawn.clear();
            self.combined = None;
        }
        if self.field.is_none() {
            let [width, height] = self.size;
            let layers = FIELD_LAYERS as u32;
            let texture = engine.storage_texture("mask fields", [width, height, layers], 1, FIELD_FORMAT);
            self.field = Some(Stack { view: array_view(&texture), texture, layers });
        }
        self.drawn.resize_with(self.drawn.len().max(count as usize), DrawnMask::default);
    }

    fn scene_group(
        &self,
        engine: &Engine,
        pipeline: &wgpu::RenderPipeline,
        scene: &MaskScene<'_>,
        extra: &[wgpu::BindGroupEntry<'_>],
    ) -> wgpu::BindGroup {
        let mut entries = vec![
            wgpu::BindGroupEntry { binding: 0, resource: self.params.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&scene.preview.view) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&engine.sampler) },
        ];
        entries.extend_from_slice(extra);
        engine.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &entries,
        })
    }

    fn draw_dabs(
        &self,
        engine: &Engine,
        encoder: &mut wgpu::CommandEncoder,
        scene: &MaskScene<'_>,
        (target, load): (&wgpu::TextureView, wgpu::LoadOp<wgpu::Color>),
        dabs: &[BrushDab],
    ) {
        let pipeline = &engine.pipelines.masks.dab;
        let instances: Vec<DabInstance> = dabs.iter().map(dab_instance).collect();
        let buffer = engine.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("brush dabs"),
            contents: bytemuck::cast_slice(&instances),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let group = self.scene_group(engine, pipeline, scene, &[]);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("brush dabs"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        if !dabs.is_empty() {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..4, 0..instances.len() as u32);
        }
    }

    fn new_paint(&self, engine: &Engine) -> Paint {
        let [width, height] = self.size;
        let texture = engine.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("brush paint"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COVERAGE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        Paint { view: texture.create_view(&Default::default()) }
    }

    fn load_raster(&self, engine: &Engine, id: &str) -> Option<Paint> {
        let raster = self.rasters?.load(id)?;
        let expected = raster.width as usize * raster.height as usize;
        if expected == 0 || raster.coverage.len() != expected {
            return None;
        }
        let texture = engine.ctx.device.create_texture_with_data(
            &engine.ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("mask raster"),
                size: wgpu::Extent3d { width: raster.width, height: raster.height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&raster.coverage),
        );
        Some(Paint { view: texture.create_view(&Default::default()) })
    }

    fn paint_component(
        &self,
        engine: &Engine,
        encoder: &mut wgpu::CommandEncoder,
        scene: &MaskScene<'_>,
        component: &MaskComponent,
    ) -> Option<Paint> {
        match &component.shape {
            MaskShape::Brush { dabs } => {
                let paint = self.new_paint(engine);
                let clear = wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT);
                self.draw_dabs(engine, encoder, scene, (&paint.view, clear), dabs);
                Some(paint)
            }
            MaskShape::Raster { id, .. } => self.load_raster(engine, id),
            _ => None,
        }
    }

    /// Draws a mask's components over part of its coverage map. Blending combines them.
    fn composite(
        &self,
        engine: &Engine,
        encoder: &mut wgpu::CommandEncoder,
        scene: &MaskScene<'_>,
        index: usize,
        rect: PixelRect,
    ) {
        let Some(stack) = &self.coverage else {
            return;
        };
        let pipelines = &engine.pipelines.masks;
        let layer = stack.layer_view(index as u32);
        let shapes: Vec<(ShapeParams, &wgpu::TextureView, bool)> = match self.drawn[index].components.as_slice() {
            [] => vec![(ShapeParams::zeroed(), &self.blank, false)],
            components => components
                .iter()
                .map(|drawn| {
                    let params = shape_params(&drawn.component.shape, drawn.paint.is_some());
                    let paint = drawn.paint.as_ref().map_or(&self.blank, |paint| &paint.view);
                    (params, paint, drawn.component.subtract)
                })
                .collect(),
        };
        let groups: Vec<(&wgpu::RenderPipeline, wgpu::BindGroup)> = shapes
            .iter()
            .enumerate()
            .map(|(order, (params, paint, subtract))| {
                // Nothing has been drawn yet, so a subtraction first of all leaves nothing.
                let blank_first = order == 0 && *subtract;
                let pipeline = match (order, subtract) {
                    (0, _) => &pipelines.first_shape,
                    (_, true) => &pipelines.subtracted_shape,
                    (_, false) => &pipelines.added_shape,
                };
                let contents = if blank_first { ShapeParams::zeroed() } else { *params };
                let uniform = engine.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mask shape"),
                    contents: bytemuck::bytes_of(&contents),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                let extra = [
                    wgpu::BindGroupEntry { binding: 3, resource: uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(paint) },
                ];
                (pipeline, self.scene_group(engine, pipeline, scene, &extra))
            })
            .collect();

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mask coverage"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &layer,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        let [width, height] = rect.size();
        pass.set_scissor_rect(rect.left, rect.top, width, height);
        for (pipeline, group) in &groups {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// Brings one mask's coverage up to date. Returns what changed.
    fn update_mask(
        &mut self,
        engine: &Engine,
        scene: &MaskScene<'_>,
        index: usize,
        mask: &Mask,
    ) -> Result<Option<PixelRect>> {
        let drawn = &self.drawn[index];
        let mut update = plan_update(&drawn.sources(), mask);
        let touches_scene = drawn.reads_scene() || mask.components.iter().any(|new| reads_scene(&new.shape));
        if drawn.scene_version != scene.version && touches_scene {
            update = Update::Redraw;
        }
        let mut encoder = engine.encoder();
        let dirty = match update {
            Update::Keep => None,
            Update::Redraw => {
                let components = mask
                    .components
                    .iter()
                    .map(|component| DrawnComponent {
                        paint: self.paint_component(engine, &mut encoder, scene, component),
                        component: component.clone(),
                    })
                    .collect();
                self.drawn[index].components = components;
                let rect = PixelRect::whole(self.size);
                self.composite(engine, &mut encoder, scene, index, rect);
                Some(rect)
            }
            Update::Grow(first_new) => self.grow_strokes(engine, &mut encoder, scene, index, mask, &first_new),
        };
        self.drawn[index].scene_version = scene.version;
        if dirty.is_some() {
            engine.submit_and_wait(encoder)?;
        }
        Ok(dirty)
    }

    /// Adds only the dabs a stroke gained, so painting costs the same at dab 10 and dab 10,000.
    fn grow_strokes(
        &mut self,
        engine: &Engine,
        encoder: &mut wgpu::CommandEncoder,
        scene: &MaskScene<'_>,
        index: usize,
        mask: &Mask,
        first_new: &[usize],
    ) -> Option<PixelRect> {
        let mut dirty: Option<PixelRect> = None;
        for (position, component) in mask.components.iter().enumerate() {
            let MaskShape::Brush { dabs } = &component.shape else {
                continue;
            };
            let fresh = &dabs[first_new[position]..];
            let Some(rect) = union_of_dabs(fresh, self.size) else {
                continue;
            };
            if let Some(paint) = &self.drawn[index].components[position].paint {
                self.draw_dabs(engine, encoder, scene, (&paint.view, wgpu::LoadOp::Load), fresh);
            }
            self.drawn[index].components[position].component = component.clone();
            dirty = Some(dirty.map_or(rect, |known| known.union(rect)));
        }
        let rect = dirty.filter(|rect| !rect.is_empty())?;
        self.composite(engine, encoder, scene, index, rect);
        Some(rect)
    }

    fn combine(
        &self,
        engine: &Engine,
        rows: &[FieldRow],
        overlay: i32,
        rect: PixelRect,
    ) -> Result<()> {
        let (Some(coverage), Some(field)) = (&self.coverage, &self.field) else {
            return Ok(());
        };
        let device = &engine.ctx.device;
        let [width, height] = rect.size();
        let params = CombineParams {
            origin: [rect.left as i32, rect.top as i32],
            size: [width as i32, height as i32],
            count: rows.len() as u32,
            overlay,
            padding: [0; 2],
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mask combine"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let placeholder = [FieldRow::zeroed()];
        let stored = if rows.is_empty() { &placeholder[..] } else { rows };
        let rows_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mask offsets"),
            contents: bytemuck::cast_slice(stored),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let pipeline = &engine.pipelines.masks.combine;
        let group = engine.bind(
            pipeline,
            &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: rows_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&coverage.view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&field.view) },
            ],
        );
        let mut encoder = engine.encoder();
        engine.dispatch(&mut encoder, pipeline, &group, [width, height], "mask combine", None);
        engine.submit_and_wait(encoder)?;
        Ok(())
    }

    /// Brings the fields up to date. Does no GPU work at all unless a mask really changed, and
    /// `overlay` names the mask to show in red, which stays in the fields once chosen so that
    /// asking for it only some of the time doesn't rebuild them.
    pub(crate) fn sync(
        &mut self,
        engine: &Engine,
        masks: &[Mask],
        scene: &MaskScene<'_>,
        overlay: Option<usize>,
    ) -> Result<()> {
        let limit = engine.ctx.device.limits().max_texture_array_layers as usize;
        let masks = &masks[..masks.len().min(limit)];
        let rows = field_rows(masks);
        let shows_overlay = overlay.is_some_and(|index| index < masks.len());
        if shows_overlay {
            self.overlay = overlay;
        }
        self.active = !rows.is_empty() || shows_overlay;
        if !self.active {
            return Ok(());
        }
        self.reserve(engine, masks.len() as u32);
        let mut dirty: Option<PixelRect> = None;
        for (index, mask) in masks.iter().enumerate() {
            if let Some(rect) = self.update_mask(engine, scene, index, mask)? {
                dirty = Some(dirty.map_or(rect, |known| known.union(rect)));
            }
        }
        let layer = self.overlay.filter(|index| *index < masks.len()).map_or(-1, |index| index as i32);
        let key = (rows, layer);
        if self.combined.as_ref() != Some(&key) {
            self.combine(engine, &key.0, layer, PixelRect::whole(self.size))?;
            self.combined = Some(key);
            self.rebuilds += 1;
        } else if let Some(rect) = dirty {
            self.combine(engine, &key.0, layer, rect)?;
            self.rebuilds += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dab(x: f32) -> BrushDab {
        BrushDab { position: [x, 0.5], radius: 0.01, feather: 0.5, flow: 1.0, erase: false, auto_mask: false }
    }

    fn brush(dabs: Vec<BrushDab>) -> MaskComponent {
        MaskComponent { shape: MaskShape::Brush { dabs }, subtract: false }
    }

    fn mask(components: Vec<MaskComponent>) -> Mask {
        Mask { components, ..Mask::default() }
    }

    #[test]
    fn the_same_mask_needs_nothing() {
        let stroke = brush(vec![dab(0.1), dab(0.2)]);
        assert_eq!(plan_update(&[&stroke], &mask(vec![stroke.clone()])), Update::Keep);
    }

    #[test]
    fn a_continued_stroke_only_adds_its_new_dabs() {
        let before = brush(vec![dab(0.1), dab(0.2)]);
        let after = mask(vec![brush(vec![dab(0.1), dab(0.2), dab(0.3)])]);
        assert_eq!(plan_update(&[&before], &after), Update::Grow(vec![2]));
    }

    #[test]
    fn undoing_a_stroke_redraws() {
        let before = brush(vec![dab(0.1), dab(0.2)]);
        assert_eq!(plan_update(&[&before], &mask(vec![brush(vec![dab(0.1)])])), Update::Redraw);
    }

    #[test]
    fn changing_how_a_component_combines_redraws() {
        let before = brush(vec![dab(0.1)]);
        let flipped = MaskComponent { subtract: true, ..before.clone() };
        assert_eq!(plan_update(&[&before], &mask(vec![flipped])), Update::Redraw);
    }

    #[test]
    fn a_dab_rect_stays_on_the_map_and_holds_the_dab() {
        let rect = dab_rect(&dab(0.0), [1000, 500]);
        assert_eq!(rect.left, 0);
        assert!(rect.right >= 10 && rect.top < 250 && rect.bottom > 250);
    }

    #[test]
    fn coverage_is_the_same_shape_as_the_photo_and_never_finer() {
        assert_eq!(coverage_size(9504, 6336), [2048, 1365]);
        assert_eq!(coverage_size(96, 96), [96, 96]);
    }

    #[test]
    fn rows_leave_out_what_can_have_no_effect() {
        let mut idle = mask(vec![brush(vec![dab(0.1)])]);
        let mut working = idle.clone();
        working.adjustments.exposure = 1.0;
        let mut disabled = working.clone();
        disabled.enabled = false;
        idle.adjustments.exposure = 0.0;
        let rows = field_rows(&[idle, disabled, working]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].layer, 2);
    }

    #[test]
    fn uniform_layouts_match_the_shaders() {
        assert_eq!(size_of::<SceneParams>(), 144);
        assert_eq!(size_of::<ShapeParams>(), 176);
        assert_eq!(size_of::<FieldRow>(), 80);
        assert_eq!(size_of::<CombineParams>(), 32);
    }
}
