// Fit-view path: averages each CFA block to one RGB pixel, skipping full demosaicing.

@group(0) @binding(1) var mosaic: texture_2d<f32>;
@group(0) @binding(2) var out_image: texture_storage_2d<rgba16float, write>;

fn site(p: vec2<i32>) -> f32 {
    return textureLoad(mosaic, p + MOSAIC_BORDER, 0).r;
}

// An X-Trans block's sites sit evenly around its centre, so a plain average is in the right place.
fn average_block(block: vec2<i32>, factor: i32) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    var count = vec3<f32>(0.0);
    for (var y = 0; y < factor; y++) {
        for (var x = 0; x < factor; x++) {
            let p = block + vec2<i32>(x, y);
            let channel = cfa_color(p);
            sum[channel] += site(p);
            count[channel] += 1.0;
        }
    }
    return sum / max(count, vec3<f32>(1.0));
}

// How much a site this far along from a block's first site counts towards the block's centre.
fn tent(offset: i32) -> f32 {
    return select(0.75, 0.25, offset == -1 || offset == 2);
}

// A Bayer block's two greens straddle its centre, but its red and blue sit half a sensor pixel
// to either side. Averaging those in place would leave a colour fringe on every edge, so each
// is interpolated to the centre from the nearest sites of its own colour.
fn center_bayer_block(block: vec2<i32>) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    var total = vec3<f32>(0.0);
    for (var y = -1; y <= 2; y++) {
        for (var x = -1; x <= 2; x++) {
            let p = block + vec2<i32>(x, y);
            let channel = cfa_color(p);
            let in_block = x >= 0 && x < 2 && y >= 0 && y < 2;
            if channel == 1u && !in_block {
                continue;
            }
            let weight = tent(x) * tent(y);
            sum[channel] += site(p) * weight;
            total[channel] += weight;
        }
    }
    return sum / max(total, vec3<f32>(1e-6));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile.size) {
        return;
    }
    let target_position = local + tile.origin;
    var rgb: vec3<f32>;
    if tile.period == 6 {
        rgb = average_block(target_position * 3, 3);
    } else {
        rgb = center_bayer_block(target_position * 2);
    }
    textureStore(out_image, target_position, vec4<f32>(rgb, 1.0));
}
