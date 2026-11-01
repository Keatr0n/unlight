// Final stage: sharpening, post-crop vignette, grain, then the output colour space and
// encoding. Clipping overlays are drawn here, so they reflect exactly what was output.

@group(0) @binding(2) var developed: texture_2d<f32>;
@group(0) @binding(3) var blurred_rows: texture_2d<f32>;
@group(0) @binding(4) var out_image: texture_storage_2d<{{OUTPUT_FORMAT}}, write>;
@group(0) @binding(5) var local_results: texture_2d<f32>;

fn load(p: vec2<i32>) -> vec3<f32> {
    let limit = tile_extent() - 1;
    return textureLoad(developed, clamp(p, vec2<i32>(0), limit), 0).xyz;
}

fn luma(rgb: vec3<f32>) -> f32 {
    return dot(rgb, finish.luma.xyz);
}

// Perceptual-ish lightness, so sharpening acts evenly across tones. The develop pass stores
// it in alpha, which saves working it out again for every tap of the kernel.
fn lightness(rgb: vec3<f32>) -> f32 {
    return pow(max(luma(rgb), 0.0), 1.0 / 2.4);
}

struct Surroundings {
    blurred: f32,
    gradient: vec2<f32>,
    low: f32,
    high: f32,
}

fn surroundings(local: vec2<i32>) -> Surroundings {
    let reach = sharpen_reach();
    let limit = tile_extent() - 1;
    var result = Surroundings(0.0, vec2<f32>(0.0), 1e6, -1e6);
    var weight = 0.0;
    var moment = 0.0;
    for (var y = -reach; y <= reach; y++) {
        let row = textureLoad(blurred_rows, clamp(local + vec2<i32>(0, y), vec2<i32>(0), limit), 0);
        let tap = sharpen_tap(y);
        result.blurred += row.x * tap;
        result.gradient += vec2<f32>(row.y, row.x * f32(y)) * tap;
        result.low = min(result.low, row.z);
        result.high = max(result.high, row.w);
        weight += tap;
        moment += tap * f32(y * y);
    }
    result.blurred /= weight;
    result.gradient = vec2<f32>(result.gradient.x / weight, result.gradient.y / moment);
    return result;
}

// Masking keeps only what stands out from the smooth surroundings. Measuring the gradient
// after the blur means noise doesn't count as an edge.
fn sharpen_mask(around: Surroundings) -> f32 {
    let strength = length(around.gradient) * max(finish.sharpen_radius, 1.0);
    let masking = finish.sharpen_masking;
    return smoothstep(masking * 0.008, masking * 0.04 + 1e-4, strength);
}

fn sharpen(local: vec2<i32>, rgb: vec3<f32>, around: Surroundings, amount: f32) -> vec3<f32> {
    let center = textureLoad(developed, local, 0).a;
    var detail = center - around.blurred;
    // Detail 0 keeps only strong edges, 100 lets fine texture through.
    let threshold = (1.0 - finish.sharpen_detail) * 0.012;
    detail *= smoothstep(threshold * 0.5, threshold * 1.5 + 1e-4, abs(detail));
    let lifted = center + detail * amount * 2.2 * sharpen_mask(around);
    // Halos are overshoot past what's nearby, so low Detail holds the result inside that range
    // and high Detail lets it run.
    let overshoot = (around.high - around.low) * (0.04 + 1.8 * finish.sharpen_detail * finish.sharpen_detail);
    let limited = max(clamp(lifted, around.low - overshoot, around.high + overshoot), 0.0);
    return rgb * pow(limited / max(center, 1e-4), 2.4);
}

fn vignette(pixel: vec2<f32>, rgb: vec3<f32>) -> vec3<f32> {
    let view = finish.view_origin + pixel * finish.view_step;
    let centred = (view - 0.5) * 2.0;
    // Roundness blends between the frame's shape and a true circle.
    let round_scale = mix(vec2<f32>(1.0), vec2<f32>(finish.aspect, 1.0) / max(finish.aspect, 1.0), finish.vignette_roundness);
    let exponent = mix(4.0, 2.0, finish.vignette_roundness);
    let scaled = abs(centred * round_scale);
    let radius = pow(pow(scaled.x, exponent) + pow(scaled.y, exponent), 1.0 / exponent);
    let start = mix(0.2, 1.15, finish.vignette_midpoint);
    let width = 0.02 + finish.vignette_feather * 1.2;
    let t = smoothstep(start, start + width, radius);
    let amount = finish.vignette_amount * t;
    if amount < 0.0 {
        let keep = 1.0 + amount;
        let protect = smoothstep(0.5, 1.0, luma(rgb)) * finish.vignette_highlights;
        return rgb * mix(keep * keep, 1.0, protect);
    }
    return mix(rgb, vec3<f32>(1.0), amount * amount);
}

fn hash(p: vec2<f32>) -> f32 {
    let q = fract(p * vec2<f32>(0.1031, 0.1030));
    let r = q + dot(q, q.yx + 33.33);
    return fract((r.x + r.y) * r.x);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let cell = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(hash(cell), hash(cell + vec2<f32>(1.0, 0.0)), u.x),
        mix(hash(cell + vec2<f32>(0.0, 1.0)), hash(cell + vec2<f32>(1.0, 1.0)), u.x),
        u.y,
    ) - 0.5;
}

fn grain(view: vec2<f32>, rgb: vec3<f32>) -> vec3<f32> {
    let p = view * finish.grain_scale * vec2<f32>(finish.aspect, 1.0);
    let noise = value_noise(p) + finish.grain_roughness * 0.6 * value_noise(p * 2.3 + 17.0);
    let tonal = 1.0 - abs(2.0 * lightness(rgb) - 1.0);
    return rgb * exp2(noise * finish.grain_amount * (0.35 + tonal));
}

fn encode(linear: vec3<f32>) -> vec3<f32> {
    switch finish.encoding {
        case 1u {
            return pow(linear, vec3<f32>(1.0 / 2.19921875));
        }
        case 2u {
            return pow(linear, vec3<f32>(256.0 / 461.0));
        }
        default {
            let low = linear * 12.92;
            let high = 1.055 * pow(linear, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
            return select(high, low, linear <= vec3<f32>(0.0031308));
        }
    }
}

fn dither_noise(p: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715)))) - 0.5;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let inner = vec2<i32>(id.xy);
    if any(inner >= tile.size) {
        return;
    }
    let local = inner + tile.pad;
    let image_p = image_position(local);
    let pixel = vec2<f32>(image_p) + 0.5;
    var rgb = load(local);

    // Below this, taking detail away would start to put it back inverted.
    const MOST_BLUR: f32 = -1.0 / 2.2;
    var local_result = vec4<f32>(0.0);
    if finish.local_detail > 0.0 || finish.mask_overlay > 0.0 {
        local_result = textureLoad(local_results, local, 0);
    }
    if finish.sharpen_amount > 0.0 || finish.local_detail > 0.0 {
        let around = surroundings(local);
        rgb = sharpen(local, rgb, around, max(finish.sharpen_amount + local_result.x, MOST_BLUR));
        if finish.show_sharpen_mask > 0.0 {
            textureStore(out_image, inner, vec4<f32>(vec3<f32>(sharpen_mask(around)), 1.0));
            return;
        }
    }
    if finish.vignette_amount != 0.0 {
        rgb = vignette(pixel, rgb);
    }
    if finish.grain_amount > 0.0 {
        rgb = grain(finish.view_origin + pixel * finish.view_step, rgb);
    }

    let linear = finish.working_to_output * max(rgb, vec3<f32>(0.0));
    let dither = dither_noise(pixel) * finish.dither;
    var color = encode(clamp(linear, vec3<f32>(0.0), vec3<f32>(1.0))) + vec3<f32>(dither);
    // {{PROOF}}

    if finish.show_clipping > 0.0 {
        if all(linear >= vec3<f32>(0.999)) {
            color = vec3<f32>(1.0, 0.0, 0.0);
        } else if all(linear <= vec3<f32>(0.0005)) {
            color = vec3<f32>(0.0, 0.3, 1.0);
        }
    }
    if finish.mask_overlay > 0.0 {
        color = mix(color, vec3<f32>(1.0, 0.0, 0.0), clamp(local_result.y, 0.0, 1.0) * 0.6);
    }
    textureStore(out_image, inner, vec4<f32>(color, 1.0));
}
