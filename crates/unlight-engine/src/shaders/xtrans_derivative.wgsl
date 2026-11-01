// Second derivative of each candidate along its own direction, in a perceptual space.

@group(0) @binding(1) var candidates: texture_2d_array<f32>;
@group(0) @binding(2) var out_derivative: texture_storage_2d<rgba32float, write>;

fn perceptual(p: vec2<i32>, layer: i32) -> vec3<f32> {
    let rgb = sqrt(max(textureLoad(candidates, p, layer, 0).xyz, vec3<f32>(0.0)));
    let y = dot(rgb, vec3<f32>(0.2627, 0.6780, 0.0593));
    return vec3<f32>(y, (rgb.z - y) * 0.56433, (rgb.x - y) * 0.67815);
}

fn derivative(p: vec2<i32>, layer: i32, step: vec2<i32>) -> f32 {
    let d = 2.0 * perceptual(p, layer) - perceptual(p + step, layer) - perceptual(p - step, layer);
    return dot(d, d);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile_extent()) {
        return;
    }
    textureStore(out_derivative, local, vec4<f32>(
        derivative(local, 0, vec2<i32>(1, 0)),
        derivative(local, 1, vec2<i32>(0, 1)),
        derivative(local, 2, vec2<i32>(1, 1)),
        derivative(local, 3, vec2<i32>(-1, 1)),
    ));
}
