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

/// The direction a camera shows at view pixel `at` of `RECT` (device pixels at scale 1).
fn dir_at(l: &SphereView, at: Pos2) -> [f64; 3] {
    ViewProjection::new(l.camera(), f64::from(RECT.width()), f64::from(RECT.height())).view_to_dir(f64::from(at.x), f64::from(at.y))
}

fn angle(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).clamp(-1.0, 1.0).acos().to_degrees()
}

/// Holding a direction at a view pixel turns the camera so that pixel shows it, wherever both are.
#[test]
fn hold_puts_the_direction_under_the_pixel() {
    let size = [f64::from(RECT.width()), f64::from(RECT.height())];
    for (start, dir_look, at) in [
        (SphereView::default(), SphereView { yaw: 40.0, pitch: 20.0, fov: 90.0 }, pos2(320.0, 60.0)),
        (SphereView { yaw: 170.0, pitch: -30.0, fov: 120.0 }, SphereView { yaw: -170.0, pitch: -10.0, fov: 120.0 }, pos2(20.0, 250.0)),
        (SphereView { yaw: 0.0, pitch: 0.0, fov: 30.0 }, SphereView { yaw: 5.0, pitch: 3.0, fov: 30.0 }, pos2(200.0, 150.0)),
    ] {
        let dir = dir_at(&dir_look, RECT.center());
        let mut l = start;
        l.hold(dir, [f64::from(at.x), f64::from(at.y)], size);
        assert!(angle(dir_at(&l, at), dir) < 0.01, "{start:?} → {l:?}: {}°", angle(dir_at(&l, at), dir));
    }
}

/// Grab Image keeps the pressed spot under the pointer; Turn Camera turns the view the way the pointer goes.
#[test]
fn the_drag_preference_grabs_or_steers() {
    let c = RECT.center();
    let path = [c, c + vec2(20.0, 10.0), c + vec2(90.0, 40.0)];
    let mut grab = app(800, 400, Tool::Hand);
    let pressed = dir_at(&look(&grab), c);
    drag(&mut grab, &path);
    assert!(angle(dir_at(&look(&grab), path[2]), pressed) < 0.05, "the pressed spot stays under the pointer");
    assert!(look(&grab).yaw < 0.0);

    let mut turn = app(800, 400, Tool::Hand);
    turn.run("prefs.set", json!({"path": "tools.sphereDrag", "value": "turnCamera"})).unwrap();
    drag(&mut turn, &path);
    assert!(look(&turn).yaw > 0.0 && look(&turn).pitch < 0.0, "{:?}", look(&turn));
}

/// The wheel narrows the field of view about the pointer: the spot under it stays put.
#[test]
fn the_wheel_zooms_about_the_pointer() {
    let mut app = app(800, 400, Tool::Hand);
    let at = pos2(330.0, 70.0);
    let before = dir_at(&look(&app), at);
    let fov = look(&app).fov;
    pass(&mut app, vec![Event::PointerMoved(at)]);
    pass(
        &mut app,
        vec![Event::MouseWheel {
            source: egui::MouseWheelSource::Unknown,
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, 120.0),
            modifiers: Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        }],
    );
    for _ in 0..10 {
        pass(&mut app, vec![]);
    }
    assert!(look(&app).fov < fov);
    assert!(angle(dir_at(&look(&app), at), before) < 0.1, "{}°", angle(dir_at(&look(&app), at), before));
}

/// Showing the Navigator leaves a camera it wasn't asked to change alone, even between its fields' rounding: writing
/// what the fields show back every pass would make the view jump while it is dragged.
#[test]
fn the_navigator_leaves_an_untouched_camera_alone() {
    let mut app = app(800, 400, Tool::Hand);
    command(&mut app, &json!({"yaw": 12.37, "pitch": -3.81, "fov": 63.3})).unwrap();
    let before = look(&app);
    let ctx = app.ctx().clone();
    for _ in 0..3 {
        let input = egui::RawInput { screen_rect: Some(RECT), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| navigator(&mut app, ui, 0));
        out.textures_delta.clear();
    }
    assert_eq!(look(&app), before);
}

/// Grab Image never flips the camera over: a long drag downward from a view looking well up stops at the pitch limit
/// and keeps the image upright, rather than jumping to the other solution.
#[test]
fn a_grab_drag_stays_on_the_near_pitch() {
    let size = [f64::from(RECT.width()), f64::from(RECT.height())];
    let mut l = SphereView { pitch: 60.0, ..SphereView::default() };
    let dir = dir_at(&l, pos2(200.0, 100.0));
    let mut last = l.pitch;
    for y in (100..=290).step_by(10) {
        l.hold(dir, [200.0, f64::from(y)], size);
        assert!((l.pitch - last).abs() < 15.0, "pitch jumped {last} → {}", l.pitch);
        last = l.pitch;
    }
}
