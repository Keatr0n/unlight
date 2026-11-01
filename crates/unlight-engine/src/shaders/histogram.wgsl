// Counts the viewport into 256 bins per channel for the histogram panel.

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>, 768>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(image);
    if any(id.xy >= size) {
        return;
    }
    let color = textureLoad(image, id.xy, 0).xyz;
    let index = vec3<u32>(color * 255.0 + 0.5);
    atomicAdd(&bins[index.x], 1u);
    atomicAdd(&bins[256u + index.y], 1u);
    atomicAdd(&bins[512u + index.z], 1u);
}
