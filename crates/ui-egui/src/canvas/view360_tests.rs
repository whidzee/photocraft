use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use serde_json::json;

use super::*;

/// An app with one `w` × `h` document in 360° View, painting with `tool`.
fn app(w: u32, h: u32, tool: Tool) -> Box<PhotocraftApp> {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("file.new", json!({"width": w, "height": h})).unwrap();
    app.sync_views();
    app.ui.views[0].clear_pending();
    app.ui.tool = tool;
    command(&mut app, &json!({"on": true})).unwrap();
    app
}

const RECT: Rect = Rect { min: Pos2::ZERO, max: pos2(400.0, 300.0) };

/// Runs one pass of the document's canvas with `events`.
fn pass(app: &mut PhotocraftApp, events: Vec<Event>) {
    let ctx = app.ctx().clone();
    let input = egui::RawInput { screen_rect: Some(RECT), events, ..Default::default() };
    let mut out = ctx.run_ui(input, |ui| {
        let view = app.ui.views[0].clone();
        crate::canvas::canvas_view(app, ui, CanvasHost::main(0), RECT, view, true);
    });
    out.textures_delta.clear();
}

fn button(at: Pos2, pressed: bool) -> Event {
    Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
}

/// A primary drag along `points`, a pass per event.
fn drag(app: &mut PhotocraftApp, points: &[Pos2]) {
    pass(app, vec![Event::PointerMoved(points[0])]);
    pass(app, vec![button(points[0], true)]);
    for &p in &points[1..] {
        pass(app, vec![Event::PointerMoved(p)]);
    }
    pass(app, vec![button(points[points.len() - 1], false)]);
    pass(app, vec![]);
}

fn look(app: &PhotocraftApp) -> SphereView {
    app.ui.views[0].projection.expect("the view is in 360° View")
}

/// Dragging with the Hand tool turns the camera so the image follows the pointer: right turns it left, down turns it
/// up.
#[test]
fn a_hand_drag_turns_the_camera_against_the_drag() {
    let mut app = app(800, 400, Tool::Hand);
    let before = look(&app);
    let steps = app.session.documents()[0].history.past_len();
    let c = RECT.center();
    drag(&mut app, &[c, c + vec2(20.0, 10.0), c + vec2(60.0, 30.0)]);
    let after = look(&app);
    assert!(after.yaw < before.yaw, "yaw {} → {}", before.yaw, after.yaw);
    assert!(after.pitch > before.pitch, "pitch {} → {}", before.pitch, after.pitch);
    assert_eq!(app.session.documents()[0].history.past_len(), steps, "looking around edits nothing");
}

/// A Brush stroke in the view paints the latlong where the view looks: at the image's centre for the starting
/// camera, in one History step, over a small part of the image.
#[test]
fn a_brush_stroke_paints_where_the_view_looks() {
    let mut app = app(800, 400, Tool::Brush);
    app.session.tools.brush.size = 12.0;
    let before = app.session.documents()[0].history.past_len();
    let c = RECT.center();
    drag(&mut app, &[c, c + vec2(10.0, 0.0), c + vec2(20.0, 0.0), c + vec2(30.0, 0.0)]);
    let st = &app.session.documents()[0];
    assert_eq!(st.history.past_len(), before + 1, "the stroke is one History step");
    let damage = st.last_damage.expect("the stroke changed pixels");
    assert!(damage.contains(400, 200), "{damage:?} holds the image centre the view looks at");
    assert!(damage.width() < 200 && damage.height() < 100, "{damage:?} is a stroke, not the image");
}

/// Escape during a stroke drops it: nothing is painted.
#[test]
fn escape_drops_a_stroke_being_drawn() {
    let mut app = app(800, 400, Tool::Brush);
    let before = app.session.documents()[0].history.past_len();
    let c = RECT.center();
    pass(&mut app, vec![Event::PointerMoved(c)]);
    pass(&mut app, vec![button(c, true)]);
    pass(&mut app, vec![Event::PointerMoved(c + vec2(20.0, 0.0))]);
    pass(&mut app, vec![Event::PointerMoved(c + vec2(40.0, 0.0))]);
    let key = Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
    pass(&mut app, vec![key]);
    pass(&mut app, vec![button(c + vec2(40.0, 0.0), false)]);
    assert_eq!(app.session.documents()[0].history.past_len(), before);
}

/// The zoom commands change the field of view, and a placement waits for the flat view.
#[test]
fn view_commands_narrow_and_widen_the_field_of_view() {
    let mut view = View::default();
    view.clear_pending();
    let mut l = SphereView::default();
    view.request(ViewCommands::ZoomIn);
    take_view_commands(&mut view, &mut l, 8192.0, 1000.0);
    assert!(l.fov < DEFAULT_FOV);
    let narrow = l.fov;
    view.request(ViewCommands::ZoomOut);
    view.request(ViewCommands::ZoomOut);
    view.place(ViewCommands::FitOnScreen);
    take_view_commands(&mut view, &mut l, 8192.0, 1000.0);
    assert!(l.fov > narrow);
    assert_eq!(view.pending, vec![ViewRequest::Place(ViewCommands::FitOnScreen)], "the placement stays for the flat view");
    view.clear_pending();
    view.request(ViewCommands::ActualPixels);
    take_view_commands(&mut view, &mut l, 8192.0, 1000.0);
    // One 8192 wide latlong pixel per device pixel is a narrower view than the default.
    assert!(l.fov < DEFAULT_FOV && l.fov >= MIN_FOV as f32, "{}", l.fov);
}

/// `view.view360` sets the camera, keeps it in range, and turns the view off.
#[test]
fn the_command_sets_the_camera_and_turns_the_view_off() {
    let mut app = app(800, 400, Tool::Hand);
    let on = command(&mut app, &json!({"yaw": 270.0, "pitch": 120.0, "fov": 1.0})).unwrap();
    assert_eq!(on["on"], true);
    let l = look(&app);
    assert!(l.yaw > -180.0 && l.yaw <= 180.0 && l.pitch < 90.0 && l.fov >= MIN_FOV as f32, "{l:?}");
    assert!(command(&mut app, &json!({"yaw": "east"})).is_err());
    let off = command(&mut app, &json!({})).unwrap();
    assert_eq!(off["on"], false);
    assert!(!is_on(&app, 0));
}

/// A document that isn't 2:1 still opens in the view, with a note in the status bar.
#[test]
fn a_document_that_isnt_two_to_one_gets_a_note() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("file.new", json!({"width": 600, "height": 400})).unwrap();
    app.sync_views();
    command(&mut app, &json!({"on": true})).unwrap();
    assert!(is_on(&app, 0));
    assert!(app.ui.status.contains("2:1"), "{}", app.ui.status);
}

/// The Navigator's outline surrounds the spot the view looks at, and splits where it crosses the image's left and
/// right edges.
#[test]
fn the_navigator_outline_surrounds_the_look_and_splits_at_the_seam() {
    let rect = Rect::from_min_size(Pos2::ZERO, vec2(200.0, 100.0));
    let ahead = outline(&SphereView::default(), [1200.0, 800.0], [8192.0, 4096.0], rect);
    assert_eq!(ahead.len(), 1, "looking at the centre the outline is one closed line");
    let points: Vec<Pos2> = ahead.concat();
    let bounds = Rect::from_points(&points);
    assert!(bounds.contains(rect.center()) && bounds.width() < rect.width(), "{bounds:?}");
    let behind = outline(&SphereView { yaw: 180.0, ..SphereView::default() }, [1200.0, 800.0], [8192.0, 4096.0], rect);
    assert!(behind.len() >= 2, "looking at the seam the outline is broken there: {} lines", behind.len());
}
