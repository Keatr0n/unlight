//! The photo itself. The engine renders into a texture at exactly the size it covers on
//! screen, so the image here is never resampled by the UI.

use crate::{
    app::App,
    darkroom::LivePhoto,
    renderer::PaneRequest,
    state::{
        AppState, Backdrop, Compare, DecodeState, Tool,
        crop::CropGesture,
        view::{Orientation, ViewPlan, Zoom},
    },
    ui::{
        crop_overlay::{self, CropInput, PhotoSpace},
        guides_overlay::{self, GuideGesture, GuideInput},
        local_overlay::{self, LocalGesture, LocalInput},
        theme,
        widgets::controls::card,
    },
};
use eframe::{
    egui::{
        self, Align2, Color32, CursorIcon, Id, Key, Mesh, Rect, Response, RichText, Sense, Shape, TextureId,
        Ui, UiBuilder, epaint::Vertex, pos2, vec2,
    },
    egui_wgpu::RenderState,
};
use unlight_engine::EngineError;
use unlight_recipe::Crop;

const CANVAS_MARGIN: f32 = 12.0;
const SIDE_BY_SIDE_GAP: f32 = 4.0;
const SCROLL_ZOOM: f32 = 1.002;
const DIVIDER_GRAB: f32 = 12.0;
const CARD_WIDTH: f32 = 520.0;

/// What the viewport keeps between frames.
#[derive(Default)]
pub struct ViewportMemory {
    pub canvas: Option<[f32; 2]>,
    pub output: [u32; 2],
    pub photo_rect: Option<Rect>,
    pub picking_white_balance: bool,
    crop_gesture: Option<CropGesture>,
    guide_gesture: Option<GuideGesture>,
    local_gesture: Option<LocalGesture>,
    hovered_pane: usize,
}

/// A quad showing the part `shown` of an image, turned to display orientation. `shown` is
/// in 0..1 of the image as displayed.
fn image_mesh(texture: TextureId, rect: Rect, shown: Rect, orientation: Orientation) -> Mesh {
    let mut mesh = Mesh::with_texture(texture);
    let corners = [
        (rect.left_top(), [shown.left(), shown.top()]),
        (rect.right_top(), [shown.right(), shown.top()]),
        (rect.right_bottom(), [shown.right(), shown.bottom()]),
        (rect.left_bottom(), [shown.left(), shown.bottom()]),
    ];
    for (pos, point) in corners {
        let [u, v] = orientation.to_sensor(point);
        mesh.vertices.push(Vertex {
            pos,
            uv: pos2(u, v),
            color: Color32::WHITE,
        });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    mesh
}

fn unit_rect() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
}

fn snap(value: f32, pixels_per_point: f32) -> f32 {
    (value * pixels_per_point).round() / pixels_per_point
}

/// Where a plan's image lands on screen, aligned to device pixels.
fn image_rect(pane: Rect, plan: &ViewPlan, pixels_per_point: f32) -> Rect {
    let min = pos2(
        snap(pane.left() + plan.offset[0] / pixels_per_point, pixels_per_point),
        snap(pane.top() + plan.offset[1] / pixels_per_point, pixels_per_point),
    );
    let size = vec2(plan.size[0] as f32, plan.size[1] as f32) / pixels_per_point;
    Rect::from_min_size(min, size)
}

/// Shows a preview fitted to an area. Previews come from the catalog already upright.
fn paint_preview(ui: &Ui, area: Rect, preview: &egui::TextureHandle) -> Rect {
    let [width, height] = preview.size().map(|side| side as f32);
    let scale = (area.width() / width.max(1.0)).min(area.height() / height.max(1.0));
    let rect = Rect::from_center_size(area.center(), vec2(width, height) * scale);
    let mesh = image_mesh(preview.id(), rect, unit_rect(), Orientation::Normal);
    ui.painter().add(Shape::mesh(mesh));
    rect
}

fn panes(canvas: Rect, compare: Compare) -> ([Rect; 2], usize) {
    if compare == Compare::SideBySide {
        let width = (canvas.width() - SIDE_BY_SIDE_GAP) * 0.5;
        let left = Rect::from_min_size(canvas.min, vec2(width, canvas.height()));
        let right = Rect::from_min_size(
            left.right_top() + vec2(SIDE_BY_SIDE_GAP, 0.0),
            vec2(width, canvas.height()),
        );
        ([left, right], 2)
    } else {
        ([canvas, canvas], 1)
    }
}

fn pane_tag(ui: &Ui, image: Rect, text: &str) {
    let painter = ui.painter_at(image);
    let galley = painter.layout_no_wrap(text.to_owned(), theme::small(), theme::TEXT);
    let box_rect = Rect::from_min_size(image.min + vec2(8.0, 8.0), galley.size() + vec2(12.0, 6.0));
    painter.rect_filled(box_rect, 3.0, Color32::from_black_alpha(140));
    painter.galley(box_rect.min + vec2(6.0, 3.0), galley, theme::TEXT);
}

fn proof_label(state: &AppState) -> String {
    let name = state.proof.chosen_name();
    if state.proof.is_baking() {
        format!("Preparing proof: {name}")
    } else {
        format!("Proof: {name}")
    }
}

#[derive(Clone, Copy)]
struct LiveFrame {
    plan: ViewPlan,
    image: Rect,
    panes: ([Rect; 2], usize),
    before: Option<TextureId>,
    after: TextureId,
    orientation: Orientation,
}

/// Paints the rendered panes for the current compare mode.
fn paint_live(ui: &mut Ui, frame: &LiveFrame, split: &mut f32) {
    let LiveFrame {
        plan,
        image,
        panes: (rects, count),
        before,
        after,
        orientation,
    } = frame;
    let pixels_per_point = ui.ctx().pixels_per_point();
    let painter = ui.painter();
    match (*count, *before) {
        (2, Some(before)) => {
            let first = image_rect(rects[0], plan, pixels_per_point);
            let second = image_rect(rects[1], plan, pixels_per_point);
            painter.add(Shape::mesh(image_mesh(before, first, unit_rect(), *orientation)));
            painter.add(Shape::mesh(image_mesh(*after, second, unit_rect(), *orientation)));
            pane_tag(ui, first, "Before");
            pane_tag(ui, second, "After");
        }
        (_, Some(before)) => {
            let x = image.left() + *split * image.width();
            let cut = |rect: Rect, from: f32, to: f32| {
                Rect::from_min_max(
                    pos2(rect.left() + from * rect.width(), rect.top()),
                    pos2(rect.left() + to * rect.width(), rect.bottom()),
                )
            };
            let left = image_mesh(
                before,
                cut(*image, 0.0, *split),
                cut(unit_rect(), 0.0, *split),
                *orientation,
            );
            let right = image_mesh(
                *after,
                cut(*image, *split, 1.0),
                cut(unit_rect(), *split, 1.0),
                *orientation,
            );
            painter.add(Shape::mesh(left));
            painter.add(Shape::mesh(right));
            split_divider(ui, *image, x, split);
            pane_tag(ui, *image, "Before");
            let right_tag = Rect::from_min_max(pos2(x, image.top()), image.max);
            pane_tag(ui, right_tag, "After");
        }
        _ => {
            painter.add(Shape::mesh(image_mesh(*after, *image, unit_rect(), *orientation)));
        }
    }
}

fn split_divider(ui: &mut Ui, image: Rect, x: f32, split: &mut f32) {
    let hit = Rect::from_center_size(pos2(x, image.center().y), vec2(DIVIDER_GRAB, image.height()));
    let response = ui
        .interact(hit, Id::unique("compare divider"), Sense::drag())
        .on_hover_cursor(CursorIcon::ResizeHorizontal);
    if response.dragged() {
        *split = (*split + response.drag_delta().x / image.width().max(1.0)).clamp(0.02, 0.98);
    }
    let weight = theme::hairline(ui.ctx()) * 2.0;
    let line = Rect::from_center_size(pos2(x, image.center().y), vec2(weight, image.height()));
    ui.painter().rect_filled(line, 0.0, theme::HANDLE_HOVER);
}

fn apply_zoom_input(ui: &Ui, response: &Response, state: &mut AppState, memory: &ViewportMemory, pane: Rect) {
    let Some(pointer) = response.hover_pos() else {
        return;
    };
    let Some(canvas) = memory.canvas else {
        return;
    };
    let pixels_per_point = ui.ctx().pixels_per_point();
    let (scroll, pinch) = ui.input(|input| (input.smooth_scroll_delta.y, input.zoom_delta()));
    if scroll == 0.0 && pinch == 1.0 {
        return;
    }
    let factor = SCROLL_ZOOM.powf(scroll) * pinch;
    let output = memory.output;
    let ratio = state.view.scale(canvas, output) * factor;
    let fit = {
        let mut fitted = state.view;
        fitted.zoom = Zoom::Fit;
        fitted.scale(canvas, output)
    };
    if ratio <= fit {
        state.view.zoom = Zoom::Fit;
    } else {
        let anchor = [
            (pointer.x - pane.left()) * pixels_per_point,
            (pointer.y - pane.top()) * pixels_per_point,
        ];
        state.view.zoom_about(ratio, anchor, canvas, output);
        state.view.remember_ratio();
    }
}

fn apply_pan_and_click(ui: &Ui, response: &Response, state: &mut AppState, plan: &ViewPlan, image: Rect) {
    let pixels_per_point = ui.ctx().pixels_per_point();
    let zoomed_in = plan.region[2] < 0.9999 || plan.region[3] < 0.9999;
    if response.dragged() && zoomed_in {
        let delta = response.drag_delta() * pixels_per_point;
        state.view.pan(plan, [delta.x, delta.y]);
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    } else if response.hovered() {
        let zoom_in = !matches!(state.view.zoom, Zoom::Ratio(_));
        ui.ctx().set_cursor_icon(if zoomed_in {
            CursorIcon::Grab
        } else if zoom_in {
            CursorIcon::ZoomIn
        } else {
            CursorIcon::ZoomOut
        });
    }
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let space = PhotoSpace { image, plan: *plan };
        let point = space.to_photo(pos);
        let inside = unit_rect().contains(pos2(point[0], point[1]));
        let target = if inside { point } else { state.view.center };
        state.view.toggle_at(target);
    }
}

fn render(
    render_state: &RenderState,
    renderer: &mut crate::renderer::ViewRenderer,
    live: &mut LivePhoto,
    state: &mut AppState,
    after: &PaneRequest,
    before: Option<&PaneRequest>,
    ctx: &egui::Context,
) {
    match renderer.update(render_state, live, after, before, state.now) {
        Ok(outcome) => {
            if outcome.refining {
                ctx.request_repaint();
            }
            if let Some(seconds) = outcome.histogram_due_in {
                ctx.request_repaint_after_secs(seconds as f32);
            }
        }
        Err(EngineError::DeviceLost(_)) => state.device_lost = true,
        Err(error) => state.report_error(format!("Render failed: {error}")),
    }
}

fn develop_live(ui: &mut Ui, app: &mut App, response: &Response, canvas: Rect, render_state: &RenderState) {
    let App {
        state,
        darkroom,
        renderer,
        viewport,
        ..
    } = app;
    let Some(edit) = state.edit() else {
        return;
    };
    let Some(path) = state.focused().map(|row| row.path.clone()) else {
        return;
    };
    let Some(live) = darkroom.live_mut(&path) else {
        return;
    };
    let pixels_per_point = ui.ctx().pixels_per_point();
    let cropping = matches!(state.tool, Tool::Crop | Tool::Guides);
    let mut recipe = match &state.preview_recipe {
        Some(preview) => preview.effective(),
        None => edit.effective(),
    };
    if cropping {
        recipe.crop = Crop {
            angle: recipe.crop.angle,
            ..Crop::default()
        };
    }
    let local_tool = matches!(state.tool, Tool::Masks | Tool::Heal | Tool::RedEye);
    let compare = if cropping || local_tool {
        Compare::Off
    } else {
        state.compare
    };
    let before_recipe = (compare != Compare::Off).then(|| {
        let mut before = edit.before();
        before.crop = recipe.crop;
        before
    });

    let orientation = live.orientation;
    let output = orientation.display_size(live.session.output_size(&recipe));
    let panes = panes(canvas, compare);
    let pane = panes.0[0];
    let pane_pixels = [pane.width() * pixels_per_point, pane.height() * pixels_per_point];
    let plan = state.view.plan(pane_pixels, output);
    viewport.canvas = Some(pane_pixels);
    viewport.output = output;

    let after_request = PaneRequest {
        recipe,
        region: orientation.sensor_region(plan.region),
        size: plan.target_size(orientation),
        clipping: state.clipping,
        mask_overlay: state.overlay_mask(),
        proof: state.proof.view(),
    };
    let before_request = before_recipe.map(|recipe| PaneRequest {
        recipe,
        ..after_request.clone()
    });
    render(
        render_state,
        renderer,
        live,
        state,
        &after_request,
        before_request.as_ref(),
        ui.ctx(),
    );
    let Some(after) = renderer.after_texture() else {
        return;
    };

    let image = image_rect(pane, &plan, pixels_per_point);
    viewport.photo_rect = Some(image);
    let frame = LiveFrame {
        plan,
        image,
        panes,
        before: before_request.as_ref().and_then(|_| renderer.before_texture()),
        after,
        orientation,
    };
    let mut split = state.split;
    paint_live(ui, &frame, &mut split);
    state.split = split;
    if state.proof.enabled {
        pane_tag(ui, image, &proof_label(state));
    }
    pointer_input(ui, response, state, viewport, live, &frame);
}

/// Hands the pointer to whichever mode owns it: crop, white balance picking, or zoom and pan.
fn pointer_input(
    ui: &mut Ui,
    response: &Response,
    state: &mut AppState,
    viewport: &mut ViewportMemory,
    live: &mut LivePhoto,
    frame: &LiveFrame,
) {
    let LiveFrame {
        plan,
        image,
        panes,
        orientation,
        ..
    } = *frame;
    let space = PhotoSpace { image, plan };
    if state.tool == Tool::Crop {
        let input = CropInput {
            space,
            orientation,
            sensor_aspect: live.sensor_aspect(),
            optics: live.session.optics(),
        };
        if let Some(rect) = crop_overlay::interact(ui, response, state, &mut viewport.crop_gesture, &input) {
            crop_overlay::paint(ui, &space, &rect, state.crop_tool.show_grid);
        }
    } else if state.tool == Tool::Guides {
        let input = GuideInput {
            space,
            orientation,
            sensor_aspect: live.sensor_aspect(),
            optics: live.session.optics(),
        };
        guides_overlay::show(ui, response, state, &mut viewport.guide_gesture, &input);
    } else if matches!(state.tool, Tool::Masks | Tool::Heal | Tool::RedEye)
        && !ui.input(|input| input.key_down(Key::Space))
    {
        apply_zoom_input(ui, response, state, viewport, panes.0[0]);
        let input = LocalInput {
            space,
            orientation,
            sensor_aspect: live.sensor_aspect(),
        };
        local_overlay::show(ui, response, state, &mut viewport.local_gesture, live, &input);
    } else if viewport.picking_white_balance {
        ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            viewport.picking_white_balance = false;
            crate::white_balance::pick(state, live, space.to_photo(pos));
        }
    } else {
        let (rects, count) = panes;
        if count == 2
            && let Some(pos) = response.hover_pos()
        {
            viewport.hovered_pane = usize::from(pos.x > rects[1].left());
        }
        let hovered = rects[viewport.hovered_pane.min(count - 1)];
        apply_zoom_input(ui, response, state, viewport, hovered);
        let image = image_rect(hovered, &plan, ui.ctx().pixels_per_point());
        apply_pan_and_click(ui, response, state, &plan, image);
    }
}

fn empty_state(ui: &Ui, rect: Rect, library_is_empty: bool) {
    if !library_is_empty {
        return;
    }
    let painter = ui.painter();
    let center = rect.center();
    painter.text(
        center - vec2(0.0, 10.0),
        Align2::CENTER_CENTER,
        "Drop a raw file or folder here",
        theme::medium(15.0),
        theme::TEXT_DIM,
    );
    painter.text(
        center + vec2(0.0, 12.0),
        Align2::CENTER_CENTER,
        "or press Ctrl+O",
        theme::body(),
        theme::TEXT_FAINT,
    );
}

/// A card pinned to one edge of an area. Text wraps at a fixed width, and the card's real size
/// comes from the last frame, since it has to be placed before its contents are known.
fn floating_card(
    ui: &mut Ui,
    area: Rect,
    anchor: Align2,
    offset: egui::Vec2,
    add_contents: impl FnOnce(&mut Ui),
) {
    let id = Id::unique(("floating card", anchor.x() as u8, anchor.y() as u8));
    let widest = vec2(area.width().min(CARD_WIDTH), area.height());
    let size = ui
        .data(|data| data.get_temp::<egui::Vec2>(id))
        .unwrap_or(vec2(widest.x, 0.0));
    let position = anchor.align_size_within_rect(size, area).translate(offset).min;
    let builder = UiBuilder::new()
        .max_rect(Rect::from_min_size(position, widest))
        .layout(egui::Layout::top_down(egui::Align::Min));
    let child = &mut ui.new_child(builder);
    card(child, add_contents);
    let measured = child.min_size();
    if measured != size {
        ui.data_mut(|data| data.insert_temp(id, measured));
        ui.ctx().request_repaint();
    }
}

fn decode_banner(ui: &mut Ui, area: Rect, message: &str, has_preview: bool) {
    floating_card(ui, area, Align2::CENTER_TOP, vec2(0.0, 16.0), |ui| {
        ui.spacing_mut().item_spacing.y = theme::GRID;
        let heading = RichText::new("This photo can't be developed");
        ui.label(heading.font(theme::medium(13.0)).color(theme::ERROR));
        ui.label(RichText::new(message).color(theme::TEXT));
        if has_preview {
            let hint = RichText::new("Showing the preview embedded in the file.");
            ui.label(hint.font(theme::small()).color(theme::TEXT_DIM));
        }
    });
}

fn status_toast(ui: &mut Ui, area: Rect, state: &AppState) {
    let Some(status) = state.status() else {
        return;
    };
    let color = match status.kind {
        crate::state::StatusKind::Info => theme::TEXT,
        crate::state::StatusKind::Error => theme::ERROR,
    };
    let text = status.text.clone();
    floating_card(ui, area, Align2::CENTER_BOTTOM, vec2(0.0, -16.0), |ui| {
        ui.label(RichText::new(text).color(color));
    });
}

fn loading_note(ui: &Ui, area: Rect) {
    let anchor = area.right_bottom() - vec2(8.0, 8.0);
    let painter = ui.painter();
    let galley = painter.layout_no_wrap("Loading raw…".to_owned(), theme::small(), theme::TEXT);
    let box_rect = Align2::RIGHT_BOTTOM.anchor_size(anchor, galley.size() + vec2(12.0, 6.0));
    painter.rect_filled(box_rect, 3.0, Color32::from_black_alpha(150));
    painter.galley(box_rect.min + vec2(6.0, 3.0), galley, theme::TEXT);
}

fn backdrop_menu(response: &Response, state: &mut AppState) {
    response.context_menu(|ui| {
        for backdrop in Backdrop::ALL {
            if ui
                .selectable_label(state.backdrop == backdrop, backdrop.name())
                .clicked()
            {
                state.backdrop = backdrop;
                ui.close();
            }
        }
    });
}

/// Draws the photo area of Develop.
pub fn show(ui: &mut Ui, app: &mut App, render_state: Option<&RenderState>) {
    let area = ui.available_rect_before_wrap();
    let response = ui.interact(area, Id::unique("viewport"), Sense::click_and_drag());
    let level = if app.state.lights == crate::state::Lights::Off {
        0
    } else {
        app.state.backdrop.level()
    };
    ui.painter().rect_filled(area, 0.0, Color32::from_gray(level));
    app.viewport.photo_rect = None;
    backdrop_menu(&response, &mut app.state);

    let canvas = area.shrink(CANVAS_MARGIN);
    let Some(path) = app.state.focused().map(|row| row.path.clone()) else {
        empty_state(ui, area, app.state.library.count() == Some(0));
        status_toast(ui, area, &app.state);
        return;
    };

    let live = app.darkroom.live(&path).is_some();
    match render_state {
        Some(render_state) if live => develop_live(ui, app, &response, canvas, render_state),
        _ => {
            if let Some(preview) = app.darkroom.preview(&path) {
                let rect = paint_preview(ui, canvas, preview);
                app.viewport.photo_rect = Some(rect);
            }
        }
    }

    match app.state.decode_state() {
        DecodeState::Failed(message) => {
            decode_banner(ui, area, &message, app.darkroom.preview(&path).is_some());
        }
        DecodeState::Pending | DecodeState::Ready if !live => loading_note(ui, area),
        _ => {}
    }
    status_toast(ui, area, &app.state);
}
