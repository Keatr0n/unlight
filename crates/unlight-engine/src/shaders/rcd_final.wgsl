// RCD: fills red and blue at green sites and writes the finished tile.

const EPS: f32 = 1e-5;

@group(0) @binding(1) var direction: texture_2d<f32>;
@group(0) @binding(2) var partial: texture_2d<f32>;
@group(0) @binding(3) var out_image: texture_storage_2d<{{RGB_FORMAT}}, write>;

fn rgb(p: vec2<i32>) -> vec3<f32> {
    return textureLoad(partial, p, 0).xyz;
}

fn discriminant(local: vec2<i32>) -> f32 {
    let central = textureLoad(direction, local, 0).x;
    let around = 0.25 * (textureLoad(direction, local + vec2<i32>(-1, -1), 0).x
        + textureLoad(direction, local + vec2<i32>(1, -1), 0).x
        + textureLoad(direction, local + vec2<i32>(-1, 1), 0).x
        + textureLoad(direction, local + vec2<i32>(1, 1), 0).x);
    return select(central, around, abs(0.5 - central) < abs(0.5 - around));
}

// Gradient and colour difference for one cardinal neighbour, per channel in xz.
struct Cardinal {
    gradient: vec3<f32>,
    estimate: vec3<f32>,
}

fn cardinal(local: vec2<i32>, center: f32, d: vec2<i32>) -> Cardinal {
    let near = rgb(local + d);
    let opposite = rgb(local - d);
    let far = rgb(local + 3 * d);
    let green_far = textureLoad(partial, local + 2 * d, 0).y;
    var result: Cardinal;
    result.gradient = vec3<f32>(EPS + abs(center - green_far)) + abs(near - opposite) + abs(near - far);
    result.estimate = near - vec3<f32>(near.y);
    return result;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let inner = vec2<i32>(id.xy);
    if any(inner >= tile.size) {
        return;
    }
    let local = inner + tile.pad;
    let image_p = image_position(local);
    var color = rgb(local);
    if cfa_color(image_p) == 1u {
        let center = color.y;
        let north = cardinal(local, center, vec2<i32>(0, -1));
        let south = cardinal(local, center, vec2<i32>(0, 1));
        let west = cardinal(local, center, vec2<i32>(-1, 0));
        let east = cardinal(local, center, vec2<i32>(1, 0));

        let vertical = (north.gradient * south.estimate + south.gradient * north.estimate)
            / (north.gradient + south.gradient);
        let horizontal = (east.gradient * west.estimate + west.gradient * east.estimate)
            / (east.gradient + west.gradient);
        let difference = mix(vertical, horizontal, discriminant(local));
        color = max(vec3<f32>(center) + difference, vec3<f32>(0.0));
        color.y = center;
    }
    textureStore(out_image, image_p, vec4<f32>(color, 1.0));
}
