// What masks add to this pixel. The fields are in source coordinates, so looking them up at the
// pixel's own source position is all it takes for a mask to follow the crop and every warp.

struct LocalEdits {
    exposure: f32,
    contrast: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    clarity: f32,
    texture_amount: f32,
    dehaze: f32,
    hue: f32,
    saturation: f32,
    temperature: f32,
    tint: f32,
    sharpness: f32,
    noise: f32,
    overlay: f32,
}

@group(0) @binding(20) var local_fields: texture_2d_array<f32>;
@group(0) @binding(21) var cleaner_image: texture_2d<f32>;
@group(0) @binding(22) var rawer_image: texture_2d<f32>;
@group(0) @binding(23) var out_local: texture_storage_2d<rgba16float, write>;

const HAS_MASKS: u32 = 1u;

fn local_edits(uv: vec2<f32>) -> LocalEdits {
    if (develop.local_flags & HAS_MASKS) == 0u {
        return LocalEdits();
    }
    let tone = textureSampleLevel(local_fields, linear_sampler, uv, 0, 0.0);
    let detail = textureSampleLevel(local_fields, linear_sampler, uv, 1, 0.0);
    let colour = textureSampleLevel(local_fields, linear_sampler, uv, 2, 0.0);
    let rest = textureSampleLevel(local_fields, linear_sampler, uv, 3, 0.0);
    return LocalEdits(
        tone.x, tone.y, tone.z, tone.w,
        detail.x, detail.y, detail.z, detail.w,
        colour.x, colour.y, colour.z, colour.w,
        rest.x, rest.y, rest.z, rest.w,
    );
}

// Sharpening and the mask overlay happen in the finish pass, which knows nothing of the
// geometry, so the develop pass leaves them what it read here.
fn store_local(local: vec2<i32>, edits: LocalEdits) {
    if (develop.local_flags & HAS_MASKS) != 0u {
        textureStore(out_local, local, vec4<f32>(edits.sharpness, edits.overlay, 0.0, 0.0));
    }
}

// Positive noise blends towards a more strongly denoised image, negative back towards the
// undenoised one. Those are both cached images, so this is a blend rather than a filter.
fn scene_at(uv: vec2<f32>, noise: f32) -> vec4<f32> {
    let base = textureSampleLevel(source_image, linear_sampler, uv, develop.lod);
    if noise > 0.0 {
        return mix(base, textureSampleLevel(cleaner_image, linear_sampler, uv, develop.lod), min(noise, 1.0));
    }
    if noise < 0.0 {
        return mix(base, textureSampleLevel(rawer_image, linear_sampler, uv, develop.lod), min(-noise, 1.0));
    }
    return base;
}

// Temperature and tint as gains that leave brightness where it was, so a warmer mask doesn't
// also read as a brighter one.
fn local_balance(edits: LocalEdits) -> vec3<f32> {
    let gain = exp2(vec3<f32>(edits.temperature, -edits.tint, -edits.temperature));
    return gain / dot(gain, develop.luma.xyz);
}
