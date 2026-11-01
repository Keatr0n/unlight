// Stands in for demosaicing on a merged photo, which has every colour at every pixel already.
// All there is to do is carry pixels over to the working precision.

@group(0) @binding(1) var source_image: texture_2d<f32>;
@group(0) @binding(2) var out_image: texture_storage_2d<{{RGB_FORMAT}}, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile.size) {
        return;
    }
    let target_position = local + tile.origin;
    textureStore(out_image, target_position, vec4<f32>(textureLoad(source_image, target_position, 0).rgb, 1.0));
}
