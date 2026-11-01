//! Drives `Session::render`. The rendered texture is what egui displays, with no copy. Each
//! render is keyed on everything that affects it, so an unchanged frame never touches the GPU.

use crate::darkroom::LivePhoto;
use eframe::{egui, egui_wgpu::RenderState};
use unlight_engine::{EngineError, Proof, ViewRequest, ViewTarget};
use unlight_recipe::Recipe;

const HISTOGRAM_EDGE: f32 = 640.0;
/// The histogram can trail a slider a little; the photo itself can't.
const HISTOGRAM_INTERVAL: f64 = 0.066;
/// Full-resolution tiles to demosaic per frame while zoomed in. One tile takes tens of
/// milliseconds, so any more would make the UI visibly lurch.
const TILES_PER_FRAME: usize = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct PaneRequest {
    pub recipe: Recipe,
    /// In sensor orientation.
    pub region: [f32; 4],
    /// In sensor orientation.
    pub size: [u32; 2],
    pub clipping: bool,
    /// Index of a mask to show in red.
    pub mask_overlay: Option<usize>,
    /// The soft proof to show. It stays in the view, and never reaches an export.
    pub proof: Option<Proof>,
}

#[derive(Clone, Debug, PartialEq)]
struct RenderKey {
    serial: u64,
    request: PaneRequest,
}

struct Pane {
    target: ViewTarget,
    texture: egui::TextureId,
    rendered: Option<RenderKey>,
    complete: bool,
}

impl Pane {
    fn ensure(slot: &mut Option<Self>, state: &RenderState, live: &LivePhoto, [width, height]: [u32; 2]) {
        if let Some(pane) = slot
            && (pane.target.width, pane.target.height) == (width, height)
        {
            return;
        }
        let target = live.session.view_target(width.max(1), height.max(1));
        let filter = wgpu::FilterMode::Linear;
        let mut renderer = state.renderer.write();
        let texture = match slot {
            Some(pane) => {
                renderer.update_egui_texture_from_wgpu_texture(
                    &state.device,
                    &target.view,
                    filter,
                    pane.texture,
                );
                pane.texture
            }
            None => renderer.register_native_texture(&state.device, &target.view, filter),
        };
        *slot = Some(Self {
            target,
            texture,
            rendered: None,
            complete: false,
        });
    }

    /// Renders unless nothing changed. Returns whether it touched the GPU.
    fn render(&mut self, live: &mut LivePhoto, request: &PaneRequest) -> Result<bool, EngineError> {
        let key = RenderKey {
            serial: live.serial,
            request: request.clone(),
        };
        if self.complete && self.rendered.as_ref() == Some(&key) {
            return Ok(false);
        }
        let view = ViewRequest {
            region: request.region,
            show_clipping: request.clipping,
            mask_overlay: request.mask_overlay,
            proof: request.proof.clone(),
        };
        self.complete =
            live.session
                .render_progressive(&request.recipe, &view, &self.target, TILES_PER_FRAME)?;
        self.rendered = Some(key);
        Ok(true)
    }
}

/// Bins for red, green and blue, with what the display needs worked out once.
#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub bins: Vec<u32>,
    pub total: u64,
}

impl Histogram {
    pub fn new(bins: Vec<u32>) -> Self {
        let total = bins.iter().take(256).map(|bin| u64::from(*bin)).sum();
        Self { bins, total }
    }

    pub fn channel(&self, channel: usize) -> &[u32] {
        &self.bins[channel * 256..(channel + 1) * 256]
    }

    fn fraction_at(&self, bin: usize) -> [f32; 3] {
        [0, 1, 2].map(|channel| self.channel(channel)[bin] as f32 / self.total.max(1) as f32)
    }

    pub fn shadow_clipping(&self) -> [f32; 3] {
        self.fraction_at(0)
    }

    pub fn highlight_clipping(&self) -> [f32; 3] {
        self.fraction_at(255)
    }
}

#[derive(Default)]
pub struct ViewRenderer {
    after: Option<Pane>,
    before: Option<Pane>,
    histogram_target: Option<ViewTarget>,
    histogram_key: Option<(u64, Recipe, Option<Proof>)>,
    histogram: Option<Histogram>,
    histogram_time: f64,
    renders: u64,
}

/// What a render left for later.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderOutcome {
    /// A zoomed view is still sharpening, so the next frame has work to do.
    pub refining: bool,
    /// Seconds until the histogram should catch up.
    pub histogram_due_in: Option<f64>,
}

impl ViewRenderer {
    /// Renders the panes if needed, and says what's still outstanding.
    pub fn update(
        &mut self,
        state: &RenderState,
        live: &mut LivePhoto,
        after: &PaneRequest,
        before: Option<&PaneRequest>,
        now: f64,
    ) -> Result<RenderOutcome, EngineError> {
        Pane::ensure(&mut self.after, state, live, after.size);
        let mut refining = false;
        if let Some(pane) = &mut self.after {
            self.renders += u64::from(pane.render(live, after)?);
            refining |= !pane.complete;
        }
        if let Some(request) = before {
            Pane::ensure(&mut self.before, state, live, request.size);
            if let Some(pane) = &mut self.before {
                self.renders += u64::from(pane.render(live, request)?);
                refining |= !pane.complete;
            }
        }
        let histogram_due_in = self.update_histogram(live, after, now)?;
        Ok(RenderOutcome {
            refining,
            histogram_due_in,
        })
    }

    /// The histogram always measures the whole photo, whatever the viewport is zoomed to,
    /// so it renders from a small target of its own. It shows the proof when there is one,
    /// which is how a photo's distribution looks in the target space, but never the warning
    /// colour, which isn't part of the photo.
    fn update_histogram(
        &mut self,
        live: &mut LivePhoto,
        request: &PaneRequest,
        now: f64,
    ) -> Result<Option<f64>, EngineError> {
        let recipe = &request.recipe;
        let proof = request.proof.clone().map(|proof| Proof { gamut_warning: false, ..proof });
        let key = (live.serial, recipe.clone(), proof.clone());
        if self.histogram_key.as_ref() == Some(&key) {
            return Ok(None);
        }
        let wait = self.histogram_time + HISTOGRAM_INTERVAL - now;
        if wait > 0.0 {
            return Ok(Some(wait));
        }
        let [width, height] = live.session.output_size(recipe).map(|side| side as f32);
        let scale = HISTOGRAM_EDGE / width.max(height).max(1.0);
        let size = [((width * scale) as u32).max(1), ((height * scale) as u32).max(1)];
        let target = match self.histogram_target.take() {
            Some(target) if [target.width, target.height] == size => target,
            _ => live.session.view_target(size[0], size[1]),
        };
        let view = ViewRequest {
            proof,
            ..ViewRequest::default()
        };
        live.session.render(recipe, &view, &target, None)?;
        self.histogram = Some(Histogram::new(live.session.histogram(&target)?));
        self.histogram_target = Some(target);
        self.histogram_key = Some(key);
        self.histogram_time = now;
        Ok(None)
    }

    /// Renders everything again, for when something outside the recipe has changed the photo.
    pub fn invalidate(&mut self) {
        for pane in [&mut self.after, &mut self.before].into_iter().flatten() {
            pane.complete = false;
        }
        self.histogram_key = None;
    }

    /// How many times a view has gone to the GPU.
    pub fn renders(&self) -> u64 {
        self.renders
    }

    pub fn after_texture(&self) -> Option<egui::TextureId> {
        self.after.as_ref().map(|pane| pane.texture)
    }

    pub fn before_texture(&self) -> Option<egui::TextureId> {
        self.before.as_ref().map(|pane| pane.texture)
    }

    pub fn histogram(&self) -> Option<&Histogram> {
        self.histogram.as_ref()
    }

    /// Forgets the histogram for a new photo. The new one waits a beat, so the switch itself
    /// is as quick as it can be.
    pub fn clear_histogram(&mut self, now: f64) {
        self.histogram = None;
        self.histogram_key = None;
        self.histogram_time = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_is_reported_per_channel() {
        let mut bins = vec![0u32; 768];
        bins[100] = 90;
        bins[0] = 10;
        bins[256 + 255] = 45;
        let histogram = Histogram::new(bins);
        assert_eq!(histogram.total, 100);
        assert_eq!(histogram.shadow_clipping()[0], 0.1);
        assert_eq!(histogram.highlight_clipping()[1], 0.45);
        assert_eq!(histogram.highlight_clipping()[2], 0.0);
    }
}
