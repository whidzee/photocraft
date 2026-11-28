//! View › 360° View (#15726): a latlong (2:1 equirectangular) document seen from the centre of its sphere, through a
//! camera you look around in. Each document view keeps its own camera ([`SphereView`] in `View::projection`).
//!
//! - Drag with the middle button, or with Space held, to look around; the Hand tool looks around with the left
//!   button too, as it scrolls the flat canvas. The wheel, the Zoom tool and
//!   View › Zoom In and Zoom Out narrow and widen the field of view; View › Fit on Screen goes back to 90°, 100% to
//!   one image pixel per screen pixel at the centre. With the view focused the arrow keys look around (Shift by
//!   10°, Alt by 0.5°) and Home looks back at the image's centre.
//! - Brush and Eraser strokes are drawn in the view and committed as one `paint.projectedStroke`, which reprojects
//!   them onto the latlong pixels: one History step. The live stroke is the engine's own brush on the view
//!   (`ProjectedPreview`).
//! - The Clone Stamp and Healing Brush paint from the source Alt-click (Option-click) sets, kept as a direction on
//!   the sphere: the stroke paints through a rotation of the sphere, so the source isn't distorted where it lands
//!   (`paint.projectedClone`, `paint.projectedTool`). Aligned keeps the first stroke's rotation for later ones. The
//!   Spot Healing Brush runs on the view too.
//! - The Rectangular and Elliptical Marquee, Lasso and Magic Wand select in the view, and the Patch tool patches the
//!   selection from where it is dropped (`view360_select`). The marching ants follow the camera.
//! - The Move tool turns the layer (or the selection's contents) across the sphere; Edit › Free Transform puts a box
//!   in the view, and Edit › Paste lands the clipboard upright where you look (`view360_xform`).
//! - The Pencil, Mixer Brush, Color Replacement, History Brush, Pattern Stamp, Background Eraser, Remove Tool and
//!   the Blur to Sponge tools paint in the view through `paint.projectedTool`, as the Spot Healing Brush does.
//! - The Eyedropper, Color Sampler, Count, Note, Magic Eraser and Red Eye tools act at the latlong pixel under the
//!   click, through their flat commands; the samplers, count markers and notes show in the view. The Paint Bucket
//!   floods from there across the ±180° seam, and the Gradient is dragged in the view and covers the whole sphere
//!   (`paint.projectedTool`, `projected_fill`).
//! - The Type and shape tools make new pixel layers upright where they're made in the view (`view360_draw`), and
//!   the Pen tools draw and edit the Work Path on the sphere (`view360_path`).
//! - Other tools don't work in the view yet; pressing with one says so in the status bar and does nothing else.
//! - The Navigator outlines what the view sees on the flat image; a click there looks at that spot.
//!
//! The picture is the GPU canvas's `fs_proj` pass over the document's own tiles (`gpu_canvas`); the maths is
//! `photocraft_engine::equirect`, shared with the engine.

use craft::behavior::IconId;
use craft::theme::Variant;
use craft::theme::variant::{Emphasis, Tone};
use craft::ui::buttons::IconButton;
use craft::ui::canvas::{ViewCommands, ViewRequest};
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _};
use craft::ui::display::{Badge, Text};
use craft::ui::notices::{Notice, NoticeLayout};
use craft::ui::style::StyleCx;
use craft::ui::text::{NumberField, ZoomField};
use craft::ui::value_drag::{Slider, SliderScale};
use egui::{Color32, PointerButton, Pos2, Rect, Sense, Stroke, vec2};
use photocraft_engine::brush_cmds::ProjectedPreview;
use photocraft_engine::equirect::{Camera, MAX_FOV, MAX_PITCH, MIN_FOV, Rot, ViewProjection, rot, rotation_between, wrap_deg};
use photocraft_engine::projected_tool::ProjectedTool;
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
    /// The Clone Stamp and Healing Brush source (Alt-click), as a direction on the sphere, so looking around keeps it.
    #[serde(default)]
    pub clone_source: Option<[f64; 3]>,
    /// Aligned: the sphere rotation from destination to source the first stroke after the source was set fixed
    /// (row-major 3×3).
    #[serde(default)]
    pub clone_rotation: Option<[f64; 9]>,
}

impl Default for SphereView {
    fn default() -> Self {
        SphereView { yaw: 0.0, pitch: 0.0, fov: DEFAULT_FOV, clone_source: None, clone_rotation: None }
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
        SphereView { yaw: c.yaw as f32, pitch: c.pitch as f32, fov: c.fov as f32, ..self }
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
        Self::fov_at_zoom(1.0, iw, h)
    }

    /// The field of view that shows `zoom` device pixels per image pixel (1 is 100%) at the centre of a view `h` device
    /// pixels tall, on an `iw` wide latlong, within the view's limits.
    fn fov_at_zoom(zoom: f64, iw: f64, h: f64) -> f32 {
        let per = std::f64::consts::TAU / iw.max(1.0);
        let fov = (2.0 * (h.max(1.0) / 2.0 * per / zoom.max(f64::MIN_POSITIVE)).atan()).to_degrees();
        fov.clamp(MIN_FOV, MAX_FOV) as f32
    }

    /// The zoom the camera shows at the centre of a view `h` device pixels tall: device pixels per image pixel of an
    /// `iw` wide latlong, 1 being 100%, as the flat view's zoom reads.
    fn zoom(&self, iw: f64, h: f64) -> f64 {
        let half = (f64::from(self.fov).to_radians() / 2.0).tan().max(f64::MIN_POSITIVE);
        h.max(1.0) / 2.0 / half * std::f64::consts::TAU / iw.max(1.0)
    }

    /// The camera as the user reads it: where it looks and how wide.
    pub(crate) fn readout(&self) -> String {
        let (yaw, pitch, fov) = (format!("{:.0}", self.yaw), format!("{:.0}", self.pitch), format!("{:.0}", self.fov));
        crate::i18n::fmt(tl!("Yaw {yaw}°, pitch {pitch}°, field of view {fov}°"), &[("yaw", &yaw), ("pitch", &pitch), ("fov", &fov)])
    }
}

/// Whether document `idx`'s own view shows the 360° View.
pub(crate) fn is_on(app: &PhotocraftApp, idx: usize) -> bool {
    app.ui.views.get(idx).is_some_and(|v| v.projection.is_some())
}

/// Whether the Navigator shows the 360° View button for document `idx`: Preferences › Tools › Show 360° View
/// Button in Navigator is on, and the document is 2:1, as a latlong is, or its view is already on.
pub(crate) fn shows_button(app: &PhotocraftApp, idx: usize) -> bool {
    let latlong = app.session.documents().get(idx).is_some_and(|st| st.doc.size.width == 2 * st.doc.size.height && st.doc.size.height > 0);
    app.session.prefs().tools.sphere_navigator_button && (latlong || is_on(app, idx))
}

/// The Navigator's 360° View button: `view.view360` on the active document, shown pressed while its view is `on`.
/// The View menu's item stays as it is.
pub(crate) fn navigator_button(ui: &mut egui::Ui, on: bool) {
    craft::ui::buttons::IconButton::command(craft::behavior::IconId::new("globe"), craft::keymap::CommandRef::new(COMMAND))
        .selected(on)
        .tooltip(tl!("360° View"))
        .show(ui);
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
    /// The command that commits it: `paint.projectedStroke`, `paint.projectedClone` or `paint.projectedTool`.
    command: &'static str,
    params: Value,
    /// The Clone Stamp or Healing Brush source rotation it paints through.
    rotation: Option<Rot>,
    points: Vec<[f64; 3]>,
    preview: Preview,
    /// The Gradient as the sphere shader draws it while it is dragged; `None` while the CPU preview shows it.
    gpu: Option<crate::gpu_canvas::SphereGradient>,
    /// The stroke so far over the whole view, updated a rect at a time.
    texture: Option<egui::TextureHandle>,
    /// The view device pixels the last update covered.
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
        // A retouch tool re-runs on the whole view: at most every 50 ms.
        let floor = if matches!(self.preview, Preview::Tool(_)) { 0.05 } else { 0.0 };
        let wait = if self.cost > 0.04 { self.cost.max(floor) } else { floor };
        if self.gpu.is_some() {
            // The sphere shader draws the Gradient: nothing to render here.
            self.rendered = self.points.len();
            self.texture = None;
            self.shown = None;
        }
        if self.rendered != self.points.len() && now - self.last >= wait {
            self.rendered = self.points.len();
            self.last = now;
            let started = crate::gpu_canvas::now_ms();
            let overlay = match &mut self.preview {
                Preview::Stroke(p) => p.render(&self.points),
                Preview::Tool(t) => t.preview(&self.points),
            };
            self.cost = (crate::gpu_canvas::now_ms() - started) / 1000.0;
            if let Some(o) = overlay {
                self.upload(ctx, &o);
            }
        }
        if self.rendered != self.points.len() {
            ctx.request_repaint();
        }
        if let Some(t) = &self.texture {
            let [w, h] = t.size();
            let at = Rect::from_min_size(rect.min, vec2(w as f32, h as f32) / ppp);
            painter.image(t.id(), at, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
    }

    /// Puts an update's overlay into the view-sized texture, sending only the rect it covers. The Brush, Eraser and
    /// Clone Stamp hand back just what changed; a tool re-run on the view hands back all it changed, so the rect it
    /// covered last time and no longer does is cleared too.
    fn upload(&mut self, ctx: &egui::Context, o: &photocraft_engine::brush_cmds::PreviewOverlay) {
        let Some(view) = self.view_size() else { return };
        let r = match (&self.preview, self.shown) {
            (Preview::Tool(_), Some(last)) => last.union(&o.rect),
            _ => o.rect,
        }
        .intersect(&photocraft_geom::Rect::new(0, 0, view[0] as i32, view[1] as i32));
        if r.is_empty() {
            return;
        }
        let (w, h) = (r.width() as usize, r.height() as usize);
        let mut px = vec![Color32::TRANSPARENT; w * h];
        let ow = o.rect.width() as usize;
        for (i, p) in o.px.iter().enumerate() {
            let (x, y) = (o.rect.x0 + (i % ow) as i32 - r.x0, o.rect.y0 + (i / ow) as i32 - r.y0);
            if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                px[y as usize * w + x as usize] = Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]);
            }
        }
        let image = egui::ColorImage { size: [w, h], source_size: vec2(w as f32, h as f32), pixels: px };
        let texture = self
            .texture
            .get_or_insert_with(|| ctx.load_texture("pc-360-stroke", egui::ColorImage::filled(view, Color32::TRANSPARENT), egui::TextureOptions::LINEAR));
        texture.set_partial([r.x0 as usize, r.y0 as usize], image, egui::TextureOptions::LINEAR);
        self.shown = Some(match (&self.preview, self.shown) {
            (Preview::Tool(_), _) => o.rect,
            (_, Some(last)) => last.union(&o.rect),
            _ => o.rect,
        });
    }

    /// The view's size in device pixels, from the stroke's `projection`.
    fn view_size(&self) -> Option<[usize; 2]> {
        let q = self.params.get("projection")?;
        let (w, h) = (q.get("width")?.as_f64()?, q.get("height")?.as_f64()?);
        (w >= 1.0 && h >= 1.0).then(|| [w.round() as usize, h.round() as usize])
    }
}

/// The Gradient dragged from `from` to `to` (view device pixels) as the sphere shader can draw it, when it can show
/// it as the release paints it (`projected_fill::gradient_look`).
fn gradient_look(
    session: &photocraft_engine::Session,
    params: &Value,
    vp: &ViewProjection,
    from: [f64; 3],
    to: [f64; 2],
) -> Option<crate::gpu_canvas::SphereGradient> {
    let fill = params.get("params")?;
    let look = photocraft_engine::projected_fill::gradient_look(session, fill, vp, [from[0], from[1]], to)?;
    let mut colors = [[0.0; 4]; crate::gpu_canvas::SPHERE_GRADIENT_COLORS];
    for (c, l) in colors.iter_mut().zip(&look.colors) {
        *c = *l;
    }
    Some(crate::gpu_canvas::SphereGradient { axes: look.axes, shape: look.shape, colors })
}

/// What shows a stroke while it is drawn.
enum Preview {
    /// Brush, Eraser, Clone Stamp: the brush engine on the view.
    Stroke(Box<ProjectedPreview>),
    /// Healing Brush and Spot Healing Brush: the flat tool re-run on the resampled view.
    Tool(Box<ProjectedTool>),
}

/// The theme colours the view draws with.
pub(crate) struct Colors {
    pub pasteboard: Color32,
    pub line: Color32,
    pub halo: Color32,
    pub marquee: Color32,
}

pub(crate) fn colors(ctx: &egui::Context) -> Colors {
    StyleCx::with_root(ctx, |cx| {
        let c = &cx.tokens.color;
        Colors {
            pasteboard: cx.color(c.surface.canvas).egui(),
            line: cx.color(c.overlay.line).egui(),
            halo: cx.color(c.overlay.halo).egui(),
            marquee: cx.color(c.overlay.marquee).egui(),
        }
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
            *look = SphereView { clone_source: look.clone_source, clone_rotation: look.clone_rotation, ..SphereView::default() };
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

/// What a left-button press with `tool` does in the view. The middle button always looks around.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Gesture {
    Paint,
    Select,
    Move,
    Zoom,
    /// A click (the Eyedropper also a drag) at one latlong pixel.
    Point,
    /// The Type and shape tools (`view360_draw`).
    Draw,
    /// The Pen tools (`view360_path`).
    Path,
    Look,
    /// A tool the view has no use for yet: the press only says so.
    Unsupported,
}

fn gesture(tool: Tool, space: bool) -> Gesture {
    match tool {
        _ if space => Gesture::Look,
        Tool::Hand => Gesture::Look,
        Tool::Brush | Tool::Eraser | Tool::CloneStamp | Tool::Healing | Tool::SpotHealing | Tool::Gradient => Gesture::Paint,
        t if projects(t) => Gesture::Paint,
        Tool::Zoom => Gesture::Zoom,
        Tool::Move => Gesture::Move,
        t if super::view360_select::supported(t) => Gesture::Select,
        t if points(t) => Gesture::Point,
        t if super::view360_draw::supported(t) => Gesture::Draw,
        t if super::view360_path::supported(t) => Gesture::Path,
        _ => Gesture::Unsupported,
    }
}

/// Whether `tool` does something in the view: the tools that don't are greyed out in the toolbar while it is on.
pub(crate) fn works(tool: Tool) -> bool {
    gesture(tool, false) != Gesture::Unsupported
}

/// The painting and retouch tools that run their flat command on the view through `paint.projectedTool`.
fn projects(tool: Tool) -> bool {
    matches!(
        tool,
        Tool::Pencil
            | Tool::MixerBrush
            | Tool::ColorReplacement
            | Tool::HistoryBrush
            | Tool::PatternStamp
            | Tool::BackgroundEraser
            | Tool::Remove
            | Tool::Blur
            | Tool::Sharpen
            | Tool::Smudge
            | Tool::Dodge
            | Tool::Burn
            | Tool::Sponge
    )
}

/// The tools that act at the one latlong pixel under a click.
fn points(tool: Tool) -> bool {
    matches!(tool, Tool::Eyedropper | Tool::ColorSampler | Tool::Count | Tool::Note | Tool::PaintBucket | Tool::MagicEraser | Tool::RedEye)
}

/// The flat command and options-bar params (without `points`) that a [`projects`] tool runs on the view.
fn projected(app: &PhotocraftApp, tool: Tool, first: &[[f64; 3]]) -> Option<(&'static str, Value)> {
    let o = &app.ui.tool_options;
    let (command, mut p) = match tool {
        Tool::Pencil => ("paint.pencil", json!({"zoom": 1.0, "autoErase": o.pencil_auto_erase})),
        Tool::MixerBrush => ("paint.mixerBrush", json!({})),
        Tool::ColorReplacement => ("paint.colorReplacement", crate::retouch_ui::color_replacement_params(app)),
        Tool::HistoryBrush => ("paint.historyBrush", crate::retouch_ui::history_brush_params(app)),
        Tool::PatternStamp => ("paint.patternStamp", crate::retouch_ui::pattern_stamp_params(app, first)),
        Tool::BackgroundEraser => ("paint.backgroundEraser", crate::eraser_ui::background_eraser_params(app, first)),
        // A new layer for the result would be made in the view's copy, not the document.
        Tool::Remove => ("paint.remove", json!({"sampleAllLayers": o.sample_all.remove, "newLayer": false})),
        t => crate::retouch_ui::dab_params(app, t)?,
    };
    if let Some(m) = p.as_object_mut() {
        m.remove("points");
        // An aligned Pattern Stamp's phase is in the flat image's pixels, not the view's.
        m.remove("phase");
    }
    p["target"] = crate::canvas::paint_target(app);
    Some((command, p))
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
    // Space typed into the Type tool's text doesn't look around.
    let space = ctx.input(|i| i.key_down(egui::Key::Space)) && !super::view360_draw::typing(&ctx, host.canvas_id());
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
    // A Free Transform holds the view: no looking around while its box is up.
    let held = super::view360_xform::holds_view(app, idx);
    let painting = (matches!(gesture, Gesture::Paint | Gesture::Select | Gesture::Move | Gesture::Point | Gesture::Draw | Gesture::Path) && active) || held;
    // The left button looks around only with the Hand tool or Space, as it scrolls the flat canvas.
    let looks = !held && (response.dragged_by(PointerButton::Middle) || (response.dragged_by(PointerButton::Primary) && gesture == Gesture::Look));
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
    if gesture == Gesture::Unsupported && (response.drag_started_by(PointerButton::Primary) || response.clicked()) {
        // Say why the tool did nothing rather than leave it a mystery.
        let name = tl!(tool.label()).to_string();
        app.ui.status = crate::i18n::fmt(
            tl!("{tool} doesn't work in 360° View yet. Turn off View › 360° View to use it, and drag with the middle button or hold Space to look around."),
            &[("tool", &name)],
        );
        app.ui.status_error = false;
    }
    let mut look = look.clamped();
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
            // Image › Rotate Panorama's preview: the content turned, before it is applied.
            sphere_turn: crate::dialogs::rotate_panorama::preview(app).map(|q| quaternion(&transpose(&q))),
            // The Gradient being dragged, when the shader can show it as the release paints it.
            sphere_gradient: app.view360_stroke.as_ref().filter(|s| s.doc == doc.id).and_then(|s| s.gpu),
        };
        crate::gpu_canvas::GpuCanvas::paint(&painter, rect, params);
        // The Navigator draws the same texture flat.
        if app.navigator_plain.contains(&doc.id) {
            app.navigator_gpu.insert(
                doc.id,
                crate::gpu_canvas::ViewParams { projection: None, sphere_turn: None, sphere_gradient: None, view_key: params.view_key ^ 1, ..params },
            );
        }
    } else {
        app.navigator_gpu.remove(&doc.id);
        let message = tl!("360° View draws on the GPU canvas, which isn't available for this document. Turn off View › 360° View to edit it flat.");
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink(16.0)), |ui| {
            let _ = Notice::new(Tone::Warn, message).layout(NoticeLayout::Centred).show(ui);
        });
    }

    // Image › Rotate Panorama is open: the true horizon and the front, to line the turned image up with.
    if crate::dialogs::rotate_panorama::preview(app).is_some() {
        guides(&painter, rect, ppp, &vp, &colors);
    }
    let pass = super::view360_select::Pass { rect, ppp, look: &look, vp: &vp };
    super::view360_select::ants(app, &ctx, &painter, host.canvas_id(), idx, &pass);
    marks(app, &painter, rect, ppp, &vp, idx, &colors);
    super::view360_path::draw(app, &ctx, &painter, host.canvas_id(), idx, &pass, &colors);
    // Text left typed when another tool is picked is placed.
    if !matches!(tool, Tool::Type | Tool::VerticalType) {
        super::view360_draw::finish_typing(app, &ctx, host.canvas_id(), &pass);
    }
    let input = Input { response: &response, painter: &painter, canvas: host.canvas_id(), idx, gesture, held, painting };
    look = run_tool(app, ui, &input, &pass, &Frame { rect, look, vp, doc: doc.id });

    look_hint(app, ui, rect, looks);

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

/// The preference that remembers the look-around hint was seen.
pub(crate) const LOOK_HINT: &str = "view.view360.lookHint";

/// The first time a 360° View is on, a note at the top of the view says how to look around now that a left drag
/// works the tools. "Got it", or looking around once, puts it away for good.
fn look_hint(app: &mut PhotocraftApp, ui: &mut egui::Ui, rect: Rect, looked: bool) {
    if crate::dialogs::alerts::dismissed(app, LOOK_HINT) {
        return;
    }
    let mut seen = looked;
    let at = Rect::from_min_size(rect.center_top() + vec2(-220.0, 12.0), vec2(440.0, 120.0)).intersect(rect);
    ui.scope_builder(egui::UiBuilder::new().max_rect(at), |ui| {
        let message = tl!("Drag with the middle mouse button, or hold Space and drag, to look around. A left drag uses the selected tool.");
        // On its own surface: the note sits over whatever the image shows.
        let out = Notice::new(Tone::Info, message).layout(NoticeLayout::Card).action(craft::ui::notices::NoticeAction::new(tl!("Got it"))).show(ui);
        seen |= out.action.is_some() || out.dismissed;
    });
    if seen {
        crate::dialogs::alerts::remember_skip(app, LOOK_HINT);
    }
}

/// What a pass hands the tool it runs: the view's response and painter, its canvas id and document, and what the
/// press does.
struct Input<'a> {
    response: &'a egui::Response,
    painter: &'a egui::Painter,
    canvas: egui::Id,
    idx: usize,
    gesture: Gesture,
    /// A Free Transform box is up.
    held: bool,
    /// The press works the selected tool rather than looking around.
    painting: bool,
}

/// Runs the selected tool for a pass of the view, or sets the look-around cursor. Returns the camera, with any new
/// Clone Stamp or Healing source.
fn run_tool(app: &mut PhotocraftApp, ui: &mut egui::Ui, input: &Input<'_>, pass: &super::view360_select::Pass<'_>, frame: &Frame) -> SphereView {
    let ctx = ui.ctx().clone();
    let Input { response, painter, canvas, idx, gesture, held, painting } = *input;
    let (rect, ppp) = (frame.rect, pass.ppp);
    if held {
        super::view360_xform::transform(app, &ctx, response, painter, rect, ppp, idx);
    } else if painting && gesture == Gesture::Move {
        super::view360_xform::move_tool(app, &ctx, response, painter, idx, pass);
    } else if painting && gesture == Gesture::Select {
        super::view360_select::tool(app, &ctx, response, painter, canvas, pass);
    } else if painting && gesture == Gesture::Point {
        point(app, &ctx, response, pass);
    } else if painting && gesture == Gesture::Draw {
        // A sizing pass lays the canvas out without input: the Type tool's field shows in the real one alone.
        if !ui.is_sizing_pass() {
            super::view360_draw::tool(app, ui, response, painter, canvas, pass);
        }
    } else if painting && gesture == Gesture::Path {
        super::view360_path::tool(app, &ctx, response, canvas, pass);
    } else if painting {
        return paint(app, &ctx, response, painter, frame);
    } else if response.hovered() {
        ctx.set_cursor_icon(match gesture {
            Gesture::Zoom if ctx.input(|i| i.modifiers.alt) => egui::CursorIcon::ZoomOut,
            Gesture::Zoom => egui::CursorIcon::ZoomIn,
            Gesture::Unsupported if !response.dragged_by(PointerButton::Middle) => egui::CursorIcon::NotAllowed,
            _ if response.dragged() => egui::CursorIcon::Grabbing,
            _ => egui::CursorIcon::Grab,
        });
    }
    frame.look
}

/// What a 360° View pass shows: its rect, camera and projection, and its document.
struct Frame {
    rect: Rect,
    look: SphereView,
    vp: ViewProjection,
    doc: DocId,
}

/// A stroke of the painting tools in the view: started on a press, extended as the pointer moves, shown live and
/// committed on release as one History step. Brush and Eraser commit `paint.projectedStroke`; the Clone Stamp
/// `paint.projectedClone`; the Healing Brush and Spot Healing Brush `paint.projectedTool`. The Clone Stamp and
/// Healing Brush paint from the source Alt-click set, through a rotation of the sphere, so the source isn't
/// distorted where it lands. The brush outline follows the pointer. Returns the camera with any new source.
fn paint(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, painter: &egui::Painter, frame: &Frame) -> SphereView {
    let Frame { rect, look, vp, doc } = frame;
    let (rect, doc, mut look) = (*rect, *doc, *look);
    let ppp = ctx.pixels_per_point();
    let to_view = |p: Pos2| -> [f64; 2] { [f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp)] };
    let pressure = f64::from(app.stylus.pressure());
    let tool = app.ui.tool;
    let sources = matches!(tool, Tool::CloneStamp | Tool::Healing);
    let gradient = tool == Tool::Gradient;
    let colors = colors(ctx);
    if sources && ctx.input(|i| i.modifiers.alt) {
        // Alt-click (or press) sets the source: a direction, so looking around keeps it.
        if (response.clicked() || response.drag_started())
            && let Some(p) = response.interact_pointer_pos()
        {
            let [x, y] = to_view(p);
            look.clone_source = Some(vp.view_to_dir(x, y));
            look.clone_rotation = None;
            app.ui.status = tl!("Source set").to_string();
            app.ui.status_error = false;
        }
        if let Some(h) = response.hover_pos().filter(|_| response.hovered()) {
            crosshair(painter, h, &colors);
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
        return look;
    }
    let projection = json!({"yaw": look.yaw, "pitch": look.pitch, "fov": look.fov, "width": vp.w, "height": vp.h});
    let start = |app: &mut PhotocraftApp, at: [f64; 2]| -> Option<Stroke360> {
        let target = crate::canvas::paint_target(app);
        let o = &app.ui.tool_options;
        // The Clone Stamp and Healing Brush source rotation: the aligned one an earlier stroke fixed, else from here
        // to the source.
        let rotation = if sources {
            let Some(source) = look.clone_source else {
                app.ui.status =
                    if cfg!(target_os = "macos") { tl!("Option-click to set a source to paint from") } else { tl!("Alt-click to set a source to paint from") }
                        .to_string();
                app.ui.status_error = true;
                return None;
            };
            Some(match look.clone_rotation.filter(|_| o.clone_aligned) {
                Some(q) => unflat(&q),
                None => rotation_between(vp.view_to_dir(at[0], at[1]), source),
            })
        } else {
            None
        };
        let (command, params) = match tool {
            Tool::CloneStamp => (
                "paint.projectedClone",
                json!({
                    "rotation": rotation.map(|q| q.concat()),
                    "sampleLayer": o.clone_sample,
                    "opacity": app.session.tools.brush.opacity * 100.0,
                    "flow": app.session.tools.brush.flow * 100.0,
                    "target": target,
                    "projection": projection,
                }),
            ),
            Tool::Healing => (
                "paint.projectedTool",
                json!({
                    "command": "paint.healingBrush",
                    "params": {"aligned": o.clone_aligned, "sampleLayer": o.clone_sample, "target": target},
                    "rotation": rotation.map(|q| q.concat()),
                    "projection": projection,
                }),
            ),
            Tool::SpotHealing => (
                "paint.projectedTool",
                json!({
                    "command": "paint.spotHealing",
                    "params": {"type": o.spot_type, "sampleAllLayers": o.sample_all.get(tool), "target": target},
                    "projection": projection,
                }),
            ),
            Tool::Gradient => {
                let o = &app.ui.tool_options;
                let fill = json!({"style": o.gradient_style, "reverse": o.gradient_reverse, "dither": o.gradient_dither, "opacity": o.fill_opacity,
                    "mode": o.gradient_blend_mode.label(), "target": target});
                ("paint.projectedTool", json!({"command": "paint.gradient", "params": fill, "projection": projection}))
            }
            t if projects(t) => {
                let (command, params) = projected(app, t, &[[at[0], at[1], pressure]])?;
                ("paint.projectedTool", json!({"command": command, "params": params, "projection": projection}))
            }
            _ => ("paint.projectedStroke", json!({"erase": tool == Tool::Eraser, "zoom": 1.0, "target": target, "projection": projection})),
        };
        let preview = match command {
            "paint.projectedTool" => ProjectedTool::new(&app.session, &params).map(|t| Preview::Tool(Box::new(t))),
            "paint.projectedClone" => photocraft_engine::retouch_cmds::projected_clone_preview(&app.session, &params).map(|p| Preview::Stroke(Box::new(p))),
            _ => ProjectedPreview::brush(&app.session, &params).map(|p| Preview::Stroke(Box::new(p))),
        };
        match preview {
            Ok(preview) => Some(Stroke360 {
                doc,
                command,
                params,
                rotation,
                points: vec![[at[0], at[1], pressure]],
                preview,
                gpu: None,
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
        if gradient {
            // The Gradient runs from the press to the pointer.
            if stroke.points.get(1).is_none_or(|q| q[0] != x || q[1] != y) {
                stroke.points.truncate(1);
                stroke.points.push([x, y, 1.0]);
                stroke.rendered = 0;
                stroke.gpu = gradient_look(&app.session, &stroke.params, vp, stroke.points[0], [x, y]);
            }
        } else if stroke.points.last().is_none_or(|q| (q[0] - x).abs() + (q[1] - y).abs() > 0.25) {
            stroke.points.push([x, y, pressure]);
        }
    }
    let finished = if response.drag_stopped_by(PointerButton::Primary) {
        // A Gradient too short to have a direction paints nothing.
        app.view360_stroke
            .take()
            .filter(|s| s.doc == doc)
            .filter(|s| !gradient || s.points.first().zip(s.points.get(1)).is_some_and(|(a, b)| (a[0] - b[0]).abs() + (a[1] - b[1]).abs() >= 2.0))
    } else if response.clicked() && !gradient {
        // A click is one dab.
        response.interact_pointer_pos().and_then(|p| start(app, to_view(p)))
    } else {
        None
    };
    if let Some(stroke) = app.view360_stroke.as_mut().filter(|s| s.doc == doc) {
        stroke.draw(ctx, painter, rect, ppp);
        if let (true, Some(a), Some(b)) = (gradient, stroke.points.first(), stroke.points.get(1)) {
            let at = |q: &[f64; 3]| rect.min + vec2(q[0] as f32, q[1] as f32) / ppp;
            painter.line_segment([at(a), at(b)], Stroke::new(3.0, colors.halo));
            painter.line_segment([at(a), at(b)], Stroke::new(1.0, colors.line));
        }
    }
    if let Some(mut stroke) = finished {
        // The stroke stays on screen in the pass it is committed in.
        stroke.draw(ctx, painter, rect, ppp);
        let mut params = stroke.params;
        params["points"] = json!(stroke.points);
        match app.run(stroke.command, params) {
            // Aligned: later strokes keep this stroke's offset on the sphere.
            Ok(_) if sources => look.clone_rotation = stroke.rotation.filter(|_| app.ui.tool_options.clone_aligned).map(|q| flat(&q)),
            Ok(_) => {}
            Err(e) => {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
    }
    // Where the source is: under the pointer's stroke, through its rotation.
    if sources && let Some(source) = look.clone_source {
        let live = app.view360_stroke.as_ref().filter(|s| s.doc == doc);
        let at = live.and_then(|s| s.points.last().map(|q| [q[0], q[1]])).or_else(|| response.hover_pos().map(to_view));
        let q = live.and_then(|s| s.rotation).or_else(|| {
            let q = look.clone_rotation.filter(|_| app.ui.tool_options.clone_aligned)?;
            Some(unflat(&q))
        });
        let shown = match (at, q) {
            (Some(a), Some(q)) => vp.dir_to_view(rot(&q, vp.view_to_dir(a[0], a[1]))),
            _ => vp.dir_to_view(source),
        };
        if let Some([x, y]) = shown {
            let p = rect.min + vec2(x as f32, y as f32) / ppp;
            if rect.contains(p) {
                crosshair(painter, p, &colors);
            }
        }
    }
    if gradient {
        if response.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
    } else if let Some(h) = response.hover_pos().filter(|_| response.hovered()) {
        let r = (app.session.tools.brush.size / ppp / 2.0).max(1.0);
        painter.circle_stroke(h, r + 1.0, Stroke::new(1.0, colors.halo));
        painter.circle_stroke(h, r, Stroke::new(1.0, colors.line));
        ctx.set_cursor_icon(egui::CursorIcon::None);
    }
    look
}

/// A sphere rotation as the 9 numbers a camera keeps, row by row.
fn flat(q: &Rot) -> [f64; 9] {
    [q[0][0], q[0][1], q[0][2], q[1][0], q[1][1], q[1][2], q[2][0], q[2][1], q[2][2]]
}

/// The rotation [`flat`] kept.
fn unflat(q: &[f64; 9]) -> Rot {
    [[q[0], q[1], q[2]], [q[3], q[4], q[5]], [q[6], q[7], q[8]]]
}

/// The true horizon (level, all the way round) and the front (straight ahead, from the ground up to the sky), dashed
/// over the view.
fn guides(painter: &egui::Painter, rect: Rect, ppp: f32, vp: &ViewProjection, colors: &Colors) {
    let tau = std::f64::consts::TAU;
    let horizon = (0..=360).map(|i| f64::from(i) * tau / 360.0).map(|t| [t.sin(), 0.0, t.cos()]).collect::<Vec<_>>();
    let front = (-89..=89).map(|i| f64::from(i).to_radians()).map(|a| [0.0, a.sin(), a.cos()]).collect::<Vec<_>>();
    for line in [horizon, front] {
        let mut run: Vec<Pos2> = Vec::new();
        let flush = |run: &mut Vec<Pos2>| {
            if run.len() > 1 {
                painter.extend(egui::Shape::dashed_line(run, Stroke::new(3.0, colors.halo), 8.0, 6.0));
                painter.extend(egui::Shape::dashed_line(run, Stroke::new(1.0, colors.line), 8.0, 6.0));
            }
            run.clear();
        };
        for d in line {
            match vp.dir_to_view(d) {
                Some([x, y]) => {
                    let p = rect.min + vec2(x as f32, y as f32) / ppp;
                    if run.last().is_some_and(|l| l.distance(p) > rect.width()) {
                        flush(&mut run);
                    }
                    run.push(p);
                }
                None => flush(&mut run),
            }
        }
        flush(&mut run);
    }
}

/// `q` transposed: for a rotation, its inverse.
fn transpose(q: &Rot) -> Rot {
    [[q[0][0], q[1][0], q[2][0]], [q[0][1], q[1][1], q[2][1]], [q[0][2], q[1][2], q[2][2]]]
}

/// Rotation `m` as a unit quaternion `x, y, z, w`.
fn quaternion(m: &Rot) -> [f32; 4] {
    let t = m[0][0] + m[1][1] + m[2][2];
    let q = if t > 0.0 {
        let s = (t + 1.0).sqrt() * 2.0;
        [(m[2][1] - m[1][2]) / s, (m[0][2] - m[2][0]) / s, (m[1][0] - m[0][1]) / s, s / 4.0]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [s / 4.0, (m[0][1] + m[1][0]) / s, (m[0][2] + m[2][0]) / s, (m[2][1] - m[1][2]) / s]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [(m[0][1] + m[1][0]) / s, s / 4.0, (m[1][2] + m[2][1]) / s, (m[0][2] - m[2][0]) / s]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [(m[0][2] + m[2][0]) / s, (m[1][2] + m[2][1]) / s, s / 4.0, (m[1][0] - m[0][1]) / s]
    };
    q.map(|v| v as f32)
}

/// The latlong pixel under view point `p` of `pass`, for a document `size` pixels.
fn latlong(pass: &super::view360_select::Pass<'_>, p: Pos2, size: [f64; 2]) -> [f64; 2] {
    let [x, y] = pass.to_view(p);
    let [u, v] = pass.vp.view_to_equirect(x, y, size[0], size[1]);
    [u.rem_euclid(size[0]), v.clamp(0.0, (size[1] - 1e-6).max(0.0))]
}

/// A click with a [`points`] tool: its flat command at the latlong pixel under the pointer. The Eyedropper also
/// samples as it drags, and Alt (Option) picks the background colour.
fn point(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, pass: &super::view360_select::Pass<'_>) {
    let tool = app.ui.tool;
    let Some(size) = app.session.active().map(|st| [f64::from(st.doc.size.width), f64::from(st.doc.size.height)]) else { return };
    if response.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if tool == Tool::Eyedropper {
        if (response.clicked_by(PointerButton::Primary) || response.dragged_by(PointerButton::Primary))
            && let Some(p) = response.interact_pointer_pos()
            && let Some(c) = crate::canvas::sampling::eyedropper_color(app, latlong(pass, p, size))
        {
            let key = if ctx.input(|i| i.modifiers.alt) { "background" } else { "foreground" };
            let _ = app.run("tools.setColors", json!({ key: [c[0], c[1], c[2], 1.0] }));
        }
        return;
    }
    let Some(p) = response.interact_pointer_pos().filter(|_| response.clicked_by(PointerButton::Primary)) else { return };
    let at = latlong(pass, p, size);
    let result = match tool {
        Tool::ColorSampler => app.run("view.colorSamplers.add", json!({"x": at[0], "y": at[1]})).map(|_| ()),
        Tool::Count => {
            let r = app.run("count.add", json!({"x": at[0], "y": at[1]}));
            // A marker that stays hidden looks like the click did nothing, as on the flat canvas.
            app.ui.view.show.count |= r.is_ok();
            r.map(|_| ())
        }
        Tool::Note => crate::analysis::create_note(app, at[0], at[1]).map(|_| ()),
        Tool::RedEye => {
            crate::canvas::tools::retouch::red_eye_click(app, at);
            Ok(())
        }
        // The bucket floods the latlong across the ±180° seam.
        Tool::PaintBucket => {
            let [x, y] = pass.to_view(p);
            let params = crate::canvas::paint_tool::bucket_params(app);
            app.run("paint.projectedTool", json!({"command": "paint.bucket", "params": params, "points": [[x, y, 1.0]], "projection": pass.projection()}))
                .map(|_| ())
        }
        _ => {
            crate::canvas::paint_tool::click_tool(app, tool, at);
            Ok(())
        }
    };
    if let Err(e) = result {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// The active document's colour samplers, count markers and notes where they are on the sphere, as small marks.
fn marks(app: &PhotocraftApp, painter: &egui::Painter, rect: Rect, ppp: f32, vp: &ViewProjection, idx: usize, colors: &Colors) {
    let Some(st) = app.session.documents().get(idx) else { return };
    let (iw, ih) = (f64::from(st.doc.size.width), f64::from(st.doc.size.height));
    let at = |q: [f64; 2]| -> Option<Pos2> {
        let [x, y] = vp.equirect_to_view(q[0], q[1], iw, ih)?;
        Some(rect.min + vec2(x as f32, y as f32) / ppp).filter(|p| rect.contains(*p))
    };
    for q in &st.color_samplers {
        if let Some(p) = at(*q) {
            crosshair(painter, p, colors);
            painter.circle_stroke(p, 4.0, Stroke::new(1.0, colors.line));
        }
    }
    if app.ui.view.show.count {
        for q in st.doc.measurement.count_groups.iter().flat_map(|g| &g.points) {
            if let Some(p) = at(*q) {
                painter.circle_filled(p, 4.0, colors.halo);
                painter.circle_filled(p, 2.5, colors.line);
            }
        }
    }
    if app.ui.view.show.notes {
        for n in &st.doc.notes {
            if let Some(p) = at(n.position) {
                let r = Rect::from_min_size(p, vec2(10.0, 10.0));
                painter.rect_filled(r, 1.0, colors.halo);
                painter.rect_stroke(r, 1.0, Stroke::new(1.0, colors.line), egui::StrokeKind::Inside);
            }
        }
    }
}

/// A small cross at `p`, over a halo so it reads on any image.
fn crosshair(painter: &egui::Painter, p: Pos2, colors: &Colors) {
    for (width, color) in [(3.0, colors.halo), (1.0, colors.line)] {
        let s = Stroke::new(width, color);
        painter.line_segment([p - vec2(7.0, 0.0), p + vec2(7.0, 0.0)], s);
        painter.line_segment([p - vec2(0.0, 7.0), p + vec2(0.0, 7.0)], s);
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
    take_navigator_move(app, &ctx);
    let Some(size) = app.session.documents().get(idx).map(|st| st.doc.size) else { return };
    let Some(mut look) = app.ui.views.get(idx).and_then(|v| v.projection) else { return };
    let (iw, ih) = (f64::from(size.width.max(1)), f64::from(size.height.max(1)));
    // The preview box fills the panel above the rows under it, as the flat Navigator's does, with the image fitted in
    // its well. The rows' height is last pass's, as the box is laid out before them.
    let rows_id = egui::Id::unique("pc-view360-navigator-rows");
    let rows = ctx.data(|d| d.get_temp::<f32>(rows_id)).unwrap_or(0.0);
    let (sunken, edge, inset) = StyleCx::with_root(&ctx, |cx| {
        let c = &cx.tokens.color;
        (cx.color(c.surface.sunken).egui(), cx.color(c.line.control_edge).egui(), cx.tokens.space.gap.inline)
    });
    let width = ui.available_width().max(1.0);
    let fitted = ((width - 2.0 * inset).max(1.0) as f64 * ih / iw) as f32 + 2.0 * inset;
    let room = ui.available_height() - rows - ui.spacing().item_spacing.y;
    let (frame, response) = ui.allocate_exact_size(vec2(width, if room.is_finite() { room.max(fitted) } else { fitted }), Sense::click_and_drag());
    ui.painter().rect(frame, 0.0, sunken, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
    let rect = {
        let room = frame.shrink(inset).size().max(vec2(1.0, 1.0));
        let scale = (room.x / iw as f32).min(room.y / ih as f32);
        Rect::from_center_size(frame.center(), vec2(iw as f32 * scale, ih as f32 * scale))
    };
    let rows_top = frame.bottom();
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
    // The flat Navigator's zoom row, as its percent field, Zoom Out, slider and Zoom In, reading the zoom at the centre
    // of the view, so the panel keeps its layout and its percent across the switch. The slider spans the zooms the field
    // of view allows, and the buttons step it as Zoom In and Zoom Out do.
    let h = f64::from(canvas.height() * ppp);
    let ppp = f64::from(ppp.max(f32::MIN_POSITIVE));
    let document = craft::behavior::kurbo::Size::new(iw, ih);
    let mut zoom = look.zoom(iw, h);
    let (low, high) = (SphereView { fov: MAX_FOV as f32, ..look }.zoom(iw, h), SphereView { fov: MIN_FOV as f32, ..look }.zoom(iw, h));
    let flat = super::document_display::active_view(app)
        .map(|mut v| {
            v.zoom = zoom / ppp;
            v
        })
        .unwrap_or_else(|| craft::behavior::CanvasView::new(canvas, craft::behavior::kurbo::Point::new(iw / 2.0, ih / 2.0), zoom / ppp));
    let button = shows_button(app, idx);
    let percent = craft::ui::runtime::env::Env::of(ui).units().zoom_percent(zoom * 100.0);
    let row = craft::ui::layout_components::Row::new().show(ui, |ui, _| {
        let field = ZoomField::new(tl!("Zoom"), Some(flat), document).stepper(true).show(ui);
        let typed = field.view.value().map(|v| v.zoom * ppp);
        let out = IconButton::new(IconId::new("minus")).label(tl!("Zoom Out")).enabled(look.fov < MAX_FOV as f32).show(ui).clicked();
        let slid =
            !Slider::new(Bind::bound(&mut zoom), low..=high).scale(SliderScale::Log).label(tl!("Zoom")).value_text(percent.clone()).show(ui).events.is_empty();
        let into = IconButton::new(IconId::new("plus")).label(tl!("Zoom In")).enabled(look.fov > MIN_FOV as f32).show(ui).clicked();
        if button {
            navigator_button(ui, true);
        }
        (typed.or(slid.then_some(zoom)), out, into, field.invalid_zoom)
    });
    if let Some((typed, out, into, invalid)) = row.inner {
        if let Some(z) = typed.filter(|z| z.is_finite()) {
            look.fov = SphereView::fov_at_zoom(z, iw, h);
        }
        if out {
            look.scale_fov(1.25);
        }
        if into {
            look.scale_fov(1.0 / 1.25);
        }
        crate::panels::chrome_ui::zoom_alert(app, invalid);
    }
    // Yaw and Pitch share a line, Field of view has its own.
    let mut typed = [None; 3];
    let fields: [CameraField; 3] =
        [(tl!("Yaw"), look.yaw, -180.0..=180.0), (tl!("Pitch"), look.pitch, -MAX_PITCH..=MAX_PITCH), (tl!("Field of view"), look.fov, MIN_FOV..=MAX_FOV)];
    let [yaw_field, pitch_field, fov_field] = fields;
    let [yaw_slot, pitch_slot, fov_slot] = &mut typed;
    ui.horizontal(|ui| {
        camera_field(ui, yaw_slot, yaw_field);
        camera_field(ui, pitch_slot, pitch_field);
    });
    ui.horizontal(|ui| camera_field(ui, fov_slot, fov_field));
    let [yaw, pitch, fov] = typed;
    let rows_now = ui.min_rect().bottom() - rows_top;
    if rows_now.is_finite() && (rows_now - rows).abs() > 0.5 {
        ctx.data_mut(|d| d.insert_temp(rows_id, rows_now));
        ctx.request_discard("the 360° View Navigator's rows changed height");
    }
    look.yaw = yaw.unwrap_or(look.yaw);
    look.pitch = pitch.unwrap_or(look.pitch);
    look.fov = fov.unwrap_or(look.fov);
    if Some(look) != app.ui.views.get(idx).and_then(|v| v.projection) {
        let p = json!({"on": true, "yaw": look.yaw, "pitch": look.pitch, "fov": look.fov});
        let (frame, doc) = (ctx.cumulative_frame_nr(), app.session.active_index());
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique(NAVIGATOR_MOVE), (frame, doc, p)));
        ctx.request_repaint();
    }
}

/// Where the Navigator keeps the camera it was moved to until the next frame.
const NAVIGATOR_MOVE: &str = "pc-view360-navigator-move";

/// Applies the camera the Navigator was moved to in an earlier frame, through the view's command. The canvas and the
/// Navigator each call it first, so the move lands between frames: the canvas draws before the Navigator in some frames
/// and after it in others, and a camera changed in the middle of a frame would reach the canvas a frame late only some
/// of the time, which jitters.
pub(crate) fn take_navigator_move(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let id = egui::Id::unique(NAVIGATOR_MOVE);
    let Some((frame, doc, p)) = ctx.data(|d| d.get_temp::<(u64, Option<usize>, Value)>(id)) else { return };
    if frame >= ctx.cumulative_frame_nr() {
        return;
    }
    ctx.data_mut(|d| d.remove::<(u64, Option<usize>, Value)>(id));
    if doc == app.session.active_index() {
        let _ = command(app, &p);
    }
}

/// One of the Navigator's camera fields: its name, the value it shows and its range.
type CameraField = (&'static str, f32, std::ops::RangeInclusive<f64>);

/// A camera field after its name; `slot` takes the value the user typed this pass.
fn camera_field(ui: &mut egui::Ui, slot: &mut Option<f32>, (name, value, range): CameraField) {
    Text::new(name).show(ui);
    let mut field = Some(f64::from(value));
    let out = NumberField::new(name, Bind::bound(&mut field)).range(range).show(ui);
    if !out.edit.events.is_empty() {
        *slot = field.filter(|v| v.is_finite()).map(|v| v as f32);
    }
}

#[cfg(test)]
#[path = "view360_tests.rs"]
mod tests;
