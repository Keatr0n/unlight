// RCD: fills red at blue sites and blue at red sites along the diagonals.

const EPS: f32 = 1e-5;

@group(0) @binding(1) var mosaic: texture_2d<f32>;
@group(0) @binding(2) var direction: texture_2d<f32>;
@group(0) @binding(3) var green_plane: texture_2d<f32>;
@group(0) @binding(4) var out_rgb: texture_storage_2d<rgba32float, write>;

fn site(p: vec2<i32>) -> f32 {
    return textureLoad(mosaic, p, 0).r;
}

fn green(p: vec2<i32>) -> f32 {
    return textureLoad(green_plane, p, 0).r;
}

fn discriminant(local: vec2<i32>) -> f32 {
    let central = textureLoad(direction, local, 0).y;
    let around = 0.25 * (textureLoad(direction, local + vec2<i32>(-1, -1), 0).y
        + textureLoad(direction, local + vec2<i32>(1, -1), 0).y
        + textureLoad(direction, local + vec2<i32>(-1, 1), 0).y
        + textureLoad(direction, local + vec2<i32>(1, 1), 0).y);
    return select(central, around, abs(0.5 - central) < abs(0.5 - around));
}

fn diagonal(p: vec2<i32>, local: vec2<i32>, d: vec2<i32>) -> vec2<f32> {
    let gradient = EPS
        + abs(site(p + d) - site(p - d))
        + abs(site(p + d) - site(p + 3 * d))
        + abs(green(local) - green(local + 2 * d));
    return vec2<f32>(gradient, site(p + d) - green(local + d));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile_extent()) {
        return;
    }
    let image_p = image_position(local);
    let p = image_p + MOSAIC_BORDER;
    let color = cfa_color(image_p);
    let center_green = green(local);
    if color == 1u {
        textureStore(out_rgb, local, vec4<f32>(0.0, center_green, 0.0, 1.0));
        return;
    }
    let nw = diagonal(p, local, vec2<i32>(-1, -1));
    let ne = diagonal(p, local, vec2<i32>(1, -1));
    let sw = diagonal(p, local, vec2<i32>(-1, 1));
    let se = diagonal(p, local, vec2<i32>(1, 1));

    let p_estimate = (nw.x * se.y + se.x * nw.y) / (nw.x + se.x);
    let q_estimate = (ne.x * sw.y + sw.x * ne.y) / (ne.x + sw.x);
    let other = max(center_green + mix(p_estimate, q_estimate, discriminant(local)), 0.0);

    let native = site(p);
    if color == 0u {
        textureStore(out_rgb, local, vec4<f32>(native, center_green, other, 1.0));
    } else {
        textureStore(out_rgb, local, vec4<f32>(other, center_green, native, 1.0));
    }
}
