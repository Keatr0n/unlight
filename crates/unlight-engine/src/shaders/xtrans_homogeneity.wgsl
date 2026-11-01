// Counts, per direction, how many neighbours are close to the smoothest direction.

@group(0) @binding(1) var derivatives: texture_2d<f32>;
@group(0) @binding(2) var out_homogeneity: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile_extent()) {
        return;
    }
    let center = textureLoad(derivatives, local, 0);
    let threshold = min(min(center.x, center.y), min(center.z, center.w)) * 8.0;
    var counts = vec4<f32>(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let around = textureLoad(derivatives, local + vec2<i32>(x, y), 0);
            counts += select(vec4<f32>(0.0), vec4<f32>(1.0), around <= vec4<f32>(threshold));
        }
    }
    textureStore(out_homogeneity, local, counts);
}
