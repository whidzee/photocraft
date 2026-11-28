//! View › 360° View timings (#15726): what a press, each live-preview update and the release cost, on a large
//! latlong, for the Brush (`ProjectedPreview`), a dab tool run through `paint.projectedTool` (the Dodge tool) and
//! the Gradient (drawn by the screen, and re-rendered on the CPU as it is where the screen can't). Run in release:
//! `cargo run --release -p photocraft-engine --example bench_360_view -- [--quick] [--reps N] [--json path]`.
//!
//! Full cases: a 16384 × 8192 and an 8192 × 4096 8-bit latlong, seen in a 2560 × 1440 view (a 1440p screen) at a
//! 90° field of view. Quick cases: 2048 × 1024 in a 1280 × 720 view. A preview update is timed with the stroke so
//! far at 50, 200 and 800 points (3 view pixels apart, so a longer stroke covers more), as the view re-renders it
//! while it is drawn; the overlay line is the size of the picture each update hands the screen.

use std::path::PathBuf;
use std::time::Instant;

use photocraft_engine::Session;
use photocraft_engine::brush_cmds::ProjectedPreview;
use photocraft_engine::projected_tool::ProjectedTool;
use photocraft_testkit::perf;
use serde_json::{Value, json};

struct Options {
    quick: bool,
    reps: usize,
    json: Option<PathBuf>,
}

fn options() -> Result<Options, String> {
    let mut o = Options { quick: false, reps: 5, json: None };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--quick" => o.quick = true,
            "--reps" => o.reps = args.next().ok_or("--reps needs an integer")?.parse().map_err(|_| "invalid --reps")?,
            "--json" => o.json = Some(args.next().ok_or("--json needs a path")?.into()),
            _ => return Err(format!("unknown option: {arg}")),
        }
    }
    if !(1..=25).contains(&o.reps) {
        return Err("expected 1..=25 repetitions".into());
    }
    Ok(o)
}

/// `n` points 3 view pixels apart, as a pointer moving steadily draws them: a stroke that zigzags across the middle
/// of a `w` × `h` view, so a longer stroke covers more of it.
fn stroke(n: usize, w: f64, h: f64) -> Vec<[f64; 3]> {
    let (x0, x1) = (w * 0.15, w * 0.85);
    let span = x1 - x0;
    (0..n)
        .map(|i| {
            let d = i as f64 * 3.0;
            let row = (d / span).floor();
            let along = d - row * span;
            let x = if row as i64 % 2 == 0 { x0 + along } else { x1 - along };
            [x, h * 0.3 + (row * 40.0) % (h * 0.4), 1.0]
        })
        .collect()
}

fn time(reps: usize, mut f: impl FnMut() -> Result<(), String>) -> Result<Vec<f64>, String> {
    f()?; // warm up
    let mut samples = Vec::with_capacity(reps);
    for _ in 0..reps {
        let start = Instant::now();
        f()?;
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(samples)
}

fn push(rows: &mut Vec<Value>, name: String, samples: &[f64]) {
    let p50 = perf::Summary::of(samples).map_or(0.0, |s| s.p50);
    println!("{name}: {p50:.1} ms p50, samples_ms={samples:.1?}");
    rows.push(perf::row(&name, samples, perf::process_peak_rss_bytes(), None));
}

fn case(rows: &mut Vec<Value>, o: &Options, (iw, ih): (u32, u32), (w, h): (f64, f64)) -> Result<(), String> {
    let e = |e: photocraft_engine::EngineError| e.to_string();
    let mut s = Session::new();
    s.execute("file.new", json!({"width": iw, "height": ih, "background": "white"})).map_err(e)?;
    s.execute("tools.setColors", json!({"foreground": [0.2, 0.3, 0.6, 1.0], "background": [0.9, 0.8, 0.5, 1.0]})).map_err(e)?;
    // A latlong with something in it, so the preview's comparisons have work to do.
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [iw, ih]})).map_err(e)?;
    let tag = format!("{iw}x{ih} view {w}x{h}");
    let projection = json!({"yaw": 20.0, "pitch": 10.0, "fov": 90.0, "width": w, "height": h});

    // The Brush: the view copy on the press, each update, and the release.
    let brush = json!({"erase": false, "zoom": 1.0, "size": 60.0, "projection": projection});
    push(rows, format!("brush press {tag}"), &time(o.reps, || ProjectedPreview::brush(&s, &brush).map(drop).map_err(e))?);
    // An update adds the points a frame brings (10 here) to a stroke of n: timed from a fresh press each time, so
    // it pays for resampling the blocks those points reach.
    for n in [50, 200, 800] {
        let pts = stroke(n, w, h);
        let mut samples = Vec::new();
        let mut bytes = 0;
        for _ in 0..=o.reps {
            let mut preview = ProjectedPreview::brush(&s, &brush).map_err(e)?;
            preview.render(&pts[..n - 10]);
            let start = Instant::now();
            let over = std::hint::black_box(preview.render(&pts));
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
            bytes = over.map_or(0, |o| o.px.len() * 4);
        }
        samples.remove(0);
        println!("brush overlay {n} points {tag}: {:.2} MB per update", bytes as f64 / 1e6);
        push(rows, format!("brush update {n} points {tag}"), &samples);
    }
    let mut commit = brush.clone();
    commit["points"] = json!(stroke(200, w, h));
    push(
        rows,
        format!("brush release 200 points {tag}"),
        &time(o.reps, || {
            s.execute("paint.projectedStroke", commit.clone()).map_err(e)?;
            s.execute("edit.undo", json!({})).map(drop).map_err(e)
        })?,
    );

    // A dab tool through paint.projectedTool: the view copy, each update, and the release.
    let dodge = json!({"command": "paint.dodge", "params": {"size": 60, "exposure": 50}, "projection": projection});
    push(rows, format!("dodge press {tag}"), &time(o.reps, || ProjectedTool::new(&s, &dodge).map(drop).map_err(e))?);
    let mut tool = ProjectedTool::new(&s, &dodge).map_err(e)?;
    for n in [50, 200, 800] {
        let pts = stroke(n, w, h);
        push(
            rows,
            format!("dodge update {n} points {tag}"),
            &time(o.reps, || {
                std::hint::black_box(tool.preview(&pts));
                Ok(())
            })?,
        );
    }
    let mut commit = dodge.clone();
    commit["points"] = json!(stroke(200, w, h));
    push(
        rows,
        format!("dodge release 200 points {tag}"),
        &time(o.reps, || {
            s.execute("paint.projectedTool", commit.clone()).map_err(e)?;
            s.execute("edit.undo", json!({})).map(drop).map_err(e)
        })?,
    );

    // The Gradient: the screen draws it while it is dragged (what each update works out for that), or, where it
    // can't, each update re-renders the whole view.
    let vp = photocraft_engine::equirect::ViewProjection::new(photocraft_engine::equirect::Camera { yaw: 20.0, pitch: 10.0, fov: 90.0 }, w, h);
    push(
        rows,
        format!("gradient update drawn by the screen {tag}"),
        &time(o.reps, || {
            std::hint::black_box(photocraft_engine::projected_fill::gradient_look(&s, &json!({}), &vp, [w * 0.3, h * 0.5], [w * 0.7, h * 0.5]));
            Ok(())
        })?,
    );
    let gradient = json!({"command": "paint.gradient", "params": {}, "projection": projection});
    let mut tool = ProjectedTool::new(&s, &gradient).map_err(e)?;
    let ends = [[w * 0.3, h * 0.5, 1.0], [w * 0.7, h * 0.5, 1.0]];
    push(
        rows,
        format!("gradient update re-rendered {tag}"),
        &time(o.reps, || {
            std::hint::black_box(tool.preview(&ends));
            Ok(())
        })?,
    );
    let mut commit = gradient.clone();
    commit["points"] = json!(ends);
    push(
        rows,
        format!("gradient release {tag}"),
        &time(o.reps, || {
            s.execute("paint.projectedTool", commit.clone()).map_err(e)?;
            s.execute("edit.undo", json!({})).map(drop).map_err(e)
        })?,
    );
    Ok(())
}

/// A document size and the view size it is looked at in.
type Case = ((u32, u32), (f64, f64));

fn run() -> Result<(), String> {
    let o = options()?;
    let mut rows = Vec::new();
    let cases: &[Case] = if o.quick { &[((2048, 1024), (1280.0, 720.0))] } else { &[((8192, 4096), (2560.0, 1440.0)), ((16384, 8192), (2560.0, 1440.0))] };
    for &(doc, view) in cases {
        case(&mut rows, &o, doc, view)?;
    }
    if let Some(path) = &o.json {
        let report = perf::report("bench_360_view", json!({"quick": o.quick, "reps": o.reps}), rows, None);
        perf::write_report(&path.to_string_lossy(), &report)?;
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
