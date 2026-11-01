// Deliberately slow enough to trip the kernel's GPU timeout. Only `unlight-cli hang-test`
// compiles this, and only after the user has opted in.

@group(0) @binding(0) var<storage, read_write> sink: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    var value = f32(id.x);
    for (var i = 0u; i < 2000000u; i++) {
        value = sqrt(abs(sin(value) + 1.5)) + f32(i & 1u);
    }
    sink[id.x % 1024u] = value;
}
