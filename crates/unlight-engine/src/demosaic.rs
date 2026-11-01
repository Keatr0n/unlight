use crate::{
    error::Result,
    params::{MAX_TILE_EDGE, TILE_PAD, TileParams, XtransParams, cfa_params},
    pipelines::Engine,
    scheduler::{SchedulerConfig, Tile},
    source::SourceImage,
};
use unlight_raw::Cfa;
use wgpu::util::DeviceExt;

const ORTHOGONAL: [i32; 12] = [1, 0, 0, 1, -1, 0, 0, -1, 1, 0, 0, 1];
const HEX_PATTERN: [[i32; 16]; 2] = [
    [0, 1, 0, -1, 2, 0, -1, 0, 1, 1, 1, -1, 0, 0, 0, 0],
    [0, 1, 0, -2, 1, 0, -2, 0, 1, 1, -2, -2, 1, -1, -1, 1],
];

/// Maps the green hexagon around each site of the 3×3 X-Trans repeat, returning (dx, dy)
/// offsets packed for the shaders, plus the position of the isolated green site.
pub fn xtrans_hex_table(cfa: &Cfa) -> ([[i32; 4]; 36], [i32; 2]) {
    let color = |row: i32, col: i32| cfa.color_at(col.rem_euclid(6) as u32, row.rem_euclid(6) as u32);
    let mut hex = [[[0i32; 2]; 8]; 9];
    let mut solitary = [0i32; 2];
    for row in 0..3i32 {
        for col in 0..3i32 {
            let is_green = usize::from(color(row, col) == 1);
            let mut non_green_run = 0usize;
            for d in (0..10usize).step_by(2) {
                if color(row + ORTHOGONAL[d], col + ORTHOGONAL[d + 2]) == 1 {
                    non_green_run = 0;
                } else {
                    non_green_run += 1;
                }
                if non_green_run == 4 {
                    solitary = [col, row];
                }
                if non_green_run == is_green + 1 {
                    for c in 0..8usize {
                        let (a, b) = (HEX_PATTERN[is_green][c * 2], HEX_PATTERN[is_green][c * 2 + 1]);
                        let dy = ORTHOGONAL[d] * a + ORTHOGONAL[d + 1] * b;
                        let dx = ORTHOGONAL[d + 2] * a + ORTHOGONAL[d + 3] * b;
                        hex[(row * 3 + col) as usize][c ^ ((is_green * 2) & d)] = [dx, dy];
                    }
                }
            }
        }
    }
    let flat: Vec<[i32; 2]> = hex.iter().flatten().copied().collect();
    let table = std::array::from_fn(|i| {
        let (a, b) = (flat[i * 2], flat[i * 2 + 1]);
        [a[0], a[1], b[0], b[1]]
    });
    (table, solitary)
}

pub(crate) fn entry<'a>(binding: u32, view: &'a wgpu::TextureView) -> wgpu::BindGroupEntry<'a> {
    wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::TextureView(view) }
}

impl Engine {
    pub(crate) fn tile_entry(&self) -> wgpu::BindGroupEntry<'_> {
        wgpu::BindGroupEntry { binding: 0, resource: self.tile_buffer.as_entire_binding() }
    }

    fn tile_view(&self, label: &str, layers: u32, format: wgpu::TextureFormat) -> wgpu::TextureView {
        let extent = self.max_tile_extent();
        let texture = self.storage_texture(label, [extent, extent, layers], 1, format);
        let dimension = (layers > 1).then_some(wgpu::TextureViewDimension::D2Array);
        texture.create_view(&wgpu::TextureViewDescriptor { dimension, ..Default::default() })
    }

    /// Full demosaic of the photo into `target`, a texture of this engine's precision.
    pub fn demosaic(
        &self,
        source: &SourceImage,
        target: &wgpu::TextureView,
        schedule: SchedulerConfig,
    ) -> Result<()> {
        let region = Tile { x: 0, y: 0, width: source.width, height: source.height };
        self.demosaic_region(source, target, region, schedule)
    }

    /// Demosaics one tile, for filling in full resolution as the user zooms around.
    pub(crate) fn demosaic_tile(
        &self,
        source: &SourceImage,
        target: &wgpu::TextureView,
        tile: Tile,
    ) -> Result<()> {
        let fixed = SchedulerConfig {
            min_edge: tile.width.max(tile.height),
            initial_edge: tile.width.max(tile.height),
            ..SchedulerConfig::INTERACTIVE
        };
        self.demosaic_region(source, target, tile, fixed)
    }

    fn demosaic_region(
        &self,
        source: &SourceImage,
        target: &wgpu::TextureView,
        region: Tile,
        schedule: SchedulerConfig,
    ) -> Result<()> {
        if source.cfa.is_full_colour() {
            self.copy_full_colour(source, target, region, schedule)
        } else if source.cfa.is_xtrans() {
            self.demosaic_xtrans(source, target, region, schedule)
        } else {
            self.demosaic_bayer(source, target, region, schedule)
        }
    }

    fn tile_params(&self, source: &SourceImage, tile: Tile) -> TileParams {
        TileParams {
            origin: [tile.x as i32, tile.y as i32],
            size: [tile.width as i32, tile.height as i32],
            image: [source.width as i32, source.height as i32],
            pad: TILE_PAD as i32,
            period: source.cfa.period as i32,
            cfa: cfa_params(&source.cfa),
        }
    }

    fn limited(schedule: SchedulerConfig) -> SchedulerConfig {
        SchedulerConfig { max_edge: schedule.max_edge.min(MAX_TILE_EDGE), ..schedule }
    }

    fn copy_full_colour(
        &self,
        source: &SourceImage,
        target: &wgpu::TextureView,
        region: Tile,
        schedule: SchedulerConfig,
    ) -> Result<()> {
        let pipeline = &self.pipelines.full_colour;
        let group = self.bind(pipeline, &[self.tile_entry(), entry(1, &source.mosaic), entry(2, target)]);
        self.run_tiles(region.width, region.height, Self::limited(schedule), |encoder, tile| {
            let tile = Tile { x: tile.x + region.x, y: tile.y + region.y, ..tile };
            self.write_tile(&self.tile_params(source, tile));
            self.dispatch(encoder, pipeline, &group, [tile.width, tile.height], "full colour", None);
        })
    }

    fn demosaic_bayer(
        &self,
        source: &SourceImage,
        target: &wgpu::TextureView,
        region: Tile,
        schedule: SchedulerConfig,
    ) -> Result<()> {
        let rgba = wgpu::TextureFormat::Rgba32Float;
        let high_pass = self.tile_view("rcd high pass", 1, rgba);
        let direction = self.tile_view("rcd direction", 1, rgba);
        let green = self.tile_view("rcd green", 1, wgpu::TextureFormat::R32Float);
        let partial = self.tile_view("rcd partial", 1, rgba);
        let p = &self.pipelines;
        let mosaic = &source.mosaic;

        let high_pass_group =
            self.bind(&p.rcd_high_pass, &[self.tile_entry(), entry(1, mosaic), entry(2, &high_pass)]);
        let direction_group = self.bind(
            &p.rcd_direction,
            &[self.tile_entry(), entry(1, mosaic), entry(2, &high_pass), entry(3, &direction)],
        );
        let green_group = self.bind(
            &p.rcd_green,
            &[self.tile_entry(), entry(1, mosaic), entry(2, &direction), entry(3, &green)],
        );
        let diagonal_group = self.bind(
            &p.rcd_diagonal,
            &[self.tile_entry(), entry(1, mosaic), entry(2, &direction), entry(3, &green), entry(4, &partial)],
        );
        let final_group = self.bind(
            &p.rcd_final,
            &[self.tile_entry(), entry(1, &direction), entry(2, &partial), entry(3, target)],
        );

        self.run_tiles(region.width, region.height, Self::limited(schedule), |encoder, tile| {
            let tile = Tile { x: tile.x + region.x, y: tile.y + region.y, ..tile };
            self.write_tile(&self.tile_params(source, tile));
            let padded = [tile.width + 2 * TILE_PAD, tile.height + 2 * TILE_PAD];
            let inner = [tile.width, tile.height];
            self.dispatch(encoder, &p.rcd_high_pass, &high_pass_group, padded, "rcd high pass", None);
            self.dispatch(encoder, &p.rcd_direction, &direction_group, padded, "rcd direction", None);
            self.dispatch(encoder, &p.rcd_green, &green_group, padded, "rcd green", None);
            self.dispatch(encoder, &p.rcd_diagonal, &diagonal_group, padded, "rcd diagonal", None);
            self.dispatch(encoder, &p.rcd_final, &final_group, inner, "rcd final", None);
        })
    }

    fn demosaic_xtrans(
        &self,
        source: &SourceImage,
        target: &wgpu::TextureView,
        region: Tile,
        schedule: SchedulerConfig,
    ) -> Result<()> {
        let rgba = wgpu::TextureFormat::Rgba32Float;
        let first = self.tile_view("xtrans candidates a", 4, rgba);
        let second = self.tile_view("xtrans candidates b", 4, rgba);
        let derivative = self.tile_view("xtrans derivative", 1, rgba);
        let homogeneity = self.tile_view("xtrans homogeneity", 1, rgba);
        let p = &self.pipelines;

        let (hex, solitary_green) = xtrans_hex_table(&source.cfa);
        let xtrans_params = XtransParams { hex, solitary_green, padding: [0; 2] };
        let xtrans = self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("xtrans params"),
            contents: bytemuck::bytes_of(&xtrans_params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let xtrans_entry =
            || wgpu::BindGroupEntry { binding: 1, resource: xtrans.as_entire_binding() };

        let green_group = self.bind(
            &p.xtrans_green,
            &[self.tile_entry(), xtrans_entry(), entry(2, &source.mosaic), entry(3, &first)],
        );
        let solitary_group = self.bind(
            &p.xtrans_solitary,
            &[self.tile_entry(), xtrans_entry(), entry(2, &first), entry(3, &second)],
        );
        let cross_group = self.bind(
            &p.xtrans_cross,
            &[self.tile_entry(), xtrans_entry(), entry(2, &second), entry(3, &first)],
        );
        let blocks_group = self.bind(
            &p.xtrans_blocks,
            &[self.tile_entry(), xtrans_entry(), entry(2, &first), entry(3, &second)],
        );
        let derivative_group = self.bind(
            &p.xtrans_derivative,
            &[self.tile_entry(), entry(1, &second), entry(2, &derivative)],
        );
        let homogeneity_group = self.bind(
            &p.xtrans_homogeneity,
            &[self.tile_entry(), entry(1, &derivative), entry(2, &homogeneity)],
        );
        let final_group = self.bind(
            &p.xtrans_final,
            &[self.tile_entry(), entry(1, &second), entry(2, &homogeneity), entry(3, target)],
        );

        self.run_tiles(region.width, region.height, Self::limited(schedule), |encoder, tile| {
            let tile = Tile { x: tile.x + region.x, y: tile.y + region.y, ..tile };
            self.write_tile(&self.tile_params(source, tile));
            let padded = [tile.width + 2 * TILE_PAD, tile.height + 2 * TILE_PAD];
            let inner = [tile.width, tile.height];
            self.dispatch(encoder, &p.xtrans_green, &green_group, padded, "xtrans green", None);
            self.dispatch(encoder, &p.xtrans_solitary, &solitary_group, padded, "xtrans solitary", None);
            self.dispatch(encoder, &p.xtrans_cross, &cross_group, padded, "xtrans cross", None);
            self.dispatch(encoder, &p.xtrans_blocks, &blocks_group, padded, "xtrans blocks", None);
            self.dispatch(encoder, &p.xtrans_derivative, &derivative_group, padded, "xtrans derivative", None);
            self.dispatch(encoder, &p.xtrans_homogeneity, &homogeneity_group, padded, "xtrans homogeneity", None);
            self.dispatch(encoder, &p.xtrans_final, &final_group, inner, "xtrans final", None);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Fujifilm X-T4 layout as LibRaw reports it.
    fn xtrans() -> Cfa {
        let rows = ["GGRGGB", "GGBGGR", "BRGRBG", "GGBGGR", "GGRGGB", "RBGBRG"];
        let mut pattern = [[0u8; 6]; 6];
        for (row, text) in rows.iter().enumerate() {
            for (col, letter) in text.bytes().enumerate() {
                pattern[row][col] = match letter {
                    b'R' => 0,
                    b'G' => 1,
                    _ => 2,
                };
            }
        }
        Cfa { period: 6, pattern }
    }

    fn offset(table: &[[i32; 4]; 36], row: usize, col: usize, index: usize) -> [i32; 2] {
        let entry = (row * 3 + col) * 8 + index;
        let pair = table[entry / 2];
        if entry.is_multiple_of(2) { [pair[0], pair[1]] } else { [pair[2], pair[3]] }
    }

    #[test]
    fn hexagon_around_every_non_green_site_lands_on_green() {
        let cfa = xtrans();
        let (table, _) = xtrans_hex_table(&cfa);
        for row in 0..3usize {
            for col in 0..3usize {
                if cfa.pattern[row][col] == 1 {
                    continue;
                }
                for index in 0..6 {
                    let [dx, dy] = offset(&table, row, col, index);
                    let x = (col as i32 + dx + 12) as u32;
                    let y = (row as i32 + dy + 12) as u32;
                    assert_eq!(cfa.color_at(x, y), 1, "site {row},{col} hex {index}: {dx},{dy}");
                }
            }
        }
    }

    #[test]
    fn solitary_green_has_no_green_neighbours() {
        let cfa = xtrans();
        let (_, [col, row]) = xtrans_hex_table(&cfa);
        let (col, row) = (col as u32 + 6, row as u32 + 6);
        assert_eq!(cfa.color_at(col, row), 1);
        for (x, y) in [(col + 1, row), (col - 1, row), (col, row + 1), (col, row - 1)] {
            assert_ne!(cfa.color_at(x, y), 1);
        }
    }
}
