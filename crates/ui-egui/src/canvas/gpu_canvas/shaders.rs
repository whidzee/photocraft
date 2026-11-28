pub(super) const CANVAS_WGSL: &str = r#"
struct View {
    a: vec4<f32>, // screen_w, screen_h, scale (device px per doc px), pixels_per_point
    b: vec4<f32>, // doc origin x, y (device px), doc w, h (doc px)
    c: vec4<f32>, // filter mode, lod, 360° View (1 on), 360° View turn quaternion w
    d: vec4<f32>, // 360° View centre x, y (device px), display (0 none, 1 LUT, 2 LUT + gamut, 3 matrix/shaper), output linear
    e: vec4<f32>, // 360° View yaw, pitch (radians), view-plane units per device px; gamut warning opacity
    f: vec4<f32>, // 360° View turn quaternion xyz, 32-bit preview gain (2^exposure; 0 = off)
    g: vec4<f32>, // gamut warning rgb, 32-bit preview 1 / gamma
    h: vec4<f32>, // cos, sin, center_doc_x, center_doc_y
    // 360° View's Gradient being dragged: start direction and angle, along tangent and length, across direction and
    // Radial length, (on, shape), then its colours from start to end (straight RGBA, opacity in alpha).
    ga: vec4<f32>,
    gc: vec4<f32>,
    ge: vec4<f32>,
    gm: vec4<f32>,
    gcol: array<vec4<f32>, 64>,
};
struct Tile { r: vec4<f32>, core: vec4<f32> }; // x, y, w, h in doc px; drawn x0, y0, x1, y1

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var samp: sampler;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var<uniform> tile: Tile;
@group(2) @binding(0) var lut: texture_3d<f32>;
@group(2) @binding(1) var display_curves: texture_2d<f32>;
struct DisplayMatrix { r: vec4<f32>, g: vec4<f32>, b: vec4<f32> };
@group(2) @binding(2) var<uniform> display_matrix: DisplayMatrix;

fn curve_entry(index: u32) -> vec3<f32> {
    let width = textureDimensions(display_curves).x;
    return textureLoad(display_curves, vec2<i32>(i32(index % width), i32(index / width)), 0).rgb;
}

fn display_curve(c: vec3<f32>, offset: u32, intervals: f32) -> vec3<f32> {
    let p = clamp(c, vec3(0.0), vec3(1.0)) * intervals;
    let lo = vec3<u32>(floor(p));
    let hi = min(lo + vec3(1u), vec3(u32(intervals)));
    let f = fract(p);
    return vec3(
        mix(curve_entry(offset + lo.r).r, curve_entry(offset + hi.r).r, f.r),
        mix(curve_entry(offset + lo.g).g, curve_entry(offset + hi.g).g, f.g),
        mix(curve_entry(offset + lo.b).b, curve_entry(offset + hi.b).b, f.b)
    );
}

fn matrix_display(c: vec3<f32>) -> vec3<f32> {
    // Layout matches cms::GpuMatrixShaper: 4097 input entries, then 4096 inverse-curve
    // entries indexed by the fourth root. This preserves near-black precision without
    // interpolating across clipped corners of a coarse RGB cube.
    let v = vec4(display_curve(c, 0u, 4096.0), 1.0);
    let linear = vec3(dot(display_matrix.r, v), dot(display_matrix.g, v), dot(display_matrix.b, v));
    let u = sqrt(sqrt(clamp(linear, vec3(0.0), vec3(1.0))));
    return display_curve(u, 4097u, 4095.0);
}

struct VOut { @builtin(position) pos: vec4<f32> };

fn corner(i: u32) -> vec2<f32> {
    var c = array<vec2<f32>, 6>(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0));
    return c[i];
}

fn px_to_clip(p: vec2<f32>) -> vec4<f32> {
    let n = p / view.a.xy * 2.0 - 1.0;
    return vec4(n.x, -n.y, 0.0, 1.0);
}

fn screen_center() -> vec2<f32> {
    return view.b.xy + view.h.zw * view.a.z;
}

fn doc_to_px(d: vec2<f32>) -> vec2<f32> {
    let cs = view.h.xy;
    let q = d - view.h.zw;
    let r = vec2(q.x * cs.x - q.y * cs.y, q.x * cs.y + q.y * cs.x);
    return screen_center() + r * view.a.z;
}

fn px_to_doc(p: vec2<f32>) -> vec2<f32> {
    let cs = view.h.xy;
    let q = (p - screen_center()) / view.a.z;
    return vec2(q.x * cs.x + q.y * cs.y, -q.x * cs.y + q.y * cs.x) + view.h.zw;
}


fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3(2.4));
    return select(hi, lo, c <= vec3(0.04045));
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let hi = 1.055 * pow(max(c, vec3(1e-12)), vec3(1.0 / 2.4)) - 0.055;
    return select(hi, c * 12.92, c <= vec3(0.0031308));
}

// --- document tiles ------------------------------------------------------------------------

@vertex
fn vs_tile(@builtin(vertex_index) vi: u32) -> VOut {
    let p = doc_to_px(tile.r.xy + tile.r.zw * corner(vi));
    return VOut(px_to_clip(p));
}

@fragment
fn fs_tile(in: VOut) -> @location(0) vec4<f32> {
    let p = in.pos.xy;
    let scale = view.a.z;
    let d = px_to_doc(p);                  // document pixel coordinates
    // A viewport page's apron is only sampled; its neighbour draws those pixels.
    if (any(d < tile.core.xy) || any(d >= tile.core.zw)) { discard; }
    let size = tile.r.zw;
    let t = d - tile.r.xy;                 // texel coordinates within this tile
    let mode = view.c.x;
    var col: vec4<f32>;
    if (mode < 0.5) {
        // Zoomed out: trilinear through the mip chain.
        col = textureSampleLevel(tex, samp, t / size, view.c.y);
    } else if (mode < 1.5) {
        col = textureSampleLevel(tex, samp, t / size, 0.0);
    } else {
        // Crisp texels with a one-device-pixel anti-aliased edge ("sharp bilinear"), worked out
        // in stored texels: a reduced viewport page's texel covers several document pixels, so
        // the edge must not depend on the reduction, only on where and how large it is drawn
        // (#11122). `per` is texels per document pixel (1 for a full-resolution texture).
        let texels = vec2<f32>(textureDimensions(tex, 0));
        let per = texels / size;
        let tt = t * per - 0.5;
        let i = floor(tt);
        let f = clamp((tt - i - 0.5) * (scale / per) + 0.5, vec2(0.0), vec2(1.0));
        col = textureSampleLevel(tex, samp, (i + 0.5 + f) / texels, 0.0);
    }
    col = shade(col);
    // Document pixels only, premultiplied: the canvas draws the checkerboard under them and the pixel grid over them.
    if (col.a <= 0.0) { discard; }
    return col;
}

// The 32-bit preview, the display transform and the output encoding of a sampled document colour `col_in`
// (premultiplied, as the texture stores it).
fn shade(col_in: vec4<f32>) -> vec4<f32> {
    var col = col_in;
    if (view.f.w > 0.0 && col.a > 0.0) {
        // View › 32-bit Preview Options: exposure and gamma in linear light on the sRGB-encoded
        // texture value, before the display LUT. A float canvas keeps values above 1.0, so they
        // are brought into range by the exposure instead of clipped.
        let lin = srgb_to_linear(max(col.rgb / col.a, vec3(0.0))) * view.f.w;
        let v = linear_to_srgb(min(pow(max(lin, vec3(1e-12)), vec3(view.g.w)), vec3(1.0)));
        col = vec4(v * col.a, col.a);
    }
    if (view.d.z > 0.5 && col.a > 0.0) {
        // Colour management, Proof Colors, Gamut Warning: the document's display LUT (alpha flags
        // out-of-gamut colours).
        let c = clamp(col.rgb / col.a, vec3(0.0), vec3(1.0));
        var shown: vec3<f32>;
        if (view.d.z > 2.5) {
            shown = matrix_display(c);
        } else {
            let n = f32(textureDimensions(lut).x);
            let l = textureSampleLevel(lut, samp, (c * (n - 1.0) + 0.5) / n, 0.0);
            shown = select(l.rgb, mix(l.rgb, view.g.xyz, view.e.w), view.d.z > 1.5 && l.a > 0.5);
        }
        col = vec4(shown * col.a, col.a);
    }
    var rgb = col.rgb;
    if (view.d.w > 0.5 && col.a > 0.0) {
        rgb = srgb_to_linear(rgb / col.a) * col.a;
    }
    return vec4(rgb, col.a);
}

// --- 360° View -----------------------------------------------------------------------------

@vertex
fn vs_proj(@builtin(vertex_index) vi: u32) -> VOut {
    // The whole screen; the scissor keeps each tile's draw to where it can show.
    let c = corner(vi) * 2.0 - 1.0;
    return VOut(vec4(c.x, c.y, 0.0, 1.0));
}

const PI: f32 = 3.14159265358979;

// The mip level of one device pixel's footprint at view-plane offset (dx, dy) and latitude `lat` of a `size`
// document. A pixel off the optical axis covers less of the sphere, by 1 / sqrt(1 + r^2) across; near the poles the
// rows stretch sideways, which only counts within about 2 degrees of them. Half a level sharp.
fn proj_lod(k: f32, dx: f32, dy: f32, lat: f32, size: vec2<f32>) -> f32 {
    let ang = k / sqrt(1.0 + dx * dx + dy * dy);
    let along_lon = ang * size.y / PI;
    let c = max(cos(lat), 0.0005);
    let along_lat = ang * size.x / (2.0 * PI) / c;
    let near_pole = 1.0 - smoothstep(0.0, 0.035, c);
    let fp = max(along_lon, mix(along_lon, along_lat * 0.25, near_pole));
    return max(log2(max(fp, 1e-6)) - 0.5, 0.0);
}

// The 360° View Gradient's colour in direction `d` (photocraft_engine::projected_fill::SphereGradient::t).
fn sphere_gradient(d: vec3<f32>) -> vec4<f32> {
    let along = asin(clamp(dot(d, view.gc.xyz), -1.0, 1.0)) - view.ga.w;
    let across = asin(clamp(dot(d, view.ge.xyz), -1.0, 1.0));
    let shape = view.gm.y;
    var t: f32;
    if (shape < 0.5) {
        t = along / view.gc.w;
    } else if (shape < 1.5) {
        t = acos(clamp(dot(d, view.ga.xyz), -1.0, 1.0)) / view.ge.w;
    } else if (shape < 2.5) {
        let a = atan2(across, along) / (2.0 * PI);
        t = a - floor(a);
    } else if (shape < 3.5) {
        t = abs(along) / view.gc.w;
    } else {
        t = (abs(along) + abs(across)) / view.gc.w;
    }
    let k = clamp(t, 0.0, 1.0) * 63.0;
    let i = u32(floor(k));
    return mix(view.gcol[i], view.gcol[min(i + 1u, 63u)], k - floor(k));
}

// The document as a latlong panorama seen from the centre of the sphere (photocraft_engine::equirect: the view
// pixel's direction, then its longitude and latitude).
@fragment
fn fs_proj(in: VOut) -> @location(0) vec4<f32> {
    let k = view.e.z;
    let dx = (in.pos.x - view.d.x) * k;
    let dy = -(in.pos.y - view.d.y) * k;
    let sy = sin(view.e.x);
    let cy = cos(view.e.x);
    let sp = sin(view.e.y);
    let cp = cos(view.e.y);
    let f = vec3(cp * sy, sp, cp * cy);
    let r = vec3(cy, 0.0, -sy);
    let u = vec3(-sp * sy, cp, -sp * cy);
    var dir = normalize(f + dx * r + dy * u);
    let seen = dir;
    // Image › Rotate Panorama's preview: the content this direction will show once applied.
    let q = view.f.xyz;
    dir = dir + 2.0 * cross(q, cross(q, dir) + view.c.w * dir);
    let lon = atan2(dir.x, dir.z);
    let lat = asin(clamp(dir.y, -1.0, 1.0));
    let size = view.b.zw;
    var d = vec2((lon / (2.0 * PI) + 0.5) * size.x, (0.5 - lat / PI) * size.y);
    d.x = d.x - floor(d.x / size.x) * size.x;
    d.y = clamp(d.y, 0.0, size.y - 0.001);
    let t = d - tile.r.xy;
    // Each tile's draw covers its share of the view: only the directions that land in the tile are its own.
    if (any(t < vec2(0.0)) || any(t >= tile.r.zw) || any(d < tile.core.xy) || any(d >= tile.core.zw)) { discard; }
    var col = textureSampleLevel(tex, samp, t / tile.r.zw, proj_lod(k, dx, dy, lat, size));
    if (view.gm.x > 0.5) {
        // The Gradient being dragged, painted over the (premultiplied) document in Normal mode.
        let g = sphere_gradient(seen);
        col = vec4(g.rgb * g.a + col.rgb * (1.0 - g.a), g.a + col.a * (1.0 - g.a));
    }
    col = shade(col);
    if (col.a <= 0.0) { discard; }
    return col;
}
"#;

pub(super) const ENCODE_WGSL: &str = r#"
@group(0) @binding(0) var acc: texture_2d<f32>;
@group(0) @binding(1) var<uniform> offset: vec4<i32>; // x, y, encode sRGB, keep values above 1.0

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, c * 12.92, c <= vec3(0.0031308));
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(acc, vec2<i32>(floor(pos.xy)) - offset.xy, 0);
    let a = clamp(c.a, 0.0, 1.0);
    var rgb = max(c.rgb, vec3(0.0));
    if (offset.w == 0) {
        rgb = min(rgb, vec3(1.0));
    }
    if (offset.z != 0) {
        // Linear composite: store its sRGB encoding (8-bit precision where it matters).
        rgb = srgb_encode(rgb);
    }
    return vec4(rgb * a, a);
}
"#;

pub(super) const MIP_WGSL: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct V { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> V {
    let uv = vec2(f32((vi << 1u) & 2u), f32(vi & 2u));
    return V(vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0), uv);
}

@fragment
fn fs(in: V) -> @location(0) vec4<f32> {
    // A bilinear tap at the destination texel centre averages the 2×2 source block.
    return textureSampleLevel(src, samp, in.uv, 0.0);
}
"#;
