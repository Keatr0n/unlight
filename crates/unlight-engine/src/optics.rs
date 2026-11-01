//! Everything a session derives from a file to place pixels: the lens profile, the Upright
//! solution and the frame fit. None of it belongs in a recipe, which only holds sliders, so it
//! is worked out once per photo and redone only when a setting that feeds it changes.

use crate::{
    error::{EngineError, Result},
    geometry::{Geometry, Optics, distortion_strength},
    lines::{LumaImage, Segment, detect_segments},
    pipelines::Engine,
    source::Preview,
    upright,
};
use rayon::prelude::*;
use std::sync::Arc;
use unlight_color::{Mat3, matrix::mul};
use unlight_lens::{CURVE_SIZE, Frame, LensError, LensProfile, PhotoLens, Source};
use unlight_raw::{GainMap, Opcode, RawImage};
use unlight_recipe::{Crop, GuideLine, LensCorrections, Recipe, Transform, Upright};
use wgpu::util::DeviceExt;

/// Line detection runs on the largest preview level no longer than this.
const LINE_IMAGE_EDGE: u32 = 2048;
/// Shortest line Upright will use, as a share of the image height.
const MIN_LINE_SHARE: f32 = 0.05;
/// The diagonal of a full-frame sensor, which is what 35 mm equivalents are relative to.
const FULL_FRAME_DIAGONAL_MM: f64 = 43.27;
/// Assumed when a file gives no focal length. A wrong guess only changes proportions a little.
const FALLBACK_FOCAL_35MM: f64 = 35.0;
const RGBA16F_BYTES: u32 = 8;
const RGBA32F_BYTES: u32 = 16;

/// A post-demosaic gain map, ready for the develop pass.
struct PlaneGainGrid {
    points: [u32; 2],
    origin: [f32; 2],
    spacing: [f32; 2],
    gains: Vec<[f32; 4]>,
}

/// Folds OpcodeList3 gain maps into one texture with a channel per colour plane. Returns `None`
/// for layouts that aren't one shared grid.
fn plane_gain_grid(list: &[Opcode]) -> Option<PlaneGainGrid> {
    let maps: Vec<&GainMap> = list
        .iter()
        .filter_map(|opcode| match opcode {
            Opcode::GainMap(map) => Some(map),
            _ => None,
        })
        .collect();
    let first = *maps.first()?;
    let same_grid = maps.iter().all(|map| {
        map.points_h == first.points_h
            && map.points_v == first.points_v
            && map.spacing_h == first.spacing_h
            && map.spacing_v == first.spacing_v
    });
    if !same_grid {
        return None;
    }
    let count = (first.points_h * first.points_v) as usize;
    let mut gains = vec![[1.0; 4]; count];
    for map in &maps {
        let stride = map.map_planes.max(1) as usize;
        for plane in map.plane..(map.plane + map.planes.max(1)).min(3) {
            let layer = ((plane - map.plane) as usize).min(stride - 1);
            for (index, gain) in gains.iter_mut().enumerate() {
                gain[plane as usize] = *map.gains.get(index * stride + layer)?;
            }
        }
    }
    Some(PlaneGainGrid {
        points: [first.points_h, first.points_v],
        origin: [first.origin_h as f32, first.origin_v as f32],
        spacing: [first.spacing_h as f32, first.spacing_v as f32],
        gains,
    })
}

/// Where a uv lands on the gain texture, as a scale and an offset.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PlaneGainMapping {
    pub enabled: bool,
    pub scale: [f32; 2],
    pub offset: [f32; 2],
}

fn plane_gain_mapping(grid: &PlaneGainGrid) -> PlaneGainMapping {
    let axis = |axis: usize| {
        let points = grid.points[axis] as f32;
        let scale = 1.0 / (grid.spacing[axis] * points);
        (scale, 0.5 / points - grid.origin[axis] * scale)
    };
    let ((scale_x, offset_x), (scale_y, offset_y)) = (axis(0), axis(1));
    PlaneGainMapping { enabled: true, scale: [scale_x, scale_y], offset: [offset_x, offset_y] }
}

/// Settings that feed the solved geometry. If none of these moved, neither did it.
#[derive(Clone, Debug, PartialEq)]
struct SolveKey {
    profile_address: Option<usize>,
    distortion: f32,
    manual_distortion: f32,
    upright: Upright,
    guides: Vec<GuideLine>,
    allow_empty_corners: bool,
}

fn half_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from(bits >> 10 & 0x1f);
    let fraction = f32::from(bits & 0x3ff);
    let magnitude = match exponent {
        0 => fraction * 2f32.powi(-24),
        31 => f32::MAX,
        _ => (1.0 + fraction / 1024.0) * 2f32.powi(exponent - 15),
    };
    sign * magnitude
}

/// Gamma-encodes a preview to the scale line detection works on.
fn luma_from_rgba16f(bytes: &[u8], width: usize, height: usize, row_bytes: usize) -> LumaImage {
    let mut values = vec![0.0; width * height];
    values.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let line = &bytes[y * row_bytes..y * row_bytes + width * RGBA16F_BYTES as usize];
        for (value, texel) in row.iter_mut().zip(line.as_chunks::<{ RGBA16F_BYTES as usize }>().0) {
            let channel = |index: usize| half_to_f32(u16::from_le_bytes([texel[index * 2], texel[index * 2 + 1]]));
            let linear = (channel(0) + 2.0 * channel(1) + channel(2)) * 0.25;
            *value = linear.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0;
        }
    });
    LumaImage { width, height, values }
}

impl Engine {
    /// Copies a preview level back as luma. This is the one slow step in solving Upright, so
    /// it happens at most once a photo.
    fn read_preview_luma(&self, preview: &Preview) -> Result<LumaImage> {
        let level = (0..preview.levels)
            .find(|&level| preview.width.max(preview.height) >> level <= LINE_IMAGE_EDGE)
            .unwrap_or(preview.levels - 1);
        let (width, height) = ((preview.width >> level).max(1), (preview.height >> level).max(1));
        let row = (width * RGBA16F_BYTES).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("preview luma"),
            size: u64::from(row) * u64::from(height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.encoder();
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: preview.view.texture(),
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.submit_and_wait(encoder)?;
        let bytes = self.map_read(&buffer)?;
        Ok(luma_from_rgba16f(&bytes, width as usize, height as usize, row as usize))
    }
}

/// Resamples an image through the lens correction, so that lines are straight when detected.
fn undistorted_luma(luma: &LumaImage, geometry: &Geometry, aspect: f32) -> LumaImage {
    let (width, height) = (luma.width, luma.height);
    let mut values = vec![0.0; width * height];
    values.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        for (x, value) in row.iter_mut().enumerate() {
            let view_x = (x as f32 + 0.5) / width as f32;
            let view_y = (y as f32 + 0.5) / height as f32;
            let [source_x, source_y] = geometry.warp([(view_x - 0.5) * aspect, view_y - 0.5]);
            *value = luma.sample(
                (source_x / aspect + 0.5) * width as f32 - 0.5,
                (source_y + 0.5) * height as f32 - 0.5,
            );
        }
    });
    LumaImage { width, height, values }
}

fn to_centred(segment: &Segment, luma: &LumaImage, aspect: f32) -> Segment {
    let place = |point: [f32; 2]| {
        [(point[0] / luma.width as f32 - 0.5) * aspect, point[1] / luma.height as f32 - 0.5]
    };
    Segment { start: place(segment.start), end: place(segment.end) }
}

pub(crate) struct OpticsState {
    lens: PhotoLens,
    aspect: f32,
    focal: f64,
    file_profile: Option<Arc<LensProfile>>,
    resolved: Option<std::result::Result<Arc<LensProfile>, LensError>>,
    luma: Option<LumaImage>,
    solved_for: Option<SolveKey>,
    optics: Optics,
    curve_texture: wgpu::Texture,
    pub curve_view: wgpu::TextureView,
    uploaded: Option<Arc<LensProfile>>,
    pub plane_gain_view: wgpu::TextureView,
    pub plane_gain: PlaneGainMapping,
}

/// Focal length in image heights, which is what `K` in Upright's `K·R·K⁻¹` wants.
fn focal_in_image_heights(raw: &RawImage, frame: Frame) -> f64 {
    let stated = f64::from(raw.metadata.focal_length_35mm);
    let equivalent = if stated > 0.0 { stated } else { FALLBACK_FOCAL_35MM };
    equivalent / FULL_FRAME_DIAGONAL_MM * 2.0 * frame.half_diagonal()
}

fn float_texture(engine: &Engine, label: &str, size: [u32; 2], usage: wgpu::TextureUsages, data: &[[f32; 4]]) -> wgpu::Texture {
    engine.ctx.device.create_texture_with_data(
        &engine.ctx.queue,
        &wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | usage,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(data),
    )
}

impl OpticsState {
    pub fn new(engine: &Engine, raw: &RawImage) -> Self {
        let lens = PhotoLens::new(raw);
        let frame = Frame::new(raw.width, raw.height);
        let curve_texture = float_texture(
            engine,
            "lens curve",
            [CURVE_SIZE as u32, 1],
            wgpu::TextureUsages::COPY_DST,
            &[[0.0; 4]; CURVE_SIZE],
        );
        let grid = plane_gain_grid(&raw.opcodes.list3);
        let identity = [[1.0f32; 4]];
        let (size, gains): ([u32; 2], &[[f32; 4]]) =
            grid.as_ref().map_or(([1, 1], &identity), |grid| (grid.points, &grid.gains));
        let plane_gain_view = float_texture(engine, "plane gain", size, wgpu::TextureUsages::empty(), gains)
            .create_view(&Default::default());
        Self {
            file_profile: lens.file_profile().map(Arc::new),
            lens,
            aspect: raw.width as f32 / raw.height as f32,
            focal: focal_in_image_heights(raw, frame),
            resolved: None,
            luma: None,
            solved_for: None,
            optics: Optics::default(),
            curve_view: curve_texture.create_view(&Default::default()),
            curve_texture,
            uploaded: None,
            plane_gain_view,
            plane_gain: grid.as_ref().map_or_else(PlaneGainMapping::default, plane_gain_mapping),
        }
    }

    pub fn current(&self) -> &Optics {
        &self.optics
    }

    /// Why a requested profile couldn't be applied, if that's what happened.
    pub fn lens_error(&self) -> Option<&LensError> {
        self.resolved.as_ref().and_then(|resolved| resolved.as_ref().err())
    }

    /// Looks the lens up only once something asks for it, because the database costs a load
    /// and most photos opened while culling never need it.
    fn select_profile(&mut self, lens: &LensCorrections) -> Option<Arc<LensProfile>> {
        if !lens.profile_enabled && !lens.remove_chromatic_aberration {
            return self.file_profile.clone();
        }
        let source = &self.lens;
        let resolved = self.resolved.get_or_insert_with(|| source.resolve().map(Arc::new));
        match resolved {
            Ok(profile) => Some(profile.clone()),
            Err(_) => self.file_profile.clone(),
        }
    }

    fn upload_curve(&mut self, engine: &Engine, profile: Option<&Arc<LensProfile>>) {
        let unchanged = match (profile, &self.uploaded) {
            (Some(new), Some(old)) => Arc::ptr_eq(new, old),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        let zeros = [[0.0f32; 4]; CURVE_SIZE];
        let curve = profile.map_or(zeros.as_slice(), |profile| profile.curve());
        engine.ctx.queue.write_texture(
            self.curve_texture.as_image_copy(),
            bytemuck::cast_slice(curve),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(CURVE_SIZE as u32 * RGBA32F_BYTES),
                rows_per_image: None,
            },
            wgpu::Extent3d { width: CURVE_SIZE as u32, height: 1, depth_or_array_layers: 1 },
        );
        self.uploaded = profile.cloned();
    }

    fn solve_upright(
        &mut self,
        engine: &Engine,
        preview: &Preview,
        recipe: &Recipe,
        optics: &Optics,
    ) -> Result<Option<Mat3>> {
        let mode = recipe.transform.upright;
        if mode == Upright::Off {
            return Ok(None);
        }
        let aspect = self.aspect;
        let geometry =
            Geometry::new(&Crop::default(), &Transform::default(), &recipe.lens, aspect, optics);
        let segments = if mode == Upright::Guided {
            Vec::new()
        } else {
            if self.luma.is_none() {
                self.luma = Some(engine.read_preview_luma(preview)?);
            }
            let luma = self.luma.as_ref().ok_or_else(|| EngineError::Gpu("preview luma".into()))?;
            let straight = undistorted_luma(luma, &geometry, aspect);
            let found = detect_segments(&straight, luma.height as f32 * MIN_LINE_SHARE);
            found.iter().map(|segment| to_centred(segment, luma, aspect)).collect()
        };
        let guides: Vec<Segment> = recipe
            .transform
            .guides
            .iter()
            .map(|guide| {
                let place = |point: [f32; 2]| geometry.unwarp([(point[0] - 0.5) * aspect, point[1] - 0.5]);
                Segment { start: place(guide.start), end: place(guide.end) }
            })
            .collect();
        Ok(upright::solve(mode, &segments, &guides, self.focal))
    }

    /// Brings profile, Upright and frame fit up to date with a recipe.
    pub fn sync(&mut self, engine: &Engine, preview: &Preview, recipe: &Recipe) -> Result<()> {
        let profile = self.select_profile(&recipe.lens);
        self.upload_curve(engine, profile.as_ref());
        let key = SolveKey {
            profile_address: profile.as_ref().map(|profile| Arc::as_ptr(profile) as usize),
            distortion: profile.as_deref().map_or(0.0, |profile| distortion_strength(&recipe.lens, profile)),
            manual_distortion: recipe.lens.manual_distortion,
            upright: recipe.transform.upright,
            guides: recipe.transform.guides.clone(),
            allow_empty_corners: recipe.transform.allow_empty_corners,
        };
        if self.solved_for.as_ref() == Some(&key) {
            return Ok(());
        }

        let mut optics = Optics { profile, correction: None };
        let upright = self.solve_upright(engine, preview, recipe, &optics)?;
        optics.correction = upright;

        let moves_frame = optics.profile.as_deref().is_some_and(|profile| {
            profile.distortion != Some(Source::DngOpcodes) && distortion_strength(&recipe.lens, profile) != 0.0
        });
        if !recipe.transform.allow_empty_corners && (upright.is_some() || moves_frame) {
            let geometry =
                Geometry::new(&Crop::default(), &Transform::default(), &recipe.lens, self.aspect, &optics);
            let fit = geometry.frame_fit(self.aspect, upright.is_some());
            optics.correction = Some(upright.map_or(fit, |matrix| mul(&fit, &matrix)));
        }
        self.optics = optics;
        self.solved_for = Some(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gain_map(plane: u32, planes: u32, map_planes: u32, gains: Vec<f32>) -> Opcode {
        Opcode::GainMap(GainMap {
            top: 0,
            left: 0,
            bottom: 100,
            right: 100,
            plane,
            planes,
            row_pitch: 1,
            col_pitch: 1,
            points_v: 1,
            points_h: 2,
            spacing_v: 1.0,
            spacing_h: 1.0,
            origin_v: 0.0,
            origin_h: 0.0,
            map_planes,
            gains,
        })
    }

    #[test]
    fn plane_gain_maps_fill_their_own_channels() {
        let list = [gain_map(0, 1, 1, vec![2.0, 3.0]), gain_map(2, 1, 1, vec![4.0, 5.0])];
        let grid = plane_gain_grid(&list).unwrap();
        assert_eq!(grid.gains, vec![[2.0, 1.0, 4.0, 1.0], [3.0, 1.0, 5.0, 1.0]]);
    }

    #[test]
    fn one_map_can_cover_every_plane() {
        let grid = plane_gain_grid(&[gain_map(0, 3, 1, vec![2.0, 3.0])]).unwrap();
        assert_eq!(grid.gains, vec![[2.0, 2.0, 2.0, 1.0], [3.0, 3.0, 3.0, 1.0]]);
    }

    #[test]
    fn a_map_with_a_layer_per_plane_keeps_them_apart() {
        let grid = plane_gain_grid(&[gain_map(0, 3, 3, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0])]).unwrap();
        assert_eq!(grid.gains, vec![[1.0, 2.0, 3.0, 1.0], [4.0, 5.0, 6.0, 1.0]]);
    }

    #[test]
    fn no_gain_maps_means_no_grid() {
        assert!(plane_gain_grid(&[Opcode::Unsupported { id: 1, optional: true }]).is_none());
    }

    #[test]
    fn plane_gain_mapping_lands_on_texel_centres() {
        let grid = PlaneGainGrid {
            points: [5, 2],
            origin: [0.0, 0.25],
            spacing: [0.25, 0.5],
            gains: Vec::new(),
        };
        let mapping = plane_gain_mapping(&grid);
        let texel = |uv: f32, axis: usize| (uv * mapping.scale[axis] + mapping.offset[axis]) * grid.points[axis] as f32;
        assert!((texel(0.0, 0) - 0.5).abs() < 1e-5);
        assert!((texel(1.0, 0) - 4.5).abs() < 1e-5);
        assert!((texel(0.25, 1) - 0.5).abs() < 1e-5);
        assert!((texel(0.75, 1) - 1.5).abs() < 1e-5);
    }

    #[test]
    fn half_floats_decode() {
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0xc000), -2.0);
        assert_eq!(half_to_f32(0x3800), 0.5);
        assert_eq!(half_to_f32(0), 0.0);
    }
}
