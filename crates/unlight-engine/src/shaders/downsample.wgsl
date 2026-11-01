// Halves a level. Each texel reads from where it sits in the level above rather than from twice
// its index: an odd-sized level has no exact half, and the index would slide the photo up to a
// whole texel towards the top left. On an even size the two are the same box filter.

@group(0) @binding(1) var source_level: texture_2d<f32>;
@group(0) @binding(2) var out_level: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile.size) {
        return;
    }
    let target_position = local + tile.origin;
    let source_size = vec2<i32>(textureDimensions(source_level, 0));
    let scale = vec2<f32>(source_size) / vec2<f32>(textureDimensions(out_level));
    let center = (vec2<f32>(target_position) + 0.5) * scale - 0.5;
    let corner = floor(center);
    let along = center - corner;
    let p = vec2<i32>(corner);
    let last = source_size - 1;
    let top = mix(
        textureLoad(source_level, min(p, last), 0),
        textureLoad(source_level, min(p + vec2<i32>(1, 0), last), 0),
        along.x,
    );
    let bottom = mix(
        textureLoad(source_level, min(p + vec2<i32>(0, 1), last), 0),
        textureLoad(source_level, min(p + vec2<i32>(1, 1), last), 0),
        along.x,
    );
    textureStore(out_level, target_position, mix(top, bottom, along.y));
}
