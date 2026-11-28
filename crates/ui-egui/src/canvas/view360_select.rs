//! The selection tools in View › 360° View (#15726): the Rectangular and Elliptical Marquee and the Lasso draw in the
//! view, and the Magic Wand clicks in it. Each commits one `select.projected`, which reprojects the shape onto the
//! latlong selection, so a marquee drawn looking up at the sky is a rectangle as seen there. The options bar's mode
//! applies, with Shift adding, Alt subtracting and both intersecting, as on the flat canvas. The marching ants are
//! the selection resampled into the view, and the painting tools in the view are clipped by it.
//!
//! The Single Row and Single Column Marquee click a one-pixel line, level or upright in the view. The Polygonal and
//! Magnetic Lasso draw freehand in the view, as the Lasso does. Quick Selection grows the selection from where its
//! brush is dragged (Alt takes away), and Object Selection finds the object in a rectangle drawn round it.
//!
//! The Patch tool uses the same drawing: a drag outside the selection draws one with the Lasso, a drag inside it
//! patches the selected area from where it is dropped.

use craft::keymap::{Mods, Platform};
use egui::{Color32, PointerButton, Pos2, Rect, Stroke, vec2};
use photocraft_engine::equirect::{ViewProjection, rotation_between};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::Tool;

use super::view360::SphereView;

/// The selection tools that work in the view.
pub(crate) fn supported(tool: Tool) -> bool {
    matches!(
        tool,
        Tool::RectMarquee
            | Tool::EllipseMarquee
            | Tool::SingleRowMarquee
            | Tool::SingleColumnMarquee
            | Tool::Lasso
            | Tool::PolygonLasso
            | Tool::MagneticLasso
            | Tool::MagicWand
            | Tool::QuickSelection
            | Tool::ObjectSelection
            | Tool::Patch
    )
}

/// The tools that select with a click rather than a drawn shape.
fn clicks(tool: Tool) -> bool {
    matches!(tool, Tool::MagicWand | Tool::SingleRowMarquee | Tool::SingleColumnMarquee)
}

/// The tools whose shape is the rectangle from the first point to the last.
fn boxes(tool: Tool) -> bool {
    matches!(tool, Tool::RectMarquee | Tool::EllipseMarquee | Tool::ObjectSelection)
}

/// A selection (or a Patch move) being drawn: view device pixels.
#[derive(Clone, Debug, PartialEq)]
enum Drag {
    /// A marquee from its first point to the pointer, or a lasso through every point.
    Shape { mode: &'static str, points: Vec<[f64; 2]> },
    /// Patch: the selection carried from `start` to the pointer.
    Patch { start: [f64; 2] },
}

/// What the view of one pass is: its rect and device pixels per point, and its camera.
pub(crate) struct Pass<'a> {
    pub rect: Rect,
    pub ppp: f32,
    pub look: &'a SphereView,
    pub vp: &'a ViewProjection,
}

impl Pass<'_> {
    pub(crate) fn to_view(&self, p: Pos2) -> [f64; 2] {
        [f64::from((p.x - self.rect.min.x) * self.ppp), f64::from((p.y - self.rect.min.y) * self.ppp)]
    }

    pub(crate) fn to_screen(&self, q: [f64; 2]) -> Pos2 {
        self.rect.min + vec2(q[0] as f32, q[1] as f32) / self.ppp
    }

    pub(crate) fn projection(&self) -> Value {
        json!({"yaw": self.look.yaw, "pitch": self.look.pitch, "fov": self.look.fov, "width": self.vp.w, "height": self.vp.h})
    }
}

/// The mode a press with `tool` and `modifiers` combines its selection with the current one by.
fn mode(app: &PhotocraftApp, tool: Tool, modifiers: egui::Modifiers) -> &'static str {
    let has = app.session.active().is_some_and(|st| st.doc.selection.is_some());
    let mods = Mods::from_egui(&modifiers, Platform::current());
    crate::tool_feedback::mode_name(crate::tool_feedback::selection_op(tool, app.ui.selection_mode, mods, has))
}

/// The mode a drawn shape combines by: the Patch tool draws with the Lasso's, and Quick Selection adds to the
/// selection (Alt takes away), as its flat brush does.
fn shape_mode(app: &PhotocraftApp, tool: Tool, modifiers: egui::Modifiers) -> &'static str {
    match tool {
        Tool::Patch => mode(app, Tool::Lasso, modifiers),
        Tool::QuickSelection if modifiers.alt => "subtract",
        Tool::QuickSelection => "add",
        t => mode(app, t, modifiers),
    }
}

/// `select.projected` params for `tool` with `points` (view device pixels).
fn params(app: &PhotocraftApp, tool: Tool, mode: &str, points: &[[f64; 2]], pass: &Pass<'_>) -> Option<Value> {
    let o = &app.ui.tool_options;
    let mut p = json!({"mode": mode, "antiAlias": o.anti_alias, "sampleAllLayers": o.sample_all.get(tool), "projection": pass.projection()});
    let (a, b) = (points.first().copied()?, points.last().copied()?);
    match tool {
        Tool::RectMarquee | Tool::EllipseMarquee => {
            p["kind"] = json!(if tool == Tool::RectMarquee { "rect" } else { "ellipse" });
            p["rect"] = json!([a[0], a[1], b[0], b[1]]);
            p["feather"] = json!(o.feather);
        }
        Tool::Lasso | Tool::PolygonLasso | Tool::MagneticLasso | Tool::Patch => {
            p["kind"] = json!("lasso");
            p["points"] = json!(points);
            p["feather"] = json!(o.feather);
        }
        Tool::MagicWand => {
            p["kind"] = json!("wand");
            p["points"] = json!([a]);
            p["tolerance"] = json!(o.tolerance);
            p["contiguous"] = json!(o.contiguous);
        }
        Tool::SingleRowMarquee | Tool::SingleColumnMarquee => {
            p["kind"] = json!(if tool == Tool::SingleRowMarquee { "row" } else { "column" });
            p["points"] = json!([a]);
        }
        Tool::QuickSelection => {
            p["kind"] = json!("quick");
            p["points"] = json!(points);
            p["size"] = json!(app.session.tools.brush.size);
        }
        Tool::ObjectSelection => {
            p["kind"] = json!("object");
            p["rect"] = json!([a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])]);
        }
        _ => return None,
    }
    Some(p)
}

/// Commits a drawn shape: a shape too small to have an inside is a click, which deselects in New mode, as on the
/// flat canvas.
fn commit(app: &mut PhotocraftApp, tool: Tool, mode: &str, points: &[[f64; 2]], pass: &Pass<'_>) {
    let small = match tool {
        t if boxes(t) => points.first().zip(points.last()).is_none_or(|(a, b)| (a[0] - b[0]).abs() < 2.0 || (a[1] - b[1]).abs() < 2.0),
        Tool::Lasso | Tool::PolygonLasso | Tool::MagneticLasso | Tool::Patch => points.len() < 3,
        Tool::QuickSelection => points.is_empty(),
        _ => false,
    };
    let r = if small {
        if mode == "replace" && app.session.is_enabled("select.deselect") { app.run("select.deselect", json!({})).map(|_| ()) } else { Ok(()) }
    } else {
        match params(app, tool, mode, points, pass) {
            Some(p) => app.run("select.projected", p).map(|_| ()),
            None => Ok(()),
        }
    };
    if let Err(e) = r {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// Whether view device pixel `v` is inside the active document's selection.
fn in_selection(app: &PhotocraftApp, vp: &ViewProjection, v: [f64; 2]) -> bool {
    let Some(st) = app.session.active() else { return false };
    let Some(sel) = st.doc.selection.as_ref() else { return false };
    let (w, h) = (st.doc.size.width, st.doc.size.height);
    let [x, y] = vp.view_to_equirect(v[0], v[1], f64::from(w), f64::from(h));
    let (x, y) = ((x.floor() as i64).rem_euclid(i64::from(w.max(1))) as i32, (y.floor() as i32).clamp(0, h as i32 - 1));
    let mut m = vec![0f32; sel.channels()];
    sel.read_pixel(x, y, &mut m);
    m.first().is_some_and(|v| *v >= 0.5)
}

/// The id a view's selection drag is kept under.
fn drag_id(canvas: egui::Id) -> egui::Id {
    canvas.with("360-select")
}

/// Where the marching ants are carried to while a Patch drag moves the selection (points).
fn shift_id(canvas: egui::Id) -> egui::Id {
    canvas.with("360-ants-shift")
}

/// Runs a selection tool (or the Patch tool) in the view of `canvas`.
pub(crate) fn tool(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, painter: &egui::Painter, canvas: egui::Id, pass: &Pass<'_>) {
    let tool = app.ui.tool;
    let id = drag_id(canvas);
    let colors = super::view360::colors(ctx);
    // A drag whose release this view never saw ends without a selection.
    if !ctx.input(|i| i.pointer.any_down()) && !response.drag_stopped() {
        ctx.data_mut(|d| {
            d.remove::<Drag>(id);
            d.remove::<egui::Vec2>(shift_id(canvas));
        });
    }
    let drag: Option<Drag> = ctx.data(|d| d.get_temp(id));
    if response.hovered() {
        let inside = tool == Tool::Patch && response.hover_pos().is_some_and(|h| in_selection(app, pass.vp, pass.to_view(h)));
        let moving = matches!(drag, Some(Drag::Patch { .. })) || (inside && drag.is_none());
        ctx.set_cursor_icon(if moving { egui::CursorIcon::Move } else { egui::CursorIcon::Crosshair });
    }
    let modifiers = ctx.input(|i| i.modifiers);
    // The Magic Wand and the single-pixel marquees select with a click.
    if clicks(tool) {
        if response.clicked_by(PointerButton::Primary)
            && let Some(p) = response.interact_pointer_pos()
        {
            let mode = mode(app, tool, modifiers);
            commit(app, tool, mode, &[pass.to_view(p)], pass);
        }
        return;
    }
    if response.drag_started_by(PointerButton::Primary)
        && let Some(p) = ctx.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos())
    {
        let q = pass.to_view(p);
        let patch = tool == Tool::Patch && !modifiers.shift && !modifiers.alt && in_selection(app, pass.vp, q);
        let d = if patch { Drag::Patch { start: q } } else { Drag::Shape { mode: shape_mode(app, tool, modifiers), points: vec![q] } };
        ctx.data_mut(|x| x.insert_temp(id, d));
    } else if response.clicked_by(PointerButton::Primary) && drag.is_none() {
        // A Quick Selection click is one dab; a click with a marquee or a lasso has no shape.
        let at: Vec<[f64; 2]> = response.interact_pointer_pos().filter(|_| tool == Tool::QuickSelection).map(|p| pass.to_view(p)).into_iter().collect();
        let mode = shape_mode(app, tool, modifiers);
        commit(app, tool, mode, &at, pass);
        return;
    }
    let Some(drag) = ctx.data(|d| d.get_temp::<Drag>(id)) else { return };
    let now = response.interact_pointer_pos().map(|p| pass.to_view(p));
    match drag {
        Drag::Shape { mode, mut points } => {
            if let Some(q) = now {
                match tool {
                    t if boxes(t) => {
                        points.truncate(1);
                        points.push(q);
                    }
                    _ if points.last().is_none_or(|l| (l[0] - q[0]).abs() + (l[1] - q[1]).abs() > 1.0) => points.push(q),
                    _ => {}
                }
            }
            if response.drag_stopped() {
                ctx.data_mut(|d| d.remove::<Drag>(id));
                commit(app, tool, mode, &points, pass);
                return;
            }
            outline(painter, &shape_points(tool, &points, pass), &colors);
            ctx.data_mut(|d| d.insert_temp(id, Drag::Shape { mode, points }));
            ctx.request_repaint();
        }
        Drag::Patch { start } => {
            let Some(now) = now else { return };
            if response.drag_stopped() {
                ctx.data_mut(|d| {
                    d.remove::<Drag>(id);
                    d.remove::<egui::Vec2>(shift_id(canvas));
                });
                if (now[0] - start[0]).abs() + (now[1] - start[1]).abs() >= 1.0 {
                    patch(app, start, now, pass);
                }
                return;
            }
            // The marching ants follow the pointer.
            let shift = vec2((now[0] - start[0]) as f32, (now[1] - start[1]) as f32) / pass.ppp;
            ctx.data_mut(|d| d.insert_temp(shift_id(canvas), shift));
            painter.line_segment([pass.to_screen(start), pass.to_screen(now)], Stroke::new(1.0, colors.line));
            ctx.request_repaint();
        }
    }
}

/// The selected area patched from where it was dropped: the Patch tool's Normal, Source mode through a rotation of
/// the sphere from `start` to `end`. In this view the Patch tool always patches from where you drag; the status bar
/// says so when the options bar asks for another mode.
fn patch(app: &mut PhotocraftApp, start: [f64; 2], end: [f64; 2], pass: &Pass<'_>) {
    if app.ui.tool_options.patch_mode != "source" || app.ui.tool_options.patch_content_aware {
        app.ui.status = tl!("In 360° View the Patch tool patches from where you drag (Normal, Source).").to_string();
        app.ui.status_error = false;
    }
    let q = rotation_between(pass.vp.view_to_dir(start[0], start[1]), pass.vp.view_to_dir(end[0], end[1]));
    let p = json!({
        "command": "paint.patch",
        "params": {"mode": "source", "target": crate::canvas::paint_target(app)},
        "rotation": q.concat(),
        "projection": pass.projection(),
    });
    if let Err(e) = app.run("paint.projectedTool", p) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// The outline a shape being drawn shows, on screen.
fn shape_points(tool: Tool, points: &[[f64; 2]], pass: &Pass<'_>) -> Vec<Pos2> {
    let (Some(a), Some(b)) = (points.first(), points.last()) else { return Vec::new() };
    let (a, b) = (pass.to_screen(*a), pass.to_screen(*b));
    match tool {
        Tool::RectMarquee | Tool::ObjectSelection => {
            let r = Rect::from_two_pos(a, b);
            vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()]
        }
        Tool::EllipseMarquee => {
            let r = Rect::from_two_pos(a, b);
            let (c, rx, ry) = (r.center(), r.width() / 2.0, r.height() / 2.0);
            (0..=64).map(|i| std::f32::consts::TAU * i as f32 / 64.0).map(|t| c + vec2(rx * t.cos(), ry * t.sin())).collect()
        }
        _ => points.iter().map(|q| pass.to_screen(*q)).collect(),
    }
}

fn outline(painter: &egui::Painter, line: &[Pos2], colors: &super::view360::Colors) {
    if line.len() > 1 {
        painter.add(egui::Shape::line(line.to_vec(), Stroke::new(3.0, colors.halo)));
        painter.add(egui::Shape::line(line.to_vec(), Stroke::new(1.0, colors.marquee)));
    }
}

/// The marching ants of one view, cached until the document, camera or view size changes.
pub(crate) struct Ants {
    key: (usize, [u32; 3], [u32; 2]),
    /// Pixels on the selection's edge (half-resolution view), and the box they span.
    edges: Vec<(u32, u32)>,
    bbox: [u32; 4],
    texture: Option<egui::TextureHandle>,
    phase: u32,
}

#[cfg(test)]
impl Ants {
    /// Whether any edge shows.
    pub(crate) fn shows(&self) -> bool {
        !self.edges.is_empty()
    }
}

/// The ants are worked out at half the view's resolution: cheap enough to redo every frame while looking around.
const ANTS_SCALE: f32 = 2.0;

/// Draws the marching ants of document `idx`'s selection over the view.
pub(crate) fn ants(app: &mut PhotocraftApp, ctx: &egui::Context, painter: &egui::Painter, canvas: egui::Id, idx: usize, pass: &Pass<'_>) {
    let Some(st) = app.session.documents().get(idx) else { return };
    if st.doc.selection.is_none() {
        app.view360_ants = None;
        return;
    }
    let (rect, ppp, look) = (pass.rect, pass.ppp, pass.look);
    let (w, h) = (((rect.width() * ppp) / ANTS_SCALE).ceil().max(1.0) as u32, ((rect.height() * ppp) / ANTS_SCALE).ceil().max(1.0) as u32);
    let key = (std::sync::Arc::as_ptr(&st.doc) as usize, [look.yaw, look.pitch, look.fov].map(f32::to_bits), [w, h]);
    if app.view360_ants.as_ref().is_none_or(|a| a.key != key) {
        let Some(m) = photocraft_engine::projected_select::projected_selection_mask(&st.doc, look.camera(), w, h) else {
            app.view360_ants = None;
            return;
        };
        let (edges, bbox) = edges(&m, w, h);
        let texture = app.view360_ants.take().and_then(|a| a.texture);
        app.view360_ants = Some(Ants { key, edges, bbox, texture, phase: u32::MAX });
    }
    let colors = super::view360::colors(ctx);
    let Some(a) = app.view360_ants.as_mut() else { return };
    if a.edges.is_empty() {
        return;
    }
    let phase = ((ctx.input(|i| i.time) * 8.0) as u32) % 8;
    if phase != a.phase || a.texture.is_none() {
        a.phase = phase;
        let [x0, y0, x1, y1] = a.bbox;
        let (bw, bh) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut px = vec![Color32::TRANSPARENT; bw * bh];
        for &(x, y) in &a.edges {
            let on = ((x + y + phase) / 4).is_multiple_of(2);
            px[(y - y0) as usize * bw + (x - x0) as usize] = if on { colors.marquee } else { colors.halo };
        }
        let image = egui::ColorImage { size: [bw, bh], pixels: px, source_size: vec2(bw as f32, bh as f32) };
        match &mut a.texture {
            Some(t) => t.set(image, egui::TextureOptions::NEAREST),
            None => a.texture = Some(ctx.load_texture("pc-360-ants", image, egui::TextureOptions::NEAREST)),
        }
    }
    if let Some(t) = &a.texture {
        let [x0, y0, x1, y1] = a.bbox;
        let k = ANTS_SCALE / ppp;
        let shift = ctx.data(|d| d.get_temp::<egui::Vec2>(shift_id(canvas))).unwrap_or_default();
        let at = Rect::from_min_max(rect.min + vec2(x0 as f32, y0 as f32) * k, rect.min + vec2(x1 as f32, y1 as f32) * k).translate(shift);
        painter.image(t.id(), at, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(125));
}

/// Pixels on the 50% edge of mask `m` (`w` × `h`), and their bounding box `[x0, y0, x1, y1)`.
fn edges(m: &[u8], w: u32, h: u32) -> (Vec<(u32, u32)>, [u32; 4]) {
    let at = |x: i64, y: i64| -> bool { x >= 0 && y >= 0 && x < i64::from(w) && y < i64::from(h) && m[(y as usize) * w as usize + x as usize] >= 128 };
    let mut out = Vec::new();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..h {
        for x in 0..w {
            let (xi, yi) = (i64::from(x), i64::from(y));
            if at(xi, yi) && !(at(xi - 1, yi) && at(xi + 1, yi) && at(xi, yi - 1) && at(xi, yi + 1)) {
                out.push((x, y));
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    (out, [x0, y0, x1, y1])
}
