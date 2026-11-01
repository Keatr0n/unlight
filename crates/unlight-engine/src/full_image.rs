//! The photo at full resolution, filled in a cell at a time as views reach it. Every derived
//! image tracks its own cells, so a repair only redoes what it covers.

use crate::{
    error::Result,
    pipelines::Engine,
    repair::{Repairs, UvRect},
    scheduler::Tile,
    source::SourceImage,
};
use unlight_recipe::NoiseReduction;

/// Tile size for on-demand full-resolution demosaicing.
pub(crate) const REGION_TILE: u32 = 512;

/// A denoised copy of the image, valid per cell for one set of noise settings.
pub(crate) struct CleanImage {
    pub view: wgpu::TextureView,
    /// Up to date.
    pub ready: Vec<bool>,
    /// Holds a result for these settings, if perhaps from before a repair moved.
    pub filled: Vec<bool>,
    settings: Option<NoiseReduction>,
}

impl CleanImage {
    fn new(engine: &Engine, label: &str, size: [u32; 2], cells: usize) -> Self {
        let view = engine
            .storage_texture(label, [size[0], size[1], 1], 1, engine.precision.format())
            .create_view(&Default::default());
        Self { view, ready: vec![false; cells], filled: vec![false; cells], settings: None }
    }

    pub(crate) fn settle(&mut self, settings: &NoiseReduction) {
        if self.settings.as_ref() != Some(settings) {
            self.ready.fill(false);
            self.filled.fill(false);
            self.settings = Some(*settings);
        }
    }

    pub(crate) fn mark(&mut self, index: usize) {
        self.ready[index] = true;
        self.filled[index] = true;
    }
}

pub(crate) struct FullImage {
    pub view: wgpu::TextureView,
    repaired: Option<wgpu::TextureView>,
    pub denoised: CleanImage,
    pub cleaner: Option<CleanImage>,
    demosaiced: Vec<bool>,
    patched: Vec<bool>,
    pub columns: u32,
    pub rows: u32,
    size: [u32; 2],
}

impl FullImage {
    pub(crate) fn new(engine: &Engine, size: [u32; 2]) -> Self {
        let view = engine
            .storage_texture("full image", [size[0], size[1], 1], 1, engine.precision.format())
            .create_view(&Default::default());
        let columns = size[0].div_ceil(REGION_TILE);
        let rows = size[1].div_ceil(REGION_TILE);
        let cells = (columns * rows) as usize;
        Self {
            view,
            repaired: None,
            denoised: CleanImage::new(engine, "denoised image", size, cells),
            cleaner: None,
            demosaiced: vec![false; cells],
            patched: vec![false; cells],
            columns,
            rows,
            size,
        }
    }

    pub(crate) fn cleaner_mut(&mut self, engine: &Engine) -> &mut CleanImage {
        let cells = self.demosaiced.len();
        let size = self.size;
        self.cleaner.get_or_insert_with(|| CleanImage::new(engine, "cleaner image", size, cells))
    }

    pub(crate) fn cell_index(&self, column: u32, row: u32) -> usize {
        (row * self.columns + column) as usize
    }

    pub(crate) fn cell_tile(&self, column: u32, row: u32) -> Tile {
        let (x, y) = (column * REGION_TILE, row * REGION_TILE);
        Tile { x, y, width: REGION_TILE.min(self.size[0] - x), height: REGION_TILE.min(self.size[1] - y) }
    }

    pub(crate) fn column_at(&self, value: f32) -> u32 {
        ((value.clamp(0.0, 1.0) * self.size[0] as f32) as u32 / REGION_TILE).min(self.columns - 1)
    }

    pub(crate) fn row_at(&self, value: f32) -> u32 {
        ((value.clamp(0.0, 1.0) * self.size[1] as f32) as u32 / REGION_TILE).min(self.rows - 1)
    }

    pub(crate) fn is_demosaiced(&self, index: usize) -> bool {
        self.demosaiced[index]
    }

    pub(crate) fn is_patched(&self, index: usize) -> bool {
        self.patched[index]
    }

    /// The image before noise reduction, with repairs if there are any.
    pub(crate) fn scene(&self, repairing: bool) -> &wgpu::TextureView {
        self.repaired.as_ref().filter(|_| repairing).unwrap_or(&self.view)
    }

    /// Forgets what a changed repair covered, and what denoising took from around it.
    pub(crate) fn invalidate(&mut self, area: UvRect) {
        let columns = self.column_at(area.min[0])..=self.column_at(area.max[0]);
        let rows = self.row_at(area.min[1])..=self.row_at(area.max[1]);
        for row in rows {
            for column in columns.clone() {
                let index = self.cell_index(column, row);
                self.patched[index] = false;
                for near_row in row.saturating_sub(1)..=(row + 1).min(self.rows - 1) {
                    for near_column in column.saturating_sub(1)..=(column + 1).min(self.columns - 1) {
                        let near = self.cell_index(near_column, near_row);
                        self.denoised.ready[near] = false;
                        if let Some(cleaner) = &mut self.cleaner {
                            cleaner.ready[near] = false;
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn forget_repairs(&mut self) {
        self.patched.fill(false);
    }

    fn demosaic_cell(&mut self, engine: &Engine, source: &SourceImage, column: u32, row: u32) -> Result<()> {
        let index = self.cell_index(column, row);
        if !self.demosaiced[index] {
            engine.demosaic_tile(source, &self.view, self.cell_tile(column, row))?;
            self.demosaiced[index] = true;
        }
        Ok(())
    }

    /// Demosaics every cell a rectangle of the image touches.
    fn demosaic_tile_area(&mut self, engine: &Engine, source: &SourceImage, area: Tile) -> Result<()> {
        if area.width == 0 || area.height == 0 {
            return Ok(());
        }
        let cell = |value: u32, cells: u32| (value / REGION_TILE).min(cells - 1);
        for row in cell(area.y, self.rows)..=cell(area.y + area.height - 1, self.rows) {
            for column in cell(area.x, self.columns)..=cell(area.x + area.width - 1, self.columns) {
                self.demosaic_cell(engine, source, column, row)?;
            }
        }
        Ok(())
    }

    /// Makes a cell's pixels before noise reduction current. A repaired cell also needs
    /// whichever cells its patches come from.
    pub(crate) fn prepare_cell(
        &mut self,
        engine: &Engine,
        source: &SourceImage,
        repairs: &Repairs,
        column: u32,
        row: u32,
    ) -> Result<()> {
        self.demosaic_cell(engine, source, column, row)?;
        let index = self.cell_index(column, row);
        if !repairs.is_active() || self.patched[index] {
            return Ok(());
        }
        let tile = self.cell_tile(column, row);
        for area in repairs.sources_of(tile, self.size) {
            self.demosaic_tile_area(engine, source, area)?;
        }
        let size = self.size;
        let output = self.repaired.get_or_insert_with(|| {
            engine
                .storage_texture("repaired full image", [size[0], size[1], 1], 1, engine.precision.format())
                .create_view(&Default::default())
        });
        let mut encoder = engine.encoder();
        let pipeline = &engine.pipelines.repair.full;
        repairs.encode_tile(engine, &mut encoder, pipeline, tile, size, [&self.view, output]);
        engine.submit_and_wait(encoder)?;
        self.patched[index] = true;
        Ok(())
    }
}
