// Rebuilds clipped sites. Intact sites nearby give the brightness and the filled field gives
// the colour, so detail from the channels that survived carries over into the clipped one.
// A clipped site only says how bright it is at least, which sets a floor on the result.

@group(0) @binding(1) var mosaic: texture_2d<f32>;
@group(0) @binding(2) var color_field: texture_2d<f32>;
@group(0) @binding(3) var linear_sampler: sampler;
@group(0) @binding(4) var out_mosaic: texture_storage_2d<r32float, write>;

fn rebuild(target_position: vec2<i32>, measured: f32) -> f32 {
    let uv = (vec2<f32>(target_position) + 0.5) / vec2<f32>(tile.image);
    let field = textureSampleLevel(color_field, linear_sampler, uv, 0.0).xy;
    let chroma = vec3<f32>(field.x, -field.x - field.y, field.y);

    var intact = vec3<f32>(0.0);
    var intact_weight = vec3<f32>(0.0);
    var clipped = vec3<f32>(0.0);
    var clipped_weight = vec3<f32>(0.0);
    let reach = select(2, 3, tile.period == 6);
    let limit = vec2<i32>(textureDimensions(mosaic)) - 1;
    for (var y = -reach; y <= reach; y++) {
        for (var x = -reach; x <= reach; x++) {
            let q = target_position + vec2<i32>(x, y);
            let value = textureLoad(mosaic, clamp(q + MOSAIC_BORDER, vec2<i32>(0), limit), 0).r;
            let channel = cfa_color(q);
            let weight = exp(-f32(x * x + y * y) / 4.5);
            if value < 0.0 {
                clipped[channel] -= value * weight;
                clipped_weight[channel] += weight;
            } else {
                intact[channel] += value * weight;
                intact_weight[channel] += weight;
            }
        }
    }

    var level = -32.0;
    var total = 0.0;
    var sum = 0.0;
    for (var channel = 0; channel < 3; channel++) {
        if intact_weight[channel] > 0.0 {
            let mean = max(intact[channel] / intact_weight[channel], 1e-5);
            sum += (log(mean) - chroma[channel]) * intact_weight[channel];
            total += intact_weight[channel];
        }
        if clipped_weight[channel] > 0.0 {
            let mean = clipped[channel] / clipped_weight[channel];
            level = max(level, log(mean) - chroma[channel]);
        }
    }
    if total > 0.0 {
        level = max(level, sum / total);
    }
    return max(measured, exp(level + chroma[cfa_color(target_position)]));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile.size) {
        return;
    }
    let target_position = local + tile.origin;
    let p = target_position + MOSAIC_BORDER;
    var value = textureLoad(mosaic, p, 0).r;
    if value < 0.0 {
        value = rebuild(target_position, -value);
    }
    textureStore(out_mosaic, p, vec4<f32>(value, 0.0, 0.0, 0.0));
}
