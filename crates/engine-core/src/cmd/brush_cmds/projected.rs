//! `paint.projectedStroke`: a Brush or Eraser stroke drawn in the View › 360° View perspective
//! view, written back to the equirect (latlong) pixels of the target.
//!
//! The stroke is rendered by the ordinary brush engine in *view* space, over the target's pixels
//! resampled into that view (`pre`); the result is `post`. Every target pixel whose direction
//! falls inside the stroke's view-space damage is then moved by `post − pre` sampled at its own
//! view position, so untouched detail is kept exactly and every blend mode, opacity and the
//! eraser work as they do on the flat canvas. One `edit` makes it one History step.

use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use super::{bad, damage_json, erase_locked, parse_points, resolve_brush, stroke_target, with_blend_mode};
use crate::equirect::{Camera, ViewProjection, view_rect_to_equirect};
use crate::{EngineError, Result, Session};

/// The view the stroke was drawn in, from `"projection": {"yaw", "pitch", "fov", "width", "height"}`.
fn projection(p: &Value) -> Result<ViewProjection> {
    let q = p.get("projection").ok_or_else(|| bad("paint.projectedStroke", "missing `projection`"))?;
    let g = |k: &str, d: f64| q.get(k).and_then(Value::as_f64).unwrap_or(d);
    let (w, h) = (g("width", 0.0), g("height", 0.0));
    if !(w >= 1.0 && h >= 1.0 && w <= 16384.0 && h <= 16384.0) {
        return Err(bad("paint.projectedStroke", "`projection.width` and `height` must be 1..=16384"));
    }
    Ok(ViewProjection::new(Camera { yaw: g("yaw", 0.0), pitch: g("pitch", 0.0), fov: g("fov", 90.0) }, w, h))
}

/// Bilinear sample of `s` at pixel-centre position (x, y) (pixel i's centre is i + 0.5), wrapping
/// x around `w` and clamping y to the image rows.
fn sample_wrapped(s: &Surface, x: f64, y: f64, w: i32, h: i32, out: &mut [f32], tmp: &mut [f32]) {
    let (fx, fy) = (x - 0.5, y - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
    let (x0, y0) = (x0 as i64, y0 as i64);
    out.iter_mut().for_each(|o| *o = 0.0);
    for (dy, wy) in [(0, 1.0 - ty), (1, ty)] {
        let yy = (y0 + dy).clamp(0, i64::from(h) - 1) as i32;
        for (dx, wx) in [(0, 1.0 - tx), (1, tx)] {
            let xx = (x0 + dx).rem_euclid(i64::from(w)) as i32;
            s.read_pixel(xx, yy, tmp);
            let k = wx * wy;
            for (o, v) in out.iter_mut().zip(tmp.iter()) {
                *o += v * k;
            }
        }
    }
}

/// Bilinear sample of a row-major `n`-channel buffer covering `r`, at a pixel-centre position;
/// edges clamp.
fn sample_buf(buf: &[f32], r: Rect, n: usize, x: f64, y: f64, out: &mut [f32]) {
    let (w, h) = (r.width() as i64, r.height() as i64);
    let (fx, fy) = (x - 0.5 - f64::from(r.x0), y - 0.5 - f64::from(r.y0));
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
    let (x0, y0) = (x0 as i64, y0 as i64);
    out.iter_mut().for_each(|o| *o = 0.0);
    for (dy, wy) in [(0, 1.0 - ty), (1, ty)] {
        let yy = (y0 + dy).clamp(0, h - 1) as usize;
        for (dx, wx) in [(0, 1.0 - tx), (1, tx)] {
            let xx = (x0 + dx).clamp(0, w - 1) as usize;
            let i = (yy * w as usize + xx) * n;
            let k = wx * wy;
            for (c, o) in out.iter_mut().enumerate() {
                *o += buf[i + c] * k;
            }
        }
    }
}

/// The equirect `target` resampled into view space over `r`, row-major.
pub(crate) fn resample(target: &Surface, proj: &ViewProjection, r: Rect, iw: i32, ih: i32) -> Vec<f32> {
    use rayon::prelude::*;
    let n = target.channels();
    let row = r.width() as usize * n;
    let mut data = vec![0f32; row * r.height() as usize];
    if row > 0 {
        data.par_chunks_mut(row).enumerate().for_each(|(j, line)| {
            let y = r.y0 + j as i32;
            let (mut px, mut tmp) = (vec![0f32; n], vec![0f32; n]);
            for (i, x) in (r.x0..r.x1).enumerate() {
                let [u, v] = proj.view_to_equirect(f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(iw), f64::from(ih));
                sample_wrapped(target, u, v, iw, ih, &mut px, &mut tmp);
                line[i * n..(i + 1) * n].copy_from_slice(&px);
            }
        });
    }
    data
}

/// Resample the equirect target into view space over `r`.
pub(crate) fn to_view(target: &Surface, proj: &ViewProjection, r: Rect, iw: i32, ih: i32) -> Surface {
    let mut s = Surface::new(target.format());
    s.write_region(r, &resample(target, proj, r, iw, ih));
    s
}

/// The side of the square blocks a [`LazyView`] resamples at a time: the brush engine's coverage tile, so a dab's
/// tiles are whole blocks.
pub(crate) const BLOCK: i32 = photocraft_paint::render::COV_TILE;

/// Which `BLOCK`-square blocks of a view have been resampled.
#[derive(Clone)]
pub(crate) struct Blocks {
    view: Rect,
    columns: i32,
    done: Vec<bool>,
}

impl Blocks {
    pub(crate) fn new(view: Rect) -> Self {
        let columns = (view.x1 - view.x0 + BLOCK - 1).max(0) / BLOCK;
        let rows = (view.y1 - view.y0 + BLOCK - 1).max(0) / BLOCK;
        Self { view, columns, done: vec![false; (columns * rows).max(0) as usize] }
    }

    /// The column and row of each block `r` touches.
    fn of(&self, r: Rect) -> impl Iterator<Item = (i32, i32)> + use<> {
        let r = r.intersect(&self.view);
        let (bx0, by0) = ((r.x0 - self.view.x0) / BLOCK, (r.y0 - self.view.y0) / BLOCK);
        let (bx1, by1) = ((r.x1 - self.view.x0 + BLOCK - 1) / BLOCK, (r.y1 - self.view.y0 + BLOCK - 1) / BLOCK);
        let (bx1, by1) = if r.is_empty() { (bx0, by0) } else { (bx1, by1) };
        (by0..by1).flat_map(move |by| (bx0..bx1).map(move |bx| (bx, by)))
    }

    /// Whether some block of `r` hasn't been resampled.
    pub(crate) fn missing(&self, r: Rect) -> bool {
        self.of(r).any(|(bx, by)| !self.done[(by * self.columns + bx) as usize])
    }

    /// The blocks of `r` not yet resampled, marked as resampled now.
    pub(crate) fn take(&mut self, r: Rect) -> Vec<Rect> {
        let mut out = Vec::new();
        for (bx, by) in self.of(r) {
            let i = (by * self.columns + bx) as usize;
            if !self.done[i] {
                self.done[i] = true;
                let (x, y) = (self.view.x0 + bx * BLOCK, self.view.y0 + by * BLOCK);
                out.push(Rect::new(x, y, x + BLOCK, y + BLOCK).intersect(&self.view));
            }
        }
        out
    }
}

/// An equirect surface resampled into a view a block at a time, as a stroke reaches each block (View › 360° View).
/// A press costs nothing up front, however large the latlong; a stroke pays only for the blocks it covers.
pub(crate) struct LazyView {
    src: Surface,
    proj: ViewProjection,
    size: (i32, i32),
    blocks: Blocks,
    /// The view so far: resampled where [`ensure`](Self::ensure) has been asked for.
    pub(crate) out: Surface,
}

impl LazyView {
    pub(crate) fn new(src: &Surface, proj: &ViewProjection, view: Rect, size: (i32, i32)) -> Self {
        Self { src: src.clone(), proj: *proj, size, blocks: Blocks::new(view), out: Surface::new(src.format()) }
    }

    /// An already resampled view: nothing left to fill.
    pub(crate) fn whole(out: Surface, proj: &ViewProjection, view: Rect, size: (i32, i32)) -> Self {
        let mut blocks = Blocks::new(view);
        blocks.take(view);
        Self { src: Surface::new(out.format()), proj: *proj, size, blocks, out }
    }

    /// Resamples the blocks of `r` not yet resampled, and hands back each with its pixels.
    pub(crate) fn ensure(&mut self, r: Rect) -> Vec<(Rect, Vec<f32>)> {
        use rayon::prelude::*;
        let missing = self.blocks.take(r);
        let (src, proj, (iw, ih)) = (&self.src, &self.proj, self.size);
        let filled: Vec<(Rect, Vec<f32>)> = missing.into_par_iter().map(|b| (b, resample(src, proj, b, iw, ih))).collect();
        for (b, data) in &filled {
            self.out.write_region(*b, data);
        }
        filled
    }
}

/// Each of `dabs`' rects, out to the brush engine's coverage tiles it touches.
pub(crate) fn dab_tiles(dabs: &[photocraft_paint::Dab]) -> Vec<Rect> {
    let down = |v: i32| v.div_euclid(BLOCK) * BLOCK;
    dabs.iter()
        .map(|d| {
            let e = f64::from(d.radius) * 1.5 + 2.0;
            let (x0, y0) = ((d.center.x - e).floor() as i32, (d.center.y - e).floor() as i32);
            let (x1, y1) = ((d.center.x + e).ceil() as i32, (d.center.y + e).ceil() as i32);
            Rect::new(down(x0), down(y0), down(x1 - 1) + BLOCK, down(y1 - 1) + BLOCK)
        })
        .collect()
}

/// Write `post − pre` (view space, over `damage`) back into the equirect `target` of `size`
/// (width, height), scaled by the equirect `selection` coverage when there is one. Returns the
/// equirect damage.
pub(crate) fn write_back(
    target: &mut Surface,
    proj: &ViewProjection,
    (pre, post): (&Surface, &Surface),
    damage: Rect,
    (iw, ih): (i32, i32),
    selection: Option<&Surface>,
) -> Rect {
    if damage.is_empty() {
        return Rect::EMPTY;
    }
    let n = target.channels();
    // The change once, in view space (bilinear is linear: sampling post − pre is sampling post
    // minus sampling pre); non-finite differences (inf/NaN pixels) change nothing.
    let mut diff = post.read_region(damage);
    for (d, p) in diff.iter_mut().zip(pre.read_region(damage)) {
        *d -= p;
        if !d.is_finite() {
            *d = 0.0;
        }
    }
    // Which 8 × 8 view blocks changed at all: most of a long stroke's bounding box didn't.
    const B: i64 = 8;
    let (dw, dh) = (i64::from(damage.width()), i64::from(damage.height()));
    let (bw, bh) = ((dw + B - 1) / B, (dh + B - 1) / B);
    let mut blocks = vec![false; (bw * bh).max(0) as usize];
    for (i, px) in diff.chunks(n).enumerate() {
        if px.iter().any(|v| *v != 0.0) {
            let (x, y) = (i as i64 % dw, i as i64 / dw);
            blocks[((y / B) * bw + x / B) as usize] = true;
        }
    }
    // A sample reads the 2 × 2 taps around it: grow the marks by one block.
    let marked = blocks.clone();
    for by in 0..bh {
        for bx in 0..bw {
            if marked[(by * bw + bx) as usize] {
                for (ox, oy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                    let (x, y) = (bx + ox, by + oy);
                    if x >= 0 && y >= 0 && x < bw && y < bh {
                        blocks[(y * bw + x) as usize] = true;
                    }
                }
            }
        }
    }
    let changed = |vx: f64, vy: f64| {
        let (x, y) = ((vx.floor() as i64 - i64::from(damage.x0)).clamp(0, dw - 1), (vy.floor() as i64 - i64::from(damage.y0)).clamp(0, dh - 1));
        blocks[((y / B) * bw + x / B) as usize]
    };
    let alpha = target.format().alpha.then(|| n - 1);
    let r = [f64::from(damage.x0), f64::from(damage.y0), f64::from(damage.x1), f64::from(damage.y1)];
    let mut total = Rect::EMPTY;
    for q in view_rect_to_equirect(proj, r, iw as u32, ih as u32, 1.0) {
        let rect = Rect::new(q[0] as i32, q[1] as i32, q[2] as i32, q[3] as i32);
        if rect.is_empty() {
            continue;
        }
        let mut buf = target.read_region(rect);
        let sel_buf = selection.map(|m| (m.read_region(rect), m.channels()));
        // Rows in parallel: a stroke near a pole touches the full image width.
        use rayon::prelude::*;
        let row = rect.width() as usize * n;
        let touched = buf
            .par_chunks_mut(row)
            .enumerate()
            .map(|(j, line)| {
                let y = rect.y0 + j as i32;
                let mut b = vec![0f32; n];
                let mut touched = false;
                for (i, x) in (rect.x0..rect.x1).enumerate() {
                    let Some([vx, vy]) = proj.equirect_to_view(f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(iw), f64::from(ih)) else { continue };
                    if !(vx >= r[0] && vx < r[2] && vy >= r[1] && vy < r[3]) || !changed(vx, vy) {
                        continue;
                    }
                    let k = match &sel_buf {
                        Some((m, mn)) => m[(j * rect.width() as usize + i) * mn].clamp(0.0, 1.0),
                        None => 1.0,
                    };
                    if k <= 0.0 {
                        continue;
                    }
                    sample_buf(&diff, damage, n, vx, vy, &mut b);
                    for c in 0..n {
                        let d = b[c] * k;
                        if d != 0.0 && d.is_finite() {
                            touched = true;
                            let v = line[i * n + c] + d;
                            line[i * n + c] = if Some(c) == alpha { v.clamp(0.0, 1.0) } else { v.max(0.0) };
                        }
                    }
                }
                touched
            })
            .reduce(|| false, |x, y| x || y);
        if touched {
            target.write_region(rect, &buf);
            total = if total.is_empty() { rect } else { total.union(&rect) };
        }
    }
    total
}

/// What a live projected stroke paints with.
pub(crate) enum PreviewKind {
    /// The Brush and Eraser: the brush engine, fed each update's new points.
    Brush {
        brush: Box<photocraft_paint::BrushSettings>,
        lock: bool,
        /// The stroke so far, and how many of its points it has had.
        live: Option<(Box<photocraft_paint::StrokeRenderer>, usize)>,
    },
    /// The Clone Stamp: the rotated source in view space, painted through the stroke coverage.
    Clone { source: Box<LazyView>, brush: Box<photocraft_paint::BrushSettings>, mode: photocraft_color::BlendMode, lock: bool },
}

/// A projected stroke shown while it is drawn (View › 360° View). The target is resampled into the view a block at
/// a time as the stroke reaches it ([`LazyView`]), so a press costs nothing up front. The Brush and Eraser feed each
/// update's new points to one brush engine stroke and draw only the tiles they touch, so an update costs what its
/// new dabs cost, however long the stroke; the Clone Stamp renders the stroke so far each update. Each update hands
/// back an RGBA8 overlay of what changed in it. The commit (`paint.projectedStroke` / `paint.projectedClone`)
/// reprojects on release.
pub struct ProjectedPreview {
    pre: LazyView,
    /// The view as the stroke leaves it: the resampled blocks, painted.
    post: Surface,
    view: Rect,
    /// The active selection resampled into the view: strokes are clipped by it.
    sel: Option<LazyView>,
    /// Where the last update showed the stroke's unfinished end.
    tail: Rect,
    pub(crate) kind: PreviewKind,
}

/// Where `shown` (the stroke with its unfinished end) differs from `post` (without it), inside `r`.
fn shown_tail(shown: &Surface, post: &Surface, r: Rect) -> Rect {
    if r.is_empty() {
        return Rect::EMPTY;
    }
    let (a, b) = (shown.read_region(r), post.read_region(r));
    let n = shown.channels().max(1);
    let w = r.width() as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for (i, (p, q)) in a.chunks_exact(n).zip(b.chunks_exact(n)).enumerate() {
        if p != q {
            let (x, y) = (r.x0 + (i % w) as i32, r.y0 + (i / w) as i32);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
        }
    }
    if x0 > x1 { Rect::EMPTY } else { Rect::new(x0, y0, x1, y1) }
}

/// One preview frame: `px` (unmultiplied sRGB RGBA8, row-major) covers view pixels `rect`.
pub struct PreviewOverlay {
    pub rect: Rect,
    pub px: Vec<[u8; 4]>,
}

impl ProjectedPreview {
    /// A Brush or Eraser preview from `paint.projectedStroke` params (points unused).
    pub fn brush(s: &Session, p: &Value) -> Result<Self> {
        let proj = projection(p)?;
        let brush = with_blend_mode(resolve_brush(s, p, "paint.projectedStroke")?, p);
        let bg = s.tools.background;
        let (id, mut brush, _) = stroke_target(s, p, brush)?;
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let mut doc = (*st.doc).clone();
        let size = (doc.size.width as i32, doc.size.height as i32);
        let view = Rect::new(0, 0, proj.w as i32, proj.h as i32);
        let sel = doc.selection.clone().map(|m| LazyView::new(&m, &proj, view, size));
        let (surf, lock) = crate::channel_cmds::target_surface(&mut doc, id, p)?;
        erase_locked(&mut brush, lock, bg);
        let pre = LazyView::new(surf, &proj, view, size);
        Ok(Self::with(pre, view, sel, PreviewKind::Brush { brush: Box::new(brush), lock, live: None }))
    }

    pub(crate) fn with(pre: LazyView, view: Rect, sel: Option<LazyView>, kind: PreviewKind) -> Self {
        let post = pre.out.clone();
        Self { pre, post, view, sel, tail: Rect::EMPTY, kind }
    }

    /// Resamples the blocks of `r` the stroke hasn't reached yet, into the view before and after it.
    fn ensure(&mut self, r: Rect) {
        for (b, data) in self.pre.ensure(r) {
            self.post.write_region(b, &data);
        }
        if let Some(m) = self.sel.as_mut() {
            m.ensure(r);
        }
    }

    /// Render the stroke so far, `points` (view pixels, `[x, y, pressure]`), and return what changed since the last
    /// update. `None` when nothing new shows or the target isn't RGB/gray.
    pub fn render(&mut self, points: &[[f64; 3]]) -> Option<PreviewOverlay> {
        let pts: Vec<photocraft_paint::StrokePoint> = points.iter().map(|q| photocraft_paint::StrokePoint::new(q[0], q[1], q[2] as f32)).collect();
        if pts.is_empty() {
            return None;
        }
        let damage = match &mut self.kind {
            PreviewKind::Brush { brush, lock, live } => {
                // A shorter stroke than the one so far starts again.
                if live.as_ref().is_some_and(|(_, n)| *n > pts.len()) {
                    *live = None;
                    self.post = self.pre.out.clone();
                }
                let fmt = self.pre.out.format();
                let (r, n) = live.get_or_insert_with(|| (Box::new(photocraft_paint::StrokeRenderer::new(brush, Some(fmt), 1.0).record_dabs()), 0));
                let before = r.dab_count();
                r.push(&pts[*n..]);
                *n = pts.len();
                // Only the blocks the new dabs (and the stroke's unfinished end) reach are resampled, and only their
                // tiles drawn.
                let tail = r.tail_preview();
                let mut reach = dab_tiles(&r.dabs()[before..]);
                if tail.is_some() {
                    reach.extend(dab_tiles(&r.tail_dabs()));
                }
                for q in reach {
                    for (b, data) in self.pre.ensure(q) {
                        self.post.write_region(b, &data);
                    }
                    if let Some(m) = self.sel.as_mut() {
                        m.ensure(q);
                    }
                }
                let sel = self.sel.as_ref().map(|m| &m.out);
                let mut damage = r.composite(&self.pre.out, &mut self.post, sel, *lock, false);
                // What finishing the stroke now would add (a lone first dab, the smoothing catch-up) shows on a copy,
                // and the next update redraws where it showed.
                let mut shown = self.post.clone();
                for q in [std::mem::take(&mut self.tail), tail.map_or(Rect::EMPTY, |mut t| t.composite(&self.pre.out, &mut shown, sel, *lock, true))] {
                    if !q.is_empty() {
                        damage = if damage.is_empty() { q } else { damage.union(&q) };
                    }
                }
                self.tail = shown_tail(&shown, &self.post, damage);
                let damage = damage.intersect(&self.view);
                return if damage.is_empty() { None } else { overlay(&self.pre.out, &shown, damage) };
            }
            PreviewKind::Clone { brush, .. } => {
                let stroke = photocraft_paint::Stroke { brush: (**brush).clone(), points: pts };
                let (bounds, cov) = photocraft_paint::retouch::stroke_coverage(&stroke);
                let work = bounds.intersect(&self.view);
                if work.is_empty() {
                    return None;
                }
                self.ensure(work);
                let PreviewKind::Clone { source, brush, mode, lock } = &mut self.kind else { return None };
                source.ensure(work);
                let cov = crop_cov(&cov, bounds, work);
                let paint = photocraft_paint::retouch::Region::read(&source.out, work);
                let mut post = self.pre.out.clone();
                let d =
                    photocraft_paint::retouch::apply_coverage(&mut post, work, &cov, brush.opacity, self.sel.as_ref().map(|m| &m.out), *lock, &paint, *mode);
                self.post = post;
                d
            }
        }
        .intersect(&self.view);
        if damage.is_empty() {
            return None;
        }
        overlay(&self.pre.out, &self.post, damage)
    }
}

/// `cov` (row-major over `from`) cut to `to` (inside `from`).
pub(crate) fn crop_cov(cov: &[f32], from: Rect, to: Rect) -> Vec<f32> {
    if from == to {
        return cov.to_vec();
    }
    let fw = from.width() as usize;
    let mut out = Vec::with_capacity(to.width() as usize * to.height() as usize);
    for y in to.y0..to.y1 {
        let row = (y - from.y0) as usize * fw;
        out.extend_from_slice(&cov[row + (to.x0 - from.x0) as usize..row + (to.x1 - from.x0) as usize]);
    }
    out
}

/// What the stroke changed over `r`, as an overlay: painted pixels as they now are (their own
/// alpha), erased ones as a translucent grey veil by how much was removed, unchanged ones clear.
pub(crate) fn overlay(pre: &Surface, post: &Surface, r: Rect) -> Option<PreviewOverlay> {
    use photocraft_color::{ColorMode, SampleType};
    let f = pre.format();
    let n = f.channels();
    let gray = match f.mode {
        ColorMode::Rgb => false,
        ColorMode::Grayscale => true,
        _ => return None,
    };
    let linear = f.sample == SampleType::F32;
    let (a, b) = (pre.read_region(r), post.read_region(r));
    let enc = |v: f32| -> u8 {
        let v = v.max(0.0);
        let v = if linear { if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.min(1.0).powf(1.0 / 2.4) - 0.055 } } else { v };
        (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    };
    let px = a
        .chunks_exact(n)
        .zip(b.chunks_exact(n))
        .map(|(p, q)| {
            if p == q {
                return [0, 0, 0, 0];
            }
            let (pa, qa) = if f.alpha { (p[n - 1], q[n - 1]) } else { (1.0, 1.0) };
            if qa + 1e-4 < pa {
                let lost = ((pa - qa) * 0.6 * 255.0) as u8;
                return [150, 150, 150, lost];
            }
            let (r, g, b) = if gray { (q[0], q[0], q[0]) } else { (q[0], q[1], q[2]) };
            [enc(r), enc(g), enc(b), (qa.clamp(0.0, 1.0) * 255.0 + 0.5) as u8]
        })
        .collect();
    Some(PreviewOverlay { rect: r, px })
}

/// `paint.projectedStroke`: `paint.stroke` params with `points` in view pixels and the view's
/// `projection` camera; the brush size is in view pixels.
pub fn paint_projected_stroke(s: &mut Session, p: &Value) -> Result<Value> {
    let proj = projection(p)?;
    let pts = parse_points(p, "paint.projectedStroke")?;
    if pts.is_empty() {
        return Err(bad("paint.projectedStroke", "no points"));
    }
    let brush = with_blend_mode(resolve_brush(s, p, "paint.projectedStroke")?, p);
    let label = if brush.erase { "Eraser" } else { "Brush Tool" };
    let bg = s.tools.background;
    let (id, brush, _) = stroke_target(s, p, brush)?;
    let dmg = s.edit(label, |doc, _| {
        let (iw, ih) = (doc.size.width as i32, doc.size.height as i32);
        let selection = doc.selection.clone();
        if iw < 2 || ih < 1 {
            return Err(EngineError::Other("document too small for a projected stroke".into()));
        }
        let (surf, lock) = crate::channel_cmds::target_surface(doc, id, p)?;
        let mut brush = brush;
        erase_locked(&mut brush, lock, bg);
        // The view-space area the stroke can reach: its points, padded by the brush.
        let reach = (f64::from(brush.size) * 0.75 + 4.0).ceil();
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for q in &pts {
            (x0, y0, x1, y1) = (x0.min(q.x), y0.min(q.y), x1.max(q.x), y1.max(q.y));
        }
        let work = Rect::new((x0 - reach).floor() as i32, (y0 - reach).floor() as i32, (x1 + reach).ceil() as i32, (y1 + reach).ceil() as i32)
            .intersect(&Rect::new(0, 0, proj.w as i32, proj.h as i32));
        if work.is_empty() {
            return Ok(Rect::EMPTY);
        }
        let pre = to_view(surf, &proj, work, iw, ih);
        let mut post = pre.clone();
        let damage = photocraft_paint::render_stroke(&mut post, &brush, &pts, None, lock, 1.0).intersect(&work);
        if damage.is_empty() {
            return Ok(Rect::EMPTY);
        }
        Ok(write_back(surf, &proj, (&pre, &post), damage, (iw, ih), selection.as_ref()))
    })?;
    Ok(damage_json(s, dmg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(s: &Session, x: i32, y: i32) -> [f32; 4] {
        let d = s.active().unwrap();
        d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(x, y)
    }

    #[test]
    fn projected_stroke_paints_where_the_view_looks_and_undoes() {
        let mut s = crate::session();
        s.execute("file.new", json!({"width": 720, "height": 360, "background": "transparent"})).unwrap();
        // Look at yaw 90° (u = 540), straight ahead; dab the view centre in red.
        let p = json!({
            "points": [[200, 150], [200, 150]],
            "size": 20, "hardness": 1.0, "color": "#ff0000",
            "projection": {"yaw": 90.0, "pitch": 0.0, "fov": 90.0, "width": 400, "height": 300}
        });
        let r = s.execute("paint.projectedStroke", p).unwrap();
        let d = r["damage"].as_array().unwrap();
        assert!(d[2].as_u64().unwrap() > 0, "{r}");
        let c = rgba(&s, 540, 180);
        assert!(c[0] > 0.9 && c[3] > 0.9, "centre painted red: {c:?}");
        assert_eq!(rgba(&s, 180, 180)[3], 0.0, "the opposite side is untouched");
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(rgba(&s, 540, 180)[3], 0.0, "undone");
    }

    #[test]
    fn live_preview_shows_the_brush_colour_with_its_tip_alpha() {
        let mut s = crate::session();
        s.execute("file.new", json!({"width": 720, "height": 360, "background": "transparent"})).unwrap();
        let p = json!({
            "size": 30, "hardness": 0.0, "color": "#ff0000",
            "projection": {"yaw": 0.0, "pitch": 0.0, "fov": 90.0, "width": 400, "height": 300}
        });
        let mut live = ProjectedPreview::brush(&s, &p).unwrap();
        let o = live.render(&[[200.0, 150.0, 1.0]]).expect("a dab shows");
        let at = |x: i32, y: i32| o.px[((y - o.rect.y0) * o.rect.width() as i32 + (x - o.rect.x0)) as usize];
        let c = at(200, 150);
        assert!(c[0] > 240 && c[1] < 10 && c[3] > 200, "centre is opaque red, not black: {c:?}");
        // A soft tip: the edge is fainter, and the corners of the dab's box stay clear.
        assert!(at(212, 150)[3] < c[3]);
        assert_eq!(at(o.rect.x0, o.rect.y0)[3], 0, "not a square stamp");
        // Nothing was written to the document.
        assert_eq!(rgba(&s, 360, 180)[3], 0.0);
    }

    /// A stroke previewed a few points at a time ends as the same picture as the whole stroke previewed at once,
    /// and each update hands back only the part of the view its new points reach.
    #[test]
    fn a_stroke_previewed_in_parts_matches_the_whole_stroke() {
        let mut s = crate::session();
        s.execute("file.new", json!({"width": 1440, "height": 720, "background": "white"})).unwrap();
        s.execute("paint.gradient", json!({"from": [0, 0], "to": [1440, 720]})).unwrap();
        let p = json!({
            "size": 24, "hardness": 0.5, "opacity": 60, "color": "#3050a0",
            "projection": {"yaw": 30.0, "pitch": 20.0, "fov": 80.0, "width": 640, "height": 400}
        });
        let points: Vec<[f64; 3]> = (0..120).map(|i| [80.0 + 4.0 * f64::from(i), 200.0 + 60.0 * (f64::from(i) / 12.0).sin(), 1.0]).collect();
        let mut parts = ProjectedPreview::brush(&s, &p).unwrap();
        let mut last = None;
        for n in (10..=points.len()).step_by(10) {
            last = parts.render(&points[..n]);
        }
        let mut whole = ProjectedPreview::brush(&s, &p).unwrap();
        let all = whole.render(&points).expect("the stroke shows");
        let last = last.expect("the last part shows");
        assert!(
            all.rect.contains_rect(&last.rect) && last.rect.width() < all.rect.width(),
            "the last update covers only its part: {:?} in {:?}",
            last.rect,
            all.rect
        );
        // What each shows over the whole stroke: the same picture.
        let r = all.rect;
        let shown = |p: &ProjectedPreview| overlay(&p.pre.out, &p.post, r).map(|o| o.px);
        assert!(shown(&parts) == shown(&whole), "the same stroke either way");
    }

    #[test]
    fn projected_stroke_across_the_seam_paints_both_edges() {
        let mut s = crate::session();
        s.execute("file.new", json!({"width": 720, "height": 360, "background": "transparent"})).unwrap();
        let p = json!({
            "points": [[200, 150]], "size": 40, "hardness": 1.0, "color": "#0000ff",
            "projection": {"yaw": 180.0, "pitch": 0.0, "fov": 90.0, "width": 400, "height": 300}
        });
        s.execute("paint.projectedStroke", p).unwrap();
        assert!(rgba(&s, 1, 180)[3] > 0.9, "left edge painted: {:?}", rgba(&s, 1, 180));
        assert!(rgba(&s, 718, 180)[3] > 0.9, "right edge painted: {:?}", rgba(&s, 718, 180));
    }
}
