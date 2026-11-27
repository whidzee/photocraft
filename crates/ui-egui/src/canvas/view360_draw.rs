//! The Type and shape tools in View › 360° View (#15726). Text is typed at the click and shapes are dragged out in
//! the view, with the options bar's settings; `layer.projectedRender` renders each upright where it was made in the
//! view and places it on the latlong as a new pixel layer. A latlong can't hold an editable text or shape layer that
//! looks right on the sphere, so the layer is pixels from the start.
//!
//! Typing: a click opens a text field at the click; Enter places the text, Escape drops it, and a click elsewhere
//! places it and starts new text there. Shapes: Shift draws a square box, Alt draws from the centre. The Custom
//! Shape tool isn't one of them yet.

use craft::behavior::kurbo::Point;
use craft::draw::geometry::shape::{BoxRule, MIN_SIDE, ShapeGeometry, box_drag};
use craft::ui::contract::{Bind, Show as _};
use craft::ui::text::TextField;
use egui::{PointerButton, Pos2, Rect, Stroke, vec2};
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;

use super::view360_select::Pass;

/// The tools this module runs.
pub(crate) fn supported(tool: Tool) -> bool {
    types(tool) || shapes(tool)
}

fn types(tool: Tool) -> bool {
    matches!(tool, Tool::Type | Tool::VerticalType)
}

fn shapes(tool: Tool) -> bool {
    matches!(tool, Tool::Rectangle | Tool::EllipseShape | Tool::Triangle | Tool::Polygon | Tool::Star | Tool::Line)
}

/// Text being typed: where it starts in the view (device pixels) and on screen, and the text so far.
#[derive(Clone, Debug)]
struct Typing {
    at: [f64; 2],
    screen: Pos2,
    text: String,
    /// The field still has to take focus.
    fresh: bool,
}

fn typing_id(canvas: egui::Id) -> egui::Id {
    canvas.with("360-typing")
}

fn shape_id(canvas: egui::Id) -> egui::Id {
    canvas.with("360-shape")
}

/// Whether text is being typed in the view of `canvas`.
pub(crate) fn typing(ctx: &egui::Context, canvas: egui::Id) -> bool {
    ctx.data(|d| d.get_temp::<Typing>(typing_id(canvas))).is_some()
}

/// Places `command` with `params` (view pixels) on the latlong through `layer.projectedRender`.
fn render(app: &mut PhotocraftApp, command: &str, params: serde_json::Value, pass: &Pass<'_>) {
    if let Err(e) = app.run("layer.projectedRender", json!({"command": command, "params": params, "projection": pass.projection()})) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// Places the text being typed in view `canvas`, if there is any, and ends the typing.
pub(crate) fn finish_typing(app: &mut PhotocraftApp, ctx: &egui::Context, canvas: egui::Id, pass: &Pass<'_>) {
    let Some(t) = ctx.data(|d| d.get_temp::<Typing>(typing_id(canvas))) else { return };
    ctx.data_mut(|d| d.remove::<Typing>(typing_id(canvas)));
    if t.text.trim().is_empty() {
        return;
    }
    let params = crate::type_tool::point_text_params(app, t.at[0], t.at[1], &t.text);
    render(app, "type.create", params, pass);
}

/// Runs the Type or a shape tool in view `canvas`.
pub(crate) fn tool(app: &mut PhotocraftApp, ui: &mut egui::Ui, response: &egui::Response, painter: &egui::Painter, canvas: egui::Id, pass: &Pass<'_>) {
    let tool = app.ui.tool;
    let ctx = &ui.ctx().clone();
    if types(tool) {
        type_text(app, ui, response, canvas, pass);
    } else {
        shape(app, ctx, response, painter, canvas, pass, tool);
    }
}

fn type_text(app: &mut PhotocraftApp, ui: &mut egui::Ui, response: &egui::Response, canvas: egui::Id, pass: &Pass<'_>) {
    let ctx = &ui.ctx().clone();
    if response.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::Text);
    }
    if response.clicked_by(PointerButton::Primary)
        && let Some(p) = response.interact_pointer_pos()
    {
        // A click elsewhere places the text being typed and starts new text there.
        finish_typing(app, ctx, canvas, pass);
        ctx.data_mut(|d| d.insert_temp(typing_id(canvas), Typing { at: pass.to_view(p), screen: p, text: String::new(), fresh: true }));
    }
    let Some(mut t) = ctx.data(|d| d.get_temp::<Typing>(typing_id(canvas))) else { return };
    let mut done = false;
    let mut dropped = false;
    // The field sits just under the click, in the view.
    let at = Rect::from_min_size(t.screen + vec2(0.0, 4.0), vec2(260.0, 40.0)).intersect(pass.rect);
    ui.scope_builder(egui::UiBuilder::new().max_rect(at).id_salt(canvas.with("360-typing-field")), |ui| {
        let out = TextField::new(tl!("Text"), Bind::bound(&mut t.text)).placeholder(tl!("Type, then press Enter")).show(ui);
        // The keyboard goes to the field as it first shows.
        if t.fresh {
            t.fresh = !craft::ui::runtime::focus::request(ui.ctx(), craft::ui::runtime::focus::FocusTarget::Id(out.response.id));
        }
        done = out.submitted;
        dropped = out.escaped;
    });
    if dropped {
        ctx.data_mut(|d| d.remove::<Typing>(typing_id(canvas)));
        return;
    }
    ctx.data_mut(|d| d.insert_temp(typing_id(canvas), t));
    if done {
        finish_typing(app, ctx, canvas, pass);
    }
}

/// The geometry a drag from `start` to `end` (view pixels) draws with `tool` and the options bar's settings: Shift
/// squares the box and Alt draws it from the centre. `None` for a drag too small to draw.
fn geometry(app: &PhotocraftApp, tool: Tool, start: [f64; 2], end: [f64; 2], modifiers: egui::Modifiers) -> Option<ShapeGeometry> {
    let o = &app.ui.tool_options;
    let (a, b) = (Point::new(start[0], start[1]), Point::new(end[0], end[1]));
    if tool == Tool::Line {
        return (a.distance(b) >= MIN_SIDE).then(|| ShapeGeometry::Line { from: a, to: b, weight: f64::from(o.line_weight).max(1.0) });
    }
    let mut rule = BoxRule::default();
    rule.square = modifiers.shift;
    rule.from_centre = modifiers.alt;
    let rect = box_drag(a, b, rule)?;
    Some(match tool {
        Tool::Rectangle => ShapeGeometry::Rect { rect, radii: [f64::from(o.corner_radius).max(0.0); 4] },
        Tool::EllipseShape => ShapeGeometry::Ellipse { rect },
        Tool::Triangle => ShapeGeometry::Triangle { rect },
        // As the flat Star tool: five points, the inner radius half the outer.
        Tool::Star => ShapeGeometry::Polygon { rect, sides: 5, star_ratio: 0.5 },
        Tool::Polygon => ShapeGeometry::Polygon { rect, sides: o.polygon_sides, star_ratio: 1.0 },
        _ => return None,
    })
}

fn shape(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, painter: &egui::Painter, canvas: egui::Id, pass: &Pass<'_>, tool: Tool) {
    if response.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let id = shape_id(canvas);
    // A drag whose release this view never saw makes nothing.
    if !ctx.input(|i| i.pointer.any_down()) && !response.drag_stopped() {
        ctx.data_mut(|d| d.remove::<[f64; 2]>(id));
    }
    if response.drag_started_by(PointerButton::Primary)
        && let Some(p) = ctx.input(|i| i.pointer.press_origin())
    {
        ctx.data_mut(|d| d.insert_temp(id, pass.to_view(p)));
    }
    let Some(start) = ctx.data(|d| d.get_temp::<[f64; 2]>(id)) else { return };
    let Some(now) = response.interact_pointer_pos() else { return };
    let end = pass.to_view(now);
    let modifiers = ctx.input(|i| i.modifiers);
    let drawn = geometry(app, tool, start, end, modifiers);
    if response.drag_stopped() {
        ctx.data_mut(|d| d.remove::<[f64; 2]>(id));
        let Some(g) = drawn else { return };
        match crate::vector_ui::tool_create_params(app, tool, &g) {
            Ok(params) => render(app, "shape.create", params, pass),
            Err(e) => {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
        return;
    }
    // The shape's box (a line's ends) while it is dragged.
    if let Some(g) = drawn {
        let colors = super::view360::colors(ctx);
        let b = g.bounds();
        let r = Rect::from_two_pos(pass.to_screen([b.x0, b.y0]), pass.to_screen([b.x1, b.y1]));
        for (width, color) in [(3.0, colors.halo), (1.0, colors.line)] {
            let s = Stroke::new(width, color);
            match g {
                ShapeGeometry::Line { from, to, .. } => {
                    painter.line_segment([pass.to_screen([from.x, from.y]), pass.to_screen([to.x, to.y])], s);
                }
                ShapeGeometry::Ellipse { .. } => {
                    painter.add(egui::Shape::ellipse_stroke(r.center(), r.size() / 2.0, s));
                }
                _ => {
                    painter.rect_stroke(r, 0.0, s, egui::StrokeKind::Inside);
                }
            }
        }
    }
    ctx.request_repaint();
}
