// Pull half of the fill: each cell blends its own colour with the coarser estimate according
// to how much support it has, so a few stray blocks can't paint a large area. Each step into a
// blown area loses some of the estimate, so with nothing left to go on it fades to neutral.

struct Expand {
    prior_weight: f32,
    padding_a: f32,
    padding_b: f32,
    padding_c: f32,
}

const BLOWN_FADE: f32 = 0.65;

@group(0) @binding(1) var pushed: texture_2d<f32>;
@group(0) @binding(2) var coarse: texture_2d<f32>;
@group(0) @binding(3) var linear_sampler: sampler;
@group(0) @binding(4) var<uniform> expand: Expand;
@group(0) @binding(5) var out_level: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile.size) {
        return;
    }
    let target_position = local + tile.origin;
    let here = textureLoad(pushed, target_position, 0);
    let uv = (vec2<f32>(target_position) + 0.5) / vec2<f32>(textureDimensions(pushed));
    let prior = textureSampleLevel(coarse, linear_sampler, uv, 0.0).xy * mix(1.0, BLOWN_FADE, here.w);
    let chroma = (here.xy + prior * expand.prior_weight) / (here.z + expand.prior_weight);
    textureStore(out_level, target_position, vec4<f32>(chroma, 1.0, here.w));
}
