// Sums every mask's offsets into the parameter fields: param(x) = global + Σ mask_i(x)·Δ_i.
// The develop and finish passes read only the result, so they don't care how many masks went in.

const FIELD_LAYERS: i32 = 4;

struct Combine {
    origin: vec2<i32>,
    size: vec2<i32>,
    count: u32,
    overlay: i32,
    padding_a: u32,
    padding_b: u32,
}

struct Row {
    offsets: array<vec4<f32>, 4>,
    layer: i32,
    padding_a: i32,
    padding_b: i32,
    padding_c: i32,
}

@group(0) @binding(0) var<uniform> combine: Combine;
@group(0) @binding(1) var<storage, read> rows: array<Row>;
@group(0) @binding(2) var coverages: texture_2d_array<f32>;
@group(0) @binding(3) var out_fields: texture_storage_2d_array<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let local = vec2<i32>(id.xy);
    if any(local >= combine.size) {
        return;
    }
    let p = local + combine.origin;
    var total: array<vec4<f32>, 4>;
    for (var index = 0u; index < combine.count; index++) {
        let coverage = textureLoad(coverages, p, rows[index].layer, 0).r;
        if coverage > 0.0 {
            for (var layer = 0; layer < FIELD_LAYERS; layer++) {
                total[layer] += rows[index].offsets[layer] * coverage;
            }
        }
    }
    if combine.overlay >= 0 {
        total[3].w = textureLoad(coverages, p, combine.overlay, 0).r;
    }
    for (var layer = 0; layer < FIELD_LAYERS; layer++) {
        textureStore(out_fields, p, layer, total[layer]);
    }
}
