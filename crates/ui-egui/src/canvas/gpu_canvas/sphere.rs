//! Where each document tile can show in a View › 360° View draw (#15726). The view's pass (`fs_proj`) draws every
//! tile over the screen and keeps the directions that land in it; scissoring each draw to the part of the view the
//! tile can reach keeps the fragment work near one screen's worth, whatever the number of tiles.

use photocraft_engine::equirect::{Camera, ViewProjection};

/// Cells across and down the view in which [`tile_boxes`] samples the projection.
const COLS: usize = 24;
const ROWS: usize = 16;

/// For each tile `[x, y, w, h]` (document pixels) of a `doc` sized latlong document seen by camera `look` (yaw,
/// pitch, vertical field of view, degrees) through view box `view` (`[x0, y0, x1, y1]`, device pixels), the device
/// pixel box its pixels can show in, or `None` when the view can't see it. Conservative: a box may be larger than
/// what the tile covers, never smaller.
pub(crate) fn tile_boxes(look: [f32; 3], view: [f32; 4], doc: [u32; 2], tiles: &[[u32; 4]]) -> Vec<Option<[f32; 4]>> {
    let (w, h) = (f64::from(view[2] - view[0]), f64::from(view[3] - view[1]));
    let mut boxes = vec![None; tiles.len()];
    if !(w >= 1.0 && h >= 1.0) || doc[0] == 0 || doc[1] == 0 {
        return boxes;
    }
    let cam = Camera { yaw: f64::from(look[0]), pitch: f64::from(look[1]), fov: f64::from(look[2]) };
    let vp = ViewProjection::new(cam, w, h);
    let (iw, ih) = (f64::from(doc[0]), f64::from(doc[1]));
    let at = |i: usize, j: usize| [w * i as f64 / COLS as f64, h * j as f64 / ROWS as f64];
    let corners: Vec<[f64; 2]> = (0..=ROWS).flat_map(|j| (0..=COLS).map(move |i| (i, j))).map(|(i, j)| at(i, j)).collect();
    let mapped: Vec<[f64; 2]> = corners.iter().map(|&[x, y]| vp.view_to_equirect(x, y, iw, ih)).collect();
    let corner = |i: usize, j: usize| mapped[j * (COLS + 1) + i];
    // The poles, where every longitude meets: a cell near one reaches all of them.
    let north = vp.dir_to_view([0.0, 1.0, 0.0]);
    let south = vp.dir_to_view([0.0, -1.0, 0.0]);
    let (cw, ch) = (w / COLS as f64, h / ROWS as f64);
    let near = |pole: Option<[f64; 2]>, x0: f64, y0: f64| pole.is_some_and(|[x, y]| x >= x0 - cw && x <= x0 + 2.0 * cw && y >= y0 - ch && y <= y0 + 2.0 * ch);
    for j in 0..ROWS {
        for i in 0..COLS {
            let quad = [corner(i, j), corner(i + 1, j), corner(i, j + 1), corner(i + 1, j + 1)];
            // Longitudes unwrapped next to the first corner's, so a cell over the seam spans it.
            let base = quad[0][0];
            let us = quad.map(|[u, _]| u - iw * ((u - base) / iw).round());
            let (mut u0, mut u1) = (us.iter().copied().fold(f64::MAX, f64::min), us.iter().copied().fold(f64::MIN, f64::max));
            let (mut v0, mut v1) = (quad.iter().map(|q| q[1]).fold(f64::MAX, f64::min), quad.iter().map(|q| q[1]).fold(f64::MIN, f64::max));
            // A cell's edges are great circles, which bow between its corners on the latlong.
            let (du, dv) = ((u1 - u0) * 0.25 + 2.0, (v1 - v0) * 0.25 + 2.0);
            (u0, u1, v0, v1) = (u0 - du, u1 + du, v0 - dv, v1 + dv);
            let [x0, y0] = at(i, j);
            if near(north, x0, y0) {
                (u0, u1, v0) = (0.0, iw, 0.0);
            }
            if near(south, x0, y0) {
                (u0, u1, v1) = (0.0, iw, ih);
            }
            if u1 - u0 >= iw / 2.0 {
                (u0, u1) = (0.0, iw);
            }
            let cell = [view[0] + x0 as f32, view[1] + y0 as f32, view[0] + (x0 + cw) as f32, view[1] + (y0 + ch) as f32];
            for (b, &[tx, ty, tw, th]) in boxes.iter_mut().zip(tiles) {
                let (tx0, ty0, tx1, ty1) = (f64::from(tx), f64::from(ty), f64::from(tx) + f64::from(tw), f64::from(ty) + f64::from(th));
                let rows = v1 >= ty0 && v0 < ty1;
                let columns = [-iw, 0.0, iw].iter().any(|s| u1 + s >= tx0 && u0 + s < tx1);
                if rows && columns {
                    *b = Some(match *b {
                        Some([a, b, c, d]) => [a.min(cell[0]), b.min(cell[1]), c.max(cell[2]), d.max(cell[3])],
                        None => cell,
                    });
                }
            }
        }
    }
    boxes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tile of the latlong pixel each view pixel shows, for a grid of tiles `side` square.
    fn tile_at(look: [f32; 3], w: f64, h: f64, doc: [u32; 2], side: u32, x: f64, y: f64) -> usize {
        let cam = Camera { yaw: f64::from(look[0]), pitch: f64::from(look[1]), fov: f64::from(look[2]) };
        let [u, v] = ViewProjection::new(cam, w, h).view_to_equirect(x, y, f64::from(doc[0]), f64::from(doc[1]));
        let across = doc[0].div_ceil(side) as usize;
        let (c, r) = ((u as u32 / side) as usize, (v.min(f64::from(doc[1]) - 0.5) as u32 / side) as usize);
        r * across + c
    }

    fn tiles(doc: [u32; 2], side: u32) -> Vec<[u32; 4]> {
        let mut out = Vec::new();
        for y in (0..doc[1]).step_by(side as usize) {
            for x in (0..doc[0]).step_by(side as usize) {
                out.push([x, y, side.min(doc[0] - x), side.min(doc[1] - y)]);
            }
        }
        out
    }

    /// Every view pixel lies in the box of the tile it shows, for looks across the seam, at the poles and wide and
    /// narrow fields of view: a box that missed one would leave a hole in the view.
    #[test]
    fn each_view_pixel_is_inside_its_tiles_box() {
        let doc = [8192, 4096];
        let side = 1024;
        let all = tiles(doc, side);
        let (w, h) = (1200.0, 700.0);
        for look in [[0.0, 0.0, 90.0], [179.0, 10.0, 120.0], [-90.0, 89.0, 150.0], [30.0, -85.0, 60.0], [100.0, 40.0, 8.0]] {
            let boxes = tile_boxes(look, [50.0, 20.0, 50.0 + w as f32, 20.0 + h as f32], doc, &all);
            for j in 0..70 {
                for i in 0..120 {
                    let (x, y) = (f64::from(i) * 10.0 + 5.0, f64::from(j) * 10.0 + 5.0);
                    let t = tile_at(look, w, h, doc, side, x, y);
                    let b = boxes[t].unwrap_or_else(|| panic!("tile {t} has no box looking {look:?} at {x}, {y}"));
                    let (sx, sy) = (50.0 + x as f32, 20.0 + y as f32);
                    assert!(sx >= b[0] && sx <= b[2] && sy >= b[1] && sy <= b[3], "pixel {x}, {y} of {look:?} is outside tile {t}'s box {b:?}");
                }
            }
        }
    }

    /// A narrow look at the horizon leaves the tiles behind the camera and at the poles undrawn.
    #[test]
    fn tiles_out_of_view_get_no_box() {
        let doc = [8192, 4096];
        let all = tiles(doc, 1024);
        let boxes = tile_boxes([0.0, 0.0, 30.0], [0.0, 0.0, 800.0, 600.0], doc, &all);
        let drawn = boxes.iter().filter(|b| b.is_some()).count();
        assert!(drawn > 0 && drawn < all.len() / 4, "{drawn} of {} tiles drawn", all.len());
        // The first column is longitude −180°, straight behind a camera looking at 0°.
        assert!(boxes[0].is_none() && boxes[all.len() - 8].is_none());
    }
}
