// First step of highlight reconstruction: the colour of each CFA block with all of its sites
// intact, as log ratios to the block's mean log level. x and y hold red and blue, since the
// three ratios sum to zero, premultiplied by the confidence in z. w marks blown blocks.

struct Seed {
    balance: vec4<f32>,
}

// Only blocks near clipping say much about the colour of what clipped. Right below it the
// surviving sites are the ones noise happened to push down, so those read too dark.
const DIM_SIGNAL: f32 = 0.5;
const FULL_SIGNAL: f32 = 0.8;
const CENSORED_START: f32 = 0.9;
const CENSORED_END: f32 = 0.95;
const NEIGHBOUR_REACH: i32 = 6;
const DARKER_NEIGHBOUR: f32 = 0.6;

@group(0) @binding(1) var<uniform> seed: Seed;
@group(0) @binding(2) var mosaic: texture_2d<f32>;
@group(0) @binding(3) var out_level: texture_storage_2d<rgba16float, write>;

// A block at the edge of a darker object blends the two colours, and that tints everything
// filled from it, so edges don't count as sources.
fn beside_darker(block: vec2<i32>, level: f32) -> bool {
    let limit = vec2<i32>(textureDimensions(mosaic)) - 1;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            for (var distance = 1; distance <= 2; distance++) {
                let p = block + vec2<i32>(x, y) * NEIGHBOUR_REACH * distance;
                let value = textureLoad(mosaic, clamp(p + MOSAIC_BORDER, vec2<i32>(0), limit), 0).r;
                if abs(value) < level * DARKER_NEIGHBOUR {
                    return true;
                }
            }
        }
    }
    return false;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile.size) {
        return;
    }
    let target_position = local + tile.origin;
    let factor = select(2, 3, tile.period == 6);
    let block = target_position * factor;
    var sum = vec3<f32>(0.0);
    var clipped = vec3<f32>(0.0);
    var count = vec3<f32>(0.0);
    for (var y = 0; y < factor; y++) {
        for (var x = 0; x < factor; x++) {
            let p = block + vec2<i32>(x, y);
            let channel = cfa_color(p);
            let value = textureLoad(mosaic, p + MOSAIC_BORDER, 0).r;
            if value < 0.0 {
                clipped[channel] += 1.0;
            } else {
                sum[channel] += value;
                count[channel] += 1.0;
            }
        }
    }

    var result = vec4<f32>(0.0);
    if all(clipped > vec3<f32>(0.0)) {
        result.w = 1.0;
    } else if all(clipped == vec3<f32>(0.0)) {
        let mean = sum / max(count, vec3<f32>(1.0));
        let logs = log(max(mean, vec3<f32>(1e-5)));
        let chroma = logs - vec3<f32>((logs.x + logs.y + logs.z) / 3.0);
        let brightest = max(mean.x / seed.balance.x, max(mean.y / seed.balance.y, mean.z / seed.balance.z));
        var confidence = smoothstep(DIM_SIGNAL, FULL_SIGNAL, brightest)
            * (1.0 - smoothstep(CENSORED_START, CENSORED_END, brightest));
        if confidence > 0.0 && beside_darker(block, min(mean.x, min(mean.y, mean.z))) {
            confidence = 0.0;
        }
        result = vec4<f32>(chroma.x * confidence, chroma.z * confidence, confidence, 0.0);
    }
    textureStore(out_level, target_position, result);
}
