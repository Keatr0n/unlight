// Fast guided filter: solves the local linear model q = a·I + b on a small image. The
// develop pass evaluates it against full-resolution luminance, which keeps edges sharp
// without carrying the whole filter at full size. x and y of the output smooth luminance
// itself. z and w do the same for the dark channel, which is what lets the haze estimate
// follow edges in the photo even though it was measured over wide cells.

struct Solve {
    epsilon: f32,
    padding_a: f32,
    padding_b: f32,
    padding_c: f32,
}

@group(0) @binding(0) var<uniform> solve: Solve;
@group(0) @binding(1) var moments: texture_2d<f32>;
@group(0) @binding(2) var out_model: texture_storage_2d<rgba32float, write>;

fn tent(p: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(moments)) - 1;
    var sum = vec4<f32>(0.0);
    var weight = 0.0;
    for (var y = -2; y <= 2; y++) {
        for (var x = -2; x <= 2; x++) {
            let w = f32((3 - abs(x)) * (3 - abs(y)));
            let texel = textureLoad(moments, clamp(p + vec2<i32>(x, y), vec2<i32>(0), size), 0);
            sum += vec4<f32>(texel.xyz, texel.x * texel.z) * w;
            weight += w;
        }
    }
    return sum / weight;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(textureDimensions(out_model));
    if any(id.xy >= size) {
        return;
    }
    let p = vec2<i32>(id.xy);
    let blurred = tent(p);
    let variance = max(blurred.y - blurred.x * blurred.x, 0.0);
    let a = variance / (variance + solve.epsilon);
    let b = (1.0 - a) * blurred.x;
    let dark_slope = (blurred.w - blurred.x * blurred.z) / (variance + solve.epsilon);
    let dark_offset = blurred.z - dark_slope * blurred.x;
    textureStore(out_model, p, vec4<f32>(a, b, dark_slope, dark_offset));
}
