struct Finish {
    working_to_output: mat3x3<f32>,
    luma: vec4<f32>,
    view_origin: vec2<f32>,
    view_step: vec2<f32>,
    sharpen_amount: f32,
    sharpen_radius: f32,
    sharpen_detail: f32,
    sharpen_masking: f32,
    vignette_amount: f32,
    vignette_midpoint: f32,
    vignette_roundness: f32,
    vignette_feather: f32,
    vignette_highlights: f32,
    grain_amount: f32,
    grain_scale: f32,
    grain_roughness: f32,
    aspect: f32,
    encoding: u32,
    dither: f32,
    show_clipping: f32,
    show_sharpen_mask: f32,
    local_detail: f32,
    mask_overlay: f32,
    padding: f32,
}

@group(0) @binding(1) var<uniform> finish: Finish;
// One side of the sharpening kernel. Sampling a Gaussian narrower than a pixel loses most of it,
// so small radii use three taps that match its variance instead. That keeps the effect the
// same size at every zoom, because a sharpened image scaled down looks like one sharpened with
// a proportionally smaller radius.
fn sharpen_reach() -> i32 {
    return max(i32(ceil(finish.sharpen_radius * 2.0)), 1);
}

fn sharpen_tap(offset: i32) -> f32 {
    let radius = finish.sharpen_radius;
    if radius < 0.7 {
        return select(1.0 - radius * radius, radius * radius * 0.5, offset != 0);
    }
    return exp(-f32(offset * offset) / (2.0 * radius * radius));
}
