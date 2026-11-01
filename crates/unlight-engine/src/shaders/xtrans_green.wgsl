// Interpolates green along four directions, bounded by the surrounding green hexagon.

@group(0) @binding(2) var mosaic: texture_2d<f32>;
@group(0) @binding(3) var out_rgb: texture_storage_2d_array<rgba32float, write>;

fn site(p: vec2<i32>) -> f32 {
    return textureLoad(mosaic, p + MOSAIC_BORDER, 0).r;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile_extent()) {
        return;
    }
    let p = image_position(local);
    let color = cfa_color(p);
    let value = site(p);
    var greens = vec4<f32>(value);

    if color != 1u {
        let h0 = hex_offset(p, 0);
        let h1 = hex_offset(p, 1);
        let h2 = hex_offset(p, 2);
        let h3 = hex_offset(p, 3);
        let h4 = hex_offset(p, 4);
        let h5 = hex_offset(p, 5);

        var low = site(p + h0);
        var high = low;
        for (var i = 1; i < 6; i++) {
            let neighbour = site(p + hex_offset(p, i));
            low = min(low, neighbour);
            high = max(high, neighbour);
        }

        let c0 = (174.0 * (site(p + h1) + site(p + h0))
            - 46.0 * (site(p + 2 * h1) + site(p + 2 * h0))) / 256.0;
        let c1 = (223.0 * site(p + h3) + 33.0 * site(p + h2)
            + 92.0 * (value - site(p - h2))) / 256.0;
        let c2 = (164.0 * site(p + h4) + 92.0 * site(p - 2 * h4)
            + 33.0 * (2.0 * value - site(p + 3 * h4) - site(p - 3 * h4))) / 256.0;
        let c3 = (164.0 * site(p + h5) + 92.0 * site(p - 2 * h5)
            + 33.0 * (2.0 * value - site(p + 3 * h5) - site(p - 3 * h5))) / 256.0;

        var estimates = vec4<f32>(c0, c1, c2, c3);
        if green_row_phase(p) == 0 {
            estimates = vec4<f32>(c1, c0, c3, c2);
        }
        greens = clamp(estimates, vec4<f32>(low), vec4<f32>(high));
    }

    for (var layer = 0; layer < 4; layer++) {
        var rgb = vec4<f32>(0.0, greens[layer], 0.0, 1.0);
        if color != 1u {
            rgb[color] = value;
        }
        textureStore(out_rgb, local, layer, rgb);
    }
}
