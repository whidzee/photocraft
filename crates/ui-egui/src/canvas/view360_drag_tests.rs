//! A look-around drag through the real document area, in many small pointer moves as a mouse sends them.

use crate::app::framework::BuildApp as _;
use egui::{Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use photocraft_engine::equirect::ViewProjection;
use serde_json::json;

use super::view360::SphereView;
use crate::PhotocraftApp;
use crate::state::Tool;

fn harness(ppp: f32) -> Harness<'static, Box<PhotocraftApp>> {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 2048, "height": 1024})).unwrap();
    app.sync_views();
    app.ui.extras.rulers = false;
    app.ui.tool = Tool::Hand;
    let mut h =
        Harness::builder().with_context(app.ctx().clone()).with_size(vec2(1000.0, 700.0)).with_pixels_per_point(ppp).with_step_dt(1.0 / 60.0).build_app(
            |ui, app: &mut PhotocraftApp| {
                let ctx = ui.ctx().clone();
                if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    return;
                }
                egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
            },
            app,
        );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::AppTheme::pro());
    h.run_steps(4);
    super::view360::command(h.state_mut(), &json!({"on": true, "fov": 60.0})).unwrap();
    h.run_steps(3);
    h
}

fn look(h: &Harness<'static, Box<PhotocraftApp>>) -> SphereView {
    h.state().ui.views[0].projection.expect("360° View is on")
}

/// The direction the view shows at screen point `p`.
fn dir_at(h: &Harness<'static, Box<PhotocraftApp>>, p: Pos2) -> [f64; 3] {
    let r = h.state().last_canvas_rect;
    let ppp = h.ctx.pixels_per_point();
    let vp = ViewProjection::new(look(h).camera(), f64::from(r.width() * ppp), f64::from(r.height() * ppp));
    vp.view_to_dir(f64::from((p.x - r.min.x) * ppp), f64::from((p.y - r.min.y) * ppp))
}

fn angle(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).clamp(-1.0, 1.0).acos().to_degrees()
}

/// A slow drag of 40 one-point moves turns the view only as far as the pointer went: the spot pressed stays under it.
#[test]
fn a_slow_grab_drag_follows_the_pointer() {
    for ppp in [1.0, 1.5] {
        let mut h = harness(ppp);
        let c = h.state().last_canvas_rect.center();
        let pressed = dir_at(&h, c);
        h.event(egui::Event::PointerMoved(c));
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos: c, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.run_steps(1);
        let mut p = c;
        for _ in 0..40 {
            p += vec2(1.0, 0.5);
            h.event(egui::Event::PointerMoved(p));
            h.run_steps(1);
        }
        let off = angle(dir_at(&h, p), pressed);
        h.event(egui::Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        h.run_steps(1);
        assert!(off < 0.2, "at {ppp}x the pressed spot is {off:.2}° from the pointer after the drag; camera {:?}", look(&h));
    }
}

/// The view with a Navigator drawn before (`first`) or after the canvas, the Navigator 300 points wide at the left.
fn with_navigator(first: bool) -> Harness<'static, Box<PhotocraftApp>> {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 2048, "height": 1024})).unwrap();
    app.sync_views();
    app.ui.extras.rulers = false;
    app.ui.tool = Tool::Hand;
    let nav = |ui: &mut egui::Ui, app: &mut PhotocraftApp| {
        ui.scope_builder(egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(300.0, 400.0))), |ui| {
            super::view360::navigator(app, ui, 0);
        });
    };
    let mut h = Harness::builder().with_context(app.ctx().clone()).with_size(vec2(1300.0, 700.0)).with_step_dt(1.0 / 60.0).build_app(
        move |ui, app: &mut PhotocraftApp| {
            let ctx = ui.ctx().clone();
            if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            if first {
                nav(ui, app);
            }
            egui::CentralPanel::default().show(ui, |ui| {
                ui.add_space(0.0);
                let r = egui::Rect::from_min_max(egui::pos2(320.0, 0.0), ui.max_rect().max);
                ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| crate::canvas::document_area(app, ui));
            });
            if !first {
                nav(ui, app);
            }
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::AppTheme::pro());
    h.run_steps(4);
    super::view360::command(h.state_mut(), &json!({"on": true})).unwrap();
    h.run_steps(3);
    h
}

/// Dragging across the Navigator turns the view steadily one way: never back, never stuck.
#[test]
fn a_navigator_drag_turns_the_view_steadily() {
    for first in [true, false] {
        let mut h = with_navigator(first);
        let start = egui::pos2(60.0, 75.0);
        h.event(egui::Event::PointerMoved(start));
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.run_steps(1);
        let mut yaws = Vec::new();
        for i in 1..=30 {
            h.event(egui::Event::PointerMoved(start + vec2(4.0 * i as f32, 0.0)));
            h.run_steps(1);
            yaws.push(look(&h).yaw);
        }
        h.event(egui::Event::PointerButton { pos: start + vec2(120.0, 0.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        h.run_steps(1);
        // The press itself looks at the spot pressed; the drag from there is what must be steady.
        let yaws = &yaws[1..];
        assert!(yaws.windows(2).all(|w| w[1] >= w[0] - 1e-3), "navigator first={first}: the view stepped back {yaws:?}");
        assert!(yaws.last().unwrap() - yaws[0] > 30.0, "navigator first={first}: {yaws:?}");
    }
}

/// Text typed after a click with the Type tool in the whole app goes into its field, not to the tool shortcuts, and
/// Enter places it as a new layer.
#[test]
fn typed_text_is_placed_as_a_new_layer() {
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_step_dt(1.0 / 60.0).with_max_steps(64).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut session = photocraft_engine::Session::new();
        session.edit_prefs(|p| p.general.auto_show_home_screen = false);
        let mut app = PhotocraftApp::with_context(cc.egui_ctx.clone(), session, crate::Services::default());
        app.run("file.new", json!({"width": 2048, "height": 1024})).unwrap();
        app.sync_views();
        app
    });
    h.run_steps(6);
    super::view360::command(h.state_mut(), &json!({"on": true})).unwrap();
    h.state_mut().ui.tool = Tool::Type;
    h.run_steps(4);
    let layers = h.state().session.documents()[0].doc.walk().len();
    let c = h.state().last_canvas_rect.center();
    h.event(egui::Event::PointerMoved(c));
    h.event(egui::Event::PointerButton { pos: c, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: c, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    h.event(egui::Event::Text("Hello b".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(h.state().ui.tool, Tool::Type, "typing picked no tool");
    assert_eq!(h.state().session.documents()[0].doc.walk().len(), layers + 1, "{}", h.state().ui.status);
}
