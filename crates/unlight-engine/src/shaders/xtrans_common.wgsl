// Markesteijn X-Trans demosaicing (one pass, four directions), after dcraw and darktable.

struct Xtrans {
    hex: array<vec4<i32>, 36>,
    solitary_green: vec2<i32>,
    padding: vec2<i32>,
}

@group(0) @binding(1) var<uniform> xtrans: Xtrans;

fn hex_offset(p: vec2<i32>, index: i32) -> vec2<i32> {
    let entry = (positive_mod(p.y, 3) * 3 + positive_mod(p.x, 3)) * 8 + index;
    let pair = xtrans.hex[entry / 2];
    return select(pair.xy, pair.zw, (entry & 1) == 1);
}

fn green_row_phase(p: vec2<i32>) -> i32 {
    return positive_mod(p.y - xtrans.solitary_green.y, 3);
}

fn green_col_phase(p: vec2<i32>) -> i32 {
    return positive_mod(p.x - xtrans.solitary_green.x, 3);
}
