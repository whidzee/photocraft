//! View › 360° View: the Gradient being dragged is drawn by the sphere shader over the document, through the
//! production callback and shader on a real device.

use super::*;
use photocraft_compose::Buffer;
use photocraft_doc::SampleType;
use photocraft_geom::Rect;
use serde_json::json;

const W: f32 = 160.0;
const H: f32 = 80.0;

/// The engine's gradient for a horizontal drag across the middle of a `W` × `H` view looking at the horizon,
/// red to blue.
fn look() -> SphereGradient {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 64, "height": 32, "background": "white"})).unwrap();
    s.execute("tools.setColors", json!({"foreground": [1.0, 0.0, 0.0, 1.0], "background": [0.0, 0.0, 1.0, 1.0]})).unwrap();
    let vp =
        photocraft_engine::equirect::ViewProjection::new(photocraft_engine::equirect::Camera { yaw: 0.0, pitch: 0.0, fov: 90.0 }, f64::from(W), f64::from(H));
    let g = photocraft_engine::projected_fill::gradient_look(&s, &json!({}), &vp, [50.0, 40.0], [110.0, 40.0]).expect("a gradient the screen draws");
    let mut colors = [[0.0; 4]; SPHERE_GRADIENT_COLORS];
    colors.copy_from_slice(&g.colors);
    SphereGradient { axes: g.axes, shape: g.shape, colors }
}

#[test]
fn the_dragged_gradient_runs_across_the_view_over_the_document() {
    let _gpu = photocraft_testkit::gpu_lock();
    let rs = std::panic::catch_unwind(|| egui_kittest::wgpu::create_render_state(wgpu_setup(), Default::default()));
    let Ok(rs) = rs else {
        eprintln!("skipping the sphere gradient readback: no GPU adapter");
        return;
    };
    let gpu = GpuCanvas::new(&rs);
    // A mid-grey latlong.
    let size = [64u32, 32];
    let buf = Buffer::filled(Rect::from_xywh(0, 0, size[0], size[1]), [0.5, 0.5, 0.5, 1.0]);
    let core = Rect::new(0, 0, size[0] as i32, size[1] as i32);
    assert!(gpu.upload_page(PageUpload { key: 1, page: 1, size, depth: SampleType::U8, factor: 1, core, buf: &buf }));
    let base = ViewParams {
        doc: 1,
        doc_size: size,
        zoom: 1.0,
        center: [0.0, 0.0],
        rotation: 0.0,
        view_key: 1,
        display: 0,
        output: 0,
        hdr: None,
        projection: Some([0.0, 0.0, 90.0]),
        sphere_turn: None,
        sphere_gradient: None,
    };
    let with = ViewParams { view_key: 2, sphere_gradient: Some(look()), ..base };
    let renderer = egui_kittest::wgpu::WgpuTestRenderer::from_render_state(rs);
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(W, 2.0 * H)).with_pixels_per_point(1.0).renderer(renderer).build_ui(move |ui| {
        let o = ui.ctx().content_rect().min;
        GpuCanvas::paint(ui.painter(), egui::Rect::from_min_size(o, egui::vec2(W, H)), base);
        GpuCanvas::paint(ui.painter(), egui::Rect::from_min_size(o + egui::vec2(0.0, H), egui::vec2(W, H)), with);
    });
    h.run_steps(2);
    let img = h.render().expect("render");
    assert!(gpu.health.is_ok(), "{:?}", gpu.fault());
    let px = |x: u32, y: u32| img.get_pixel(x, y).0;
    // Without the gradient: the grey document, the same across the view.
    let (l, r) = (px(20, 40), px(140, 40));
    assert!(l[0].abs_diff(l[2]) < 8 && l[0].abs_diff(r[0]) < 8, "grey without the gradient: {l:?} {r:?}");
    // With it: red before the drag's start, blue past its end, a mix between.
    let (l, m, r) = (px(20, 120), px(80, 120), px(140, 120));
    assert!(l[0] > 200 && l[2] < 60, "red at the start: {l:?}");
    assert!(r[2] > 200 && r[0] < 60, "blue past the end: {r:?}");
    assert!(m[0] > 40 && m[2] > 40, "a mix between: {m:?}");
}
