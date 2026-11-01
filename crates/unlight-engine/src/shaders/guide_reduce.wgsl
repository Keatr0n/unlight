// Halves the guide image. Means stay means, while the dark channel keeps its minimum so a
// bright speck can't lift the haze estimate. Each texel reads from where it sits in the level
// above, because an odd-sized level has no exact half and its index would drift off the photo.

@group(0) @binding(0) var source_level: texture_2d<f32>;
@group(0) @binding(1) var out_level: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(textureDimensions(out_level));
    if any(id.xy >= size) {
        return;
    }
    let source_size = vec2<i32>(textureDimensions(source_level, 0));
    let center = (vec2<f32>(id.xy) + 0.5) * vec2<f32>(source_size) / vec2<f32>(size) - 0.5;
    let corner = floor(center);
    let along = center - corner;
    let p = vec2<i32>(corner);
    let last = source_size - 1;
    let a = textureLoad(source_level, min(p, last), 0);
    let b = textureLoad(source_level, min(p + vec2<i32>(1, 0), last), 0);
    let c = textureLoad(source_level, min(p + vec2<i32>(0, 1), last), 0);
    let d = textureLoad(source_level, min(p + vec2<i32>(1, 1), last), 0);
    let mean = mix(mix(a, b, along.x), mix(c, d, along.x), along.y);
    let darkest = min(a.z, min(b.z, min(c.z, d.z)));
    textureStore(out_level, vec2<i32>(id.xy), vec4<f32>(mean.x, mean.y, darkest, 1.0));
}
