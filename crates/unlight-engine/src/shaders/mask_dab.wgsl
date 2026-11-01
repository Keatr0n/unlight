// One brush dab per instance. Dabs land through ordinary alpha blending, which is what makes
// flow build up and an eraser take away in stroke order without touching what's already there.

struct DabOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) offset: vec2<f32>,
    @location(1) @interpolate(flat) feather: f32,
    @location(2) @interpolate(flat) flow: f32,
    @location(3) @interpolate(flat) value: f32,
    @location(4) @interpolate(flat) reference: vec4<f32>,
}

const ERASE: u32 = 1u;
const AUTO_MASK: u32 = 2u;

@vertex
fn dab_vertex(
    @builtin(vertex_index) corner: u32,
    @location(0) position: vec2<f32>,
    @location(1) radius: f32,
    @location(2) feather: f32,
    @location(3) flow: f32,
    @location(4) flags: u32,
) -> DabOutput {
    let offset = vec2<f32>(f32(corner & 1u) * 2.0 - 1.0, f32((corner >> 1u) & 1u) * 2.0 - 1.0);
    let reach = max(radius * scene.long_edge, 0.75) / scene.raster_size;
    let uv = position + offset * reach;
    var output: DabOutput;
    output.clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    output.offset = offset;
    output.feather = feather;
    output.flow = flow;
    output.value = select(1.0, 0.0, (flags & ERASE) != 0u);
    if (flags & AUTO_MASK) != 0u {
        output.reference = vec4<f32>(oklab(scene_rgb(position)), 1.0);
    }
    return output;
}

@fragment
fn dab_fragment(input: DabOutput) -> @location(0) vec4<f32> {
    const AUTO_MASK_LOW: f32 = 0.02;
    const AUTO_MASK_HIGH: f32 = 0.09;
    let radius = length(input.offset);
    let edge = min(1.0 - input.feather, 0.98);
    var strength = input.flow * (1.0 - smoothstep(edge, 1.0, radius));
    if input.reference.w > 0.0 {
        let uv = input.clip.xy / scene.raster_size;
        let distance = colour_distance(oklab(scene_rgb(uv)), input.reference.xyz);
        strength *= 1.0 - smoothstep(AUTO_MASK_LOW, AUTO_MASK_HIGH, distance);
    }
    return vec4<f32>(input.value, 0.0, 0.0, strength);
}
