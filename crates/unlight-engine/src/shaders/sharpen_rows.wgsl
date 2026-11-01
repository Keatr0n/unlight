// The horizontal half of the sharpening blur. The finish pass does the vertical half, so the
// kernel costs a row plus a column of taps. x is blurred lightness, y its horizontal slope,
// z and w the darkest and brightest values nearby, which limit halos.

@group(0) @binding(2) var developed: texture_2d<f32>;
@group(0) @binding(3) var out_rows: texture_storage_2d<{{RGB_FORMAT}}, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    let extent = tile_extent();
    if any(local >= extent) {
        return;
    }
    let reach = sharpen_reach();
    var blurred = 0.0;
    var slope = 0.0;
    var low = 1e6;
    var high = -1e6;
    var weight = 0.0;
    var moment = 0.0;
    for (var x = -reach; x <= reach; x++) {
        let value = textureLoad(developed, clamp(local + vec2<i32>(x, 0), vec2<i32>(0), extent - 1), 0).a;
        let tap = sharpen_tap(x);
        blurred += value * tap;
        slope += value * f32(x) * tap;
        low = min(low, value);
        high = max(high, value);
        weight += tap;
        moment += tap * f32(x * x);
    }
    textureStore(out_rows, local, vec4<f32>(blurred / weight, slope / moment, low, high));
}
