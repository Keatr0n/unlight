// RCD: directional discrimination strengths and the low-pass guide.

const EPS_SQUARED: f32 = 1e-10;

@group(0) @binding(1) var mosaic: texture_2d<f32>;
@group(0) @binding(2) var high_pass: texture_2d<f32>;
@group(0) @binding(3) var out_direction: texture_storage_2d<rgba32float, write>;

fn hp(p: vec2<i32>) -> vec4<f32> {
    return textureLoad(high_pass, p, 0);
}

fn site(p: vec2<i32>) -> f32 {
    return textureLoad(mosaic, p, 0).r;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile_extent()) {
        return;
    }
    let center = hp(local);
    let v_stat = max(EPS_SQUARED, hp(local + vec2<i32>(0, -1)).x + center.x + hp(local + vec2<i32>(0, 1)).x);
    let h_stat = max(EPS_SQUARED, hp(local + vec2<i32>(-1, 0)).y + center.y + hp(local + vec2<i32>(1, 0)).y);
    let p_stat = max(EPS_SQUARED, hp(local + vec2<i32>(-1, -1)).z + center.z + hp(local + vec2<i32>(1, 1)).z);
    let q_stat = max(EPS_SQUARED, hp(local + vec2<i32>(1, -1)).w + center.w + hp(local + vec2<i32>(-1, 1)).w);

    let p = image_position(local) + MOSAIC_BORDER;
    let low_pass = site(p)
        + 0.5 * (site(p + vec2<i32>(0, -1)) + site(p + vec2<i32>(0, 1))
            + site(p + vec2<i32>(-1, 0)) + site(p + vec2<i32>(1, 0)))
        + 0.25 * (site(p + vec2<i32>(-1, -1)) + site(p + vec2<i32>(1, -1))
            + site(p + vec2<i32>(-1, 1)) + site(p + vec2<i32>(1, 1)));

    textureStore(out_direction, local, vec4<f32>(
        v_stat / (v_stat + h_stat),
        p_stat / (p_stat + q_stat),
        low_pass,
        0.0,
    ));
}
