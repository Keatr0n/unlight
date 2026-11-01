// Fills red and blue at the isolated green sites.

@group(0) @binding(2) var previous: texture_2d_array<f32>;
@group(0) @binding(3) var out_rgb: texture_storage_2d_array<rgba32float, write>;

fn load(p: vec2<i32>, layer: i32) -> vec4<f32> {
    return textureLoad(previous, p, layer, 0);
}

struct Estimate {
    color: vec3<f32>,
    difference: f32,
}

// Estimates channel `near_channel` from ±step and the other from ±2·step.
fn along(local: vec2<i32>, layer: i32, step: vec2<i32>, near_channel: u32) -> Estimate {
    let center = load(local, layer).y;
    var sum = vec3<f32>(0.0);
    var difference = 0.0;
    var channel = near_channel;
    for (var reach = 1; reach <= 2; reach++) {
        let forward = load(local + step * reach, layer);
        let backward = load(local - step * reach, layer);
        let g = 2.0 * center - forward.y - backward.y;
        sum[channel] = g + forward[channel] + backward[channel];
        let d = forward.y - backward.y - forward[channel] + backward[channel];
        difference += d * d + g * g;
        channel ^= 2u;
    }
    var result: Estimate;
    result.color = max(sum * 0.5, vec3<f32>(0.0));
    result.difference = difference;
    return result;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile_extent()) {
        return;
    }
    let p = image_position(local);
    let solitary = green_row_phase(p) == 0 && green_col_phase(p) == 0;
    let horizontal_channel = cfa_color(p + vec2<i32>(1, 0));
    for (var layer = 0; layer < 4; layer++) {
        var rgb = load(local, layer);
        if solitary {
            let horizontal = along(local, layer, vec2<i32>(1, 0), horizontal_channel);
            let vertical = along(local, layer, vec2<i32>(0, 1), horizontal_channel ^ 2u);
            var chosen = horizontal.color;
            if layer == 1 || (layer > 1 && vertical.difference <= horizontal.difference) {
                chosen = vertical.color;
            }
            rgb = vec4<f32>(chosen.x, rgb.y, chosen.z, 1.0);
        }
        textureStore(out_rgb, local, layer, rgb);
    }
}
