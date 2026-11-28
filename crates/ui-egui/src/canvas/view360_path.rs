//! The Pen tools in View › 360° View (#15726). The Pen, Freeform Pen and Curvature Pen draw in the view and store the
//! path on the latlong where it was drawn (`path.projectedSet`); the Work Path is drawn back through the view. Path
//! Selection turns the path across the sphere, Direct Selection drags anchors and handles, and the Add, Delete and
//! Convert Point tools edit its points. Make Selection and Fill Path count the path across the ±180° seam (`wrap`),
//! and Stroke Path strokes it with the brush as it is seen in the view (`paint.projectedStroke`).
//!
//! The Pen: a click adds a corner point, a drag pulls a smooth one; clicking the first point closes the path, and
//! Enter or Escape keeps an open one. The Curvature Pen's points flow through a smooth curve. The Freeform Pen keeps
//! the drawn line; ending near its start closes it.

use egui::{PointerButton, Pos2, Stroke, vec2};
use photocraft_doc::Path;
use photocraft_engine::equirect::{ViewProjection, equirect_to_dir, rotation_between};
use photocraft_engine::projected_path::{rotate_path, view_polylines, view_to_equirect_near};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::Tool;

use super::view360::Colors;
use super::view360_select::Pass;

/// The tools this module runs.
pub(crate) fn supported(tool: Tool) -> bool {
    matches!(
        tool,
        Tool::Pen
            | Tool::FreeformPen
            | Tool::CurvaturePen
            | Tool::AddAnchorPoint
            | Tool::DeleteAnchorPoint
            | Tool::ConvertPoint
            | Tool::PathSelection
            | Tool::DirectSelection
    )
}

/// A point being drawn, in view device pixels: the anchor and its handles.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ViewKnot {
    a: [f64; 2],
    i: [f64; 2],
    o: [f64; 2],
    smooth: bool,
}

/// A path being drawn.
#[derive(Clone, Debug, Default, PartialEq)]
struct Draft {
    knots: Vec<ViewKnot>,
    /// The Pen: a drag is pulling the last point's handles.
    pulling: bool,
    /// The Freeform Pen: the line so far.
    free: Vec<[f64; 2]>,
}

/// What a Path Selection or Direct Selection drag holds.
#[derive(Clone, Copy, Debug)]
enum Grab {
    /// The whole path, held at this direction.
    Path([f64; 3]),
    Anchor(usize, usize),
    /// A handle: its subpath, knot, and whether it is the out handle.
    Handle(usize, usize, bool),
}

fn draft_id(canvas: egui::Id) -> egui::Id {
    canvas.with("360-pen")
}

fn grab_id(canvas: egui::Id) -> egui::Id {
    canvas.with("360-path-grab")
}

fn moved_id(canvas: egui::Id) -> egui::Id {
    canvas.with("360-path-moved")
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// The point at `t` on cubic `[p0, c1, c2, p1]`.
fn bezier(s: [[f64; 2]; 4], t: f64) -> [f64; 2] {
    let mt = 1.0 - t;
    let (a, b, c, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
    [a * s[0][0] + b * s[1][0] + c * s[2][0] + d * s[3][0], a * s[0][1] + b * s[1][1] + c * s[2][1] + d * s[3][1]]
}

/// The Curvature Pen's handles, so its curve flows smoothly through `points`.
fn curvature(points: &[[f64; 2]]) -> Vec<ViewKnot> {
    let n = points.len();
    (0..n)
        .map(|i| {
            let a = points[i];
            if n < 3 || i == 0 || i == n - 1 {
                return ViewKnot { a, i: a, o: a, smooth: n >= 3 };
            }
            let (p, q) = (points[i - 1], points[i + 1]);
            let t = [(q[0] - p[0]) / 6.0, (q[1] - p[1]) / 6.0];
            ViewKnot { a, i: [a[0] - t[0], a[1] - t[1]], o: [a[0] + t[0], a[1] + t[1]], smooth: true }
        })
        .collect()
}

fn report(app: &mut PhotocraftApp, result: Result<Value, String>) {
    if let Err(e) = result {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// Stores a drawn path on the latlong, added to the Work Path when there is one.
fn commit(app: &mut PhotocraftApp, pass: &Pass<'_>, knots: &[ViewKnot], closed: bool) {
    if knots.len() < 2 {
        return;
    }
    let k: Vec<Value> = knots.iter().map(|k| json!({"anchor": k.a, "in": k.i, "out": k.o, "smooth": k.smooth})).collect();
    let mut p = json!({"path": {"subpaths": [{"closed": closed, "knots": k}]}, "projection": pass.projection()});
    if app.session.active().is_some_and(|d| d.doc.work_path.is_some()) {
        p["op"] = json!("combine");
    }
    let r = app.run("path.projectedSet", p);
    if r.is_ok() {
        app.ui.selected_path = Some("work".into());
    }
    report(app, r);
}

/// Each point of the Work Path in the view: its subpath and knot, and its anchor and handles where the view sees them.
type ViewPoint = (usize, usize, Option<[f64; 2]>, Option<[f64; 2]>, Option<[f64; 2]>);

fn view_knots(app: &PhotocraftApp, vp: &ViewProjection) -> Vec<ViewPoint> {
    let Some(st) = app.session.active() else { return Vec::new() };
    let Some(path) = st.doc.work_path.as_ref() else { return Vec::new() };
    let (iw, ih) = (f64::from(st.doc.size.width), f64::from(st.doc.size.height));
    let v = |p: photocraft_geom::Point| vp.dir_to_view(equirect_to_dir(p.x, p.y, iw, ih));
    let mut out = Vec::new();
    for (si, sp) in path.subpaths.iter().enumerate() {
        for (ki, k) in sp.knots.iter().enumerate() {
            out.push((si, ki, v(k.anchor), v(k.in_ctrl), v(k.out_ctrl)));
        }
    }
    out
}

/// Draws the Work Path (or the path being dragged) and the path being drawn over the view of `canvas`, while a Pen,
/// path or shape tool is selected and View › Show › Target Path is on, as on the flat canvas.
pub(crate) fn draw(app: &PhotocraftApp, ctx: &egui::Context, painter: &egui::Painter, canvas: egui::Id, idx: usize, pass: &Pass<'_>, colors: &Colors) {
    let tool = app.ui.tool;
    let line = |pts: Vec<Pos2>, closed: bool| {
        for (width, color) in [(3.0, colors.halo), (1.0, colors.marquee)] {
            let s = Stroke::new(width, color);
            painter.add(if closed { egui::Shape::closed_line(pts.clone(), s) } else { egui::Shape::line(pts.clone(), s) });
        }
    };
    let live = live_path(ctx, canvas);
    let path_tool = supported(tool) || crate::vector_ui::is_shape_tool(tool);
    let show = live.is_some() || (path_tool && app.ui.view.shows(app.ui.view.show.target_path));
    if show
        && let Some(st) = app.session.documents().get(idx)
        && let Some(path) = live.as_ref().or(st.doc.work_path.as_ref())
    {
        let (iw, ih) = (f64::from(st.doc.size.width), f64::from(st.doc.size.height));
        for (pts, closed) in view_polylines(path, pass.vp, iw, ih) {
            line(pts.into_iter().map(|q| pass.to_screen(q)).collect(), closed);
        }
        if supported(tool) {
            for (_, _, a, i, o) in view_knots(app, pass.vp) {
                let Some(a) = a else { continue };
                if matches!(tool, Tool::DirectSelection | Tool::ConvertPoint) {
                    for h in [i, o].into_iter().flatten().filter(|h| dist(*h, a) > 0.5) {
                        painter.line_segment([pass.to_screen(a), pass.to_screen(h)], Stroke::new(1.0, colors.marquee));
                        painter.circle_filled(pass.to_screen(h), 2.5, colors.marquee);
                    }
                }
                let r = egui::Rect::from_center_size(pass.to_screen(a), vec2(5.0, 5.0));
                painter.rect_filled(r, 0.0, colors.halo);
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, colors.marquee), egui::StrokeKind::Inside);
            }
        }
    }
    let Some(d) = ctx.data(|x| x.get_temp::<Draft>(draft_id(canvas))) else { return };
    for w in d.knots.windows(2) {
        let s = [w[0].a, w[0].o, w[1].i, w[1].a];
        line((0..=16).map(|i| pass.to_screen(bezier(s, f64::from(i) / 16.0))).collect(), false);
    }
    for k in &d.knots {
        painter.rect_filled(egui::Rect::from_center_size(pass.to_screen(k.a), vec2(5.0, 5.0)), 0.0, colors.marquee);
    }
    if d.free.len() >= 2 {
        line(d.free.iter().map(|q| pass.to_screen(*q)).collect(), false);
    }
}

/// Runs a Pen or path tool in the view of `canvas`.
pub(crate) fn tool(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, canvas: egui::Id, pass: &Pass<'_>) {
    let tool = app.ui.tool;
    let tol = f64::from(6.0 * pass.ppp);
    if response.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let (enter, esc) = ctx.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
    let id = draft_id(canvas);
    let mut d: Draft = ctx.data(|x| x.get_temp(id)).unwrap_or_default();
    match tool {
        Tool::Pen | Tool::CurvaturePen => {
            if (enter || esc) && !d.knots.is_empty() {
                commit(app, pass, &d.knots, false);
                ctx.data_mut(|x| x.remove::<Draft>(id));
                return;
            }
            if (response.drag_started_by(PointerButton::Primary) || (response.clicked_by(PointerButton::Primary) && !d.pulling))
                && let Some(p) = ctx.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos())
            {
                let q = pass.to_view(p);
                if d.knots.len() >= 2 && dist(q, d.knots[0].a) <= tol {
                    // Back on the first point: close the path and keep it.
                    commit(app, pass, &d.knots, true);
                    ctx.data_mut(|x| x.remove::<Draft>(id));
                    return;
                }
                d.knots.push(ViewKnot { a: q, i: q, o: q, smooth: false });
                if tool == Tool::CurvaturePen {
                    let points: Vec<[f64; 2]> = d.knots.iter().map(|k| k.a).collect();
                    d.knots = curvature(&points);
                }
                d.pulling = tool == Tool::Pen && response.drag_started_by(PointerButton::Primary);
            }
            if d.pulling
                && response.dragged_by(PointerButton::Primary)
                && let (Some(p), Some(k)) = (response.interact_pointer_pos(), d.knots.last_mut())
            {
                // Dragging out of a new point pulls both handles: a smooth point.
                let q = pass.to_view(p);
                *k = ViewKnot { a: k.a, o: q, i: [2.0 * k.a[0] - q[0], 2.0 * k.a[1] - q[1]], smooth: true };
            }
            if response.drag_stopped() {
                d.pulling = false;
            }
        }
        Tool::FreeformPen => {
            if let Some(p) = response.interact_pointer_pos()
                && (response.drag_started_by(PointerButton::Primary) || response.dragged_by(PointerButton::Primary))
            {
                let q = pass.to_view(p);
                if d.free.last().is_none_or(|l| dist(*l, q) >= 6.0) {
                    d.free.push(q);
                }
            }
            if response.drag_stopped() {
                let knots: Vec<ViewKnot> = d.free.iter().map(|&a| ViewKnot { a, i: a, o: a, smooth: false }).collect();
                let closed = d.free.len() > 3 && d.free.first().zip(d.free.last()).is_some_and(|(a, b)| dist(*a, *b) <= tol);
                commit(app, pass, &knots, closed);
                ctx.data_mut(|x| x.remove::<Draft>(id));
                return;
            }
        }
        _ => {
            edit(app, ctx, response, canvas, pass, tol);
            return;
        }
    }
    // Repaint only when the draft changed: egui repaints on pointer moves anyway.
    let changed = ctx.data(|x| x.get_temp::<Draft>(id)) != Some(d.clone());
    ctx.data_mut(|x| x.insert_temp(id, d));
    if changed {
        ctx.request_repaint();
    }
}

/// Path Selection, Direct Selection and the Add, Delete and Convert Point tools on the Work Path.
fn edit(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, canvas: egui::Id, pass: &Pass<'_>, tol: f64) {
    let tool = app.ui.tool;
    let vp = pass.vp;
    let Some(st) = app.session.active() else { return };
    let (iw, ih) = (f64::from(st.doc.size.width), f64::from(st.doc.size.height));
    let Some(path) = st.doc.work_path.clone() else { return };
    let knots = view_knots(app, vp);
    let near_anchor =
        |q: [f64; 2]| knots.iter().filter_map(|(s, k, a, ..)| a.map(|a| (dist(a, q), *s, *k))).filter(|(d, ..)| *d <= tol).min_by(|a, b| a.0.total_cmp(&b.0));
    let near_handle = |q: [f64; 2]| {
        knots
            .iter()
            .flat_map(|(s, k, a, i, o)| {
                [(i, false), (o, true)]
                    .into_iter()
                    .filter_map(move |(h, out)| h.filter(|h| a.is_none_or(|a| dist(a, *h) > 0.5)).map(|h| (dist(h, q), *s, *k, out)))
            })
            .filter(|(d, ..)| *d <= tol)
            .min_by(|a, b| a.0.total_cmp(&b.0))
    };
    if response.clicked_by(PointerButton::Primary)
        && let Some(p) = response.interact_pointer_pos()
    {
        let q = pass.to_view(p);
        let r = match tool {
            Tool::DeleteAnchorPoint => near_anchor(q).map(|(_, s, k)| app.run("path.deleteAnchor", json!({"subpath": s, "knot": k}))),
            Tool::ConvertPoint => near_anchor(q).map(|(_, s, k)| app.run("path.convertPoint", json!({"subpath": s, "knot": k}))),
            Tool::AddAnchorPoint => {
                // The nearest point on a segment, sampled through the view.
                let mut best: Option<(f64, usize, usize, f64)> = None;
                for (si, sp) in path.subpaths.iter().enumerate() {
                    for (ki, s) in sp.segments().iter().enumerate() {
                        let s = s.map(|p| [p.x, p.y]);
                        for i in 1..32 {
                            let t = f64::from(i) / 32.0;
                            let [x, y] = bezier(s, t);
                            if let Some(v) = vp.dir_to_view(equirect_to_dir(x, y, iw, ih)) {
                                let d = dist(v, q);
                                if d <= tol && best.is_none_or(|b| d < b.0) {
                                    best = Some((d, si, ki, t));
                                }
                            }
                        }
                    }
                }
                best.map(|(_, s, k, t)| app.run("path.addAnchor", json!({"subpath": s, "knot": k, "t": t})))
            }
            _ => None,
        };
        if let Some(r) = r {
            report(app, r);
        }
        return;
    }
    if response.drag_started_by(PointerButton::Primary)
        && let Some(p) = ctx.input(|i| i.pointer.press_origin())
    {
        let q = pass.to_view(p);
        let g = match tool {
            Tool::PathSelection => Some(Grab::Path(vp.view_to_dir(q[0], q[1]))),
            Tool::DirectSelection | Tool::ConvertPoint => {
                near_handle(q).map(|(_, s, k, o)| Grab::Handle(s, k, o)).or_else(|| near_anchor(q).map(|(_, s, k)| Grab::Anchor(s, k)))
            }
            _ => None,
        };
        if let Some(g) = g {
            ctx.data_mut(|x| x.insert_temp(grab_id(canvas), (g, path.clone())));
        }
    }
    let Some((g, orig)) = ctx.data(|x| x.get_temp::<(Grab, Path)>(grab_id(canvas))) else { return };
    let Some(p) = response.interact_pointer_pos() else { return };
    let q = pass.to_view(p);
    // Each pass restates the drag from where it began; the release is one step.
    let moved = match g {
        Grab::Path(from) => rotate_path(&orig, &rotation_between(from, vp.view_to_dir(q[0], q[1])), iw, ih),
        Grab::Anchor(s, k) => {
            let mut np = orig.clone();
            if let Some(kn) = np.subpaths.get_mut(s).and_then(|sp| sp.knots.get_mut(k)) {
                let to = view_to_equirect_near(vp, q, iw, ih, kn.anchor.x);
                let (dx, dy) = (to[0] - kn.anchor.x, to[1] - kn.anchor.y);
                for c in [&mut kn.anchor, &mut kn.in_ctrl, &mut kn.out_ctrl] {
                    c.x += dx;
                    c.y += dy;
                }
            }
            np
        }
        Grab::Handle(s, k, out) => {
            let mut np = orig.clone();
            if let Some(kn) = np.subpaths.get_mut(s).and_then(|sp| sp.knots.get_mut(k)) {
                let to = view_to_equirect_near(vp, q, iw, ih, kn.anchor.x);
                let to = photocraft_geom::Point::new(to[0], to[1]);
                // A smooth point keeps its other handle opposite; the Convert Point tool breaks it.
                let other = if out { kn.in_ctrl } else { kn.out_ctrl };
                let len = (other.x - kn.anchor.x).hypot(other.y - kn.anchor.y);
                let dl = (to.x - kn.anchor.x).hypot(to.y - kn.anchor.y).max(1e-9);
                let mirrored = photocraft_geom::Point::new(kn.anchor.x - (to.x - kn.anchor.x) / dl * len, kn.anchor.y - (to.y - kn.anchor.y) / dl * len);
                let keep = kn.smooth && tool == Tool::DirectSelection;
                if out {
                    kn.out_ctrl = to;
                    if keep {
                        kn.in_ctrl = mirrored;
                    }
                } else {
                    kn.in_ctrl = to;
                    if keep {
                        kn.out_ctrl = mirrored;
                    }
                }
                if tool == Tool::ConvertPoint {
                    kn.smooth = false;
                }
            }
            np
        }
    };
    if response.drag_stopped() {
        ctx.data_mut(|x| {
            x.remove::<(Grab, Path)>(grab_id(canvas));
            x.remove::<Path>(moved_id(canvas));
        });
        let r = app.run("path.set", json!({"name": "work", "path": photocraft_engine::vector_cmds::path_json(&moved)}));
        report(app, r);
    } else if response.dragged() {
        // Shown in place of the stored path until the release.
        ctx.data_mut(|x| x.insert_temp(moved_id(canvas), moved));
        ctx.request_repaint();
    }
}

/// The path being dragged in the view of `canvas`, drawn instead of the stored one.
fn live_path(ctx: &egui::Context, canvas: egui::Id) -> Option<Path> {
    if ctx.data(|x| x.get_temp::<(Grab, Path)>(grab_id(canvas))).is_none() {
        ctx.data_mut(|x| x.remove::<Path>(moved_id(canvas)));
        return None;
    }
    ctx.data(|x| x.get_temp(moved_id(canvas)))
}

/// Make Selection, Fill Path and Stroke Path while the active document is in 360° View: the first two count the
/// path across the ±180° seam, and Stroke Path strokes it with the brush as the view sees it.
pub(crate) fn intercept(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !matches!(id, "path.toSelection" | "path.fill" | "path.stroke") || params.get("wrap").is_some() {
        return None;
    }
    let idx = app.session.active_index()?;
    let look = app.ui.views.get(idx)?.projection?;
    if id != "path.stroke" {
        let mut p = params.clone();
        p["wrap"] = json!(true);
        return Some(app.run(id, p));
    }
    let ppp = app.canvas_ppp();
    let (w, h) = (f64::from(app.last_canvas_rect.width() * ppp), f64::from(app.last_canvas_rect.height() * ppp));
    if w < 1.0 || h < 1.0 {
        return None;
    }
    let vp = ViewProjection::new(look.camera(), w, h);
    let st = app.session.active()?;
    let (iw, ih) = (f64::from(st.doc.size.width), f64::from(st.doc.size.height));
    let path = match params.get("name").and_then(Value::as_str) {
        Some(n) if !n.eq_ignore_ascii_case("work") => st.doc.paths.iter().find(|q| q.name == n).map(|q| q.path.clone()),
        _ => st.doc.work_path.clone(),
    }?;
    let projection = json!({"yaw": look.yaw, "pitch": look.pitch, "fov": look.fov, "width": w, "height": h});
    let mut last = Ok(Value::Null);
    for (pts, closed) in view_polylines(&path, &vp, iw, ih) {
        let mut pts: Vec<[f64; 3]> = pts.iter().map(|q| [q[0], q[1], 1.0]).collect();
        if closed && let Some(f) = pts.first().copied() {
            pts.push(f);
        }
        let q = json!({"erase": false, "zoom": 1.0, "target": crate::canvas::paint_target(app), "points": pts, "projection": projection});
        last = app.run("paint.projectedStroke", q);
        if last.is_err() {
            break;
        }
    }
    Some(last)
}
