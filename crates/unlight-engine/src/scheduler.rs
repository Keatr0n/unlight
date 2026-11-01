//! Splits long GPU work into submits short enough to stay clear of the kernel's GPU timeout
//! and, while editing, to keep the viewport responsive. Tile size adapts to measured speed.

use std::{collections::VecDeque, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tile {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Tile {
    pub fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SchedulerConfig {
    /// Target GPU time for one submit.
    pub budget: Duration,
    pub min_edge: u32,
    pub max_edge: u32,
    pub initial_edge: u32,
}

impl SchedulerConfig {
    /// For work the user is waiting on while also interacting.
    pub const INTERACTIVE: Self = Self {
        budget: Duration::from_millis(16),
        min_edge: 128,
        max_edge: crate::params::MAX_TILE_EDGE,
        initial_edge: 256,
    };

    pub const BACKGROUND: Self = Self {
        budget: Duration::from_millis(250),
        min_edge: 128,
        max_edge: crate::params::MAX_TILE_EDGE,
        initial_edge: 512,
    };
}

/// Weight of the newest measurement in the running cost estimate.
const COST_SMOOTHING: f64 = 0.4;
/// Aim under the budget so an unlucky tile still lands inside it.
const BUDGET_HEADROOM: f64 = 0.8;
const MAX_EDGE_GROWTH: f64 = 2.0;

#[derive(Clone, Debug)]
pub struct TileScheduler {
    width: u32,
    height: u32,
    config: SchedulerConfig,
    edge: u32,
    ns_per_pixel: Option<f64>,
    x: u32,
    y: u32,
    band_height: u32,
    requeued: VecDeque<Tile>,
}

impl TileScheduler {
    pub fn new(width: u32, height: u32, config: SchedulerConfig) -> Self {
        let edge = config.initial_edge.clamp(config.min_edge, config.max_edge);
        Self {
            width,
            height,
            config,
            edge,
            ns_per_pixel: None,
            x: 0,
            y: 0,
            band_height: 0,
            requeued: VecDeque::new(),
        }
    }

    pub fn edge(&self) -> u32 {
        self.edge
    }

    pub fn next_tile(&mut self) -> Option<Tile> {
        if let Some(tile) = self.requeued.pop_front() {
            return Some(tile);
        }
        if self.width == 0 || self.height == 0 {
            return None;
        }
        if self.x >= self.width {
            self.x = 0;
            self.y += self.band_height;
            self.band_height = 0;
        }
        if self.y >= self.height {
            return None;
        }
        if self.band_height == 0 {
            self.band_height = self.edge.min(self.height - self.y);
        }
        let tile = Tile {
            x: self.x,
            y: self.y,
            width: self.edge.min(self.width - self.x),
            height: self.band_height,
        };
        self.x += tile.width;
        Some(tile)
    }

    /// Feeds back how long a tile took and resizes the tiles that follow.
    pub fn record(&mut self, tile: Tile, elapsed: Duration) {
        let measured = elapsed.as_nanos() as f64 / tile.pixels().max(1) as f64;
        let cost = match self.ns_per_pixel {
            Some(previous) => previous + (measured - previous) * COST_SMOOTHING,
            None => measured,
        };
        self.ns_per_pixel = Some(cost);
        let target_pixels = self.config.budget.as_nanos() as f64 * BUDGET_HEADROOM / cost.max(1e-3);
        let growth_limit = f64::from(self.edge) * MAX_EDGE_GROWTH;
        let edge = target_pixels.sqrt().min(growth_limit) as u32;
        self.edge = edge.clamp(self.config.min_edge, self.config.max_edge);
    }

    /// Puts a tile back after its work was lost, so it runs again.
    pub fn requeue(&mut self, tile: Tile) {
        self.requeued.push_back(tile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: SchedulerConfig = SchedulerConfig {
        budget: Duration::from_millis(100),
        min_edge: 16,
        max_edge: 512,
        initial_edge: 64,
    };

    fn assert_exact_cover(tiles: &[Tile], width: u32, height: u32) {
        let mut hits = vec![0u8; (width * height) as usize];
        for tile in tiles {
            assert!(tile.x + tile.width <= width && tile.y + tile.height <= height, "{tile:?}");
            for y in tile.y..tile.y + tile.height {
                for x in tile.x..tile.x + tile.width {
                    hits[(y * width + x) as usize] += 1;
                }
            }
        }
        assert!(hits.iter().all(|&count| count == 1), "not an exact cover");
    }

    fn drain(scheduler: &mut TileScheduler, mut ns_per_pixel: impl FnMut(usize) -> f64) -> Vec<Tile> {
        let mut tiles = Vec::new();
        while let Some(tile) = scheduler.next_tile() {
            let elapsed = tile.pixels() as f64 * ns_per_pixel(tiles.len());
            scheduler.record(tile, Duration::from_nanos(elapsed as u64));
            tiles.push(tile);
        }
        tiles
    }

    #[test]
    fn covers_the_image_exactly_once_at_a_steady_speed() {
        let mut scheduler = TileScheduler::new(1000, 700, CONFIG);
        assert_exact_cover(&drain(&mut scheduler, |_| 50.0), 1000, 700);
    }

    #[test]
    fn covers_the_image_exactly_once_while_speed_swings() {
        let mut scheduler = TileScheduler::new(997, 613, CONFIG);
        let tiles = drain(&mut scheduler, |i| [5.0, 900.0, 40.0, 3000.0, 1.0][i % 5]);
        assert_exact_cover(&tiles, 997, 613);
    }

    #[test]
    fn grows_toward_the_budget_on_fast_hardware() {
        let mut scheduler = TileScheduler::new(10_000, 10_000, CONFIG);
        drain(&mut scheduler, |_| 0.5);
        assert_eq!(scheduler.edge(), 512);
    }

    #[test]
    fn shrinks_after_a_slow_tile() {
        let mut scheduler = TileScheduler::new(10_000, 10_000, CONFIG);
        let tile = scheduler.next_tile().unwrap();
        scheduler.record(tile, CONFIG.budget * 20);
        assert!(scheduler.edge() < 64, "edge stayed at {}", scheduler.edge());
    }

    #[test]
    fn keeps_a_converged_tile_within_budget() {
        let ns_per_pixel = 200.0;
        let mut scheduler = TileScheduler::new(5000, 5000, CONFIG);
        let tiles = drain(&mut scheduler, |_| ns_per_pixel);
        let last = tiles[tiles.len() / 2];
        let elapsed = last.pixels() as f64 * ns_per_pixel;
        assert!(elapsed <= CONFIG.budget.as_nanos() as f64, "{elapsed} ns for {last:?}");
    }

    #[test]
    fn never_leaves_the_configured_edge_range() {
        let mut scheduler = TileScheduler::new(4000, 4000, CONFIG);
        for tile in drain(&mut scheduler, |i| if i % 2 == 0 { 0.01 } else { 1e6 }) {
            assert!(tile.width <= 512 && tile.height <= 512);
        }
    }

    #[test]
    fn requeued_tiles_run_again_before_new_work() {
        let mut scheduler = TileScheduler::new(100, 100, CONFIG);
        let first = scheduler.next_tile().unwrap();
        let second = scheduler.next_tile().unwrap();
        scheduler.requeue(second);
        assert_eq!(scheduler.next_tile(), Some(second));
        assert_ne!(first, second);
    }

    #[test]
    fn empty_image_has_no_tiles() {
        assert_eq!(TileScheduler::new(0, 100, CONFIG).next_tile(), None);
        assert_eq!(TileScheduler::new(100, 0, CONFIG).next_tile(), None);
    }
}
