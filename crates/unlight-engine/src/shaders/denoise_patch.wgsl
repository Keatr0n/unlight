// Summarises the 5×5 patch around each pixel for non-local means. Luma is variance-stabilised
// first, so the same distance means the same thing in shadows and highlights, then projected
// onto low-order polynomials, where image structure concentrates and noise doesn't. Comparing
// a few of these costs a fraction of comparing every pixel of two patches.

struct Stabilise {
    shot: f32,
    read: f32,
    padding_a: f32,
    padding_b: f32,
}

@group(0) @binding(1) var<uniform> stabilise: Stabilise;
@group(0) @binding(2) var image: texture_2d<f32>;
@group(0) @binding(3) var out_first: texture_storage_2d<rgba32float, write>;
@group(0) @binding(4) var out_second: texture_storage_2d<rgba32float, write>;

const LUMA: vec3<f32> = vec3<f32>(0.25, 0.5, 0.25);

var<private> FLAT: array<f32, 5> = array<f32, 5>(0.44721, 0.44721, 0.44721, 0.44721, 0.44721);
var<private> SLOPE: array<f32, 5> = array<f32, 5>(-0.63246, -0.31623, 0.0, 0.31623, 0.63246);
var<private> CURVE: array<f32, 5> = array<f32, 5>(0.53452, -0.26726, -0.53452, -0.26726, 0.53452);

// The integral of 1/σ(x), taken from zero. Written this way it stays accurate, and defined,
// when a photo's shot term is tiny or zero, as it is for one with no range of brightness to
// measure it from.
fn stabilised(rgb: vec3<f32>) -> f32 {
    let luma = max(dot(rgb, LUMA), 0.0);
    return 2.0 * luma / (sqrt(stabilise.shot * luma + stabilise.read) + sqrt(stabilise.read));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    let extent = tile_extent();
    if any(local >= extent) {
        return;
    }
    var first = vec3<f32>(0.0);
    var second = vec4<f32>(0.0);
    for (var y = 0; y < 5; y++) {
        for (var x = 0; x < 5; x++) {
            let q = clamp(local + vec2<i32>(x - 2, y - 2), vec2<i32>(0), extent - 1);
            let value = stabilised(textureLoad(image, q, 0).xyz);
            first += value * vec3<f32>(FLAT[x] * FLAT[y], SLOPE[x] * FLAT[y], FLAT[x] * SLOPE[y]);
            second += value * vec4<f32>(
                CURVE[x] * FLAT[y],
                FLAT[x] * CURVE[y],
                SLOPE[x] * SLOPE[y],
                CURVE[x] * SLOPE[y] + SLOPE[x] * CURVE[y],
            );
        }
    }
    let center = dot(textureLoad(image, local, 0).xyz, LUMA);
    textureStore(out_first, local, vec4<f32>(first, center));
    textureStore(out_second, local, second);
}
