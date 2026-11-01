// Ratio Corrected Demosaicing by Luis Sanz Rodríguez (GPL-3.0), split into GPU passes.
// This pass: squared high-pass response along the four directions.

@group(0) @binding(1) var mosaic: texture_2d<f32>;
@group(0) @binding(2) var out_high_pass: texture_storage_2d<rgba32float, write>;

fn site(p: vec2<i32>) -> f32 {
    return textureLoad(mosaic, p, 0).r;
}

fn high_pass(p: vec2<i32>, d: vec2<i32>) -> f32 {
    let response = (site(p - 3 * d) - site(p - d) - site(p + d) + site(p + 3 * d))
        - 3.0 * (site(p - 2 * d) + site(p + 2 * d))
        + 10.0 * site(p);
    return response * response;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile_extent()) {
        return;
    }
    let p = image_position(local) + MOSAIC_BORDER;
    textureStore(out_high_pass, local, vec4<f32>(
        high_pass(p, vec2<i32>(0, 1)),
        high_pass(p, vec2<i32>(1, 0)),
        high_pass(p, vec2<i32>(1, 1)),
        high_pass(p, vec2<i32>(1, -1)),
    ));
}
