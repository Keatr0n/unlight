// What a heal has to bend its patch by: the ratio of the photo around the target to the photo
// around the source, at evenly spaced points on a ring just outside the spot. It's read from
// the shared pyramid at one fixed scale, so preview and export solve exactly the same problem.

const RING_SAMPLES: u32 = 48u;
const RING_REACH: f32 = 1.12;
const TAU: f32 = 6.28318530718;

struct RingSpot {
    centre: vec2<f32>,
    patch_centre: vec2<f32>,
    radius: vec2<f32>,
    lod: f32,
    padding: f32,
}

@group(0) @binding(0) var<storage, read> ring_spots: array<RingSpot>;
@group(0) @binding(1) var pyramid: texture_2d<f32>;
@group(0) @binding(2) var linear_sampler: sampler;
@group(0) @binding(3) var out_rings: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= RING_SAMPLES || id.y >= arrayLength(&ring_spots) {
        return;
    }
    let spot = ring_spots[id.y];
    let angle = TAU * (f32(id.x) + 0.5) / f32(RING_SAMPLES);
    let around = vec2<f32>(cos(angle), sin(angle)) * spot.radius * RING_REACH;
    let wanted = textureSampleLevel(pyramid, linear_sampler, spot.centre + around, spot.lod).xyz;
    let supplied = textureSampleLevel(pyramid, linear_sampler, spot.patch_centre + around, spot.lod).xyz;
    let ratio = log(max(wanted, vec3<f32>(1e-4)) / max(supplied, vec3<f32>(1e-4)));
    textureStore(out_rings, vec2<i32>(id.xy), vec4<f32>(ratio, 1.0));
}
