const MOSAIC_BORDER: i32 = 24;

struct TileParams {
    origin: vec2<i32>,
    size: vec2<i32>,
    image: vec2<i32>,
    pad: i32,
    period: i32,
    cfa: array<vec4<u32>, 9>,
}

@group(0) @binding(0) var<uniform> tile: TileParams;

fn positive_mod(value: i32, modulus: i32) -> i32 {
    return ((value % modulus) + modulus) % modulus;
}

fn cfa_color(p: vec2<i32>) -> u32 {
    let index = positive_mod(p.y, 6) * 6 + positive_mod(p.x, 6);
    return tile.cfa[index / 4][index % 4];
}

fn tile_extent() -> vec2<i32> {
    return tile.size + 2 * tile.pad;
}

fn image_position(local: vec2<i32>) -> vec2<i32> {
    return local + tile.origin - tile.pad;
}
