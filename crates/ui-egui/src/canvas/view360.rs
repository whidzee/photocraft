//! View › 360° View (#15726): a latlong (2:1 equirectangular) document seen from the centre of its sphere, through a
//! camera you look around in. Each document view keeps its own camera ([`SphereView`] in `View::projection`).
//!
//! - Drag to look around (the Hand tool, Space or the middle button while painting); the wheel, the Zoom tool and
//!   View › Zoom In and Zoom Out narrow and widen the field of view; View › Fit on Screen goes back to 90°, 100% to
//!   one image pixel per screen pixel at the centre. With the view focused the arrow keys look around (Shift by
//!   10°, Alt by 0.5°) and Home looks back at the image's centre.
//! - Brush and Eraser strokes are drawn in the view and committed as one `paint.projectedStroke`, which reprojects
//!   them onto the latlong pixels: one History step. The live stroke is the engine's own brush on the view
//!   (`ProjectedPreview`).
//! - Other tools don't work in the view yet; pressing with one says so in the status bar and looks around.
//! - The Navigator outlines what the view sees on the flat image; a click there looks at that spot.
//!
//! The picture is the GPU canvas's `fs_proj` pass over the document's own tiles (`gpu_canvas`); the maths is
//! `photocraft_engine::equirect`, shared with the engine.

use craft::theme::Variant;
use craft::theme::variant::{Emphasis, Tone};
use craft::ui::canvas::{ViewCommands, ViewRequest};
use craft::ui::contract::{Bind, Props as _, Show as _};
use craft::ui::display::{Badge, Text};
use craft::ui::notices::{Notice, NoticeLayout};
use craft::ui::style::StyleCx;
use craft::ui::text::NumberField;
use craft::ui::value_drag::Slider;
use egui::{Color32, PointerButton, Pos2, Rect, Sense, Stroke, vec2};
use photocraft_engine::brush_cmds::ProjectedPreview;
use photocraft_engine::equirect::{Camera, MAX_FOV, MAX_PITCH, MIN_FOV, ViewProjection, wrap_deg};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::document_display::CanvasHost;
use crate::PhotocraftApp;
use crate::state::{Tool, View};
use photocraft_doc::DocId;

/// The View menu item's command id.
pub(crate) const COMMAND: &str = "view.view360";

/// The field of view a view starts with and Fit on Screen goes back to, in degrees.
const DEFAULT_FOV: f32 = 90.0;

/// One view's 360° View camera, in degrees.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SphereView {
    /// Where it looks around the vertical axis: 0 is the image's centre column, wrapped to (−180, 180].
    pub yaw: f32,
    /// How far it looks up (+) or down (−).
    pub pitch: f32,
    /// The vertical field of view.
    pub fov: f32,
}

impl Default for SphereView {
    fn default() -> Self {
        SphereView { yaw: 0.0, pitch: 0.0, fov: DEFAULT_FOV }
    }
}

impl SphereView {
    /// The engine's camera.
    pub fn camera(&self) -> Camera {
        Camera { yaw: f64::from(self.yaw), pitch: f64::from(self.pitch), fov: f64::from(self.fov) }.normalized()
    }

    /// This camera with its yaw wrapped and its pitch and field of view in range.
    fn clamped(self) -> Self {
        let c = self.camera();
        SphereView { yaw: c.yaw as f32, pitch: c.pitch as f32, fov: c.fov as f32 }
    }

    /// Turn Camera: a pointer drag of `d` points over a view `h` points tall turns the view the way the pointer
    /// moves, each point by the angle one point spans at the centre of the view.
    pub fn steer(&mut self, d: egui::Vec2, h: f32) {
        // As Panosphere: the view-plane width of one point at the centre, as an angle.
        let k = (2.0 * (self.fov.to_radians() / 2.0).tan() / h.max(1.0)).to_degrees();
        self.turn(d.x * k, -d.y * k);
    }

    /// Grab Image: turns the camera so world direction `dir` shows at view pixel `at` of a `size` (device pixels)
    /// view, with no roll. Near a pole, where no upright camera can put it there, the pitch stops at the nearest
    /// one that comes closest.
    pub fn hold(&mut self, dir: [f64; 3], at: [f64; 2], size: [f64; 2]) {
        let vp = ViewProjection::new(self.camera(), size[0], size[1]);
        // The ray through `at` in the camera's own frame: right, up, forward.
        let k = (vp.cam.fov.to_radians() / 2.0).tan() / (vp.h / 2.0);
        let (cx, cy, cz) = ((at[0] - vp.w / 2.0) * k, -(at[1] - vp.h / 2.0) * k, 1.0);
        // Pitch p with cy·cos p + cz·sin p = dir.y·|c|.
        let n = (cx * cx + cy * cy + cz * cz).sqrt();
        let r = (cy * cy + cz * cz).sqrt();
        let want = (dir[1] * n / r).clamp(-1.0, 1.0);
        // Two pitches put it there; take the one in range nearest the current pitch, so a drag never flips over.
        let base = cy.atan2(cz);
        let lim = MAX_PITCH.to_radians();
        let now = f64::from(self.pitch).to_radians();
        let wrap = |t: f64| t.sin().atan2(t.cos());
        let cost = |t: f64| (t.clamp(-lim, lim) - t).abs() * 10.0 + (t.clamp(-lim, lim) - now).abs();
        let (a, b) = (wrap(want.asin() - base), wrap(std::f64::consts::PI - want.asin() - base));
        let pitch = if cost(a) <= cost(b) { a } else { b }.clamp(-lim, lim);
        let a = cz * pitch.cos() - cy * pitch.sin();
        let yaw = dir[0].atan2(dir[2]) - cx.atan2(a);
        self.yaw = wrap_deg(yaw.to_degrees()) as f32;
        self.pitch = (pitch.to_degrees() as f32).clamp(-MAX_PITCH as f32, MAX_PITCH as f32);
    }

    /// Turns by `yaw` and `pitch` degrees.
    fn turn(&mut self, yaw: f32, pitch: f32) {
        self.yaw = wrap_deg(f64::from(self.yaw + yaw)) as f32;
        self.pitch = (self.pitch + pitch).clamp(-MAX_PITCH as f32, MAX_PITCH as f32);
    }

    /// Scales the field of view by `f`: less than 1 zooms in.
    pub fn scale_fov(&mut self, f: f32) {
        if f.is_finite() && f > 0.0 {
            self.fov = (self.fov * f).clamp(MIN_FOV as f32, MAX_FOV as f32);
        }
    }

    /// Looks at latlong pixel (`u`, `v`) of an `iw` × `ih` image.
    pub fn look_at(&mut self, u: f64, v: f64, iw: f64, ih: f64) {
        if iw > 0.0 && ih > 0.0 {
            self.yaw = wrap_deg((u / iw - 0.5) * 360.0) as f32;
            self.pitch = (((0.5 - v / ih) * 180.0) as f32).clamp(-MAX_PITCH as f32, MAX_PITCH as f32);
        }
    }

    /// The field of view that shows one image pixel of an `iw` wide latlong per device pixel at the centre of a view
    /// `h` device pixels tall.
    fn actual_pixels_fov(iw: f64, h: f64) -> f32 {
        let per = std::f64::consts::TAU / iw.max(1.0);
        (2.0 * (h.max(1.0) / 2.0 * per).atan()).to_degrees() as f32
    }

    /// The camera as the user reads it: where it looks and how wide.
    fn readout(&self) -> String {
        let (yaw, pitch, fov) = (format!("{:.0}", self.yaw), format!("{:.0}", self.pitch), format!("{:.0}", self.fov));
        crate::i18n::fmt(tl!("Yaw {yaw}°, pitch {pitch}°, field of view {fov}°"), &[("yaw", &yaw), ("pitch", &pitch), ("fov", &fov)])
    }
}

/// Whether document `idx`'s own view shows the 360° View.
pub(crate) fn is_on(app: &PhotocraftApp, idx: usize) -> bool {
    app.ui.views.get(idx).is_some_and(|v| v.projection.is_some())
}

/// `view.view360`: turns the active document's 360° View on (`on: true`), off (`on: false`) or over (no `on`), and
/// sets its camera from `yaw`, `pitch` and `fov` (degrees), which turn it on when `on` isn't given. Answers whether
/// it is on and its camera.
pub(crate) fn command(app: &mut PhotocraftApp, p: &Value) -> Result<Value, String> {
    let idx = app.session.active_index().ok_or("no document open")?;
    let size = app.session.documents().get(idx).map(|st| (st.doc.size.width, st.doc.size.height)).ok_or("no document open")?;
    let num = |k: &str| -> Result<Option<f32>, String> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_f64().filter(|v| v.is_finite()).map(|v| Some(v as f32)).ok_or_else(|| format!("{k} must be a number")),
        }
    };
    let (yaw, pitch, fov) = (num("yaw")?, num("pitch")?, num("fov")?);
    let view = app.ui.views.get_mut(idx).ok_or("no document open")?;
    let on = match p.get("on") {
        None | Some(Value::Null) => yaw.is_some() || pitch.is_some() || fov.is_some() || view.projection.is_none(),
        Some(v) => v.as_bool().ok_or("on must be a boolean")?,
    };
    let was_on = view.projection.is_some();
    view.projection = on.then(|| {
        let mut look = view.projection.unwrap_or_default();
        look.yaw = yaw.unwrap_or(look.yaw);
        look.pitch = pitch.unwrap_or(look.pitch);
        look.fov = fov.unwrap_or(look.fov);
        look.clamped()
    });
    let look = view.projection;
    // A stroke being drawn ends with the view it was drawn in.
    if !on {
        app.view360_stroke = None;
    }
    if on && !was_on {
        // The Navigator outlines what the view sees on the flat image.
        if let Some(Err(e)) = crate::panel_docking::command(app, "window.panel.navigator", &json!({"show": true})) {
            app.ui.status = e;
        }
        let (w, h) = size;
        if w != 2 * h {
            let (w, h) = (w.to_string(), h.to_string());
            app.ui.status = crate::i18n::fmt(tl!("360° View expects a 2:1 latlong image; this one is {w} × {h} pixels."), &[("w", &w), ("h", &h)]);
            app.ui.status_error = false;
        }
    }
    Ok(match look {
        Some(l) => json!({"on": true, "yaw": l.yaw, "pitch": l.pitch, "fov": l.fov}),
        None => json!({"on": false}),
    })
}

/// A Brush or Eraser stroke being drawn in a 360° View: its `paint.projectedStroke` params (without the points), the
/// points so far (view device pixels and pressure) and the engine's live preview of them.
pub(crate) struct Stroke360 {
    doc: DocId,
    params: Value,
    points: Vec<[f64; 3]>,
    preview: ProjectedPreview,
    texture: Option<egui::TextureHandle>,
    /// The view device pixels the texture covers.
    shown: Option<photocraft_geom::Rect>,
    /// How many points the texture shows.
    rendered: usize,
    /// When the preview last rendered, and how long that took (seconds): a slow render waits about as long before
    /// the next, so a long stroke never eats every frame.
    last: f64,
    cost: f64,
}

impl Stroke360 {
    /// Renders the points that arrived since the last render, and draws the stroke so far over `rect`.
    fn draw(&mut self, ctx: &egui::Context, painter: &egui::Painter, rect: Rect, ppp: f32) {
        let now = ctx.input(|i| i.time);
        let wait = if self.cost > 0.04 { self.cost } else { 0.0 };
        if self.rendered != self.points.len() && now - self.last >= wait {
            self.rendered = self.points.len();
            self.last = now;
            let started = crate::gpu_canvas::now_ms();
            let overlay = self.preview.render(&self.points);
            self.cost = (crate::gpu_canvas::now_ms() - started) / 1000.0;
            if let Some(o) = overlay {
                let size = [o.rect.width() as usize, o.rect.height() as usize];
                let image = egui::ColorImage::from_rgba_unmultiplied(size, o.px.as_flattened());
                match &mut self.texture {
                    Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                    None => self.texture = Some(ctx.load_texture("pc-360-stroke", image, egui::TextureOptions::LINEAR)),
                }
                self.shown = Some(o.rect);
            }
        }
        if self.rendered != self.points.len() {
            ctx.request_repaint();
        }
        if let (Some(t), Some(r)) = (&self.texture, self.shown) {
            let at = Rect::from_min_size(rect.min + vec2(r.x0 as f32, r.y0 as f32) / ppp, vec2(r.width() as f32, r.height() as f32) / ppp);
            painter.image(t.id(), at, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
    }
}

/// The theme colours the view draws with.
struct Colors {
    pasteboard: Color32,
    line: Color32,
    halo: Color32,
}

fn colors(ctx: &egui::Context) -> Colors {
    StyleCx::with_root(ctx, |cx| {
        let c = &cx.tokens.color;
        Colors { pasteboard: cx.color(c.surface.canvas).egui(), line: cx.color(c.overlay.line).egui(), halo: cx.color(c.overlay.halo).egui() }
    })
}

/// The view commands asked of a 360° View (a menu row, a shortcut, the Navigator's zoom row) on its field of view: Zoom
/// In and Zoom Out narrow and widen it, Fit on Screen and Fill Screen go back to the default, 100% shows one image
/// pixel per device pixel at the centre. The others have no meaning here. A placement (a new document's first fit)
/// waits for the flat view.
fn take_view_commands(view: &mut View, look: &mut SphereView, iw: f64, h: f64) {
    view.pending.retain(|request| {
        let ViewRequest::Command(command) = *request else { return true };
        match command {
            ViewCommands::ZoomIn => look.scale_fov(1.0 / 1.25),
            ViewCommands::ZoomOut => look.scale_fov(1.25),
            ViewCommands::FitOnScreen | ViewCommands::FillScreen => look.fov = DEFAULT_FOV,
            ViewCommands::ActualPixels => look.fov = SphereView::actual_pixels_fov(iw, h),
            _ => {}
        }
        false
    });
    *look = look.clamped();
}

/// Arrow keys look around by 2° (Shift 10°, Alt 0.5°); Home looks back at the image's centre with the default field
/// of view. The keys are taken, so they don't also nudge a layer.
fn look_keys(ctx: &egui::Context, look: &mut SphereView) {
    use egui::Key;
    ctx.input_mut(|i| {
        let m = i.modifiers;
        let step = if m.shift {
            10.0
        } else if m.alt {
            0.5
        } else {
            2.0
        };
        for (key, yaw, pitch) in [(Key::ArrowLeft, -1.0, 0.0), (Key::ArrowRight, 1.0, 0.0), (Key::ArrowUp, 0.0, 1.0), (Key::ArrowDown, 0.0, -1.0)] {
            if i.consume_key(m, key) {
                look.turn(yaw * step, pitch * step);
            }
        }
        if i.consume_key(egui::Modifiers::NONE, Key::Home) {
            *look = SphereView::default();
        }
    });
}

/// The latlong pixels a view sees, for the GPU canvas to bring up to date first.
fn seen(vp: &ViewProjection, size: photocraft_geom::Size) -> photocraft_geom::Rect {
    let full = photocraft_geom::Rect::new(0, 0, size.width as i32, size.height as i32);
    let rects = photocraft_engine::equirect::view_rect_to_equirect(vp, [0.0, 0.0, vp.w, vp.h], size.width, size.height, 2.0);
    let seen = rects.iter().fold(photocraft_geom::Rect::EMPTY, |acc, r| {
        let r = photocraft_geom::Rect::new(r[0] as i32, r[1] as i32, r[2] as i32, r[3] as i32);
        if acc.is_empty() { r } else { acc.union(&r) }
    });
    if seen.is_empty() { full } else { seen.intersect(&full) }
}

/// What a press with `tool` does in the view: paint, zoom, or look around.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Gesture {
    Paint,
    Zoom,
    Look,
}

fn gesture(tool: Tool, space: bool) -> Gesture {
    match tool {
        _ if space => Gesture::Look,
        Tool::Brush | Tool::Eraser => Gesture::Paint,
        Tool::Zoom => Gesture::Zoom,
        _ => Gesture::Look,
    }
}

/// Draws document `host.idx`'s canvas as a 360° View into `rect` and runs its camera and strokes. Returns the view
/// after the pass.
pub(crate) fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui, host: CanvasHost, rect: Rect, mut view: View, output: Option<u32>) -> View {
    let ctx = ui.ctx().clone();
    let idx = host.idx;
    let ppp = ctx.pixels_per_point();
    let Some(doc) = app.session.documents().get(idx).map(|st| std::sync::Arc::clone(&st.doc)) else { return view };
    let (iw, ih) = (f64::from(doc.size.width), f64::from(doc.size.height));
    let (w_px, h_px) = (f64::from(rect.width() * ppp), f64::from(rect.height() * ppp));
    let mut look = view.projection.unwrap_or_default();
    take_view_commands(&mut view, &mut look, iw, h_px);

    let response = ui.interact(rect, host.canvas_id().with("360"), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let colors = colors(&ctx);
    painter.rect_filled(rect, 0.0, colors.pasteboard);
    let active = app.session.active_index() == Some(idx);
    if response.is_pointer_button_down_on() {
        response.request_focus();
        if !active {
            app.session.set_active(idx);
        }
    }

    let tool = app.ui.tool;
    let space = ctx.input(|i| i.key_down(egui::Key::Space));
    let gesture = gesture(tool, space);
    // Escape drops a stroke being drawn.
    if app.view360_stroke.is_some() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        app.view360_stroke = None;
    }
    // A stroke whose release this view never saw (the window lost the pointer) ends without painting.
    if !ctx.input(|i| i.pointer.any_down()) && !response.drag_stopped() {
        app.view360_stroke = None;
    }
    let to_view = |p: Pos2| -> [f64; 2] { [f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp)] };
    let size = [w_px, h_px];
    if response.hovered() {
        let dy = ctx.input(|i| i.smooth_scroll_delta.y);
        if dy != 0.0 {
            // The spot under the pointer stays under it.
            let under = response.hover_pos().map(|p| (ViewProjection::new(look.camera(), w_px, h_px).view_to_dir(to_view(p)[0], to_view(p)[1]), to_view(p)));
            look.scale_fov((-dy * 0.002).exp());
            if let Some((dir, at)) = under {
                look.hold(dir, at, size);
            }
        }
    }
    if response.has_focus() {
        look_keys(&ctx, &mut look);
    }
    let painting = gesture == Gesture::Paint && active;
    let looks = response.dragged_by(PointerButton::Middle) || (response.dragged_by(PointerButton::Primary) && !painting);
    let grabbed = host.canvas_id().with("360-grab");
    if looks {
        match app.session.prefs().tools.sphere_drag {
            photocraft_engine::prefs::SphereDrag::TurnCamera => look.steer(response.drag_delta(), rect.height()),
            photocraft_engine::prefs::SphereDrag::GrabImage => {
                // The direction first pressed stays under the pointer for the whole drag.
                let dir = ctx.data(|d| d.get_temp::<[f64; 3]>(grabbed)).or_else(|| {
                    let start = ctx.input(|i| i.pointer.press_origin())?;
                    let [x, y] = to_view(start);
                    let dir = ViewProjection::new(look.camera(), w_px, h_px).view_to_dir(x, y);
                    ctx.data_mut(|d| d.insert_temp(grabbed, dir));
                    Some(dir)
                });
                if let (Some(dir), Some(p)) = (dir, response.interact_pointer_pos()) {
                    look.hold(dir, to_view(p), size);
                }
            }
        }
    }
    // The grabbed spot lasts until the button is let go, not just while a pass reports the drag: egui can run a pass
    // between moves that doesn't, and grabbing again from the turned view would add the whole drag again each move.
    if !ctx.input(|i| i.pointer.any_down()) {
        ctx.data_mut(|d| d.remove::<[f64; 3]>(grabbed));
    }
    if gesture == Gesture::Zoom
        && response.clicked()
        && let Some(p) = response.interact_pointer_pos()
    {
        // Narrow (Alt widens) and look at the click.
        let vp = ViewProjection::new(look.camera(), w_px, h_px);
        let [u, v] = vp.view_to_equirect(f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp), iw, ih);
        look.look_at(u, v, iw, ih);
        look.scale_fov(if ctx.input(|i| i.modifiers.alt) { 1.25 } else { 0.8 });
    }
    if gesture == Gesture::Look && tool != Tool::Hand && !space && (response.drag_started_by(PointerButton::Primary) || response.clicked()) {
        // Say why the tool did nothing rather than leave it a mystery.
        let name = tl!(tool.label()).to_string();
        app.ui.status =
            crate::i18n::fmt(tl!("{tool} doesn't work in 360° View yet, so dragging looks around. Turn off View › 360° View to use it."), &[("tool", &name)]);
        app.ui.status_error = false;
    }
    let look = look.clamped();
    let vp = ViewProjection::new(look.camera(), w_px, h_px);

    // The picture.
    let drawn = app.gpu.is_some() && super::textures::ensure_gpu(app, &ctx, idx, seen(&vp, doc.size), None);
    if drawn {
        app.perf.gpu = true;
        let params = crate::gpu_canvas::ViewParams {
            doc: doc.id.0,
            doc_size: [doc.size.width, doc.size.height],
            zoom: 1.0,
            center: [0.0, 0.0],
            rotation: 0.0,
            view_key: egui::Id::unique(("pc-canvas-360", ctx.viewport_id(), host)).value(),
            display: super::display::sync_display_lut(app, &doc, doc.id.0, output),
            output: output.unwrap_or(0),
            hdr: super::display::hdr_preview(app, &doc),
            projection: Some([look.yaw, look.pitch, look.fov]),
        };
        crate::gpu_canvas::GpuCanvas::paint(&painter, rect, params);
        // The Navigator draws the same texture flat.
        if app.navigator_plain.contains(&doc.id) {
            app.navigator_gpu.insert(doc.id, crate::gpu_canvas::ViewParams { projection: None, view_key: params.view_key ^ 1, ..params });
        }
    } else {
        app.navigator_gpu.remove(&doc.id);
        let message = tl!("360° View draws on the GPU canvas, which isn't available for this document. Turn off View › 360° View to edit it flat.");
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink(16.0)), |ui| {
            let _ = Notice::new(Tone::Warn, message).layout(NoticeLayout::Centred).show(ui);
        });
    }

    if painting {
        paint(app, &ctx, &response, &painter, &Frame { rect, look, vp, doc: doc.id });
    } else if response.hovered() {
        ctx.set_cursor_icon(match gesture {
            Gesture::Zoom if ctx.input(|i| i.modifiers.alt) => egui::CursorIcon::ZoomOut,
            Gesture::Zoom => egui::CursorIcon::ZoomIn,
            _ if response.dragged() => egui::CursorIcon::Grabbing,
            _ => egui::CursorIcon::Grab,
        });
    }

    // Where the camera looks, readable over any image.
    let readout = look.readout();
    let badge = Rect::from_min_max(rect.left_bottom() + vec2(8.0, -40.0), rect.right_bottom() + vec2(-8.0, -8.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(badge).layout(egui::Layout::left_to_right(egui::Align::Max)), |ui| {
        Badge::new(readout.clone()).variant(Variant::look(Emphasis::Solid, Tone::Neutral)).show(ui);
    });
    let name = crate::i18n::fmt(tl!("360° View, {readout}"), &[("readout", &readout)]);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Canvas, true, &name));
    view.projection = Some(look);
    view
}

/// What a 360° View pass shows: its rect, camera and projection, and its document.
struct Frame {
    rect: Rect,
    look: SphereView,
    vp: ViewProjection,
    doc: DocId,
}

/// A Brush or Eraser stroke in the view: started on a press, extended as the pointer moves, shown with the engine's
/// brush and committed on release as `paint.projectedStroke`. The brush outline follows the pointer.
fn paint(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, painter: &egui::Painter, frame: &Frame) {
    let Frame { rect, look, vp, doc } = frame;
    let (rect, doc) = (*rect, *doc);
    let ppp = ctx.pixels_per_point();
    let to_view = |p: Pos2| -> [f64; 2] { [f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp)] };
    let pressure = f64::from(app.stylus.pressure());
    let start = |app: &mut PhotocraftApp, at: [f64; 2]| -> Option<Stroke360> {
        let params = json!({
            "erase": app.ui.tool == Tool::Eraser,
            "zoom": 1.0,
            "target": crate::canvas::paint_target(app),
            "projection": {"yaw": look.yaw, "pitch": look.pitch, "fov": look.fov, "width": vp.w, "height": vp.h},
        });
        match ProjectedPreview::brush(&app.session, &params) {
            Ok(preview) => Some(Stroke360 {
                doc,
                params,
                points: vec![[at[0], at[1], pressure]],
                preview,
                texture: None,
                shown: None,
                rendered: 0,
                last: f64::NEG_INFINITY,
                cost: 0.0,
            }),
            Err(e) => {
                app.ui.status = e.to_string();
                app.ui.status_error = true;
                None
            }
        }
    };
    if response.drag_started_by(PointerButton::Primary) {
        let at = ctx.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos());
        app.view360_stroke = at.and_then(|p| start(app, to_view(p)));
    }
    if response.dragged_by(PointerButton::Primary)
        && let Some(p) = response.interact_pointer_pos()
        && let Some(stroke) = app.view360_stroke.as_mut().filter(|s| s.doc == doc)
    {
        let [x, y] = to_view(p);
        if stroke.points.last().is_none_or(|q| (q[0] - x).abs() + (q[1] - y).abs() > 0.25) {
            stroke.points.push([x, y, pressure]);
        }
    }
    let finished = if response.drag_stopped_by(PointerButton::Primary) {
        app.view360_stroke.take().filter(|s| s.doc == doc)
    } else if response.clicked() {
        // A click is one dab.
        response.interact_pointer_pos().and_then(|p| start(app, to_view(p)))
    } else {
        None
    };
    if let Some(stroke) = app.view360_stroke.as_mut().filter(|s| s.doc == doc) {
        stroke.draw(ctx, painter, rect, ppp);
    }
    if let Some(mut stroke) = finished {
        // The stroke stays on screen in the pass it is committed in.
        stroke.draw(ctx, painter, rect, ppp);
        let mut params = stroke.params;
        params["points"] = json!(stroke.points);
        if let Err(e) = app.run("paint.projectedStroke", params) {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
    if let Some(h) = response.hover_pos().filter(|_| response.hovered()) {
        let colors = colors(ctx);
        let r = (app.session.tools.brush.size / ppp / 2.0).max(1.0);
        painter.circle_stroke(h, r + 1.0, Stroke::new(1.0, colors.halo));
        painter.circle_stroke(h, r, Stroke::new(1.0, colors.line));
        ctx.set_cursor_icon(egui::CursorIcon::None);
    }
}

/// The view's outline on a Navigator preview of the latlong in `rect`: its four edges through the projection, as
/// lines broken where they cross the ±180° seam. `view` is the canvas size in device pixels.
pub(crate) fn outline(look: &SphereView, view: [f64; 2], size: [f64; 2], rect: Rect) -> Vec<Vec<Pos2>> {
    let vp = ViewProjection::new(look.camera(), view[0], view[1]);
    let (w, h) = (vp.w, vp.h);
    let n = 64;
    let t = |i: usize| i as f64 / n as f64;
    let edge = (0..n)
        .map(|i| [w * t(i), 0.0])
        .chain((0..n).map(|i| [w, h * t(i)]))
        .chain((0..n).map(|i| [w * (1.0 - t(i)), h]))
        .chain((0..=n).map(|i| [0.0, h * (1.0 - t(i))]));
    let (sx, sy) = (rect.width() / size[0] as f32, rect.height() / size[1] as f32);
    let mut lines: Vec<Vec<Pos2>> = vec![Vec::new()];
    let mut last: Option<f64> = None;
    for [x, y] in edge {
        let [u, v] = vp.view_to_equirect(x, y, size[0], size[1]);
        if last.is_some_and(|l| (u - l).abs() > size[0] / 2.0) {
            lines.push(Vec::new());
        }
        last = Some(u);
        if let Some(line) = lines.last_mut() {
            line.push(rect.min + vec2(u as f32 * sx, v as f32 * sy));
        }
    }
    lines.retain(|l| l.len() > 1);
    lines
}

/// The Navigator while document `idx` shows the 360° View: the flat image with the view's outline on it, and its
/// camera under it. A click or drag on the image looks at that spot; the wheel there changes the field of view.
pub(crate) fn navigator(app: &mut PhotocraftApp, ui: &mut egui::Ui, idx: usize) {
    let ctx = ui.ctx().clone();
    let Some(size) = app.session.documents().get(idx).map(|st| st.doc.size) else { return };
    let Some(mut look) = app.ui.views.get(idx).and_then(|v| v.projection) else { return };
    let (iw, ih) = (f64::from(size.width.max(1)), f64::from(size.height.max(1)));
    let width = ui.available_width().max(1.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, (width as f64 * ih / iw) as f32), Sense::click_and_drag());
    let colors = colors(&ctx);
    if let Some(params) = crate::canvas::navigator_gpu_view(app, idx, rect, &ctx) {
        crate::gpu_canvas::GpuCanvas::paint(ui.painter(), rect, params);
    } else if let Some(texture) = crate::canvas::navigator_texture(app, &ctx, idx) {
        ui.painter().image(texture, rect, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
    } else {
        ui.painter().rect_filled(rect, 0.0, colors.pasteboard);
    }
    if (response.clicked() || response.dragged())
        && let Some(p) = response.interact_pointer_pos()
    {
        let (fx, fy) = (f64::from((p.x - rect.min.x) / rect.width()), f64::from((p.y - rect.min.y) / rect.height()));
        look.look_at(fx.clamp(0.0, 1.0) * iw, fy.clamp(0.0, 1.0) * ih, iw, ih);
    }
    if response.hovered() {
        let dy = ctx.input(|i| i.smooth_scroll_delta.y);
        if dy != 0.0 {
            look.scale_fov((-dy * 0.002).exp());
        }
    }
    let canvas = app.last_canvas_rect;
    let ppp = app.canvas_ppp();
    let view = [f64::from(canvas.width() * ppp), f64::from(canvas.height() * ppp)];
    let painter = ui.painter_at(rect);
    for line in outline(&look, view, [iw, ih], rect) {
        painter.add(egui::Shape::line(line.clone(), Stroke::new(3.0, colors.halo)));
        painter.add(egui::Shape::line(line, Stroke::new(1.5, colors.line)));
    }
    let readout = look.readout();
    let name = crate::i18n::fmt(tl!("Navigator, 360° View, {readout}"), &[("readout", &readout)]);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Canvas, true, &name));
    // The camera typed or slid, through the view's own command. Only a value the user edited this pass is taken:
    // the fields round what they show (yaw to a whole degree), so taking every pass's value would snap the camera
    // to the rounding while it is dragged, and jitter.
    let mut fov = f64::from(look.fov);
    if !Slider::new(Bind::bound(&mut fov), MIN_FOV..=MAX_FOV).step(0.5).label(tl!("Field of view")).show(ui).events.is_empty() {
        look.fov = fov as f32;
    }
    let fields: [(&str, f32, std::ops::RangeInclusive<f64>); 3] =
        [(tl!("Yaw"), look.yaw, -180.0..=180.0), (tl!("Pitch"), look.pitch, -MAX_PITCH..=MAX_PITCH), (tl!("Field of view"), look.fov, MIN_FOV..=MAX_FOV)];
    let mut typed = [None; 3];
    for (slot, (name, value, range)) in typed.iter_mut().zip(fields) {
        ui.horizontal(|ui| {
            Text::new(name).show(ui);
            let mut field = Some(f64::from(value));
            let out = NumberField::new(name, Bind::bound(&mut field)).range(range).show(ui);
            if !out.edit.events.is_empty() {
                *slot = field.filter(|v| v.is_finite()).map(|v| v as f32);
            }
        });
    }
    let [yaw, pitch, fov] = typed;
    look.yaw = yaw.unwrap_or(look.yaw);
    look.pitch = pitch.unwrap_or(look.pitch);
    look.fov = fov.unwrap_or(look.fov);
    if Some(look) != app.ui.views.get(idx).and_then(|v| v.projection) {
        let _ = command(app, &json!({"on": true, "yaw": look.yaw, "pitch": look.pitch, "fov": look.fov}));
    }
}

#[cfg(test)]
#[path = "view360_tests.rs"]
mod tests;
