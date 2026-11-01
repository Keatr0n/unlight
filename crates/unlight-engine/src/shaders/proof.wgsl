// Soft proofing. This only exists in a view-only copy of the finish pass, so neither export nor
// a view with proofing off carries any of it.

struct Proof {
    working_to_target: mat3x3<f32>,
    target_to_display: mat3x3<f32>,
    warning: vec4<f32>,
    uses_table: f32,
    show_warning: f32,
    table_cells: f32,
    padding: f32,
}

@group(0) @binding(6) var<uniform> proof: Proof;
@group(0) @binding(7) var proof_table: texture_3d<f32>;
@group(0) @binding(8) var proof_sampler: sampler;

const PROOF_GAMMA: f32 = 2.2;
// How far a colour may leave a matrix target's cube and still count as inside. Below zero, less
// than this is only rounding and sensor noise in the deep shadows. Above one it's a pale sky a
// little over white in one channel, which clipping barely changes: this much is about the
// colour difference a baked profile's warning starts at.
const SHADOW_TOLERANCE: f32 = 0.002;
const HIGHLIGHT_TOLERANCE: f32 = 0.04;

fn proofed(rgb: vec3<f32>, shown: vec3<f32>, dither: f32) -> vec3<f32> {
    var color = shown;
    var outside: bool;
    if proof.uses_table > 0.0 {
        let cells = proof.table_cells;
        let encoded = pow(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / PROOF_GAMMA));
        let entry = textureSampleLevel(proof_table, proof_sampler, (encoded * (cells - 1.0) + 0.5) / cells, 0.0);
        color = entry.xyz + vec3<f32>(dither);
        outside = entry.w > 0.5;
    } else {
        let in_target = proof.working_to_target * max(rgb, vec3<f32>(0.0));
        outside = any(in_target < vec3<f32>(-SHADOW_TOLERANCE)) || any(in_target > vec3<f32>(1.0 + HIGHLIGHT_TOLERANCE));
        if outside {
            let clipped = clamp(in_target, vec3<f32>(0.0), vec3<f32>(1.0));
            let display = clamp(proof.target_to_display * clipped, vec3<f32>(0.0), vec3<f32>(1.0));
            color = encode(display) + vec3<f32>(dither);
        }
    }
    if outside && proof.show_warning > 0.0 {
        return proof.warning.xyz;
    }
    return color;
}
