use bytemuck::{Pod, Zeroable};
use unlight_color::Mat3;
use unlight_raw::Cfa;

pub const MOSAIC_BORDER: u32 = 24;
pub const TILE_PAD: u32 = 18;
pub const MAX_TILE_EDGE: u32 = 2048;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct TileParams {
    pub origin: [i32; 2],
    pub size: [i32; 2],
    pub image: [i32; 2],
    pub pad: i32,
    pub period: i32,
    pub cfa: [[u32; 4]; 9],
}

pub fn pack_tile<T: Copy>(values: &[[T; 6]; 6]) -> [[T; 4]; 9] {
    std::array::from_fn(|entry| {
        std::array::from_fn(|lane| {
            let index = entry * 4 + lane;
            values[index / 6][index % 6]
        })
    })
}

pub fn cfa_params(cfa: &Cfa) -> [[u32; 4]; 9] {
    pack_tile(&cfa.pattern.map(|row| row.map(u32::from)))
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct LinearizeParams {
    pub black: [[f32; 4]; 9],
    pub balance: [f32; 4],
    pub gain_origin: [f32; 2],
    pub gain_spacing: [f32; 2],
    pub gain_points: [i32; 2],
    pub gain_pitch: [i32; 2],
    pub white: f32,
    pub clip: f32,
    pub gain_enabled: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct HighlightParams {
    pub balance: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct ExpandParams {
    pub prior_weight: f32,
    pub padding: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct DenoiseParams {
    pub shot: [f32; 4],
    pub read: [f32; 4],
    pub luma_threshold: f32,
    pub chroma_threshold: f32,
    pub edge_color: f32,
    pub in_image: u32,
    pub quick: u32,
    pub padding: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct StabiliseParams {
    pub shot: f32,
    pub read: f32,
    pub padding: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct MeansParams {
    pub scale: f32,
    pub allowance: f32,
    pub keep: f32,
    pub padding: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct XtransParams {
    pub hex: [[i32; 4]; 36],
    pub solitary_green: [i32; 2],
    pub padding: [i32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct DevelopParams {
    pub camera_to_working: [[f32; 4]; 3],
    pub working_to_lms: [[f32; 4]; 3],
    pub lms_to_working: [[f32; 4]; 3],
    pub inverse_homography: [[f32; 4]; 3],
    pub view_origin: [f32; 2],
    pub view_step: [f32; 2],
    pub crop_origin: [f32; 2],
    pub crop_size: [f32; 2],
    pub rotation: [f32; 2],
    pub aspect: f32,
    pub lod: f32,
    pub lens: [f32; 4],
    pub luma: [f32; 4],

    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub clarity: f32,
    pub texture_amount: f32,
    pub dehaze: f32,
    pub vibrance: f32,
    pub saturation: f32,
    pub white_ev: f32,
    pub black_ev: f32,
    pub look_chroma: f32,
    pub tone_padding: f32,
    pub curves_enabled: f32,
    pub atmosphere: f32,
    pub shadow_tint: f32,
    pub mixer_enabled: f32,
    pub monochrome: f32,

    pub mixer_hue: [[f32; 4]; 2],
    pub mixer_saturation: [[f32; 4]; 2],
    pub mixer_luminance: [[f32; 4]; 2],
    pub bw_mix: [[f32; 4]; 2],

    pub grade_shadows: [f32; 4],
    pub grade_midtones: [f32; 4],
    pub grade_highlights: [f32; 4],
    pub grade_global: [f32; 4],
    pub grade_blending: f32,
    pub grade_balance: f32,
    pub grade_enabled: f32,
    pub lens_curve_scale: f32,

    pub lens_center: [f32; 2],
    pub lens_tangential: [f32; 2],
    pub vignette_center: [f32; 2],
    pub profile_distortion: f32,
    pub profile_chromatic: f32,
    pub profile_vignette: f32,
    pub manual_vignette: f32,
    pub manual_vignette_start: f32,
    pub manual_vignette_norm: f32,
    pub defringe_purple: [f32; 4],
    pub defringe_green: [f32; 4],
    pub fringe_reach: [f32; 2],
    pub plane_gain_enabled: f32,
    pub local_flags: u32,
    pub plane_gain_scale: [f32; 2],
    pub plane_gain_offset: [f32; 2],
    pub hue_sat_cells: [f32; 4],
    pub look_cells: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct FinishParams {
    pub working_to_output: [[f32; 4]; 3],
    pub luma: [f32; 4],
    pub view_origin: [f32; 2],
    pub view_step: [f32; 2],
    pub sharpen_amount: f32,
    pub sharpen_radius: f32,
    pub sharpen_detail: f32,
    pub sharpen_masking: f32,
    pub vignette_amount: f32,
    pub vignette_midpoint: f32,
    pub vignette_roundness: f32,
    pub vignette_feather: f32,
    pub vignette_highlights: f32,
    pub grain_amount: f32,
    pub grain_scale: f32,
    pub grain_roughness: f32,
    pub aspect: f32,
    pub encoding: u32,
    pub dither: f32,
    pub show_clipping: f32,
    pub show_sharpen_mask: f32,
    pub local_detail: f32,
    pub mask_overlay: f32,
    pub padding: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GuideParams {
    pub camera_to_working: [[f32; 4]; 3],
    pub luma: [f32; 4],
    pub size: [f32; 2],
    pub lod: f32,
    pub padding: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct SolveParams {
    pub epsilon: f32,
    pub padding: [f32; 3],
}

/// Column-major with vec4 stride, which is how WGSL lays out a uniform `mat3x3<f32>`.
pub fn wgsl_mat3(matrix: &Mat3) -> [[f32; 4]; 3] {
    std::array::from_fn(|col| {
        [matrix[0][col] as f32, matrix[1][col] as f32, matrix[2][col] as f32, 0.0]
    })
}
