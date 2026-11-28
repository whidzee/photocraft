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
    // The first-time look-around note would sit over the view.
    app.session.edit_prefs(|p| {
        p.dialogs.insert(super::LOOK_HINT.into(), json!({"skip": true}));
    });
    command(&mut app, &json!({"on": true})).unwrap();
    app
}

const RECT: Rect = Rect { min: Pos2::ZERO, max: pos2(400.0, 300.0) };

/// Runs one pass of the document's canvas with `events`.
fn pass(app: &mut PhotocraftApp, events: Vec<Event>) {
    pass_held(app, events, Modifiers::NONE);
}

/// [`pass`] with `modifiers` held.
fn pass_held(app: &mut PhotocraftApp, mut events: Vec<Event>, modifiers: Modifiers) {
    let ctx = app.ctx().clone();
    events.insert(0, Event::ModifiersChanged(modifiers));
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
        (SphereView::default(), SphereView { yaw: 40.0, pitch: 20.0, fov: 90.0, ..SphereView::default() }, pos2(320.0, 60.0)),
        (
            SphereView { yaw: 170.0, pitch: -30.0, fov: 120.0, ..SphereView::default() },
            SphereView { yaw: -170.0, pitch: -10.0, fov: 120.0, ..SphereView::default() },
            pos2(20.0, 250.0),
        ),
        (
            SphereView { yaw: 0.0, pitch: 0.0, fov: 30.0, ..SphereView::default() },
            SphereView { yaw: 5.0, pitch: 3.0, fov: 30.0, ..SphereView::default() },
            pos2(200.0, 150.0),
        ),
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

/// The active layer's pixel at latlong `(u, v)`.
fn pixel(app: &PhotocraftApp, u: f64, v: f64) -> [f32; 4] {
    let st = &app.session.documents()[0];
    st.doc.layer(st.active_layer().unwrap()).unwrap().surface().unwrap().rgba(u as i32, v as i32)
}

/// The latlong pixel the starting camera shows at view point `at`.
fn latlong_at(at: Pos2) -> [f64; 2] {
    ViewProjection::new(SphereView::default().camera(), f64::from(RECT.width()), f64::from(RECT.height())).view_to_equirect(
        f64::from(at.x),
        f64::from(at.y),
        800.0,
        400.0,
    )
}

/// Fills a 24 px square of the latlong around `(u, v)` with `color`.
fn spot(app: &mut PhotocraftApp, [u, v]: [f64; 2], color: &str) {
    app.run("select.rect", json!({"x": u - 12.0, "y": v - 12.0, "width": 24, "height": 24})).unwrap();
    app.run("edit.fill", json!({"color": color})).unwrap();
    app.run("select.deselect", json!({})).unwrap();
}

/// The Clone Stamp paints from the source Alt-click set: the red under the source lands under the stroke.
#[test]
fn the_clone_stamp_paints_from_the_alt_click_source() {
    let mut app = app(800, 400, Tool::CloneStamp);
    app.session.tools.brush.size = 16.0;
    let (source, dest) = (pos2(300.0, 150.0), pos2(110.0, 150.0));
    spot(&mut app, latlong_at(source), "#ff0000");
    let [u, v] = latlong_at(dest);
    assert!(pixel(&app, u, v)[1] > 0.9, "white before");
    pass(&mut app, vec![Event::PointerMoved(source)]);
    pass_held(&mut app, vec![button_held(source, true, Modifiers::ALT)], Modifiers::ALT);
    pass_held(&mut app, vec![button_held(source, false, Modifiers::ALT)], Modifiers::ALT);
    assert!(look(&app).clone_source.is_some(), "the source is set");
    drag(&mut app, &[dest, dest + vec2(4.0, 0.0), dest + vec2(8.0, 0.0)]);
    let c = pixel(&app, u, v);
    assert!(c[0] > 0.9 && c[1] < 0.2, "cloned red: {c:?}");
}

/// The Healing Brush without a source says so and paints nothing.
#[test]
fn healing_without_a_source_says_so() {
    let mut app = app(800, 400, Tool::Healing);
    let steps = app.session.documents()[0].history.past_len();
    let c = RECT.center();
    drag(&mut app, &[c, c + vec2(10.0, 0.0), c + vec2(20.0, 0.0)]);
    assert_eq!(app.session.documents()[0].history.past_len(), steps);
    assert!(app.ui.status_error && app.ui.status.contains("source"), "{}", app.ui.status);
}

/// The Spot Healing Brush in the view takes out a dark spot on the latlong.
#[test]
fn spot_healing_takes_out_a_spot() {
    let mut app = app(800, 400, Tool::SpotHealing);
    app.session.tools.brush.size = 60.0;
    let c = RECT.center();
    let at = latlong_at(c);
    spot(&mut app, at, "#000000");
    assert!(pixel(&app, at[0], at[1])[0] < 0.1);
    drag(&mut app, &[c, c + vec2(2.0, 0.0), c + vec2(4.0, 0.0)]);
    assert!(pixel(&app, at[0], at[1])[0] > 0.5, "healed: {:?}", pixel(&app, at[0], at[1]));
}

fn button_held(at: Pos2, pressed: bool, modifiers: Modifiers) -> Event {
    Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers }
}

/// Whether the latlong pixel `(u, v)` is selected.
fn selected(app: &PhotocraftApp, [u, v]: [f64; 2]) -> bool {
    let st = &app.session.documents()[0];
    st.doc.selection.as_ref().is_some_and(|m| {
        let mut px = vec![0f32; m.channels()];
        m.read_pixel(u as i32, v as i32, &mut px);
        px[0] >= 0.5
    })
}

/// A marquee drawn in the view selects the latlong pixels it covers there, in one History step, and its ants show.
#[test]
fn a_marquee_in_the_view_selects_what_it_covers() {
    let mut app = app(800, 400, Tool::RectMarquee);
    let steps = app.session.documents()[0].history.past_len();
    let (a, b) = (pos2(150.0, 100.0), pos2(250.0, 200.0));
    drag(&mut app, &[a, a + vec2(30.0, 30.0), b]);
    assert_eq!(app.session.documents()[0].history.past_len(), steps + 1);
    assert!(selected(&app, latlong_at(RECT.center())), "the middle of the marquee is selected");
    assert!(!selected(&app, latlong_at(pos2(50.0, 150.0))), "outside it is not");
    assert!(app.view360_ants.as_ref().is_some_and(|a| a.shows()), "the ants show");
}

/// Shift adds a second marquee to the first, as on the flat canvas.
#[test]
fn shift_adds_a_second_marquee() {
    let mut app = app(800, 400, Tool::RectMarquee);
    drag(&mut app, &[pos2(60.0, 120.0), pos2(80.0, 140.0), pos2(100.0, 180.0)]);
    let shift = |app: &mut PhotocraftApp, events: Vec<Event>| pass_held(app, events, Modifiers::SHIFT);
    let (a, b) = (pos2(300.0, 120.0), pos2(340.0, 180.0));
    shift(&mut app, vec![Event::PointerMoved(a)]);
    shift(&mut app, vec![button_held(a, true, Modifiers::SHIFT)]);
    shift(&mut app, vec![Event::PointerMoved(a + vec2(20.0, 20.0))]);
    shift(&mut app, vec![Event::PointerMoved(b)]);
    shift(&mut app, vec![button_held(b, false, Modifiers::SHIFT)]);
    assert!(selected(&app, latlong_at(pos2(80.0, 150.0))) && selected(&app, latlong_at(pos2(320.0, 150.0))));
}

/// The Magic Wand selects the colour clicked in the view.
#[test]
fn the_magic_wand_selects_the_spot_clicked() {
    let mut app = app(800, 400, Tool::MagicWand);
    let c = RECT.center();
    spot(&mut app, latlong_at(c), "#ff0000");
    pass(&mut app, vec![Event::PointerMoved(c)]);
    pass(&mut app, vec![button(c, true)]);
    pass(&mut app, vec![button(c, false)]);
    assert!(selected(&app, latlong_at(c)) && !selected(&app, latlong_at(pos2(40.0, 40.0))));
}

/// The Patch tool: a drag inside the selection patches it from where it is dropped.
#[test]
fn the_patch_tool_patches_from_where_the_selection_is_dropped() {
    let mut app = app(800, 400, Tool::Patch);
    let c = RECT.center();
    let at = latlong_at(c);
    spot(&mut app, at, "#000000");
    app.run("select.rect", json!({"x": at[0] - 14.0, "y": at[1] - 14.0, "width": 28, "height": 28})).unwrap();
    drag(&mut app, &[c, c + vec2(20.0, 0.0), c + vec2(80.0, 0.0)]);
    assert!(pixel(&app, at[0], at[1])[0] > 0.5, "patched from the white: {:?}", pixel(&app, at[0], at[1]));
}

/// The Move tool drags the layer across the sphere: the spot pressed ends up under the release.
#[test]
fn the_move_tool_turns_the_layer_under_the_pointer() {
    let mut app = app(800, 400, Tool::Move);
    let (from, to) = (pos2(150.0, 150.0), pos2(260.0, 120.0));
    spot(&mut app, latlong_at(from), "#ff0000");
    let steps = app.session.documents()[0].history.past_len();
    drag(&mut app, &[from, from + vec2(20.0, -5.0), to]);
    assert_eq!(app.session.documents()[0].history.past_len(), steps + 1);
    let [u, v] = latlong_at(to);
    assert!(pixel(&app, u, v)[1] < 0.2, "the red moved here: {:?}", pixel(&app, u, v));
    let [u, v] = latlong_at(from);
    assert!(pixel(&app, u, v)[1] > 0.5, "and left here");
}

/// Free Transform in the view: a drag inside its box moves the selected pixels, Enter applies, and the view holds
/// still while it is up.
#[test]
fn free_transform_moves_the_selection_and_holds_the_view() {
    let mut app = app(800, 400, Tool::Hand);
    app.last_canvas_rect = RECT;
    let c = RECT.center();
    let at = latlong_at(c);
    spot(&mut app, at, "#ff0000");
    app.run("select.rect", json!({"x": at[0] - 12.0, "y": at[1] - 12.0, "width": 24, "height": 24})).unwrap();
    let ctx = app.ctx().clone();
    crate::menus::invoke(&mut app, &ctx, "edit.freeTransform", json!({})).unwrap();
    let before = look(&app);
    let to = c + vec2(60.0, 0.0);
    drag(&mut app, &[c, c + vec2(20.0, 0.0), to]);
    assert_eq!(look(&app).yaw, before.yaw, "the view held still");
    let enter = Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
    pass(&mut app, vec![enter]);
    let [u, v] = latlong_at(to);
    assert!(pixel(&app, u, v)[1] < 0.2, "moved: {:?}", pixel(&app, u, v));
}

/// Paste in the view lands the clipboard as a new layer in the middle of the view.
#[test]
fn paste_lands_where_the_view_looks() {
    let mut app = app(800, 400, Tool::Hand);
    app.last_canvas_rect = RECT;
    let side = latlong_at(pos2(60.0, 150.0));
    spot(&mut app, side, "#ff0000");
    app.run("select.rect", json!({"x": side[0] - 10.0, "y": side[1] - 10.0, "width": 20, "height": 20})).unwrap();
    app.run("edit.copy", json!({})).unwrap();
    app.run("select.deselect", json!({})).unwrap();
    let layers = app.session.documents()[0].doc.layers.len();
    let ctx = app.ctx().clone();
    crate::menus::invoke(&mut app, &ctx, "edit.paste", json!({})).unwrap();
    assert_eq!(app.session.documents()[0].doc.layers.len(), layers + 1);
    let [u, v] = latlong_at(RECT.center());
    assert!(pixel(&app, u, v)[0] > 0.9 && pixel(&app, u, v)[1] < 0.2, "{:?}", pixel(&app, u, v));
}

/// The preview's quaternion turns a direction the same way as the rotation it came from: a wrong one would preview a
/// different turn from the one OK applies.
#[test]
fn the_preview_quaternion_matches_its_rotation() {
    for [y, p, r] in [[30.0, 0.0, 0.0], [-120.0, 40.0, 10.0], [179.0, -80.0, -45.0], [0.0, 0.0, 170.0]] {
        let m = photocraft_engine::projected_transform::panorama_rotation(y, p, r);
        let q = quaternion(&m).map(f64::from);
        for d in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.36, -0.48, 0.8]] {
            // v + 2·q × (q × v + w·v)
            let cross = |a: [f64; 3], b: [f64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
            let v = [q[0], q[1], q[2]];
            let t = cross(v, d);
            let t = [t[0] + q[3] * d[0], t[1] + q[3] * d[1], t[2] + q[3] * d[2]];
            let t = cross(v, t);
            let turned = [d[0] + 2.0 * t[0], d[1] + 2.0 * t[1], d[2] + 2.0 * t[2]];
            assert!(angle(turned, rot(&m, d)) < 0.05, "{y} {p} {r}: {turned:?} vs {:?}", rot(&m, d));
        }
    }
}

/// Image › Rotate Panorama with no turn opens the dialog, whose fields the 360° View previews; with one it applies.
#[test]
fn rotate_panorama_opens_previews_and_applies() {
    let mut app = app(800, 400, Tool::Hand);
    let ctx = app.ctx().clone();
    crate::menus::invoke(&mut app, &ctx, "image.rotatePanorama", json!({})).unwrap();
    assert!(crate::dialogs::rotate_panorama::preview(&app).is_some_and(|q| q == photocraft_engine::equirect::IDENTITY));
    let c = RECT.center();
    let at = latlong_at(c);
    spot(&mut app, at, "#ff0000");
    let steps = app.session.documents()[0].history.past_len();
    crate::menus::invoke(&mut app, &ctx, "image.rotatePanorama", json!({"yaw": 90})).unwrap();
    assert_eq!(app.session.documents()[0].history.past_len(), steps + 1);
    assert!(pixel(&app, at[0], at[1])[1] > 0.5, "the red turned away from the front");
    assert!(pixel(&app, at[0] + 200.0, at[1])[1] < 0.2, "a quarter turn to the right: {:?}", pixel(&app, at[0] + 200.0, at[1]));
}

/// A drag with `button` along `points`, a pass per event, holding Space when `space`.
fn drag_with(app: &mut PhotocraftApp, button: PointerButton, space: bool, points: &[Pos2]) {
    let key = |pressed| Event::Key { key: egui::Key::Space, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE };
    let press = |at, pressed| Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE };
    let mut first = vec![Event::PointerMoved(points[0])];
    if space {
        first.push(key(true));
    }
    pass(app, first);
    pass(app, vec![press(points[0], true)]);
    for &p in &points[1..] {
        pass(app, vec![Event::PointerMoved(p)]);
    }
    pass(app, vec![press(points[points.len() - 1], false)]);
    pass(app, if space { vec![key(false)] } else { vec![] });
}

/// The left button looks around only with the Hand tool or with Space held; the middle button always does. A tool
/// the view can't use yet says so and leaves the camera alone.
#[test]
fn the_left_button_looks_around_only_with_the_hand_or_space() {
    let mut app = app(800, 400, Tool::Crop);
    let c = RECT.center();
    let path = [c, c + vec2(20.0, 0.0), c + vec2(60.0, 0.0)];
    let before = look(&app);
    drag_with(&mut app, PointerButton::Primary, false, &path);
    assert_eq!(look(&app).yaw, before.yaw, "a left drag with the Crop tool doesn't look around");
    assert!(app.ui.status.contains("360° View"), "{}", app.ui.status);
    drag_with(&mut app, PointerButton::Middle, false, &path);
    let middle = look(&app);
    assert!(middle.yaw != before.yaw, "the middle button looks around");
    drag_with(&mut app, PointerButton::Primary, true, &path);
    assert!(look(&app).yaw != middle.yaw, "Space and the left button look around");
}

/// The Eyedropper picks the colour under the click, and the Color Sampler and Count tools mark the latlong pixel
/// under it, without turning the view.
#[test]
fn point_tools_act_at_the_latlong_pixel_under_the_click() {
    let mut app = app(800, 400, Tool::Eyedropper);
    let at = pos2(300.0, 120.0);
    let [u, v] = latlong_at(at);
    spot(&mut app, [u, v], "#ff0000");
    let before = look(&app);
    drag(&mut app, &[at]);
    let fg = app.session.tools.foreground;
    assert!(fg[0] > 0.9 && fg[1] < 0.1, "picked red: {fg:?}");
    assert_eq!(look(&app).yaw, before.yaw);
    app.ui.tool = Tool::ColorSampler;
    drag(&mut app, &[at]);
    let samplers = &app.session.documents()[0].color_samplers;
    assert!(samplers.iter().any(|q| (q[0] - u).abs() < 2.0 && (q[1] - v).abs() < 2.0), "{samplers:?} near ({u}, {v})");
    app.ui.tool = Tool::Count;
    drag(&mut app, &[at]);
    let counted = app.session.documents()[0].doc.measurement.count_groups.iter().flat_map(|g| g.points.clone()).collect::<Vec<_>>();
    assert!(counted.iter().any(|q| (q[0] - u).abs() < 2.0 && (q[1] - v).abs() < 2.0), "{counted:?} near ({u}, {v})");
}

/// The Pencil paints in the view through its flat command, where the view looks, in one History step.
#[test]
fn the_pencil_paints_in_the_view() {
    let mut app = app(800, 400, Tool::Pencil);
    app.session.tools.brush.size = 8.0;
    let c = RECT.center();
    let [u, v] = latlong_at(c);
    assert!(pixel(&app, u, v)[0] > 0.9, "white before");
    let steps = app.session.documents()[0].history.past_len();
    drag(&mut app, &[c, c + vec2(6.0, 0.0), c + vec2(12.0, 0.0)]);
    assert_eq!(app.session.documents()[0].history.past_len(), steps + 1);
    assert!(pixel(&app, u, v)[0] < 0.1, "the foreground black: {:?}", pixel(&app, u, v));
}

/// Quick Selection selects in the view where its brush is dragged.
#[test]
fn quick_selection_selects_where_it_is_dragged() {
    let mut app = app(800, 400, Tool::QuickSelection);
    app.session.tools.brush.size = 10.0;
    let c = RECT.center();
    spot(&mut app, latlong_at(c), "#000000");
    drag(&mut app, &[c, c + vec2(2.0, 0.0), c + vec2(4.0, 0.0)]);
    assert!(selected(&app, latlong_at(c)), "the dark spot is selected");
    assert!(!selected(&app, latlong_at(pos2(40.0, 40.0))), "the far white isn't");
}

/// The Gradient dragged in the view covers the whole sphere, behind the camera too, in one History step.
#[test]
fn the_gradient_covers_the_whole_sphere() {
    let mut app = app(800, 400, Tool::Gradient);
    let steps = app.session.documents()[0].history.past_len();
    let c = RECT.center();
    drag(&mut app, &[c - vec2(40.0, 0.0), c, c + vec2(40.0, 0.0)]);
    assert_eq!(app.session.documents()[0].history.past_len(), steps + 1);
    // Well past the view on either side (120° left and right, out of the 90° view): the two ends of the default
    // foreground to background gradient, so the paint reached there.
    let (left, right) = (pixel(&app, 133.0, 200.0), pixel(&app, 667.0, 200.0));
    assert!((left[0] - right[0]).abs() > 0.8, "the ends on either side behind the view: {left:?} and {right:?}");
}

/// A Rectangle dragged in the view becomes a new pixel layer, filled where it was drawn and empty elsewhere.
#[test]
fn a_rectangle_drawn_in_the_view_is_a_new_layer_there() {
    let mut app = app(800, 400, Tool::Rectangle);
    let layers = app.session.documents()[0].doc.walk().len();
    let c = RECT.center();
    drag(&mut app, &[c - vec2(30.0, 20.0), c, c + vec2(30.0, 20.0)]);
    assert_eq!(app.session.documents()[0].doc.walk().len(), layers + 1);
    let [u, v] = latlong_at(c);
    assert!(pixel(&app, u, v)[3] > 0.9, "filled where it was drawn: {:?}", pixel(&app, u, v));
    let [u, v] = latlong_at(pos2(20.0, 20.0));
    assert!(pixel(&app, u, v)[3] < 0.1, "empty elsewhere");
}

/// Four clicks with the Pen and a click back on the first point make a closed Work Path, which Make Selection turns
/// into a selection of what it encloses in the view.
#[test]
fn a_pen_path_drawn_in_the_view_selects_what_it_encloses() {
    let mut app = app(800, 400, Tool::Pen);
    let c = RECT.center();
    let corners = [c + vec2(-40.0, -30.0), c + vec2(40.0, -30.0), c + vec2(40.0, 30.0), c + vec2(-40.0, 30.0), c + vec2(-40.0, -30.0)];
    for p in corners {
        drag(&mut app, &[p]);
    }
    let path = app.session.documents()[0].doc.work_path.clone().expect("a work path");
    assert!(path.subpaths.first().is_some_and(|s| s.closed), "closed");
    app.run("path.toSelection", json!({})).unwrap();
    assert!(selected(&app, latlong_at(c)), "inside the path");
    assert!(!selected(&app, latlong_at(pos2(20.0, 20.0))), "not outside it");
}

/// The look-around note shows until the view is first looked around, then stays away.
#[test]
fn looking_around_once_puts_the_look_note_away() {
    let mut app = app(800, 400, Tool::Brush);
    app.session.edit_prefs(|p| {
        p.dialogs.remove(super::LOOK_HINT);
    });
    let c = RECT.center();
    drag(&mut app, &[c]);
    assert!(!crate::dialogs::alerts::dismissed(&app, super::LOOK_HINT), "still showing after a paint click");
    drag_with(&mut app, PointerButton::Middle, false, &[c, c + vec2(30.0, 0.0), c + vec2(60.0, 0.0)]);
    assert!(crate::dialogs::alerts::dismissed(&app, super::LOOK_HINT), "put away by a look around");
}

/// The Navigator's zoom row reads the camera as a zoom and turns a zoom back into the same camera: a narrower view
/// reads as a closer zoom, and a zoom past either end stops at the field of view's limits.
#[test]
fn the_zoom_row_reads_and_sets_the_field_of_view() {
    let (iw, h) = (4096.0, 800.0);
    let wide = SphereView { fov: 120.0, ..SphereView::default() };
    let narrow = SphereView { fov: 30.0, ..SphereView::default() };
    assert!(narrow.zoom(iw, h) > wide.zoom(iw, h));
    for l in [wide, narrow] {
        let back = SphereView::fov_at_zoom(l.zoom(iw, h), iw, h);
        assert!((back - l.fov).abs() < 1e-3, "{} → {back}", l.fov);
    }
    assert!(SphereView::fov_at_zoom(1e9, iw, h) <= narrow.fov && SphereView::fov_at_zoom(1e-9, iw, h) >= wide.fov);
}
