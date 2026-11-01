//! The camera profile's part of develop: the base tone curve, the HueSatMap and the LookTable.
//! The tone curve is a lookup table whether a profile supplies it or not, which keeps one path
//! through the shader. The two tables are gated, so a photo without them doesn't pay for them.

use crate::{curves, pipelines::Engine};
use unlight_color::{CameraProfile, HueSatTable, Vec3};
use wgpu::util::DeviceExt;

/// These two must stay in step with develop.wgsl.
pub const BASE_CURVE_SIZE: usize = 1024;
pub const BASE_CURVE_REACH: f32 = 4.0;

/// The default rendering, as stops of scene light against gamma 2.2 output. Fitted to camera
/// JPEGs from nine bodies across six makes, then given a slower shoulder than the best fit
/// because that keeps skies the cameras kept.
const BASE_CURVE: [(f32, f32); 11] = [
    (-8.0, 0.077),
    (-7.0, 0.106),
    (-6.0, 0.152),
    (-5.0, 0.249),
    (-4.0, 0.398),
    (-3.0, 0.580),
    (-2.0, 0.770),
    (-1.0, 0.872),
    (-0.5, 0.925),
    (0.0, 0.985),
    (1.0, 1.0),
];
const OUTPUT_GAMMA: f32 = 2.2;

/// Oklab chroma gain on the default rendering. The curve's contrast supplies most of the
/// saturation that camera JPEGs have, and this is what remains. No hue shift goes with it,
/// because the hue differences from those cameras disagree with each other.
const LOOK_CHROMA: f32 = 1.08;

/// The DNG SDK's default Shadows setting, as a fraction of white.
const AUTO_BLACK: f32 = 0.005;

fn default_tone(light: f32) -> f32 {
    let (first_stop, first_output) = BASE_CURVE[0];
    let first_light = first_stop.exp2();
    if light <= first_light {
        return first_output.powf(OUTPUT_GAMMA) * light / first_light;
    }
    curves::monotone_spline(&BASE_CURVE)(light.log2()).powf(OUTPUT_GAMMA)
}

/// The DNG exposure ramp with no exposure: black moves to zero with a short curved toe.
fn subtract_black(light: f32) -> f32 {
    let radius = AUTO_BLACK * 0.5;
    let slope = 1.0 / (1.0 - AUTO_BLACK);
    let start = AUTO_BLACK - radius;
    if light <= start {
        return 0.0;
    }
    if light >= AUTO_BLACK + radius {
        return ((light - AUTO_BLACK) * slope).min(1.0);
    }
    let along = light - start;
    slope / (4.0 * radius) * along * along
}

/// Scene light to output light, spaced evenly in the square root of light.
pub fn base_curve(profile: &CameraProfile) -> Vec<f32> {
    let lights = (0..BASE_CURVE_SIZE).map(|index| {
        let along = index as f32 / (BASE_CURVE_SIZE - 1) as f32;
        along * along * BASE_CURVE_REACH
    });
    let rendering = profile.rendering();
    match rendering.and_then(|rendering| rendering.tone_curve.as_deref()) {
        Some(curve) => {
            let points: Vec<(f32, f32)> = curve.iter().map(|point| (point[0], point[1])).collect();
            let spline = curves::monotone_spline(&points);
            let auto_black = rendering.is_some_and(|rendering| rendering.auto_black);
            lights
                .map(|light| {
                    let light = if auto_black { subtract_black(light) } else { light.min(1.0) };
                    spline(light).clamp(0.0, 1.0)
                })
                .collect()
        }
        None => lights.map(default_tone).collect(),
    }
}

/// Grid size and value encoding for the shader. Zero cells mean no table.
pub fn table_cells(table: Option<&HueSatTable>) -> [f32; 4] {
    table.map_or([0.0; 4], |table| {
        let [hue, saturation, value] = table.divisions.map(|count| count as f32);
        [hue, saturation, value, f32::from(u8::from(table.srgb_values))]
    })
}

pub fn hue_sat_cells(profile: &CameraProfile) -> [f32; 4] {
    let maps = profile.rendering().into_iter().flat_map(|rendering| &rendering.calibrations);
    table_cells(maps.filter_map(|calibration| calibration.hue_sat_map.as_ref()).next())
}

pub fn look_cells(profile: &CameraProfile) -> [f32; 4] {
    table_cells(profile.rendering().and_then(|rendering| rendering.look_table.as_ref()))
}

/// A profile with colour tables has already decided how saturated things should be.
pub fn look_chroma(profile: &CameraProfile) -> f32 {
    if hue_sat_cells(profile)[0] > 0.0 || look_cells(profile)[0] > 0.0 { 1.0 } else { LOOK_CHROMA }
}

pub fn exposure_offset(profile: &CameraProfile) -> f32 {
    profile.rendering().map_or(0.0, |rendering| rendering.baseline_exposure_offset)
}

fn texels(table: &HueSatTable) -> Vec<[f32; 4]> {
    table.entries.iter().map(|&[hue, saturation, value]| [saturation, hue, value, 0.0]).collect()
}

fn extent(divisions: [u32; 3]) -> wgpu::Extent3d {
    let [hue, saturation, value] = divisions;
    wgpu::Extent3d { width: saturation, height: hue, depth_or_array_layers: value }
}

struct Table {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Table {
    /// Saturation runs along x and hue along y, because that's the order the file stores them
    /// in, which leaves hue as the axis the sampler wraps.
    fn new(engine: &Engine, table: Option<&HueSatTable>) -> Self {
        let identity = [[1.0, 0.0, 1.0, 0.0f32]];
        let data = table.map(texels);
        let size = table.map_or(extent([1, 1, 1]), |table| extent(table.divisions));
        let texture = engine.ctx.device.create_texture_with_data(
            &engine.ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("profile table"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D3,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(data.as_deref().unwrap_or(&identity)),
        );
        Self { view: texture.create_view(&Default::default()), texture }
    }

    fn write(&self, engine: &Engine, table: &HueSatTable) {
        let size = extent(table.divisions);
        engine.ctx.queue.write_texture(
            self.texture.as_image_copy(),
            bytemuck::cast_slice(&texels(table)),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size.width * 16),
                rows_per_image: Some(size.height),
            },
            size,
        );
    }
}

pub(crate) struct ProfileTextures {
    pub base_curve: wgpu::TextureView,
    hue_sat: Table,
    look: Table,
    hue_sat_neutral: Vec3,
}

impl ProfileTextures {
    pub fn new(engine: &Engine, profile: &CameraProfile) -> Self {
        let curve = engine.ctx.device.create_texture_with_data(
            &engine.ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("base curve"),
                size: wgpu::Extent3d {
                    width: BASE_CURVE_SIZE as u32,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&base_curve(profile)),
        );
        let map = profile.hue_sat_map(&profile.as_shot_neutral());
        let rendering = profile.rendering();
        Self {
            base_curve: curve.create_view(&Default::default()),
            hue_sat: Table::new(engine, map.as_deref()),
            look: Table::new(engine, rendering.and_then(|rendering| rendering.look_table.as_ref())),
            hue_sat_neutral: profile.as_shot_neutral(),
        }
    }

    pub fn hue_sat_view(&self) -> &wgpu::TextureView {
        &self.hue_sat.view
    }

    pub fn look_view(&self) -> &wgpu::TextureView {
        &self.look.view
    }

    /// Two illuminants blend by the scene white, so a white balance drag has to refill this.
    pub fn sync(&mut self, engine: &Engine, profile: &CameraProfile, neutral: &Vec3) {
        if self.hue_sat_neutral == *neutral {
            return;
        }
        if let Some(map) = profile.hue_sat_map(neutral) {
            self.hue_sat.write(engine, &map);
        }
        self.hue_sat_neutral = *neutral;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use unlight_color::{Dcp, dcp::ProfileCalibration};
    use unlight_raw::ColorData;

    fn color() -> ColorData {
        ColorData {
            xyz_to_camera: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            as_shot_multipliers: [1.0; 3],
            daylight_multipliers: [1.0; 3],
            baseline_exposure: 0.0,
            dng: None,
        }
    }

    fn with_curve(curve: Vec<[f32; 2]>, auto_black: bool) -> CameraProfile {
        let profile = Dcp {
            unique_camera_model: String::new(),
            name: String::new(),
            calibrations: Default::default(),
            look_table: None,
            tone_curve: Some(curve),
            baseline_exposure_offset: 0.0,
            auto_black,
        };
        CameraProfile::with_dcp(&color(), Arc::new(profile))
    }

    fn at(lut: &[f32], light: f32) -> f32 {
        let along = (light / BASE_CURVE_REACH).sqrt() * (BASE_CURVE_SIZE - 1) as f32;
        let index = (along as usize).min(BASE_CURVE_SIZE - 2);
        let fraction = along - index as f32;
        lut[index] * (1.0 - fraction) + lut[index + 1] * fraction
    }

    #[test]
    fn default_curve_climbs_from_black_to_white() {
        let lut = base_curve(&CameraProfile::new(&color()));
        assert_eq!(lut[0], 0.0);
        assert!(lut.windows(2).all(|pair| pair[1] >= pair[0]), "not monotonic");
        assert!((lut[BASE_CURVE_SIZE - 1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn default_curve_is_steepest_in_the_midtones() {
        let lut = base_curve(&CameraProfile::new(&color()));
        let encoded = |light: f32| at(&lut, light).powf(1.0 / OUTPUT_GAMMA);
        let contrast = |light: f32| encoded(light * 1.41) - encoded(light / 1.41);
        assert!(contrast(0.125) > contrast(0.003) * 2.0, "the toe should be gentler");
        assert!(contrast(0.125) > contrast(0.7) * 1.5, "the shoulder should roll off");
    }

    #[test]
    fn sensor_white_leaves_room_below_output_white() {
        let lut = base_curve(&CameraProfile::new(&color()));
        assert!(at(&lut, 1.0) < 0.975, "{}", at(&lut, 1.0));
        assert!(at(&lut, 1.0) > 0.94, "{}", at(&lut, 1.0));
    }

    #[test]
    fn the_table_tracks_the_curve_through_the_shadows() {
        let lut = base_curve(&CameraProfile::new(&color()));
        for light in [1e-4f32, 1e-3, 0.01, 0.05, 0.2, 0.5, 0.9] {
            let error = (at(&lut, light) - default_tone(light)).abs() / default_tone(light);
            assert!(error < 0.01, "{light}: {error}");
        }
    }

    #[test]
    fn a_profile_curve_replaces_the_default() {
        let linear = with_curve(vec![[0.0, 0.0], [1.0, 1.0]], false);
        let lut = base_curve(&linear);
        for light in [0.01f32, 0.18, 0.5, 0.9] {
            assert!((at(&lut, light) - light).abs() < 1e-3, "{light}");
        }
        assert_eq!(lut[BASE_CURVE_SIZE - 1], 1.0, "a profile curve ends at sensor white");
    }

    #[test]
    fn auto_black_applies_only_when_the_profile_allows_it() {
        let curve = vec![[0.0, 0.0], [1.0, 1.0]];
        let with_black = base_curve(&with_curve(curve.clone(), true));
        let without = base_curve(&with_curve(curve, false));
        assert_eq!(at(&with_black, 0.002), 0.0);
        assert!(at(&without, 0.002) > 0.0015);
        assert!((at(&with_black, 0.5) - 0.4975).abs() < 1e-3);
    }

    #[test]
    fn the_black_ramp_has_no_step_in_it() {
        let values: Vec<f32> = (0..2000).map(|index| subtract_black(index as f32 * 1e-5)).collect();
        assert!(values.windows(2).all(|pair| pair[1] >= pair[0] && pair[1] - pair[0] < 5e-5));
    }

    fn with_tables(hue_sat: Option<HueSatTable>, look: Option<HueSatTable>) -> CameraProfile {
        let mut calibrations: [ProfileCalibration; 2] = Default::default();
        calibrations[0].hue_sat_map = hue_sat;
        let profile = Dcp {
            unique_camera_model: String::new(),
            name: String::new(),
            calibrations,
            look_table: look,
            tone_curve: None,
            baseline_exposure_offset: 0.0,
            auto_black: true,
        };
        CameraProfile::with_dcp(&color(), Arc::new(profile))
    }

    #[test]
    fn the_built_in_saturation_stays_until_a_profile_brings_colour_tables() {
        let table = HueSatTable { divisions: [1, 2, 1], srgb_values: false, entries: vec![[0.0, 1.0, 1.0]; 2] };
        assert_eq!(look_chroma(&CameraProfile::new(&color())), LOOK_CHROMA);
        let curve_only = with_curve(vec![[0.0, 0.0], [1.0, 1.0]], true);
        assert_eq!(look_chroma(&curve_only), LOOK_CHROMA);
        assert_eq!(look_chroma(&with_tables(Some(table.clone()), None)), 1.0);
        assert_eq!(look_chroma(&with_tables(None, Some(table))), 1.0);
    }

    #[test]
    fn no_table_reads_as_zero_cells() {
        assert_eq!(table_cells(None), [0.0; 4]);
        let table = HueSatTable { divisions: [90, 30, 1], srgb_values: true, entries: Vec::new() };
        assert_eq!(table_cells(Some(&table)), [90.0, 30.0, 1.0, 1.0]);
    }
}
