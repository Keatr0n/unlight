//! One open photo. Tracks which derived data is current, so a slider drag only redoes the
//! work its setting can actually affect.

use crate::{
    auto_tone::{self, AutoTone, ToneModel, ToneStats},
    curves,
    denoise::{self, DenoiseJob, NoiseFilter},
    error::Result,
    full_image::{FullImage, REGION_TILE},
    geometry::Geometry,
    guides::Guides,
    heal_search::{Claimed, Patchwork, SEARCH_RADII},
    local::{self, LocalNoise},
    masks::{MaskFields, MaskRasterSource, MaskScene},
    optics::OpticsState,
    params::{DevelopParams, FinishParams, MAX_TILE_EDGE, TILE_PAD, TileParams, cfa_params},
    pipelines::Engine,
    proof::{FinishViews, Proof, ProofTable},
    rendering::{self, ProfileTextures},
    repair::Repairs,
    scheduler::{SchedulerConfig, Tile},
    source::{Preview, SourceImage},
    timing::GpuProfiler,
    tools::{self, ImageStats},
};
use unlight_color::{PhotoColor, ProfileChoices, ProfileLibrary, matrix::apply};
use unlight_raw::RawImage;
use unlight_lens::{LensError, LensProfile};
use unlight_recipe::{Crop, NoiseReduction, Recipe, ToneCurve};
use wgpu::util::DeviceExt;

/// Extra source to prepare around a view, in uv, since red and blue can land outside it when
/// chromatic aberration is corrected.
const CHANNEL_SPREAD: f32 = 0.002;

/// Pyramid level a patch search reads, chosen to leave a spot about this many pixels in radius.
const SEARCH_SPOT_RADIUS: f32 = 6.0;
/// How far from a spot to look when no good patch turns up, in radii.
const FALLBACK_RADII: f32 = 3.0;
/// Pyramid pixels averaged either side of a picked colour.
const PICK_REACH: u32 = 2;

/// What part of the photo to show. `region` is x, y, width, height in output coordinates,
/// where 0..1 spans the cropped photo.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewRequest {
    pub region: [f32; 4],
    pub show_clipping: bool,
    /// Index of a mask to show in red over the photo.
    pub mask_overlay: Option<usize>,
    /// Shows the photo as it would look in another colour space. Exports never see this.
    pub proof: Option<Proof>,
}

impl Default for ViewRequest {
    fn default() -> Self {
        Self { region: [0.0, 0.0, 1.0, 1.0], show_clipping: false, mask_overlay: None, proof: None }
    }
}

/// An 8-bit surface the viewport renders into. The app displays this texture directly.
pub struct ViewTarget {
    pub width: u32,
    pub height: u32,
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    developed: wgpu::TextureView,
    blurred_rows: wgpu::TextureView,
    local_results: wgpu::TextureView,
}

/// The images one develop pass reads: the photo, and what local noise reduction blends it with.
pub(crate) struct SceneViews<'a> {
    pub image: &'a wgpu::TextureView,
    pub cleaner: &'a wgpu::TextureView,
    pub rawer: &'a wgpu::TextureView,
}

/// How much of a view's region is at full resolution yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Region {
    /// Some of it has never been made, so the view has to come from the preview.
    Missing,
    /// Showable, though a repair has moved under some of it and that part is on its way.
    Stale,
    Ready,
}

pub struct Session {
    pub engine: Engine,
    /// Shows the sharpening mask in place of the photo, as holding Alt on Masking does.
    pub show_sharpen_mask: bool,
    pub source: SourceImage,
    pub preview: Preview,
    pub stats: ImageStats,
    pub(crate) optics: OpticsState,
    guides: Guides,
    guide_key: Option<(unlight_color::Mat3, u64)>,
    noise: NoiseFilter,
    /// The stronger pyramid masks blend towards, made once a mask asks for it.
    cleaner: Option<NoiseFilter>,
    repairs: Repairs,
    masks: MaskFields,
    lut_view: wgpu::TextureView,
    lut_texture: wgpu::Texture,
    lut_key: ToneCurve,
    photo_color: PhotoColor,
    profile_key: (Option<String>, u64),
    profile_textures: ProfileTextures,
    full: Option<FullImage>,
    /// The file's exposure settings, for telling a night scene from a dark mistake.
    exposure_value: Option<f32>,
    proof_table: ProofTable,
}

/// Points around a view region. A warp bends its edges, so the corners alone can undershoot
/// the area of the source it touches.
fn region_outline(region: [f32; 4]) -> [[f32; 2]; 12] {
    let [x, y, width, height] = region;
    let along = [0.0, 0.5, 1.0];
    std::array::from_fn(|index| {
        let (side, step) = (index / 3, along[index % 3]);
        match side {
            0 => [x + width * step, y],
            1 => [x + width * step, y + height],
            2 => [x, y + height * step],
            _ => [x + width, y + height * step],
        }
    })
}

fn denoise_whole(
    engine: &Engine,
    job: &DenoiseJob<'_>,
    demosaiced: &wgpu::TextureView,
    schedule: SchedulerConfig,
) -> Result<wgpu::TextureView> {
    let (width, height) = (job.source.width, job.source.height);
    let output = engine
        .storage_texture("denoised image", [width, height, 1], 1, engine.precision.format())
        .create_view(&Default::default());
    let region = Tile { x: 0, y: 0, width, height };
    engine.denoise_region(job, demosaiced, &output, region, schedule)?;
    Ok(output)
}

fn scene_extent(view_size: [u32; 2], region: [f32; 4]) -> [f32; 2] {
    [region[2] / view_size[0].max(1) as f32, region[3] / view_size[1].max(1) as f32]
}

impl Session {
    pub fn open(engine: Engine, raw: &RawImage, recipe: &Recipe) -> Result<Self> {
        let schedule = SchedulerConfig::BACKGROUND;
        let mut source = engine.load(raw, schedule)?;
        let photo_color = PhotoColor::new(raw);
        let library = ProfileLibrary::installed();
        source.profile = photo_color.profile(recipe.camera_profile.as_deref(), &library);
        let preview = engine.build_preview(&source, schedule)?;
        let guides = engine.create_guides(raw.width, raw.height);
        let camera_to_working = tools::camera_to_working(&source, recipe);
        engine.update_guides(&guides, &preview, &camera_to_working)?;
        let stats = engine.measure_stats(&guides)?;
        // Range masks read the photo at its own white balance, so that a global slider can
        // never move a selection and rebuild the fields.
        let scene_matrix = tools::camera_to_working(&source, &Recipe::default());
        let masks = MaskFields::new(&engine, &preview, &scene_matrix, source.baseline_exposure);

        let lut_texture = engine.ctx.device.create_texture_with_data(
            &engine.ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("tone curve"),
                size: wgpu::Extent3d {
                    width: curves::LUT_SIZE as u32,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&curves::build_lut(&recipe.tone_curve)),
        );
        let lut_view = lut_texture.create_view(&Default::default());

        Ok(Self {
            noise: engine.create_noise_filter(&preview),
            cleaner: None,
            repairs: Repairs::default(),
            masks,
            show_sharpen_mask: false,
            optics: OpticsState::new(&engine, raw),
            guide_key: Some((camera_to_working, 0)),
            lut_key: recipe.tone_curve.clone(),
            profile_textures: ProfileTextures::new(&engine, &source.profile),
            photo_color,
            profile_key: (recipe.camera_profile.clone(), library.generation),
            engine,
            source,
            preview,
            stats,
            guides,
            lut_view,
            lut_texture,
            full: None,
            exposure_value: auto_tone::exposure_value(&raw.metadata),
            proof_table: ProofTable::default(),
        })
    }

    pub fn view_target(&self, width: u32, height: u32) -> ViewTarget {
        let engine = &self.engine;
        let texture =
            engine.storage_texture("view", [width, height, 1], 1, wgpu::TextureFormat::Rgba8Unorm);
        let view = texture.create_view(&Default::default());
        let working = |label| {
            engine
                .storage_texture(label, [width, height, 1], 1, engine.precision.format())
                .create_view(&Default::default())
        };
        let (developed, blurred_rows) = (working("developed view"), working("sharpen rows"));
        let local_results = self.local_results_view([width, height]);
        ViewTarget { width, height, texture, view, developed, blurred_rows, local_results }
    }

    fn local_results_view(&self, [width, height]: [u32; 2]) -> wgpu::TextureView {
        self.engine
            .storage_texture("local results", [width, height, 1], 1, wgpu::TextureFormat::Rgba16Float)
            .create_view(&Default::default())
    }

    /// Lets `MaskShape::Raster` find its data.
    pub fn set_mask_rasters(&mut self, rasters: Option<&'static dyn MaskRasterSource>) {
        self.masks.rasters = rasters;
    }

    /// Brings guides, repairs, masks and curve data up to date with a recipe.
    pub(crate) fn sync(&mut self, recipe: &Recipe) -> Result<()> {
        self.sync_with_overlay(recipe, None)
    }

    fn take_on_repairs(&mut self, recipe: &Recipe) -> Result<()> {
        let changed = self.repairs.sync(&self.engine, &self.preview, &recipe.healing, &recipe.red_eye)?;
        if changed.is_empty() {
            return Ok(());
        }
        self.noise.invalidate();
        if let Some(cleaner) = &mut self.cleaner {
            cleaner.invalidate();
        }
        if let Some(full) = &mut self.full {
            if !self.repairs.is_active() {
                full.forget_repairs();
            }
            changed.iter().for_each(|area| full.invalidate(*area));
        }
        Ok(())
    }

    fn sync_profile(&mut self, recipe: &Recipe) {
        let library = ProfileLibrary::installed();
        let key = (recipe.camera_profile.clone(), library.generation);
        if self.profile_key != key {
            self.source.profile = self.photo_color.profile(recipe.camera_profile.as_deref(), &library);
            self.profile_textures = ProfileTextures::new(&self.engine, &self.source.profile);
            self.profile_key = key;
        }
        let neutral = tools::resolve_neutral(&self.source, recipe);
        self.profile_textures.sync(&self.engine, &self.source.profile, &neutral);
    }

    fn sync_with_overlay(&mut self, recipe: &Recipe, overlay: Option<usize>) -> Result<()> {
        self.take_on_repairs(recipe)?;
        self.sync_profile(recipe);
        let preview = self.repairs.preview().unwrap_or(&self.preview);
        let camera_to_working = tools::camera_to_working(&self.source, recipe);
        let guide_key = (camera_to_working, self.repairs.version());
        if self.guide_key != Some(guide_key) {
            self.engine.update_guides(&self.guides, preview, &camera_to_working)?;
            self.guide_key = Some(guide_key);
        }
        let scene = MaskScene { preview, version: self.repairs.version() };
        self.masks.sync(&self.engine, &recipe.masks, &scene, overlay)?;
        if self.lut_key != recipe.tone_curve {
            self.engine.ctx.queue.write_texture(
                self.lut_texture.as_image_copy(),
                bytemuck::cast_slice(&curves::build_lut(&recipe.tone_curve)),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(curves::LUT_SIZE as u32 * 16),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: curves::LUT_SIZE as u32,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            self.lut_key = recipe.tone_curve.clone();
        }
        self.optics.sync(&self.engine, &self.preview, recipe)
    }

    /// What Auto would look at for a recipe's white balance, profile and framing.
    pub fn tone_stats(&mut self, recipe: &Recipe) -> Result<ToneStats> {
        self.sync(recipe)?;
        let guide = self.engine.read_guide(&self.guides)?;
        let aspect = self.source.width as f32 / self.source.height as f32;
        let geometry = Geometry::new(&recipe.crop, &recipe.transform, &recipe.lens, aspect, self.optics.current());
        let [width, height] = self.output_size(recipe);
        let model = ToneModel {
            exposure_offset: self.source.baseline_exposure + rendering::exposure_offset(&self.source.profile),
            white_ev: self.stats.white_ev,
            black_ev: self.stats.black_ev,
            curve: rendering::base_curve(&self.source.profile),
        };
        let shape = width as f32 / height as f32;
        Ok(auto_tone::measure(&guide, &geometry, aspect, shape, self.exposure_value, model))
    }

    /// The Basic sliders Auto picks for this photo under a recipe.
    pub fn auto_tone(&mut self, recipe: &Recipe) -> Result<AutoTone> {
        Ok(auto_tone::auto_tone(&self.tone_stats(recipe)?))
    }

    /// The file's part of the geometry for the last recipe rendered, for `Geometry::new`.
    pub fn optics(&self) -> &crate::geometry::Optics {
        self.optics.current()
    }

    /// The camera profiles this photo can be given.
    pub fn profile_choices(&self) -> ProfileChoices {
        self.photo_color.choices(&ProfileLibrary::installed())
    }

    /// The lens profile in effect for the last recipe rendered, if any.
    pub fn lens_profile(&self) -> Option<&LensProfile> {
        self.optics.current().profile.as_deref()
    }

    /// Why a requested lens profile couldn't be applied, so the UI can say so.
    pub fn lens_error(&self) -> Option<&LensError> {
        self.optics.lens_error()
    }

    /// The largest crop of a given shape with no empty corners, for Constrain Crop. `shape` is
    /// width over height.
    pub fn largest_crop(&mut self, recipe: &Recipe, shape: f32) -> Result<Crop> {
        self.sync(recipe)?;
        let aspect = self.source.width as f32 / self.source.height as f32;
        let geometry = Geometry::new(&Crop::default(), &recipe.transform, &recipe.lens, aspect, self.optics.current());
        Ok(geometry.largest_crop(aspect, shape))
    }

    pub(crate) fn develop_group(
        &self,
        scene: &SceneViews<'_>,
        target: &wgpu::TextureView,
        local_results: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        let engine = &self.engine;
        let view = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        engine.bind(
            &engine.pipelines.develop,
            &[
                wgpu::BindGroupEntry { binding: 0, resource: engine.tile_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: engine.develop_buffer.as_entire_binding() },
                view(2, scene.image),
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&engine.sampler) },
                view(4, &self.guides.base),
                view(5, &self.guides.detail),
                view(6, &self.lut_view),
                view(7, target),
                view(8, &self.optics.curve_view),
                view(9, &self.optics.plane_gain_view),
                view(10, &self.guides.fine),
                view(16, &self.profile_textures.base_curve),
                view(17, self.profile_textures.hue_sat_view()),
                view(18, self.profile_textures.look_view()),
                wgpu::BindGroupEntry { binding: 19, resource: wgpu::BindingResource::Sampler(&engine.table_sampler) },
                view(20, self.masks.field_view()),
                view(21, scene.cleaner),
                view(22, scene.rawer),
                view(23, local_results),
            ],
        )
    }

    pub(crate) fn sharpen_group(
        &self,
        developed: &wgpu::TextureView,
        blurred_rows: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        let engine = &self.engine;
        engine.bind(
            &engine.pipelines.sharpen_rows,
            &[
                wgpu::BindGroupEntry { binding: 0, resource: engine.tile_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: engine.finish_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(developed) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(blurred_rows) },
            ],
        )
    }

    pub(crate) fn finish_group(
        &self,
        pipeline: &wgpu::ComputePipeline,
        developed: &wgpu::TextureView,
        blurred_rows: &wgpu::TextureView,
        local_results: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        let engine = &self.engine;
        engine.bind(
            pipeline,
            &[
                wgpu::BindGroupEntry { binding: 0, resource: engine.tile_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: engine.finish_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(developed) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(blurred_rows) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(target) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(local_results) },
            ],
        )
    }

    /// Turns on what the shaders only do for masks, for a recipe already synced.
    pub(crate) fn apply_masks(
        &self,
        recipe: &Recipe,
        develop: &mut DevelopParams,
        finish: &mut FinishParams,
        pixel_scale: f32,
    ) {
        let active = self.masks.is_active();
        develop.local_flags = u32::from(active);
        let sharpens = active && local::local_sharpening(&recipe.masks) && tools::sharpens_at(recipe, pixel_scale);
        finish.local_detail = f32::from(u8::from(sharpens));
    }

    pub(crate) fn tile_params(&self, tile: Tile, pad: u32) -> TileParams {
        TileParams {
            origin: [tile.x as i32, tile.y as i32],
            size: [tile.width as i32, tile.height as i32],
            image: [self.source.width as i32, self.source.height as i32],
            pad: pad as i32,
            period: self.source.cfa.period as i32,
            cfa: cfa_params(&self.source.cfa),
        }
    }

    pub(crate) fn write_develop(&self, develop: &DevelopParams, finish: &FinishParams) {
        let queue = &self.engine.ctx.queue;
        queue.write_buffer(&self.engine.develop_buffer, 0, bytemuck::bytes_of(develop));
        queue.write_buffer(&self.engine.finish_buffer, 0, bytemuck::bytes_of(finish));
    }

    /// The noise settings of the stronger image, if this recipe's masks ask for one.
    fn cleaner_settings(&self, recipe: &Recipe, wanted: LocalNoise) -> Option<NoiseReduction> {
        let settings = local::cleaner_settings(&recipe.detail.noise_reduction);
        (wanted.cleaner && denoise::is_active(&settings, &self.source.noise)).then_some(settings)
    }

    fn wanted_noise(&self, recipe: &Recipe) -> LocalNoise {
        if self.masks.is_active() { local::local_noise(&recipe.masks) } else { LocalNoise::default() }
    }

    /// Readies the full-resolution tiles a region of the source touches, spending at most
    /// `budget` on slow ones. A repair only has to patch cells that are already demosaiced,
    /// and that's quick enough not to count.
    fn ensure_region(
        &mut self,
        settings: &NoiseReduction,
        cleaner: Option<NoiseReduction>,
        (uv_min, uv_max): ([f32; 2], [f32; 2]),
        mut budget: usize,
    ) -> Result<Region> {
        let size = [self.source.width, self.source.height];
        let full = self.full.get_or_insert_with(|| FullImage::new(&self.engine, size));
        let preview = self.repairs.preview().unwrap_or(&self.preview);
        let denoising = denoise::is_active(settings, &self.source.noise);
        if denoising {
            self.engine.solve_noise(&mut self.noise, &self.source, preview, settings, [0, 0])?;
            full.denoised.settle(settings);
        }
        if let Some(stronger) = &cleaner {
            let filter = match &mut self.cleaner {
                Some(filter) => filter,
                slot => slot.insert(self.engine.create_noise_filter(&self.preview)),
            };
            self.engine.solve_noise(filter, &self.source, preview, stronger, [0, 0])?;
            full.cleaner_mut(&self.engine).settle(stronger);
        }
        let repairing = self.repairs.is_active();
        let reach = u32::from(denoising || cleaner.is_some());
        let schedule = SchedulerConfig { min_edge: REGION_TILE, initial_edge: REGION_TILE, ..SchedulerConfig::INTERACTIVE };
        let mut region = Region::Ready;
        for row in full.row_at(uv_min[1])..=full.row_at(uv_max[1]) {
            for column in full.column_at(uv_min[0])..=full.column_at(uv_max[0]) {
                let index = full.cell_index(column, row);
                let (stale_clean, unmade_clean) = [denoising.then_some(&full.denoised), full.cleaner.as_ref().filter(|_| cleaner.is_some())]
                    .into_iter()
                    .flatten()
                    .fold((false, false), |(stale, unmade), image| {
                        (stale || !image.ready[index], unmade || !image.filled[index])
                    });
                let unrepaired = repairing && !full.is_patched(index);
                let undemosaiced = !full.is_demosaiced(index);
                if !(stale_clean || unrepaired || undemosaiced) {
                    continue;
                }
                if stale_clean || undemosaiced {
                    if budget == 0 {
                        if unmade_clean || undemosaiced {
                            return Ok(Region::Missing);
                        }
                        region = Region::Stale;
                        continue;
                    }
                    budget -= 1;
                }
                // Denoising reads past the cell's edge, so it needs the neighbours too.
                for near_row in row.saturating_sub(reach)..=(row + reach).min(full.rows - 1) {
                    for near_column in column.saturating_sub(reach)..=(column + reach).min(full.columns - 1) {
                        full.prepare_cell(&self.engine, &self.source, &self.repairs, near_column, near_row)?;
                    }
                }
                let tile = full.cell_tile(column, row);
                if denoising && !full.denoised.ready[index] {
                    let job = DenoiseJob { filter: &self.noise, source: &self.source, preview, settings };
                    self.engine.denoise_region(&job, full.scene(repairing), &full.denoised.view, tile, schedule)?;
                    full.denoised.mark(index);
                }
                if let (Some(stronger), Some(filter)) = (&cleaner, &self.cleaner) {
                    let job = DenoiseJob { filter, source: &self.source, preview, settings: stronger };
                    if let Some(image) = full.cleaner.as_ref().filter(|image| !image.ready[index]) {
                        self.engine.denoise_region(&job, full.scene(repairing), &image.view, tile, schedule)?;
                    }
                    if let Some(image) = &mut full.cleaner {
                        image.mark(index);
                    }
                }
            }
        }
        Ok(region)
    }

    /// Renders a view of the photo. This is what a slider drag runs each frame.
    pub fn render(
        &mut self,
        recipe: &Recipe,
        request: &ViewRequest,
        target: &ViewTarget,
        profiler: Option<&mut GpuProfiler>,
    ) -> Result<()> {
        self.render_budgeted(recipe, request, target, profiler, usize::MAX).map(|_| ())
    }

    /// Like `render`, but demosaics at most `tile_budget` full-resolution tiles, so zooming
    /// never stalls the caller. The view comes from the preview until its region is ready.
    /// Returns whether this render was final.
    pub fn render_progressive(
        &mut self,
        recipe: &Recipe,
        request: &ViewRequest,
        target: &ViewTarget,
        tile_budget: usize,
    ) -> Result<bool> {
        self.render_budgeted(recipe, request, target, None, tile_budget)
    }

    fn render_budgeted(
        &mut self,
        recipe: &Recipe,
        request: &ViewRequest,
        target: &ViewTarget,
        mut profiler: Option<&mut GpuProfiler>,
        tile_budget: usize,
    ) -> Result<bool> {
        let overlay = request.mask_overlay.filter(|index| *index < recipe.masks.len());
        self.sync_with_overlay(recipe, overlay)?;
        let aspect = self.source.width as f32 / self.source.height as f32;
        let mut develop = tools::develop_params(&self.source, recipe, &self.stats, &self.optics);

        let [x, y, region_width, _] = request.region;
        let step = scene_extent([target.width, target.height], request.region);
        let source_per_output = region_width * develop.crop_size[0] * self.source.width as f32
            / target.width.max(1) as f32;
        let preview_per_output = source_per_output / (self.source.width as f32 / self.preview.width as f32);
        let needs_full = preview_per_output < 1.0;

        develop.view_origin = [x, y];
        develop.view_step = step;
        develop.lod = if needs_full {
            0.0
        } else {
            preview_per_output.max(1.0).log2().min(self.preview.levels as f32 - 1.0)
        };

        let mut finish = tools::finish_params(recipe, aspect, source_per_output);
        finish.view_origin = [x, y];
        finish.view_step = step;
        finish.show_clipping = f32::from(u8::from(request.show_clipping));
        finish.show_sharpen_mask = f32::from(u8::from(self.show_sharpen_mask));
        self.apply_masks(recipe, &mut develop, &mut finish, source_per_output);
        finish.mask_overlay = f32::from(u8::from(overlay.is_some()));

        let settings = recipe.detail.noise_reduction;
        let denoising = denoise::is_active(&settings, &self.source.noise);
        let cleaner = self.cleaner_settings(recipe, self.wanted_noise(recipe));
        let preview = self.repairs.preview().unwrap_or(&self.preview);
        if !needs_full {
            let levels = [develop.lod.round() as u32, develop.lod.floor() as u32];
            if denoising {
                self.engine.solve_noise(&mut self.noise, &self.source, preview, &settings, levels)?;
            }
            if let Some(stronger) = &cleaner {
                let filter = match &mut self.cleaner {
                    Some(filter) => filter,
                    slot => slot.insert(self.engine.create_noise_filter(&self.preview)),
                };
                self.engine.solve_noise(filter, &self.source, preview, stronger, levels)?;
            }
        }
        self.write_develop(&develop, &finish);

        let mut region = Region::Missing;
        if needs_full {
            let geometry = Geometry::new(&recipe.crop, &recipe.transform, &recipe.lens, aspect, self.optics.current());
            let corners = region_outline(request.region).map(|point| geometry.source_uv(point, aspect));
            let bound = |axis: usize, pick: fn(f32, f32) -> f32| {
                corners.iter().map(|corner| corner[axis]).reduce(pick).unwrap_or(0.0)
            };
            let uv_min = [bound(0, f32::min) - CHANNEL_SPREAD, bound(1, f32::min) - CHANNEL_SPREAD];
            let uv_max = [bound(0, f32::max) + CHANNEL_SPREAD, bound(1, f32::max) + CHANNEL_SPREAD];
            region = self.ensure_region(&settings, cleaner, (uv_min, uv_max), tile_budget)?;
        }
        let shows_full = region != Region::Missing;

        let preview = self.repairs.preview().unwrap_or(&self.preview);
        let repairing = self.repairs.is_active();
        let scene = match &self.full {
            Some(full) if shows_full => {
                let rawer = full.scene(repairing);
                let image = if denoising { &full.denoised.view } else { rawer };
                let stronger = full.cleaner.as_ref().filter(|_| cleaner.is_some());
                SceneViews { image, cleaner: stronger.map_or(image, |clean| &clean.view), rawer }
            }
            _ => {
                let rawer = &preview.view;
                let image = if denoising { &self.noise.view } else { rawer };
                let stronger = self.cleaner.as_ref().filter(|_| cleaner.is_some());
                SceneViews { image, cleaner: stronger.map_or(image, |filter| &filter.view), rawer }
            }
        };
        let develop_group = self.develop_group(&scene, &target.developed, &target.local_results);
        let sharpen_group = self.sharpen_group(&target.developed, &target.blurred_rows);
        let (finish_pipeline, finish_group) = match &request.proof {
            None => (
                &self.engine.pipelines.finish_display,
                self.finish_group(
                    &self.engine.pipelines.finish_display,
                    &target.developed,
                    &target.blurred_rows,
                    &target.local_results,
                    &target.view,
                ),
            ),
            Some(proof) => {
                let views = FinishViews {
                    developed: &target.developed,
                    blurred_rows: &target.blurred_rows,
                    local_results: &target.local_results,
                    output: &target.view,
                };
                (self.engine.proof_finish(), self.engine.proof_group(proof, &mut self.proof_table, &views))
            }
        };

        let tile = Tile { x: 0, y: 0, width: target.width, height: target.height };
        self.engine.write_tile(&self.tile_params(tile, 0));
        let mut encoder = self.engine.encoder();
        let size = [target.width, target.height];
        let pipelines = &self.engine.pipelines;
        self.engine.dispatch(&mut encoder, &pipelines.develop, &develop_group, size, "develop", profiler.as_deref_mut());
        if finish.sharpen_amount > 0.0 || finish.local_detail > 0.0 {
            self.engine.dispatch(&mut encoder, &pipelines.sharpen_rows, &sharpen_group, size, "sharpen rows", profiler.as_deref_mut());
        }
        self.engine.dispatch(&mut encoder, finish_pipeline, &finish_group, size, "finish", profiler.as_deref_mut());
        if let Some(profiler) = profiler.as_deref() {
            profiler.resolve(&mut encoder);
        }
        self.engine.submit_and_wait(encoder)?;
        Ok(region == Region::Ready || !needs_full)
    }

    /// Denoises a whole demosaiced image. Returns `None` if there's nothing to do for this
    /// recipe and photo, in which case the input is already the result.
    pub(crate) fn denoise_image(
        &mut self,
        settings: &NoiseReduction,
        demosaiced: &wgpu::TextureView,
        schedule: SchedulerConfig,
    ) -> Result<Option<wgpu::TextureView>> {
        if !denoise::is_active(settings, &self.source.noise) {
            return Ok(None);
        }
        let preview = self.repairs.preview().unwrap_or(&self.preview);
        self.engine.solve_noise(&mut self.noise, &self.source, preview, settings, [0, 0])?;
        let job = DenoiseJob { filter: &self.noise, source: &self.source, preview, settings };
        denoise_whole(&self.engine, &job, demosaiced, schedule).map(Some)
    }

    /// The stronger image masks blend towards, if this recipe's masks ask for one.
    pub(crate) fn cleaner_image(
        &mut self,
        recipe: &Recipe,
        demosaiced: &wgpu::TextureView,
        schedule: SchedulerConfig,
    ) -> Result<Option<wgpu::TextureView>> {
        let Some(settings) = self.cleaner_settings(recipe, self.wanted_noise(recipe)) else {
            return Ok(None);
        };
        let preview = self.repairs.preview().unwrap_or(&self.preview);
        let filter = match &mut self.cleaner {
            Some(filter) => filter,
            slot => slot.insert(self.engine.create_noise_filter(&self.preview)),
        };
        self.engine.solve_noise(filter, &self.source, preview, &settings, [0, 0])?;
        let job = DenoiseJob { filter, source: &self.source, preview, settings: &settings };
        denoise_whole(&self.engine, &job, demosaiced, schedule).map(Some)
    }

    /// A demosaiced image with this recipe's repairs applied, or `None` if it has none.
    pub(crate) fn repaired_image(
        &self,
        demosaiced: &wgpu::TextureView,
        schedule: SchedulerConfig,
    ) -> Result<Option<wgpu::TextureView>> {
        if !self.repairs.is_active() {
            return Ok(None);
        }
        let size = [self.source.width, self.source.height];
        self.repairs.repair_image(&self.engine, demosaiced, size, schedule).map(Some)
    }

    /// Finds somewhere to heal a spot from, for when only its position is known. `target` is a
    /// normalised source position and `radius` a share of the long edge, as in `HealSpot`.
    pub fn find_heal_source(&mut self, recipe: &Recipe, target: [f32; 2], radius: f32) -> Result<[f32; 2]> {
        self.sync(recipe)?;
        let preview = self.repairs.preview().unwrap_or(&self.preview);
        let long_edge = preview.width.max(preview.height) as f32;
        let spot_radius = radius * long_edge;
        let level = ((spot_radius / SEARCH_SPOT_RADIUS).max(1.0).log2().floor() as u32).min(preview.levels - 1);
        let scale = (1u32 << level) as f32;
        let size = [(preview.width >> level).max(1), (preview.height >> level).max(1)];
        let level_radius = spot_radius / scale;
        let center = [target[0] * size[0] as f32, target[1] * size[1] as f32];
        let reach = level_radius * (SEARCH_RADII + 2.0);
        let low = |axis: usize| (center[axis] - reach).max(0.0).min(size[axis] as f32 - 1.0) as u32;
        let high = |axis: usize| ((center[axis] + reach).ceil() as u32).clamp(low(axis) + 1, size[axis]);
        let rect = [low(0), low(1), high(0) - low(0), high(1) - low(1)];
        let pixels = self.engine.read_preview_region(preview, level, rect)?;
        let local = |point: [f32; 2]| {
            [point[0] * size[0] as f32 - rect[0] as f32, point[1] * size[1] as f32 - rect[1] as f32]
        };
        let claimed: Vec<Claimed> = self
            .repairs
            .heal_targets()
            .map(|spot| Claimed { center: local(spot.target), radius: spot.radius * long_edge / scale })
            .collect();
        let patchwork = Patchwork::new(&pixels, rect[2] as usize, rect[3] as usize);
        let found = patchwork.best_source(local(target), level_radius, &claimed);
        let fallback = |axis: usize| {
            let step = FALLBACK_RADII * spot_radius / [preview.width, preview.height][axis] as f32;
            if target[axis] < 0.5 { target[axis] + step } else { target[axis] - step }
        };
        Ok(found.map_or([fallback(0), target[1]], |patch| {
            [(patch[0] + rect[0] as f32) / size[0] as f32, (patch[1] + rect[1] as f32) / size[1] as f32]
        }))
    }

    /// The colour a range mask sees at a normalised source position, for `ColorRange::samples`.
    pub fn scene_color(&mut self, recipe: &Recipe, uv: [f32; 2]) -> Result<[f32; 3]> {
        self.sync(recipe)?;
        let preview = self.repairs.preview().unwrap_or(&self.preview);
        let side = |axis: usize, extent: u32| {
            let center = (uv[axis].clamp(0.0, 1.0) * extent as f32) as u32;
            let low = center.saturating_sub(PICK_REACH).min(extent - 1);
            (low, (center + PICK_REACH + 1).min(extent) - low)
        };
        let ((x, width), (y, height)) = (side(0, preview.width), side(1, preview.height));
        let pixels = self.engine.read_preview_region(preview, 0, [x, y, width, height])?;
        let count = pixels.len().max(1) as f32;
        let mean = pixels.iter().fold([0.0f32; 3], |sum, pixel| std::array::from_fn(|c| sum[c] + pixel[c] / count));
        let matrix = tools::camera_to_working(&self.source, &Recipe::default());
        let gain = f64::from(self.source.baseline_exposure.exp2());
        let working = apply(&matrix, &mean.map(f64::from));
        Ok(working.map(|value| (value.max(0.0) * gain) as f32))
    }

    /// How many times the mask fields have been rebuilt. Only a mask should move this.
    pub fn mask_rebuilds(&self) -> u64 {
        self.masks.rebuilds()
    }

    /// How many denoising levels have been solved. Only a noise setting should move this.
    pub fn denoise_solves(&self) -> u64 {
        self.noise.solves
    }

    /// Size of the output image for a recipe's crop.
    pub fn output_size(&self, recipe: &Recipe) -> [u32; 2] {
        let crop = &recipe.crop;
        [
            (((crop.right - crop.left) * self.source.width as f32).round() as u32).max(1),
            (((crop.bottom - crop.top) * self.source.height as f32).round() as u32).max(1),
        ]
    }

    pub(crate) fn pad() -> u32 {
        TILE_PAD
    }

    pub(crate) fn develop_tile_view(&self) -> wgpu::TextureView {
        let extent = MAX_TILE_EDGE + 2 * TILE_PAD;
        self.engine
            .storage_texture("developed tile", [extent, extent, 1], 1, self.engine.precision.format())
            .create_view(&Default::default())
    }

    pub(crate) fn local_tile_view(&self) -> wgpu::TextureView {
        let extent = MAX_TILE_EDGE + 2 * TILE_PAD;
        self.local_results_view([extent, extent])
    }

    /// Reads back a rendered view as tightly packed RGBA8.
    pub fn read_view(&self, target: &ViewTarget) -> Result<Vec<u8>> {
        self.engine.read_rgba8(&target.texture, target.width, target.height)
    }

    /// 256 bins for each of red, green and blue, in that order.
    pub fn histogram(&self, target: &ViewTarget) -> Result<Vec<u32>> {
        let engine = &self.engine;
        let device = &engine.ctx.device;
        let size = 768 * 4;
        let bins = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("histogram"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("histogram readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let group = engine.bind(
            &engine.pipelines.histogram,
            &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&target.view) },
                wgpu::BindGroupEntry { binding: 1, resource: bins.as_entire_binding() },
            ],
        );
        let mut encoder = engine.encoder();
        let dimensions = [target.width, target.height];
        engine.dispatch(&mut encoder, &engine.pipelines.histogram, &group, dimensions, "histogram", None);
        encoder.copy_buffer_to_buffer(&bins, 0, &readback, 0, size);
        engine.submit_and_wait(encoder)?;
        let bytes = engine.map_read(&readback)?;
        Ok(bytemuck::cast_slice(&bytes).to_vec())
    }
}
