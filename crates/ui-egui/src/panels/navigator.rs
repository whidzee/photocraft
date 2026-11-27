//! The Navigator panel: the framework's `NavigatorPanel` bound to the active document's main canvas view,
//! with the document's picture in its preview box.

use craft::behavior::kurbo::Size;
use craft::theme::variant::Tone;
use craft::behavior::{CanvasView, EditEvents};
use craft::ui::canvas::{AppPaint, NavigationOptions, Navigator, NavigatorPreview, NavigatorZoomRow};
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _, Sizing};
use craft::ui::layout_components::{Row, Stack};
use craft::ui::text::InvalidZoom;
use craft::ui::notices::{Notice, NoticeLayout};
use craft::ui::patterns::canvas::NavigatorPanel;
use craft::ui::primitives::ContentPainterCx;
use egui::{Pos2, Rect, pos2};

use super::PhotocraftApp;
use crate::canvas::document_display;

pub(super) fn navigator(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let Some(idx) = app.session.active_index() else {
        let _ = Notice::new(Tone::Neutral, tl!("No document")).layout(NoticeLayout::Centred).show(ui);
        return;
    };
    // View › 360° View outlines what the view sees on the flat image.
    if crate::canvas::view360::is_on(app, idx) {
        crate::canvas::view360::navigator(app, ui, idx);
        return;
    }
    // The canvas as its last pass showed it, the eased camera of an Animated Zoom included.
    let Some(mut view) = document_display::active_view(app) else { return };
    let size = app.session.documents()[idx].doc.size;
    let document = Size::new(f64::from(size.width), f64::from(size.height));
    let navigation = document_display::navigation(app);
    let animated_zoom = app.session.prefs().general.animated_zoom;
    let button = crate::canvas::view360::shows_button(app, idx);
    let out = {
        let paint = AppPaint::new(|painter: &ContentPainterCx<'_>, rect: Rect| picture(app, &ctx, idx, painter, rect));
        if button {
            with_button(ui, &mut view, document, navigation, NavigatorPreview::Paint(paint))
        } else {
            let out = NavigatorPanel::new(tl!("Navigator"), Bind::bound(&mut view), document).navigation(navigation).preview(NavigatorPreview::Paint(paint)).show(ui);
            (out.view, out.invalid_zoom)
        }
    };
    let (edits, invalid_zoom) = out;
    // The panel edits the bound view in place and reports an edit of this pass; a pass with no input
    // writes nothing back.
    if !edits.is_empty() {
        let ppp = app.canvas_ppp();
        let target = app.ui.views.get_mut(idx);
        if let Some(target) = target {
            // The ease starts from the zoom the view has now, so it is requested before the zoom changes.
            let zoom = (view.zoom * f64::from(ppp)) as f32;
            if (zoom - target.zoom).abs() > target.zoom.abs() * 1e-5 {
                target.request_zoom_animation(animated_zoom);
            }
            target.set_from(&view, ppp);
            target.clear_pending();
        }
    }
    crate::chrome_ui::zoom_alert(app, invalid_zoom);
}

/// The Navigator's preview box over its zoom row, as `NavigatorPanel` lays them out, with the 360° View button
/// after the row's Zoom In (`view360::navigator_button`). Returns the view's edits and a refused zoom entry.
fn with_button(
    ui: &mut egui::Ui,
    view: &mut CanvasView,
    document: Size,
    navigation: NavigationOptions,
    mut preview: NavigatorPreview<'_>,
) -> (EditEvents<CanvasView>, Option<InvalidZoom>) {
    let mut edits = EditEvents::new();
    let mut invalid = None;
    let mut view = Bind::bound(view);
    Stack::new().fill().show(ui, |ui, _| {
        // The body runs for the measuring pass and again for the real one, so each run lends the picture.
        let shown = Navigator::new(tl!("Navigator"), view.reborrowed(), document).preview(preview.reborrowed()).navigation(navigation).sizing(Sizing::Fill).show(ui);
        let row = Row::new().show(ui, |ui, _| {
            let row = NavigatorZoomRow::new(view.reborrowed(), document).show(ui);
            // The flat Navigator: 360° View is off.
            crate::canvas::view360::navigator_button(ui, false);
            row
        });
        let row = row.inner;
        invalid = row.as_ref().and_then(|r| r.invalid_zoom.clone());
        edits = shown.view.events.clone().into_iter().chain(row.map(|r| r.view).unwrap_or_default()).collect();
    });
    (edits, invalid)
}

/// The document's picture in the preview box `rect`: the GPU canvas's own texture at this size while the
/// canvas is on the GPU (#4518), the checkerboard while that texture is not ready, else the CPU thumbnail.
fn picture(app: &mut PhotocraftApp, ctx: &egui::Context, idx: usize, painter: &ContentPainterCx<'_>, rect: Rect) {
    if let Some(params) = crate::canvas::navigator_gpu_view(app, idx, rect, ctx) {
        crate::gpu_canvas::GpuCanvas::paint(&painter.painter, rect, params);
        return;
    }
    painter.checker(rect);
    if crate::canvas::navigator_waits_for_gpu(app, idx, ctx) {
        return;
    }
    if let Some(texture) = crate::canvas::navigator_texture(app, ctx, idx) {
        painter.picture(texture, rect, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), 0.0);
    }
}

#[cfg(test)]
mod tests;
