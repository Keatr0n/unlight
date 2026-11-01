// Seeds the guide stack: log luminance and its square, plus the darkest channel for dehaze.
// All guide products come from this one fixed-size image, so preview and export agree.

struct GuideParams {
    camera_to_working: mat3x3<f32>,
    luma: vec4<f32>,
    size: vec2<f32>,
    lod: f32,
    padding: f32,
}

@group(0) @binding(0) var<uniform> guide: GuideParams;
@group(0) @binding(1) var source_image: texture_2d<f32>;
@group(0) @binding(2) var linear_sampler: sampler;
@group(0) @binding(3) var out_guide: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<f32>(id.xy);
    if any(local >= guide.size) {
        return;
    }
    let uv = (local + 0.5) / guide.size;
    let camera = textureSampleLevel(source_image, linear_sampler, uv, guide.lod).xyz;
    let rgb = max(guide.camera_to_working * camera, vec3<f32>(0.0));
    let log_luma = log2(max(dot(rgb, guide.luma.xyz), 1e-5));
    let darkest = min(rgb.x, min(rgb.y, rgb.z));
    textureStore(out_guide, vec2<i32>(id.xy), vec4<f32>(log_luma, log_luma * log_luma, darkest, 1.0));
}
