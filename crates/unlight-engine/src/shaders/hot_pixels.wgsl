// Replaces sites that stand far above everything around them, whatever its colour. A real
// point of light spreads into its neighbours through the lens, so this leaves stars alone.
// Clipped sites carry a negative sign, and a repaired one stops being clipped.

const RATIO: f32 = 8.0;
const FLOOR: f32 = 0.01;

@group(0) @binding(1) var mosaic: texture_2d<f32>;
@group(0) @binding(2) var out_mosaic: texture_storage_2d<r32float, write>;

fn site(p: vec2<i32>) -> f32 {
    let limit = vec2<i32>(textureDimensions(mosaic)) - 1;
    return abs(textureLoad(mosaic, clamp(p, vec2<i32>(0), limit), 0).r);
}

// The mean of the nearby sites that share this one's colour, or nothing if any site within
// reach is bright enough to explain it.
fn repair(p: vec2<i32>, image_p: vec2<i32>, value: f32) -> f32 {
    let channel = cfa_color(image_p);
    var sum = 0.0;
    var count = 0.0;
    for (var y = -2; y <= 2; y++) {
        for (var x = -2; x <= 2; x++) {
            let d = vec2<i32>(x, y);
            if all(d == vec2<i32>(0)) {
                continue;
            }
            let around = site(p + d);
            if around * RATIO + FLOOR > value {
                return -1.0;
            }
            if cfa_color(image_p + d) == channel {
                sum += around;
                count += 1.0;
            }
        }
    }
    return sum / max(count, 1.0);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= tile.size) {
        return;
    }
    let image_p = local + tile.origin;
    let p = image_p + MOSAIC_BORDER;
    let stored = textureLoad(mosaic, p, 0).r;
    let value = abs(stored);
    var result = stored;
    // Nearly every site fails one of these first checks, which skips the full neighbourhood.
    if value > FLOOR
        && site(p + vec2<i32>(1, 0)) * RATIO + FLOOR <= value
        && site(p + vec2<i32>(0, 1)) * RATIO + FLOOR <= value {
        let repaired = repair(p, image_p, value);
        if repaired >= 0.0 {
            result = repaired;
        }
    }
    textureStore(out_mosaic, p, vec4<f32>(result, 0.0, 0.0, 0.0));
}
