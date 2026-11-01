// Averages the most homogeneous directions and writes the finished tile.

@group(0) @binding(1) var candidates: texture_2d_array<f32>;
@group(0) @binding(2) var homogeneity: texture_2d<f32>;
@group(0) @binding(3) var out_image: texture_storage_2d<{{RGB_FORMAT}}, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let inner = vec2<i32>(id.xy);
    if any(inner >= tile.size) {
        return;
    }
    let local = inner + tile.pad;
    var score = vec4<f32>(0.0);
    for (var y = -2; y <= 2; y++) {
        for (var x = -2; x <= 2; x++) {
            score += textureLoad(homogeneity, local + vec2<i32>(x, y), 0);
        }
    }
    let best = max(max(score.x, score.y), max(score.z, score.w));
    let threshold = best - floor(best / 8.0);
    var sum = vec3<f32>(0.0);
    var count = 0.0;
    for (var layer = 0; layer < 4; layer++) {
        if score[layer] >= threshold {
            sum += textureLoad(candidates, local, layer, 0).xyz;
            count += 1.0;
        }
    }
    textureStore(out_image, image_position(local), vec4<f32>(sum / max(count, 1.0), 1.0));
}
