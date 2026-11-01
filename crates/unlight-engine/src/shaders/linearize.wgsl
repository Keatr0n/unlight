struct Linearize {
    black: array<vec4<f32>, 9>,
    balance: vec4<f32>,
    gain_origin: vec2<f32>,
    gain_spacing: vec2<f32>,
    gain_points: vec2<i32>,
    gain_pitch: vec2<i32>,
    white: f32,
    clip: f32,
    gain_enabled: u32,
    padding: u32,
}

@group(0) @binding(1) var<uniform> lin: Linearize;
@group(0) @binding(2) var raw: texture_2d<u32>;
@group(0) @binding(3) var gain_map: texture_2d<f32>;
@group(0) @binding(4) var out_mosaic: texture_storage_2d<r32float, write>;

// Steps outside coordinates back inside by whole CFA periods, so the site colour holds.
fn fold(value: i32, size: i32) -> i32 {
    var folded = value;
    if folded < 0 {
        folded += ((-folded + tile.period - 1) / tile.period) * tile.period;
    }
    let over = folded - (size - 1);
    if over > 0 {
        folded -= ((over + tile.period - 1) / tile.period) * tile.period;
    }
    return folded;
}

fn gain_at(p: vec2<i32>) -> f32 {
    let position = (vec2<f32>(p) + 0.5) / vec2<f32>(tile.image);
    let grid = clamp(
        (position - lin.gain_origin) / lin.gain_spacing,
        vec2<f32>(0.0),
        vec2<f32>(lin.gain_points - 1),
    );
    let cell = min(vec2<i32>(grid), max(lin.gain_points - 2, vec2<i32>(0)));
    let blend = grid - vec2<f32>(cell);
    let phase = p % lin.gain_pitch;
    let channel = phase.y * lin.gain_pitch.x + phase.x;
    let top = mix(
        textureLoad(gain_map, cell, 0)[channel],
        textureLoad(gain_map, cell + vec2<i32>(1, 0), 0)[channel],
        blend.x,
    );
    let bottom = mix(
        textureLoad(gain_map, cell + vec2<i32>(0, 1), 0)[channel],
        textureLoad(gain_map, cell + vec2<i32>(1, 1), 0)[channel],
        blend.x,
    );
    return mix(top, bottom, blend.y);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile.size) {
        return;
    }
    let target_position = local + tile.origin;
    let p = vec2<i32>(fold(target_position.x, tile.image.x), fold(target_position.y, tile.image.y));

    let tile_index = positive_mod(p.y, 6) * 6 + positive_mod(p.x, 6);
    let black = lin.black[tile_index / 4][tile_index % 4];
    let sample = f32(textureLoad(raw, p, 0).r);
    var value = max(sample - black, 0.0) / (lin.white - black);

    // Judged before the gain map, which moves the level a site saturates at.
    let clipped = value >= lin.clip;
    if lin.gain_enabled != 0u {
        value *= gain_at(p);
    }
    value *= lin.balance[cfa_color(p)];
    // The sign carries the clip mask through the raw clean-up passes.
    let stored = select(value, -value, clipped);
    textureStore(out_mosaic, target_position + MOSAIC_BORDER, vec4<f32>(stored, 0.0, 0.0, 0.0));
}
