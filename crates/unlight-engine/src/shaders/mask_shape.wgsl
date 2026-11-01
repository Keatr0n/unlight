// One component of a mask, drawn across the coverage map. Blending does the combining: add
// keeps the larger value, subtract multiplies by what's left, so a soft edge fades either way.

const MAX_COLOUR_SAMPLES: u32 = 8u;

struct Shape {
    kind: u32,
    inverted: u32,
    sample_count: u32,
    padding: u32,
    first: vec4<f32>,
    second: vec4<f32>,
    samples: array<vec4<f32>, MAX_COLOUR_SAMPLES>,
}

@group(0) @binding(3) var<uniform> shape: Shape;
@group(0) @binding(4) var painted: texture_2d<f32>;

const KIND_NOTHING: u32 = 0u;
const KIND_PAINTED: u32 = 1u;
const KIND_LINEAR: u32 = 2u;
const KIND_RADIAL: u32 = 3u;
const KIND_LUMINANCE: u32 = 4u;
const KIND_COLOUR: u32 = 5u;

@vertex
fn shape_vertex(@builtin(vertex_index) corner: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((corner << 1u) & 2u), f32(corner & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

// Measured in units of the long edge, so a circle stays round on any frame.
fn square_units(uv: vec2<f32>) -> vec2<f32> {
    return uv * scene.raster_size / scene.long_edge;
}

fn linear_coverage(uv: vec2<f32>) -> f32 {
    let start = square_units(shape.first.xy);
    let along = square_units(shape.first.zw) - start;
    let travelled = dot(square_units(uv) - start, along) / max(dot(along, along), 1e-12);
    return 1.0 - smoothstep(0.0, 1.0, travelled);
}

fn radial_coverage(uv: vec2<f32>) -> f32 {
    let offset = square_units(uv) - square_units(shape.first.xy);
    let cosine = shape.second.x;
    let sine = shape.second.y;
    let turned = vec2<f32>(cosine * offset.x + sine * offset.y, cosine * offset.y - sine * offset.x);
    let radius = length(turned / max(shape.first.zw, vec2<f32>(1e-6)));
    return 1.0 - smoothstep(min(1.0 - shape.second.z, 0.995), 1.0, radius);
}

fn soft_band(value: f32, low: f32, high: f32, softness: f32) -> f32 {
    let rising = smoothstep(low - softness, low + 1e-4, value);
    return rising * (1.0 - smoothstep(high - 1e-4, high + softness, value));
}

// Close to the default rendering's curve, so the range reads roughly as tones on screen do. It's
// fixed rather than the real one, which would move a selection whenever the look changed.
fn scene_brightness(uv: vec2<f32>) -> f32 {
    const LIFT: f32 = 2.2;
    let luminance = dot(scene_rgb(uv), scene.luma.xyz);
    let lifted = luminance * (1.0 + LIFT) / (1.0 + LIFT * luminance);
    return pow(clamp(lifted, 0.0, 1.0), 1.0 / 2.4);
}

fn colour_coverage(uv: vec2<f32>) -> f32 {
    const TOLERANCE_SCALE: f32 = 0.3;
    let colour = oklab(scene_rgb(uv));
    var nearest = 1e6;
    for (var index = 0u; index < shape.sample_count; index++) {
        nearest = min(nearest, colour_distance(colour, oklab(shape.samples[index].xyz)));
    }
    let tolerance = max(shape.first.x * TOLERANCE_SCALE, 1e-3);
    return 1.0 - smoothstep(tolerance * 0.5, tolerance, nearest);
}

@fragment
fn shape_fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = position.xy / scene.raster_size;
    var coverage = 0.0;
    switch shape.kind {
        case KIND_PAINTED {
            coverage = textureSampleLevel(painted, linear_sampler, uv, 0.0).r;
        }
        case KIND_LINEAR {
            coverage = linear_coverage(uv);
        }
        case KIND_RADIAL {
            coverage = radial_coverage(uv);
        }
        case KIND_LUMINANCE {
            let softness = shape.first.z * 0.5;
            coverage = soft_band(scene_brightness(uv), shape.first.x, shape.first.y, softness);
        }
        case KIND_COLOUR {
            coverage = colour_coverage(uv);
        }
        default {}
    }
    if shape.inverted != 0u && shape.kind != KIND_NOTHING {
        coverage = 1.0 - coverage;
    }
    return vec4<f32>(coverage, 0.0, 0.0, 1.0);
}
