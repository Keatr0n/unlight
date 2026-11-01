// What every mask rasterising pass shares: the photo's scene colours, read at the coverage
// size from the preview pyramid, so a range mask is the same data at every zoom and in export.

struct MaskScene {
    camera_to_working: mat3x3<f32>,
    working_to_lms: mat3x3<f32>,
    luma: vec4<f32>,
    raster_size: vec2<f32>,
    long_edge: f32,
    lod: f32,
    gain: f32,
    padding_a: f32,
    padding_b: f32,
    padding_c: f32,
}

@group(0) @binding(0) var<uniform> scene: MaskScene;
@group(0) @binding(1) var scene_image: texture_2d<f32>;
@group(0) @binding(2) var linear_sampler: sampler;

fn scene_rgb(uv: vec2<f32>) -> vec3<f32> {
    let camera = textureSampleLevel(scene_image, linear_sampler, uv, scene.lod).xyz;
    return max(scene.camera_to_working * camera, vec3<f32>(0.0)) * scene.gain;
}

fn oklab(rgb: vec3<f32>) -> vec3<f32> {
    let lms = pow(max(scene.working_to_lms * rgb, vec3<f32>(0.0)), vec3<f32>(1.0 / 3.0));
    return vec3<f32>(
        0.2104542553 * lms.x + 0.7936177850 * lms.y - 0.0040720468 * lms.z,
        1.9779984951 * lms.x - 2.4285922050 * lms.y + 0.4505937099 * lms.z,
        0.0259040371 * lms.x + 0.7827717662 * lms.y - 0.8086757660 * lms.z,
    );
}

// Lightness counts for less than colour, because a sampled sky should still select its
// brighter and darker parts.
fn colour_distance(a: vec3<f32>, b: vec3<f32>) -> f32 {
    let difference = (a - b) * vec3<f32>(0.4, 1.0, 1.0);
    return length(difference);
}
