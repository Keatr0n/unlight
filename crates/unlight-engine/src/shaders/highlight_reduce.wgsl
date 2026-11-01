// Push half of a push-pull fill: averages colour and support, both still premultiplied, so a
// coarse cell knows how much of it was backed by real data.

@group(0) @binding(1) var source_level: texture_2d<f32>;
@group(0) @binding(2) var out_level: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile.size) {
        return;
    }
    let target_position = local + tile.origin;
    let p = target_position * 2;
    let limit = vec2<i32>(textureDimensions(source_level)) - 1;
    let sum = textureLoad(source_level, p, 0)
        + textureLoad(source_level, min(p + vec2<i32>(1, 0), limit), 0)
        + textureLoad(source_level, min(p + vec2<i32>(0, 1), limit), 0)
        + textureLoad(source_level, min(p + vec2<i32>(1, 1), limit), 0);
    textureStore(out_level, target_position, sum * 0.25);
}
