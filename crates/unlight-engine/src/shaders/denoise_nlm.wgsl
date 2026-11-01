// Non-local means on luminance. Each pixel averages the neighbours whose patches look like its
// own, which follows edges and texture where a blur would cross them. Only the decision is
// stabilised: the average itself is linear, so it can't shift brightness.

struct Means {
    scale: f32,
    allowance: f32,
    keep: f32,
    padding: f32,
}

const SEARCH: i32 = 5;

@group(0) @binding(1) var<uniform> means: Means;
@group(0) @binding(2) var image: texture_2d<f32>;
@group(0) @binding(3) var first_patch: texture_2d<f32>;
@group(0) @binding(4) var second_patch: texture_2d<f32>;
@group(0) @binding(5) var out_image: texture_storage_2d<{{RGB_FORMAT}}, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let inner = vec2<i32>(id.xy);
    if any(inner >= tile.size) {
        return;
    }
    let local = inner + tile.pad;
    let here_first = textureLoad(first_patch, local, 0);
    let here_second = textureLoad(second_patch, local, 0);
    var total = 0.0;
    var sum = 0.0;
    var heaviest = 0.0;
    for (var y = -SEARCH; y <= SEARCH; y++) {
        for (var x = -SEARCH; x <= SEARCH; x++) {
            if x == 0 && y == 0 {
                continue;
            }
            let q = local + vec2<i32>(x, y);
            let there_first = textureLoad(first_patch, q, 0);
            let there_second = textureLoad(second_patch, q, 0);
            let first = here_first.xyz - there_first.xyz;
            let second = here_second - there_second;
            let excess = max((dot(first, first) + dot(second, second)) * means.scale - means.allowance, 0.0);
            let closeness = max(1.0 - excess, 0.0);
            let weight = closeness * closeness;
            total += weight;
            sum += weight * there_first.w;
            heaviest = max(heaviest, weight);
        }
    }
    // A pixel always matches itself exactly, and letting that count in full would keep the
    // noise of any pixel with few matches. Counting it like its best match avoids that, while
    // the floor covers a pixel with no match at all.
    let luma = here_first.w;
    let own_weight = max(heaviest, 1e-3);
    let smoothed = (sum + own_weight * luma) / (total + own_weight);
    let rgb = textureLoad(image, local, 0).xyz + vec3<f32>(mix(smoothed, luma, means.keep) - luma);
    textureStore(out_image, image_position(local), vec4<f32>(rgb, 1.0));
}
