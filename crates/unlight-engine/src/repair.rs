//! Healing, cloning and red eye. These run straight after demosaicing, on the preview pyramid
//! and on the full image alike, so every later tool sees the repaired photo.
//!
//! A heal needs the ratio of the photo around the target to the photo around the source. That
//! ratio comes from the shared pyramid at a scale set by the spot's size, so preview and export
//! solve the same problem, and the solve itself is the closed form for a disc. Each repaired
//! pixel is a pure function of the unrepaired image, so tile size can't show in the result.

use crate::{
    demosaic::entry,
    error::Result,
    mosaic_repair::level_view,
    params::TileParams,
    pipelines::{Engine, compile},
    scheduler::{SchedulerConfig, Tile},
    source::Preview,
};
use bytemuck::{Pod, Zeroable};
use unlight_recipe::{HealMode, HealSpot, RedEye};
use wgpu::util::DeviceExt;

/// Must match `RING_SAMPLES` in repair.wgsl and repair_rings.wgsl.
const RING_SAMPLES: u32 = 48;
const RING_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
/// The gap between ring points as a share of the radius. Reading the pyramid at that scale
/// makes each point stand for its stretch of the ring, rather than a few noisy pixels.
const RING_SPACING: f32 = 0.147;

/// How far past a changed area a pyramid level has to be redone: a texel for the filter's
/// footprint, and one for how far an odd-sized level's texels drift from half of the level above.
const MIP_MARGIN: u32 = 2;

const KIND_CLONE: u32 = 0;
const KIND_HEAL: u32 = 1;
const KIND_RED_EYE: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RepairCell {
    origin: [i32; 2],
    size: [i32; 2],
    count: u32,
    padding: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct RepairSpot {
    center: [f32; 2],
    offset: [i32; 2],
    radius: f32,
    feather: f32,
    opacity: f32,
    kind: u32,
    ring: i32,
    pupil: f32,
    darken: f32,
    padding: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RingSpot {
    center: [f32; 2],
    patch_center: [f32; 2],
    radius: [f32; 2],
    lod: f32,
    padding: f32,
}

/// An axis-aligned area of the photo, in normalised image positions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct UvRect {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

fn rect_around(center: [f32; 2], radius: f32, size: [u32; 2]) -> UvRect {
    let long_edge = size[0].max(size[1]) as f32;
    let reach = [radius * long_edge / size[0] as f32, radius * long_edge / size[1] as f32];
    UvRect {
        min: [center[0] - reach[0], center[1] - reach[1]],
        max: [center[0] + reach[0], center[1] + reach[1]],
    }
}

/// Where a spot lands on an image of this size. Offsets are whole pixels, because resampling
/// the patch would soften it next to the grain around it.
fn place_heal(spot: &HealSpot, ring: usize, size: [u32; 2]) -> RepairSpot {
    let (width, height) = (size[0] as f32, size[1] as f32);
    let offset = |axis: usize, side: f32| ((spot.source[axis] - spot.target[axis]) * side).round() as i32;
    RepairSpot {
        center: [spot.target[0] * width, spot.target[1] * height],
        offset: [offset(0, width), offset(1, height)],
        radius: spot.radius * width.max(height),
        feather: spot.feather,
        opacity: spot.opacity,
        kind: if spot.mode == HealMode::Heal { KIND_HEAL } else { KIND_CLONE },
        ring: ring as i32,
        pupil: 0.0,
        darken: 0.0,
        padding: 0.0,
    }
}

fn place_eye(eye: &RedEye, size: [u32; 2]) -> RepairSpot {
    let (width, height) = (size[0] as f32, size[1] as f32);
    RepairSpot {
        center: [eye.center[0] * width, eye.center[1] * height],
        offset: [0; 2],
        radius: eye.radius * width.max(height),
        feather: 0.0,
        opacity: 1.0,
        kind: KIND_RED_EYE,
        ring: 0,
        pupil: eye.pupil_size,
        darken: eye.darken,
        padding: 0.0,
    }
}

fn touches(spot: &RepairSpot, tile: Tile) -> bool {
    let reach = spot.radius + 1.0;
    spot.center[0] + reach >= tile.x as f32
        && spot.center[0] - reach <= (tile.x + tile.width) as f32
        && spot.center[1] + reach >= tile.y as f32
        && spot.center[1] - reach <= (tile.y + tile.height) as f32
}

/// Whatever is in one list and not the other, as areas of the image that have to be redone.
fn changed_areas(
    (old_healing, old_eyes): (&[HealSpot], &[RedEye]),
    (healing, eyes): (&[HealSpot], &[RedEye]),
    size: [u32; 2],
) -> Vec<UvRect> {
    let spot_rect = |spot: &HealSpot| rect_around(spot.target, spot.radius, size);
    let eye_rect = |eye: &RedEye| rect_around(eye.center, eye.radius, size);
    let gone = old_healing.iter().filter(|spot| !healing.contains(spot)).map(spot_rect);
    let added = healing.iter().filter(|spot| !old_healing.contains(spot)).map(spot_rect);
    let eyes_gone = old_eyes.iter().filter(|eye| !eyes.contains(eye)).map(eye_rect);
    let eyes_added = eyes.iter().filter(|eye| !old_eyes.contains(eye)).map(eye_rect);
    gone.chain(added).chain(eyes_gone).chain(eyes_added).collect()
}

fn pixel_tile(area: UvRect, size: [u32; 2]) -> Tile {
    let low = |axis: usize| ((area.min[axis].clamp(0.0, 1.0) * size[axis] as f32).floor() as u32).saturating_sub(1);
    let high = |axis: usize| {
        (((area.max[axis].clamp(0.0, 1.0) * size[axis] as f32).ceil() as u32) + 1).min(size[axis])
    };
    let (left, top) = (low(0), low(1));
    Tile { x: left, y: top, width: high(0).saturating_sub(left), height: high(1).saturating_sub(top) }
}

/// The part of a pyramid level that a changed area of the base can reach. A level's texels sit
/// in proportion to the image, so this scales rather than shifts: on an odd size they're not at
/// half of the level above.
fn level_area(area: Tile, base: [u32; 2], size: [u32; 2]) -> Tile {
    let side = |start: u32, length: u32, axis: usize| {
        let scale = size[axis] as f32 / base[axis] as f32;
        let low = ((start as f32 * scale).floor() as u32).saturating_sub(MIP_MARGIN);
        let high = (((start + length) as f32 * scale).ceil() as u32 + MIP_MARGIN).min(size[axis]);
        (low, high.saturating_sub(low))
    };
    let ((x, width), (y, height)) = (side(area.x, area.width, 0), side(area.y, area.height, 1));
    Tile { x, y, width, height }
}

fn bounds(areas: &[UvRect]) -> Option<UvRect> {
    areas.iter().copied().reduce(|a, b| UvRect {
        min: [a.min[0].min(b.min[0]), a.min[1].min(b.min[1])],
        max: [a.max[0].max(b.max[0]), a.max[1].max(b.max[1])],
    })
}

pub(crate) struct RepairPipelines {
    pub preview: wgpu::ComputePipeline,
    pub full: wgpu::ComputePipeline,
    rings: wgpu::ComputePipeline,
}

impl RepairPipelines {
    pub(crate) fn new(device: &wgpu::Device, full_format: &str) -> Self {
        let repair = [include_str!("shaders/repair.wgsl")];
        Self {
            preview: compile(device, "repair preview", &repair, &[("{{OUTPUT_FORMAT}}", "rgba16float")]),
            full: compile(device, "repair full", &repair, &[("{{OUTPUT_FORMAT}}", full_format)]),
            rings: compile(device, "repair rings", &[include_str!("shaders/repair_rings.wgsl")], &[]),
        }
    }
}

/// The repairs in force, and the pyramid they've been applied to.
#[derive(Default)]
pub(crate) struct Repairs {
    healing: Vec<HealSpot>,
    red_eye: Vec<RedEye>,
    preview: Option<Preview>,
    rings: Option<wgpu::TextureView>,
    version: u64,
}

impl Repairs {
    pub(crate) fn is_active(&self) -> bool {
        !self.healing.is_empty() || !self.red_eye.is_empty()
    }

    /// Moves whenever the repaired image does, so whatever was derived from it knows to redo.
    pub(crate) fn version(&self) -> u64 {
        self.version
    }

    /// The repaired pyramid, or `None` when there's nothing to repair.
    pub(crate) fn preview(&self) -> Option<&Preview> {
        self.preview.as_ref()
    }

    pub(crate) fn heal_targets(&self) -> impl Iterator<Item = &HealSpot> {
        self.healing.iter()
    }

    fn placed(&self, size: [u32; 2]) -> impl Iterator<Item = RepairSpot> {
        let healing = self.healing.iter().enumerate().map(move |(ring, spot)| place_heal(spot, ring, size));
        healing.chain(self.red_eye.iter().map(move |eye| place_eye(eye, size)))
    }

    /// Parts of an image of this size, outside `tile`, that repairing `tile` reads.
    pub(crate) fn sources_of(&self, tile: Tile, size: [u32; 2]) -> Vec<Tile> {
        self.placed(size)
            .filter(|spot| spot.kind != KIND_RED_EYE && touches(spot, tile))
            .map(|spot| {
                let reach = spot.radius.ceil() as i32 + 1;
                let side = |axis: usize, extent: u32| {
                    let center = spot.center[axis] as i32 + spot.offset[axis];
                    let low = (center - reach).clamp(0, extent as i32);
                    let high = (center + reach).clamp(0, extent as i32);
                    (low as u32, (high - low) as u32)
                };
                let ((x, width), (y, height)) = (side(0, size[0]), side(1, size[1]));
                Tile { x, y, width, height }
            })
            .collect()
    }

    /// Repairs one tile of `input` into `output`. Pixels no spot touches pass through.
    pub(crate) fn encode_tile(
        &self,
        engine: &Engine,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::ComputePipeline,
        tile: Tile,
        size: [u32; 2],
        [input, output]: [&wgpu::TextureView; 2],
    ) {
        let device = &engine.ctx.device;
        let spots: Vec<RepairSpot> = self.placed(size).filter(|spot| touches(spot, tile)).collect();
        let cell = RepairCell {
            origin: [tile.x as i32, tile.y as i32],
            size: [tile.width as i32, tile.height as i32],
            count: spots.len() as u32,
            padding: [0; 3],
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("repair cell"),
            contents: bytemuck::bytes_of(&cell),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let placeholder = [RepairSpot::zeroed()];
        let stored = if spots.is_empty() { &placeholder[..] } else { &spots };
        let list = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("repair spots"),
            contents: bytemuck::cast_slice(stored),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let Some(rings) = &self.rings else {
            return;
        };
        let group = engine.bind(
            pipeline,
            &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: list.as_entire_binding() },
                entry(2, input),
                entry(3, rings),
                entry(4, output),
            ],
        );
        engine.dispatch(encoder, pipeline, &group, [tile.width, tile.height], "repair", None);
    }

    fn solve_rings(&mut self, engine: &Engine, original: &Preview) -> Result<()> {
        let device = &engine.ctx.device;
        let size = [original.width, original.height];
        let (width, height) = (size[0] as f32, size[1] as f32);
        let long_edge = width.max(height);
        let spots: Vec<RingSpot> = self
            .healing
            .iter()
            .map(|spot| {
                let placed = place_heal(spot, 0, size);
                let spacing = placed.radius * RING_SPACING;
                RingSpot {
                    center: spot.target,
                    patch_center: [
                        spot.target[0] + placed.offset[0] as f32 / width,
                        spot.target[1] + placed.offset[1] as f32 / height,
                    ],
                    radius: [spot.radius * long_edge / width, spot.radius * long_edge / height],
                    lod: spacing.max(1.0).log2().min(original.levels as f32 - 1.0),
                    padding: 0.0,
                }
            })
            .collect();
        let rows = (spots.len() as u32).max(1);
        let texture = engine.storage_texture("repair rings", [RING_SAMPLES, rows, 1], 1, RING_FORMAT);
        let view = texture.create_view(&Default::default());
        if !spots.is_empty() {
            let list = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ring spots"),
                contents: bytemuck::cast_slice(&spots),
                usage: wgpu::BufferUsages::STORAGE,
            });
            let pipeline = &engine.pipelines.repair.rings;
            let group = engine.bind(
                pipeline,
                &[
                    wgpu::BindGroupEntry { binding: 0, resource: list.as_entire_binding() },
                    entry(1, &original.view),
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&engine.sampler) },
                    entry(3, &view),
                ],
            );
            let mut encoder = engine.encoder();
            engine.dispatch(&mut encoder, pipeline, &group, [RING_SAMPLES, rows], "repair rings", None);
            engine.submit_and_wait(encoder)?;
        }
        self.rings = Some(view);
        Ok(())
    }

    fn patch_preview(&self, engine: &Engine, original: &Preview, repaired: &Preview, area: Tile) -> Result<()> {
        let size = [original.width, original.height];
        let input = level_view(original.texture(), 0);
        let output = level_view(repaired.texture(), 0);
        let pipeline = &engine.pipelines.repair.preview;
        let schedule = SchedulerConfig { initial_edge: 1024, ..SchedulerConfig::BACKGROUND };
        engine.run_tiles(area.width, area.height, schedule, |encoder, tile| {
            let placed = Tile { x: tile.x + area.x, y: tile.y + area.y, ..tile };
            self.encode_tile(engine, encoder, pipeline, placed, size, [&input, &output]);
        })?;
        for level in 1..repaired.levels {
            let (from, to) = (level_view(repaired.texture(), level - 1), level_view(repaired.texture(), level));
            let group = engine.bind(
                &engine.pipelines.downsample,
                &[engine.tile_entry(), entry(1, &from), entry(2, &to)],
            );
            let level_size = size.map(|side| (side >> level).max(1));
            let changed = level_area(area, size, level_size);
            engine.run_tiles(changed.width, changed.height, schedule, |encoder, tile| {
                engine.write_tile(&TileParams {
                    origin: [(tile.x + changed.x) as i32, (tile.y + changed.y) as i32],
                    size: [tile.width as i32, tile.height as i32],
                    image: [level_size[0] as i32, level_size[1] as i32],
                    pad: 0,
                    period: 1,
                    cfa: [[0; 4]; 9],
                });
                let extent = [tile.width, tile.height];
                engine.dispatch(encoder, &engine.pipelines.downsample, &group, extent, "repair downsample", None);
            })?;
        }
        Ok(())
    }

    /// Takes on a recipe's repairs. Returns the areas that changed, for full-resolution work
    /// that was derived from them.
    pub(crate) fn sync(
        &mut self,
        engine: &Engine,
        original: &Preview,
        healing: &[HealSpot],
        red_eye: &[RedEye],
    ) -> Result<Vec<UvRect>> {
        if self.healing == healing && self.red_eye == red_eye {
            return Ok(Vec::new());
        }
        let size = [original.image_width, original.image_height];
        let changed = changed_areas((&self.healing, &self.red_eye), (healing, red_eye), size);
        self.healing = healing.to_vec();
        self.red_eye = red_eye.to_vec();
        self.version += 1;
        if !self.is_active() {
            self.preview = None;
            self.rings = None;
            return Ok(changed);
        }
        self.solve_rings(engine, original)?;
        let whole = Tile { x: 0, y: 0, width: original.width, height: original.height };
        let area = match (&self.preview, bounds(&changed)) {
            (Some(_), Some(area)) => pixel_tile(area, [original.width, original.height]),
            _ => whole,
        };
        let repaired = match self.preview.take() {
            Some(repaired) => repaired,
            None => engine.blank_preview(original),
        };
        let patched = self.patch_preview(engine, original, &repaired, area);
        self.preview = Some(repaired);
        patched?;
        Ok(changed)
    }

    /// A whole demosaiced image with the repairs applied, for export.
    pub(crate) fn repair_image(
        &self,
        engine: &Engine,
        input: &wgpu::TextureView,
        size: [u32; 2],
        schedule: SchedulerConfig,
    ) -> Result<wgpu::TextureView> {
        let output = engine
            .storage_texture("repaired image", [size[0], size[1], 1], 1, engine.precision.format())
            .create_view(&Default::default());
        let pipeline = &engine.pipelines.repair.full;
        engine.run_tiles(size[0], size[1], schedule, |encoder, tile| {
            self.encode_tile(engine, encoder, pipeline, tile, size, [input, &output]);
        })?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(target: [f32; 2]) -> HealSpot {
        HealSpot { mode: HealMode::Heal, target, source: [0.8, 0.8], radius: 0.02, feather: 0.5, opacity: 1.0 }
    }

    #[test]
    fn a_patch_moves_by_whole_pixels_of_the_image_it_lands_on() {
        let healing = HealSpot { source: [0.55, 0.5], target: [0.5, 0.5], ..spot([0.5, 0.5]) };
        assert_eq!(place_heal(&healing, 0, [1000, 500]).offset, [50, 0]);
        assert_eq!(place_heal(&healing, 0, [500, 250]).offset, [25, 0]);
    }

    #[test]
    fn radii_are_a_share_of_the_long_edge_on_either_orientation() {
        assert_eq!(place_heal(&spot([0.5, 0.5]), 0, [1000, 500]).radius, 20.0);
        assert_eq!(place_heal(&spot([0.5, 0.5]), 0, [500, 1000]).radius, 20.0);
    }

    #[test]
    fn only_tiles_under_a_spot_pick_it_up() {
        let placed = place_heal(&spot([0.5, 0.5]), 0, [1000, 1000]);
        let near = Tile { x: 400, y: 400, width: 100, height: 100 };
        let far = Tile { x: 0, y: 0, width: 100, height: 100 };
        assert!(touches(&placed, near));
        assert!(!touches(&placed, far));
    }

    #[test]
    fn moving_a_spot_redoes_both_where_it_was_and_where_it_went() {
        let old = [spot([0.2, 0.2])];
        let new = [spot([0.7, 0.7])];
        let areas = changed_areas((&old, &[]), (&new, &[]), [1000, 1000]);
        assert_eq!(areas.len(), 2);
    }

    #[test]
    fn an_unchanged_spot_redoes_nothing() {
        let same = [spot([0.2, 0.2])];
        assert!(changed_areas((&same, &[]), (&same, &[]), [1000, 1000]).is_empty());
    }

    #[test]
    fn a_changed_area_reaches_past_its_half_on_an_odd_level() {
        let area = Tile { x: 1000, y: 0, width: 20, height: 20 };
        let level = level_area(area, [1051, 699], [525, 349]);
        assert!(level.x < 499 && level.x + level.width > 510, "{level:?}");
    }

    #[test]
    fn a_changed_area_stays_on_its_level() {
        let area = Tile { x: 1040, y: 690, width: 11, height: 9 };
        let level = level_area(area, [1051, 699], [525, 349]);
        assert_eq!((level.x + level.width, level.y + level.height), (525, 349));
    }

    #[test]
    fn a_tile_reads_the_source_of_a_spot_above_it() {
        let repairs = Repairs { healing: vec![spot([0.5, 0.5])], ..Repairs::default() };
        let tile = Tile { x: 480, y: 480, width: 40, height: 40 };
        let sources = repairs.sources_of(tile, [1000, 1000]);
        assert_eq!(sources.len(), 1);
        assert!(sources[0].x > 700 && sources[0].y > 700, "{sources:?}");
    }

    #[test]
    fn a_red_eye_reads_nothing_elsewhere() {
        let eye = RedEye { center: [0.5, 0.5], radius: 0.02, pupil_size: 0.7, darken: 0.5 };
        let repairs = Repairs { red_eye: vec![eye], ..Repairs::default() };
        let tile = Tile { x: 480, y: 480, width: 40, height: 40 };
        assert!(repairs.sources_of(tile, [1000, 1000]).is_empty());
    }

    #[test]
    fn storage_layout_matches_the_shader() {
        assert_eq!(size_of::<RepairSpot>(), 48);
        assert_eq!(size_of::<RingSpot>(), 32);
        assert_eq!(size_of::<RepairCell>(), 32);
    }
}
