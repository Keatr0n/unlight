// Healing, cloning and red eye, applied straight after demosaicing so that everything later
// sees the repaired photo. Each pixel depends only on the unrepaired image and its own
// position, which is why tile size can't change the result.

const RING_SAMPLES: i32 = 48;
const RING_REACH: f32 = 1.12;
const TAU: f32 = 6.28318530718;

const KIND_HEAL: u32 = 1u;
const KIND_RED_EYE: u32 = 2u;

struct Cell {
    origin: vec2<i32>,
    size: vec2<i32>,
    count: u32,
    padding_a: u32,
    padding_b: u32,
    padding_c: u32,
}

struct Spot {
    centre: vec2<f32>,
    offset: vec2<i32>,
    radius: f32,
    feather: f32,
    opacity: f32,
    kind: u32,
    ring: i32,
    pupil: f32,
    darken: f32,
    padding: f32,
}

@group(0) @binding(0) var<uniform> cell: Cell;
@group(0) @binding(1) var<storage, read> spots: array<Spot>;
@group(0) @binding(2) var unrepaired: texture_2d<f32>;
@group(0) @binding(3) var rings: texture_2d<f32>;
@group(0) @binding(4) var out_image: texture_storage_2d<{{OUTPUT_FORMAT}}, write>;

// The harmonic function inside a circle that takes the ring's values on its edge, from the
// Poisson integral. A disc has this closed form, so a heal needs no iterative solve and nothing
// about it depends on resolution. Every weight is positive, so it can't overshoot the ring.
fn membrane(ring: i32, position: vec2<f32>) -> vec3<f32> {
    var total = vec3<f32>(0.0);
    var weight = 0.0;
    for (var index = 0; index < RING_SAMPLES; index++) {
        let angle = TAU * (f32(index) + 0.5) / f32(RING_SAMPLES);
        let gap = vec2<f32>(cos(angle), sin(angle)) - position;
        let pull = 1.0 / max(dot(gap, gap), 1e-5);
        total += textureLoad(rings, vec2<i32>(index, ring), 0).xyz * pull;
        weight += pull;
    }
    return total / weight;
}

fn repaired_patch(spot: Spot, p: vec2<i32>, offset: vec2<f32>) -> vec3<f32> {
    let limit = vec2<i32>(textureDimensions(unrepaired)) - 1;
    let moved = textureLoad(unrepaired, clamp(p + spot.offset, vec2<i32>(0), limit), 0).xyz;
    if spot.kind == KIND_HEAL {
        return moved * exp(membrane(spot.ring, offset / RING_REACH));
    }
    return moved;
}

// A pupil lit by flash is red with little green or blue, and a catchlight isn't, so only the
// red turns neutral and dark. The red channel is what's wrong, so the others stand in for it.
fn fix_red_eye(spot: Spot, rgb: vec3<f32>, radius: f32) -> vec3<f32> {
    const REDNESS_LOW: f32 = 1.25;
    const REDNESS_HIGH: f32 = 1.9;
    const DARKEST: f32 = 0.12;
    let rest = 0.5 * (rgb.y + rgb.z);
    let redness = rgb.x / max(rest, 1e-4);
    let in_pupil = 1.0 - smoothstep(spot.pupil * 0.85, spot.pupil * 1.1 + 1e-3, radius);
    let amount = smoothstep(REDNESS_LOW, REDNESS_HIGH, redness) * in_pupil * spot.opacity;
    return mix(rgb, vec3<f32>(rest) * mix(1.0, DARKEST, spot.darken), amount);
}

fn apply_spot(spot: Spot, p: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let offset = (vec2<f32>(p) + 0.5 - spot.centre) / spot.radius;
    let radius = length(offset);
    if radius >= 1.0 {
        return rgb;
    }
    if spot.kind == KIND_RED_EYE {
        return fix_red_eye(spot, rgb, radius);
    }
    let edge = min(1.0 - spot.feather, 0.98);
    let coverage = (1.0 - smoothstep(edge, 1.0, radius)) * spot.opacity;
    return mix(rgb, repaired_patch(spot, p, offset), coverage);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= cell.size) {
        return;
    }
    let p = local + cell.origin;
    let original = textureLoad(unrepaired, p, 0);
    var rgb = original.xyz;
    for (var index = 0u; index < cell.count; index++) {
        rgb = apply_spot(spots[index], p, rgb);
    }
    textureStore(out_image, p, vec4<f32>(rgb, original.a));
}
