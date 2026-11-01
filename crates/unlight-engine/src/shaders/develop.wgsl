// Fused per-pixel stage. Everything from the camera matrix through colour grading happens in
// one dispatch, so a slider drag costs a single pass. Geometry is a UV transform here rather
// than a separate warp pass, which keeps resampling to one step.

struct Develop {
    camera_to_working: mat3x3<f32>,
    working_to_lms: mat3x3<f32>,
    lms_to_working: mat3x3<f32>,
    inverse_homography: mat3x3<f32>,
    view_origin: vec2<f32>,
    view_step: vec2<f32>,
    crop_origin: vec2<f32>,
    crop_size: vec2<f32>,
    rotation: vec2<f32>,
    aspect: f32,
    lod: f32,
    lens: vec4<f32>,
    luma: vec4<f32>,

    exposure: f32,
    contrast: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    clarity: f32,
    texture_amount: f32,
    dehaze: f32,
    vibrance: f32,
    saturation: f32,
    white_ev: f32,
    black_ev: f32,
    look_chroma: f32,
    tone_padding: f32,
    curves_enabled: f32,
    atmosphere: f32,
    shadow_tint: f32,
    mixer_enabled: f32,
    monochrome: f32,

    mixer_hue: array<vec4<f32>, 2>,
    mixer_saturation: array<vec4<f32>, 2>,
    mixer_luminance: array<vec4<f32>, 2>,
    bw_mix: array<vec4<f32>, 2>,

    grade_shadows: vec4<f32>,
    grade_midtones: vec4<f32>,
    grade_highlights: vec4<f32>,
    grade_global: vec4<f32>,
    grade_blending: f32,
    grade_balance: f32,
    grade_enabled: f32,
    lens_curve_scale: f32,

    lens_center: vec2<f32>,
    lens_tangential: vec2<f32>,
    vignette_center: vec2<f32>,
    profile_distortion: f32,
    profile_chromatic: f32,
    profile_vignette: f32,
    manual_vignette: f32,
    manual_vignette_start: f32,
    manual_vignette_norm: f32,
    defringe_purple: vec4<f32>,
    defringe_green: vec4<f32>,
    fringe_reach: vec2<f32>,
    plane_gain_enabled: f32,
    local_flags: u32,
    plane_gain_scale: vec2<f32>,
    plane_gain_offset: vec2<f32>,
    hue_sat_cells: vec4<f32>,
    look_cells: vec4<f32>,
}

@group(0) @binding(1) var<uniform> develop: Develop;
@group(0) @binding(2) var source_image: texture_2d<f32>;
@group(0) @binding(3) var linear_sampler: sampler;
@group(0) @binding(4) var base_model: texture_2d<f32>;
@group(0) @binding(5) var detail_model: texture_2d<f32>;
@group(0) @binding(6) var curve_lut: texture_2d<f32>;
@group(0) @binding(7) var out_image: texture_storage_2d<{{RGB_FORMAT}}, write>;
@group(0) @binding(8) var lens_curve: texture_2d<f32>;
@group(0) @binding(9) var plane_gain: texture_2d<f32>;
@group(0) @binding(10) var fine_model: texture_2d<f32>;
@group(0) @binding(16) var base_curve: texture_2d<f32>;
@group(0) @binding(17) var hue_sat_table: texture_3d<f32>;
@group(0) @binding(18) var look_table: texture_3d<f32>;
@group(0) @binding(19) var table_sampler: sampler;

const TAU: f32 = 6.28318530718;
const LENS_CURVE_SIZE: f32 = 256.0;
const BASE_CURVE_SIZE: f32 = 1024.0;
const BASE_CURVE_REACH: f32 = 4.0;
// Hue of each mixer band in OKLCh, in radians.
var<private> BAND_HUES: array<f32, 8> = array<f32, 8>(
    0.4887, 0.9599, 1.9199, 2.4784, 3.4034, 4.5902, 5.2360, 5.7247,
);

// One entry of the lens curve: distortion, then red and blue scale, then vignette in stops.
fn lens_curve_at(radius: f32) -> vec4<f32> {
    let texel = (radius * develop.lens_curve_scale + 0.5) / LENS_CURVE_SIZE;
    return textureSampleLevel(lens_curve, linear_sampler, vec2<f32>(texel, 0.5), 0.0);
}

// Output pixel to its place in the source: undo the crop, the straighten and the perspective,
// then apply the lens model, each in image-centred coordinates with the aspect ratio removed.
// The lens comes last because it is the first thing light passed through.
fn source_position(view: vec2<f32>) -> vec2<f32> {
    let cropped = develop.crop_origin + view * develop.crop_size;
    var p = (cropped - 0.5) * vec2<f32>(develop.aspect, 1.0);
    p = vec2<f32>(
        develop.rotation.x * p.x - develop.rotation.y * p.y,
        develop.rotation.y * p.x + develop.rotation.x * p.y,
    );
    let projected = develop.inverse_homography * vec3<f32>(p, 1.0);
    let offset = projected.xy / projected.z - develop.lens_center;
    let r2 = dot(offset, offset);
    var radial = develop.lens.w + r2 * (develop.lens.x + r2 * (develop.lens.y + r2 * develop.lens.z));
    if develop.profile_distortion != 0.0 {
        radial *= 1.0 + develop.profile_distortion * lens_curve_at(sqrt(r2)).x;
    }
    let t = develop.lens_tangential;
    let tangential = vec2<f32>(
        t.y * (r2 + 2.0 * offset.x * offset.x) + 2.0 * t.x * offset.x * offset.y,
        t.x * (r2 + 2.0 * offset.y * offset.y) + 2.0 * t.y * offset.x * offset.y,
    );
    return develop.lens_center + offset * radial + tangential;
}

fn to_uv(position: vec2<f32>) -> vec2<f32> {
    return position / vec2<f32>(develop.aspect, 1.0) + 0.5;
}

// Demosaicing builds red and blue's fine detail from green, so moving those channels misplaces
// that detail against green, which shows up as colour speckle. A small green difference between
// the two positions is that detail, and goes back. A large one is an edge being corrected.
fn fine_detail(moved_green: f32, green: f32) -> f32 {
    const DETAIL_LOW: f32 = 0.03;
    const DETAIL_HIGH: f32 = 0.15;
    let difference = green - moved_green;
    let contrast = abs(difference) / max(green, 1e-4);
    return difference * (1.0 - smoothstep(DETAIL_LOW, DETAIL_HIGH, contrast));
}

// Red and blue get their own lookups only when lateral chromatic aberration is being removed.
fn sample_camera(position: vec2<f32>, uv: vec2<f32>, noise: f32) -> vec3<f32> {
    var camera = scene_at(uv, noise).xyz;
    if develop.profile_chromatic > 0.0 {
        let offset = position - develop.lens_center;
        let scale = 1.0 + develop.profile_chromatic * lens_curve_at(length(offset)).yz;
        let red_uv = to_uv(develop.lens_center + offset * scale.x);
        let blue_uv = to_uv(develop.lens_center + offset * scale.y);
        let red = scene_at(red_uv, noise);
        let blue = scene_at(blue_uv, noise);
        camera.x = red.x + fine_detail(red.y, camera.y);
        camera.z = blue.z + fine_detail(blue.y, camera.y);
    }
    if develop.plane_gain_enabled > 0.0 {
        let cell = uv * develop.plane_gain_scale + develop.plane_gain_offset;
        camera *= textureSampleLevel(plane_gain, linear_sampler, cell, 0.0).xyz;
    }
    return camera;
}

fn vignette_stops(position: vec2<f32>) -> f32 {
    var stops = 0.0;
    if develop.profile_vignette != 0.0 {
        stops = develop.profile_vignette * lens_curve_at(distance(position, develop.vignette_center)).w;
    }
    if develop.manual_vignette != 0.0 {
        let radius = length(position) * develop.manual_vignette_norm;
        stops += develop.manual_vignette * smoothstep(develop.manual_vignette_start, 1.0, radius);
    }
    return stops;
}

fn bump(low: f32, high: f32, value: f32) -> f32 {
    return smoothstep(low, high, value);
}

// Spaced by the square root of light, which puts the entries where the curve bends.
fn base_tone(value: f32) -> f32 {
    let along = sqrt(clamp(value / BASE_CURVE_REACH, 0.0, 1.0)) * (BASE_CURVE_SIZE - 1.0);
    let texel = (along + 0.5) / BASE_CURVE_SIZE;
    return textureSampleLevel(base_curve, linear_sampler, vec2<f32>(texel, 0.5), 0.0).r;
}

// The DNG's HSV: hue runs 0..6 and value is the largest channel.
fn rgb_to_hsv(rgb: vec3<f32>) -> vec3<f32> {
    let value = max(rgb.x, max(rgb.y, rgb.z));
    let gap = value - min(rgb.x, min(rgb.y, rgb.z));
    if gap <= 0.0 {
        return vec3<f32>(0.0, 0.0, value);
    }
    var hue = 4.0 + (rgb.x - rgb.y) / gap;
    if rgb.x == value {
        hue = (rgb.y - rgb.z) / gap;
        if hue < 0.0 {
            hue += 6.0;
        }
    } else if rgb.y == value {
        hue = 2.0 + (rgb.z - rgb.x) / gap;
    }
    return vec3<f32>(hue, gap / value, value);
}

fn hsv_to_rgb(hsv: vec3<f32>) -> vec3<f32> {
    let phases = fract(hsv.x / 6.0 + vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0));
    let channels = clamp(abs(phases * 6.0 - 3.0) - 1.0, vec3<f32>(0.0), vec3<f32>(1.0));
    return hsv.z * mix(vec3<f32>(1.0), channels, hsv.y);
}

fn srgb_encode(value: f32) -> f32 {
    if value <= 0.0031308 {
        return value * 12.92;
    }
    return 1.055 * pow(value, 1.0 / 2.4) - 0.055;
}

fn srgb_decode(value: f32) -> f32 {
    if value <= 0.04045 {
        return value / 12.92;
    }
    return pow((value + 0.055) / 1.055, 2.4);
}

// A HueSatMap or LookTable lookup, as the DNG spec defines it. `cells` is the grid size along
// hue, saturation and value, then whether value is sRGB-encoded. Scene light can run past 1,
// so only the lookup clamps; the scale still applies to all of it.
fn apply_profile_table(rgb: vec3<f32>, table: texture_3d<f32>, cells: vec4<f32>) -> vec3<f32> {
    let hsv = rgb_to_hsv(rgb);
    let encoded = cells.w > 0.0;
    let value = select(hsv.z, srgb_encode(hsv.z), encoded);
    let along = vec3<f32>(
        hsv.y * (cells.y - 1.0),
        hsv.x / 6.0 * cells.x,
        min(value, 1.0) * (cells.z - 1.0),
    );
    let at = (along + 0.5) / cells.yxz;
    let change = textureSampleLevel(table, table_sampler, at, 0.0).xyz;
    let scaled = value * change.z;
    return hsv_to_rgb(vec3<f32>(
        hsv.x + change.y / 60.0,
        min(hsv.y * change.x, 1.0),
        select(scaled, srgb_decode(scaled), encoded),
    ));
}

fn hue_preserving_tone(rgb: vec3<f32>) -> vec3<f32> {
    let high = max(rgb.x, max(rgb.y, rgb.z));
    let low = min(rgb.x, min(rgb.y, rgb.z));
    let top = base_tone(high);
    let bottom = base_tone(low);
    if high <= low + 1e-7 {
        return vec3<f32>(top);
    }
    return vec3<f32>(bottom) + (rgb - vec3<f32>(low)) / (high - low) * (top - bottom);
}

fn apply_curves(rgb: vec3<f32>) -> vec3<f32> {
    let encoded = pow(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / 2.2));
    let high = max(encoded.x, max(encoded.y, encoded.z));
    let low = min(encoded.x, min(encoded.y, encoded.z));
    let top = textureSampleLevel(curve_lut, linear_sampler, vec2<f32>(high, 0.5), 0.0).a;
    let bottom = textureSampleLevel(curve_lut, linear_sampler, vec2<f32>(low, 0.5), 0.0).a;
    var result = vec3<f32>(top);
    if high > low + 1e-6 {
        result = vec3<f32>(bottom) + (encoded - vec3<f32>(low)) / (high - low) * (top - bottom);
    }
    let r = textureSampleLevel(curve_lut, linear_sampler, vec2<f32>(result.x, 0.5), 0.0).r;
    let g = textureSampleLevel(curve_lut, linear_sampler, vec2<f32>(result.y, 0.5), 0.0).g;
    let b = textureSampleLevel(curve_lut, linear_sampler, vec2<f32>(result.z, 0.5), 0.0).b;
    return pow(vec3<f32>(r, g, b), vec3<f32>(2.2));
}

fn cbrt(value: vec3<f32>) -> vec3<f32> {
    return sign(value) * pow(abs(value), vec3<f32>(1.0 / 3.0));
}

fn to_oklab(rgb: vec3<f32>) -> vec3<f32> {
    let lms = cbrt(develop.working_to_lms * rgb);
    return vec3<f32>(
        0.2104542553 * lms.x + 0.7936177850 * lms.y - 0.0040720468 * lms.z,
        1.9779984951 * lms.x - 2.4285922050 * lms.y + 0.4505937099 * lms.z,
        0.0259040371 * lms.x + 0.7827717662 * lms.y - 0.8086757660 * lms.z,
    );
}

fn from_oklab(lab: vec3<f32>) -> vec3<f32> {
    let lms = vec3<f32>(
        lab.x + 0.3963377774 * lab.y + 0.2158037573 * lab.z,
        lab.x - 0.1055613458 * lab.y - 0.0638541728 * lab.z,
        lab.x - 0.0894841775 * lab.y - 1.2914855480 * lab.z,
    );
    return develop.lms_to_working * (lms * lms * lms);
}

const MIX_HUE: i32 = 0;
const MIX_SATURATION: i32 = 1;
const MIX_LUMINANCE: i32 = 2;
const MIX_MONOCHROME: i32 = 3;

fn band_value(kind: i32, band: i32) -> f32 {
    switch kind {
        case MIX_HUE {
            return develop.mixer_hue[band / 4][band % 4];
        }
        case MIX_SATURATION {
            return develop.mixer_saturation[band / 4][band % 4];
        }
        case MIX_LUMINANCE {
            return develop.mixer_luminance[band / 4][band % 4];
        }
        default {
            return develop.bw_mix[band / 4][band % 4];
        }
    }
}

// Blends a per-band setting by hue with a raised cosine between neighbouring bands, so the
// weights always sum to one and no hue is left out.
fn mix_bands(kind: i32, hue: f32) -> f32 {
    var lower = 7;
    for (var band = 0; band < 8; band++) {
        if hue >= BAND_HUES[band] {
            lower = band;
        }
    }
    let upper = (lower + 1) % 8;
    let start = BAND_HUES[lower];
    var span = BAND_HUES[upper] - start;
    var offset = hue - start;
    if span <= 0.0 {
        span += TAU;
    }
    if offset < 0.0 {
        offset += TAU;
    }
    let t = 0.5 - 0.5 * cos(3.14159265 * clamp(offset / span, 0.0, 1.0));
    return mix(band_value(kind, lower), band_value(kind, upper), t);
}

fn grade_tint(wheel: vec4<f32>) -> vec2<f32> {
    return wheel.w * vec2<f32>(cos(wheel.x), sin(wheel.x));
}

fn apply_color(rgb: vec3<f32>, hue_shift: f32, saturation_shift: f32) -> vec3<f32> {
    var lab = to_oklab(max(rgb, vec3<f32>(0.0)));
    var chroma = length(lab.yz) * develop.look_chroma;
    var hue = atan2(lab.z, lab.y);
    if hue < 0.0 {
        hue += TAU;
    }

    if develop.mixer_enabled > 0.0 {
        let band_hue = hue;
        hue += mix_bands(MIX_HUE, band_hue);
        chroma *= 1.0 + mix_bands(MIX_SATURATION, band_hue);
        lab.x *= 1.0 + mix_bands(MIX_LUMINANCE, band_hue) * clamp(chroma * 6.0, 0.0, 1.0);
    }
    hue += hue_shift;

    // Vibrance favours muted colours and eases off around skin hues.
    let skin = exp(-pow((hue - 0.85) / 0.45, 2.0));
    let muted = 1.0 - clamp(chroma * 4.0, 0.0, 1.0);
    chroma *= 1.0 + develop.vibrance * muted * (1.0 - 0.6 * skin);
    chroma *= max(1.0 + develop.saturation + saturation_shift, 0.0);

    if develop.monochrome > 0.0 {
        lab.x *= 1.0 + mix_bands(MIX_MONOCHROME, hue) * clamp(chroma * 5.0, 0.0, 1.0);
        chroma = 0.0;
    }

    var a = chroma * cos(hue);
    var b = chroma * sin(hue);

    if develop.grade_enabled > 0.0 {
        let spread = 0.15 + 0.35 * develop.grade_blending;
        let pivot = 0.55 + develop.grade_balance * 0.3;
        let shadow = 1.0 - smoothstep(pivot - spread, pivot, lab.x);
        let highlight = smoothstep(pivot, pivot + spread, lab.x);
        let middle = 1.0 - shadow - highlight;
        let tint = grade_tint(develop.grade_shadows) * shadow
            + grade_tint(develop.grade_midtones) * middle
            + grade_tint(develop.grade_highlights) * highlight
            + grade_tint(develop.grade_global);
        a += tint.x;
        b += tint.y;
        lab.x += develop.grade_shadows.y * shadow
            + develop.grade_midtones.y * middle
            + develop.grade_highlights.y * highlight
            + develop.grade_global.y;
    }

    return from_oklab(vec3<f32>(lab.x, a, b));
}

// Soft-edged membership of a hue range, in radians around the circle.
fn hue_band(hue: f32, low: f32, high: f32) -> f32 {
    const SOFTNESS: f32 = 0.12;
    return smoothstep(low - SOFTNESS, low, hue) * (1.0 - smoothstep(high, high + SOFTNESS, hue));
}

// Fringes hug hard edges, so a colour only counts as one where the neighbourhood has a large
// jump in brightness. That keeps a purple flower purple while a backlit twig's halo goes grey.
fn defringe(rgb: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    const EDGE_LOW: f32 = 0.5;
    const EDGE_HIGH: f32 = 2.0;
    let texel = 1.0 / vec2<f32>(textureDimensions(source_image, u32(develop.lod)));
    let reach = max(develop.fringe_reach, texel);
    var low = 1e10;
    var high = 1e-5;
    for (var tap = 0; tap < 4; tap++) {
        let angle = f32(tap) * TAU / 4.0;
        let at = uv + vec2<f32>(cos(angle), sin(angle)) * reach;
        let camera = textureSampleLevel(source_image, linear_sampler, at, develop.lod).xyz;
        let brightness = max(dot(develop.camera_to_working * camera, develop.luma.xyz), 1e-5);
        low = min(low, brightness);
        high = max(high, brightness);
    }
    let edge = smoothstep(EDGE_LOW, EDGE_HIGH, log2(high / low));
    if edge <= 0.0 {
        return rgb;
    }
    let lab = to_oklab(max(rgb, vec3<f32>(0.0)));
    var hue = atan2(lab.z, lab.y);
    if hue < 0.0 {
        hue += TAU;
    }
    let purple = develop.defringe_purple;
    let green = develop.defringe_green;
    let strength = max(
        purple.x * hue_band(hue, purple.y, purple.z),
        green.x * hue_band(hue, green.y, green.z),
    );
    return from_oklab(vec3<f32>(lab.x, lab.yz * (1.0 - edge * strength)));
}

// The atmospheric scattering model, I = J·t + A·(1 − t), solved for J. Above the atmosphere
// that would brighten by 1/t, which blows out anything lighter than the haze, so haze is only
// ever taken away. A pixel the model would send to black keeps a share of what it was, since
// a dark channel estimate can't tell a dark object in haze from a hole.
fn apply_dehaze(rgb: vec3<f32>, dark: f32, amount: f32) -> vec3<f32> {
    let atmosphere = vec3<f32>(develop.atmosphere);
    if amount < 0.0 {
        return mix(rgb, atmosphere, min(-amount, 1.0) * 0.5);
    }
    const STRENGTH: f32 = 0.85;
    const MIN_TRANSMISSION: f32 = 0.2;
    const DARKEST: f32 = 0.3;
    let haze = clamp(dark, 0.0, develop.atmosphere);
    let transmission = max(1.0 - amount * STRENGTH * haze / develop.atmosphere, MIN_TRANSMISSION);
    let removed = max(atmosphere - rgb, vec3<f32>(0.0)) * (1.0 / transmission - 1.0);
    return max(rgb - removed, rgb * DARKEST);
}

fn scene_luminance(uv: vec2<f32>) -> f32 {
    let camera = textureSampleLevel(source_image, linear_sampler, uv, develop.lod).xyz;
    return max(dot(develop.camera_to_working * camera, develop.luma.xyz), 1e-5);
}

// The band Texture works on: what the fine guide model can't explain, measured on a slightly
// blurred copy of the photo, which leaves the finest detail and most noise to sharpening.
// Near strong edges the model explains everything, so nothing is left there to cause a halo.
fn texture_band(uv: vec2<f32>) -> f32 {
    let texel = 1.0 / vec2<f32>(textureDimensions(source_image, 0));
    let around = scene_luminance(uv)
        + scene_luminance(uv + vec2<f32>(1.5, 0.5) * texel)
        + scene_luminance(uv + vec2<f32>(-0.5, 1.5) * texel)
        + scene_luminance(uv + vec2<f32>(-1.5, -0.5) * texel)
        + scene_luminance(uv + vec2<f32>(0.5, -1.5) * texel);
    let blurred = log2(around * 0.2);
    let terms = textureSampleLevel(fine_model, linear_sampler, uv, 0.0);
    return blurred - (terms.x * blurred + terms.y);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile_extent()) {
        return;
    }
    let pixel = vec2<f32>(image_position(local)) + 0.5;
    // Tile padding can reach past the frame. Repeating the edge there stops sharpening from
    // treating the frame boundary as an edge in the photo.
    let half_step = develop.view_step * 0.5;
    let view = clamp(develop.view_origin + pixel * develop.view_step, half_step, vec2<f32>(1.0) - half_step);
    let position = source_position(view);
    let uv = to_uv(position);
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        textureStore(out_image, local, vec4<f32>(0.0));
        store_local(local, LocalEdits());
        return;
    }

    let edits = local_edits(uv);
    let camera = sample_camera(position, uv, edits.noise);
    var rgb = max(develop.camera_to_working * camera, vec3<f32>(0.0));
    if develop.defringe_purple.x > 0.0 || develop.defringe_green.x > 0.0 {
        rgb = defringe(rgb, uv);
    }
    if edits.temperature != 0.0 || edits.tint != 0.0 {
        rgb *= local_balance(edits);
    }

    // The guides were built without the vignette gain. They model a pixel's luma, so they get
    // the luma they were built on, and the gain then applies to the smooth layers they return.
    let vignette = vignette_stops(position);
    let unlit_luma = log2(max(dot(rgb, develop.luma.xyz), 1e-5));
    rgb *= exp2(vignette);
    if develop.hue_sat_cells.x > 0.0 {
        rgb = apply_profile_table(rgb, hue_sat_table, develop.hue_sat_cells);
    }
    let log_luma = unlit_luma + vignette;
    let base_terms = textureSampleLevel(base_model, linear_sampler, uv, 0.0);
    let detail_terms = textureSampleLevel(detail_model, linear_sampler, uv, 0.0);
    let base = base_terms.x * unlit_luma + base_terms.y + vignette;
    let mid_base = detail_terms.x * unlit_luma + detail_terms.y + vignette;

    let dehaze = develop.dehaze + edits.dehaze;
    if dehaze != 0.0 {
        rgb = apply_dehaze(rgb, detail_terms.z * log_luma + detail_terms.w, dehaze);
    }

    let exposure = develop.exposure + edits.exposure;
    let exposed_base = base + exposure;
    let exposed = log_luma + exposure;

    // Gains come from the smooth base layer, so local contrast survives untouched and edges
    // don't halo the way a plain blur-based mask would.
    let highlight_weight = bump(-3.5, -0.5, exposed_base);
    let shadow_weight = 1.0 - bump(-7.0, -2.5, exposed_base);
    var log_gain = exposure
        + (develop.highlights + edits.highlights) * 1.6 * highlight_weight
        + (develop.shadows + edits.shadows) * 1.8 * shadow_weight;

    let midtones = bump(-8.0, -4.5, exposed_base) * (1.0 - bump(-2.0, 0.0, exposed_base));
    log_gain += (develop.clarity + edits.clarity) * 1.3 * (log_luma - mid_base) * midtones;
    let texture_amount = develop.texture_amount + edits.texture_amount;
    if texture_amount != 0.0 {
        // Compressing the band keeps the strongest of it from turning into sharpening.
        let band = texture_band(uv);
        log_gain += texture_amount * 1.1 * band / (1.0 + abs(band) * 2.0) * midtones;
    }

    let whites_weight = bump(develop.white_ev - 2.5, develop.white_ev, exposed);
    let blacks_weight = 1.0 - bump(develop.black_ev, develop.black_ev + 3.0, exposed);
    log_gain += develop.whites * 1.5 * whites_weight + develop.blacks * 2.0 * blacks_weight;
    log_gain += edits.whites * 1.5 * whites_weight + edits.blacks * 2.0 * blacks_weight;

    const PIVOT_EV: f32 = -2.47;
    log_gain += (exposed - PIVOT_EV) * (develop.contrast + edits.contrast) * 0.55
        * bump(-11.0, -6.0, exposed) * (1.0 - bump(0.5, 3.0, exposed));

    rgb *= exp2(log_gain);

    if develop.shadow_tint != 0.0 {
        let shadow_amount = 1.0 - bump(-7.0, -1.5, exposed);
        rgb *= vec3<f32>(1.0, 1.0 - develop.shadow_tint * shadow_amount, 1.0);
    }

    if develop.look_cells.x > 0.0 {
        rgb = apply_profile_table(rgb, look_table, develop.look_cells);
    }
    rgb = hue_preserving_tone(rgb);
    if develop.curves_enabled > 0.0 {
        rgb = apply_curves(rgb);
    }
    rgb = apply_color(rgb, edits.hue, edits.saturation);
    // Alpha carries lightness for sharpening, which would otherwise work it out per tap.
    let lightness = pow(max(dot(rgb, develop.luma.xyz), 0.0), 1.0 / 2.4);
    textureStore(out_image, local, vec4<f32>(rgb, lightness));
    store_local(local, edits);
}
