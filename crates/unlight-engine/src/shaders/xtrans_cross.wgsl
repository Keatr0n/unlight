// Fills red at blue sites and blue at red sites.

@group(0) @binding(2) var previous: texture_2d_array<f32>;
@group(0) @binding(3) var out_rgb: texture_storage_2d_array<rgba32float, write>;

fn load(p: vec2<i32>, layer: i32) -> vec4<f32> {
    return textureLoad(previous, p, layer, 0);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile_extent()) {
        return;
    }
    let p = image_position(local);
    let color = cfa_color(p);
    let vertical_step = green_row_phase(p) != 0;
    let near = select(vec2<i32>(1, 0), vec2<i32>(0, 1), vertical_step);
    let far = select(vec2<i32>(0, 3), vec2<i32>(3, 0), vertical_step);
    for (var layer = 0; layer < 4; layer++) {
        var rgb = load(local, layer);
        if color != 1u {
            let missing = 2u - color;
            let g = rgb.y;
            let aligned = (layer & 1) == select(0, 1, vertical_step);
            var step = far;
            if layer > 1 || aligned {
                step = near;
            } else {
                let near_change = abs(g - load(local + near, layer).y) + abs(g - load(local - near, layer).y);
                let far_change = abs(g - load(local + far, layer).y) + abs(g - load(local - far, layer).y);
                if near_change < 2.0 * far_change {
                    step = near;
                }
            }
            let forward = load(local + step, layer);
            let backward = load(local - step, layer);
            rgb[missing] = max(
                (forward[missing] + backward[missing] + 2.0 * g - forward.y - backward.y) * 0.5,
                0.0,
            );
        }
        textureStore(out_rgb, local, layer, rgb);
    }
}
