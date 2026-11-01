// One step of the denoising pyramid, coarse to fine. The band between this level and the next
// coarser one is scaled by how far it rises above the noise expected here, then added to the
// already denoised coarser level. Everything stays linear, so averaging can't shift the
// brightness of noisy shadows the way filtering in a variance-stabilised space does.

struct Denoise {
    shot: vec4<f32>,
    read: vec4<f32>,
    luma_threshold: f32,
    chroma_threshold: f32,
    edge_color: f32,
    in_image: u32,
    quick: u32,
    padding_a: u32,
    padding_b: u32,
    padding_c: u32,
}

@group(0) @binding(1) var<uniform> denoise: Denoise;
@group(0) @binding(2) var fine_level: texture_2d<f32>;
@group(0) @binding(3) var coarse_level: texture_2d<f32>;
@group(0) @binding(4) var coarse_solved: texture_2d<f32>;
@group(0) @binding(5) var linear_sampler: sampler;
@group(0) @binding(6) var out_level: texture_storage_2d<{{OUTPUT_FORMAT}}, write>;

const LUMA: vec3<f32> = vec3<f32>(0.25, 0.5, 0.25);

fn to_opponent(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(rgb, LUMA), rgb.x - rgb.y, rgb.z - rgb.y);
}

fn from_opponent(value: vec3<f32>) -> vec3<f32> {
    let green = value.x - 0.25 * (value.y + value.z);
    return vec3<f32>(green + value.y, green, green + value.z);
}

// Passes a band in proportion to how much of its energy can't be explained by noise.
fn band_gain(signal_to_noise: f32, threshold: f32) -> f32 {
    if threshold <= 0.0 {
        return 1.0;
    }
    return max(signal_to_noise - threshold, 0.0) / max(signal_to_noise, 1e-6);
}

// Variance of the 3×3 neighbourhood in each opponent channel.
fn local_energy(p: vec2<i32>) -> vec3<f32> {
    let limit = tile.image - 1;
    var sum = vec3<f32>(0.0);
    var squares = vec3<f32>(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let q = clamp(p + vec2<i32>(x, y), vec2<i32>(0), limit);
            let around = to_opponent(textureLoad(fine_level, q, 0).xyz);
            sum += around;
            squares += around * around;
        }
    }
    let mean = sum / 9.0;
    return max(squares / 9.0 - mean * mean, vec3<f32>(0.0));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile_extent()) {
        return;
    }
    let p = image_position(local);
    let limit = tile.image - 1;
    let uv = (vec2<f32>(p) + 0.5) / vec2<f32>(tile.image);
    let center = to_opponent(textureLoad(fine_level, clamp(p, vec2<i32>(0), limit), 0).xyz);
    let low = to_opponent(textureSampleLevel(coarse_level, linear_sampler, uv, 0.0).xyz);
    let solved = textureSampleLevel(coarse_solved, linear_sampler, uv, 0.0).xyz;

    let variance = denoise.shot.xyz * max(solved, vec3<f32>(0.0)) + denoise.read.xyz;
    let luma_variance = max(dot(variance, LUMA * LUMA), 1e-12);
    var luma_gain = 1.0;
    var chroma_gain = 0.0;
    if denoise.quick == 0u {
        let energy = local_energy(p);
        let chroma_variance = max(vec2<f32>(variance.x + variance.y, variance.z + variance.y), vec2<f32>(1e-12));
        let luma_ratio = energy.x / luma_variance;
        let chroma_ratio = 0.5 * (energy.y / chroma_variance.x + energy.z / chroma_variance.y);
        luma_gain = band_gain(luma_ratio, denoise.luma_threshold);
        // Colour edges usually sit on luminance edges, which stand out from noise far better.
        let edge_gain = band_gain(luma_ratio, 3.0) * denoise.edge_color;
        chroma_gain = max(band_gain(chroma_ratio, denoise.chroma_threshold), edge_gain);
    }

    // Chroma is proportional to brightness, so taking it from a coarser level as it stands would
    // pour a bright surface's colour into anything darker beside it. Scaling it by the change in
    // brightness carries over the surface's colour instead of its amount.
    let settled = to_opponent(solved);
    let noise_floor = 2.0 * sqrt(luma_variance);
    let brightness = clamp((center.x + noise_floor) / (settled.x + noise_floor), 0.0, 2.0);
    let chroma = settled.yz * brightness + (center.yz - low.yz * brightness) * chroma_gain;
    let luma = settled.x + (center.x - low.x) * luma_gain;
    let rgb = from_opponent(vec3<f32>(luma, chroma));
    let written = select(local, p, denoise.in_image != 0u);
    textureStore(out_level, written, vec4<f32>(rgb, 1.0));
}
