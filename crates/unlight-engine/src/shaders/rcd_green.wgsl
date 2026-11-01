// RCD: fills green at red and blue sites from ratio-corrected cardinal estimates.

const EPS: f32 = 1e-5;

@group(0) @binding(1) var mosaic: texture_2d<f32>;
@group(0) @binding(2) var direction: texture_2d<f32>;
@group(0) @binding(3) var out_green: texture_storage_2d<r32float, write>;

fn site(p: vec2<i32>) -> f32 {
    return textureLoad(mosaic, p, 0).r;
}

fn low_pass(p: vec2<i32>) -> f32 {
    return textureLoad(direction, p, 0).z;
}

// Gradient along +d from the centre, plus the estimate from the green neighbour that way.
fn cardinal(p: vec2<i32>, local: vec2<i32>, d: vec2<i32>) -> vec2<f32> {
    let gradient = EPS
        + abs(site(p - d) - site(p + d))
        + abs(site(p) - site(p + 2 * d))
        + abs(site(p + d) - site(p + 3 * d))
        + abs(site(p + 2 * d) - site(p + 4 * d));
    let center = low_pass(local);
    let neighbour = low_pass(local + 2 * d);
    let estimate = site(p + d) * (1.0 + (center - neighbour) / (EPS + center + neighbour));
    return vec2<f32>(gradient, estimate);
}

fn discriminant(local: vec2<i32>) -> f32 {
    let central = textureLoad(direction, local, 0).x;
    let around = 0.25 * (textureLoad(direction, local + vec2<i32>(-1, -1), 0).x
        + textureLoad(direction, local + vec2<i32>(1, -1), 0).x
        + textureLoad(direction, local + vec2<i32>(-1, 1), 0).x
        + textureLoad(direction, local + vec2<i32>(1, 1), 0).x);
    return select(central, around, abs(0.5 - central) < abs(0.5 - around));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile_extent()) {
        return;
    }
    let image_p = image_position(local);
    let p = image_p + MOSAIC_BORDER;
    if cfa_color(image_p) == 1u {
        textureStore(out_green, local, vec4<f32>(site(p), 0.0, 0.0, 0.0));
        return;
    }
    let north = cardinal(p, local, vec2<i32>(0, -1));
    let south = cardinal(p, local, vec2<i32>(0, 1));
    let west = cardinal(p, local, vec2<i32>(-1, 0));
    let east = cardinal(p, local, vec2<i32>(1, 0));

    let vertical = (south.x * north.y + north.x * south.y) / (north.x + south.x);
    let horizontal = (west.x * east.y + east.x * west.y) / (east.x + west.x);
    let green = mix(vertical, horizontal, discriminant(local));
    textureStore(out_green, local, vec4<f32>(max(green, 0.0), 0.0, 0.0, 0.0));
}
