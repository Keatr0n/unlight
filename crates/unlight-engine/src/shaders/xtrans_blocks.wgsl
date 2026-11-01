// Fills red and blue inside the 2×2 green blocks.

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
    let in_block = green_row_phase(p) != 0 && green_col_phase(p) != 0;
    for (var layer = 0; layer < 4; layer++) {
        var rgb = load(local, layer);
        if in_block {
            let first = hex_offset(p, 2 * layer);
            let second = hex_offset(p, 2 * layer + 1);
            let a = load(local + first, layer);
            let b = load(local + second, layer);
            if any(first + second != vec2<i32>(0)) {
                let g = 3.0 * rgb.y - 2.0 * a.y - b.y;
                rgb.x = max((g + 2.0 * a.x + b.x) / 3.0, 0.0);
                rgb.z = max((g + 2.0 * a.z + b.z) / 3.0, 0.0);
            } else {
                let g = 2.0 * rgb.y - a.y - b.y;
                rgb.x = max((g + a.x + b.x) * 0.5, 0.0);
                rgb.z = max((g + a.z + b.z) * 0.5, 0.0);
            }
        }
        textureStore(out_rgb, local, layer, rgb);
    }
}
