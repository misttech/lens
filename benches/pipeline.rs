// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Per-stage and end-to-end timings for the filtering pipeline.
//!
//! Lens has to be invisible in the workflow. A tool that saves an agent tokens
//! and costs a developer a visible pause has traded the wrong resource, so the
//! budget is single-digit milliseconds on top of the child process — and the
//! only way to keep a budget is to measure it.
//!
//! Two things are checked, and only one of them can gate CI.
//!
//! * **Growth**, at 1x, 4x and 16x the input, is the gate. It compares a machine
//!   against itself, so it means the same thing on a laptop and on a CI runner —
//!   and it catches the failure that actually hurts. A constant-factor slowdown
//!   is a cost; superlinear growth is a hang waiting for a large enough command.
//!   Parsing was quadratic once, and nobody noticed until a 40,000-line run took
//!   half a second.
//! * **Latency**, against the committed baseline, is reported but does not fail
//!   unless asked with `--gate-latency`. Absolute microseconds are a property of
//!   the machine that recorded them; gating shared runners on one laptop's
//!   numbers produces failures that mean nothing, and a gate that cries wolf is
//!   a gate somebody turns off.
//!
//! Hand-rolled rather than criterion: this runs a fixture a few hundred times
//! and reports percentiles, and a statistics framework is a large dependency for
//! arithmetic that fits on a screen.
//!
//! ```text
//! cargo bench                     measure, gate on growth, report latency
//! cargo bench -- --gate-latency   also fail on regression against the baseline
//! cargo bench -- --save           rewrite the baseline from this run
//! cargo bench -- --format json    emit the measurements as data
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lens::pipeline::{Ctx, Stream, default_stages};
use lens::render::{self, Level};

/// How much slower than the baseline a stage may get before this fails.
const REGRESSION_LIMIT: f64 = 1.20;

/// Growth allowed when the input grows 4x. Linear is 4.0; the headroom absorbs
/// allocator and cache effects on shared hardware. Quadratic growth is 16.
const GROWTH_LIMIT: f64 = 8.0;

/// Samples per measurement.
const SAMPLES: usize = 100;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // cargo passes its own flags to bench binaries; ignore what is not ours.
    let save = args.iter().any(|a| a == "--save");
    let gate_latency = args.iter().any(|a| a == "--gate-latency");
    let json = args.windows(2).any(|w| w[0] == "--format" && w[1] == "json");

    let fixtures = load_fixtures();
    if fixtures.is_empty() {
        eprintln!("no fixtures in tests/fixtures — nothing to measure");
        std::process::exit(1);
    }

    let measured = measure_all(&fixtures);

    if json {
        println!("{}", to_json(&measured));
    } else {
        report(&measured);
    }

    let baseline_path = baseline_path();
    if save {
        std::fs::create_dir_all(baseline_path.parent().expect("a parent directory"))
            .expect("create bench/results");
        std::fs::write(&baseline_path, to_json(&measured)).expect("write baseline");
        println!("\nbaseline written to {}", baseline_path.display());
        return;
    }

    // Growth gates; latency reports. See the module comment for why.
    let mut failed = check_growth(&fixtures);
    let regressed = check_against_baseline(&measured, &baseline_path);
    if gate_latency {
        failed |= regressed;
    } else if regressed {
        println!("(latency is reported only; pass --gate-latency to fail on it)");
    }

    if failed {
        std::process::exit(1);
    }
}

/// One fixture: a name and the bytes a real command produced.
struct Fixture {
    name: String,
    raw: Vec<u8>,
}

/// Timings for one fixture.
struct Measured {
    fixture: String,
    bytes: usize,
    lines: usize,
    /// Stage name to (p50, p99), in microseconds.
    stages: BTreeMap<String, (u128, u128)>,
    total: (u128, u128),
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/results/micro-baseline.json")
}

fn load_fixtures() -> Vec<Fixture> {
    let Ok(entries) = std::fs::read_dir(fixtures_dir()) else { return Vec::new() };
    let mut fixtures: Vec<Fixture> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "txt"))
        .filter_map(|e| {
            Some(Fixture {
                name: e.path().file_stem()?.to_string_lossy().into_owned(),
                raw: std::fs::read(e.path()).ok()?,
            })
        })
        .collect();
    fixtures.sort_by(|a, b| a.name.cmp(&b.name));
    fixtures
}

/// Run `body` `SAMPLES` times and return its p50 and p99 in microseconds.
fn percentiles(mut body: impl FnMut()) -> (u128, u128) {
    // One untimed run so the first sample is not paying for cold allocations.
    body();

    let mut times: Vec<Duration> = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        body();
        times.push(start.elapsed());
    }
    times.sort();

    let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize].as_micros();
    (at(0.50), at(0.99))
}

fn measure_all(fixtures: &[Fixture]) -> Vec<Measured> {
    fixtures.iter().map(measure).collect()
}

fn measure(fixture: &Fixture) -> Measured {
    let ctx = Ctx::default();
    let parsed = lens::adapters::parse(&fixture.raw, Stream::Stdout);

    let mut stages = BTreeMap::new();
    stages.insert(
        "parse".to_string(),
        percentiles(|| {
            std::hint::black_box(lens::adapters::parse(&fixture.raw, Stream::Stdout));
        }),
    );

    // Each stage is measured on the document as the stages before it left it,
    // because that is the input it actually sees. Measuring every stage against
    // a pristine parse would flatter the ones that run late.
    let mut progressive = parsed.clone();
    for stage in default_stages() {
        let before = progressive.clone();
        stages.insert(
            stage.name().to_string(),
            percentiles(|| {
                let mut doc = before.clone();
                stage.apply(&mut doc, &ctx);
                std::hint::black_box(&doc);
            }),
        );
        stage.apply(&mut progressive, &ctx);
    }

    let rendered = progressive.clone();
    stages.insert(
        "render".to_string(),
        percentiles(|| {
            std::hint::black_box(render::render(&rendered, Level::Detail, Some("00000000")));
        }),
    );

    let total = percentiles(|| {
        std::hint::black_box(end_to_end(&fixture.raw, &ctx));
    });

    Measured {
        fixture: fixture.name.clone(),
        bytes: fixture.raw.len(),
        lines: parsed.line_count(),
        stages,
        total,
    }
}

/// Everything Lens does between capturing bytes and having output to write.
fn end_to_end(raw: &[u8], ctx: &Ctx) -> String {
    let mut doc = lens::adapters::parse(raw, Stream::Stdout);
    lens::pipeline::run(&mut doc, &default_stages(), ctx);
    render::render(&doc, Level::Detail, Some("00000000"))
}

fn report(measured: &[Measured]) {
    println!("{:<24} {:>9} {:<12} {:>9} {:>9}", "fixture", "bytes", "stage", "p50", "p99");
    for m in measured {
        let mut first = true;
        for (stage, (p50, p99)) in &m.stages {
            let label = if first { m.fixture.as_str() } else { "" };
            let bytes = if first { format_bytes(m.bytes) } else { String::new() };
            println!("{label:<24} {bytes:>9} {stage:<12} {:>8.2}ms {:>8.2}ms", ms(*p50), ms(*p99));
            first = false;
        }
        println!(
            "{:<24} {:>9} {:<12} {:>8.2}ms {:>8.2}ms   ({} lines)",
            "",
            "",
            "total",
            ms(m.total.0),
            ms(m.total.1),
            m.lines
        );
        println!();
    }
}

fn ms(micros: u128) -> f64 {
    micros as f64 / 1000.0
}

fn format_bytes(bytes: usize) -> String {
    if bytes >= 1024 { format!("{} KB", bytes / 1024) } else { format!("{bytes} B") }
}

/// The measurements as data, for the baseline file and for anything that wants
/// to plot them. Hand-written: the shape is four fields deep and fixed.
fn to_json(measured: &[Measured]) -> String {
    let mut out = String::from("{\n");
    for (i, m) in measured.iter().enumerate() {
        out.push_str(&format!("  \"{}\": {{\n", m.fixture));
        out.push_str(&format!("    \"bytes\": {},\n", m.bytes));
        out.push_str(&format!("    \"lines\": {},\n", m.lines));
        out.push_str("    \"stages\": {\n");
        for (j, (stage, (p50, p99))) in m.stages.iter().enumerate() {
            let comma = if j + 1 < m.stages.len() { "," } else { "" };
            out.push_str(&format!(
                "      \"{stage}\": {{ \"p50_us\": {p50}, \"p99_us\": {p99} }}{comma}\n"
            ));
        }
        out.push_str("    },\n");
        out.push_str(&format!(
            "    \"total\": {{ \"p50_us\": {}, \"p99_us\": {} }}\n",
            m.total.0, m.total.1
        ));
        out.push_str(if i + 1 < measured.len() { "  },\n" } else { "  }\n" });
    }
    out.push_str("}\n");
    out
}

/// Read `{"fixture": {"stages": {"name": {"p50_us": N}}}}` out of the baseline.
///
/// A hand-rolled reader for a file this benchmark wrote itself. Adding a JSON
/// parser to the dependency tree to read one shape is not a trade worth making,
/// and a malformed baseline is reported rather than guessed at.
fn read_baseline(path: &Path) -> Option<BTreeMap<String, BTreeMap<String, u128>>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut out: BTreeMap<String, BTreeMap<String, u128>> = BTreeMap::new();
    let mut fixture = String::new();

    for line in text.lines() {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();

        if indent == 2 && trimmed.ends_with('{') {
            fixture = trimmed.trim_matches(|c| c == '"' || c == ':' || c == '{' || c == ' ').into();
            out.entry(fixture.clone()).or_default();
            continue;
        }
        let Some((name, rest)) = trimmed.split_once("\": {") else { continue };
        let Some(p50) = rest.split("\"p50_us\":").nth(1) else { continue };
        let Ok(value) = p50.trim().trim_end_matches(['}', ',', ' ']).trim().parse::<u128>() else {
            continue;
        };
        let name = name.trim_start_matches('"').to_string();
        out.entry(fixture.clone()).or_default().insert(name, value);
    }

    Some(out)
}

/// Report stages that got materially slower than the committed baseline.
///
/// The baseline records one machine's microseconds. Comparing another machine
/// against it measures the hardware, not the change, so the caller decides
/// whether this is a gate.
fn check_against_baseline(measured: &[Measured], path: &Path) -> bool {
    let Some(baseline) = read_baseline(path) else {
        println!("no baseline at {} — run `cargo bench -- --save`", path.display());
        return false;
    };

    let mut failed = false;
    for m in measured {
        let Some(before) = baseline.get(&m.fixture) else {
            println!("note: {} is not in the baseline yet", m.fixture);
            continue;
        };
        for (stage, (p50, _)) in &m.stages {
            let Some(&was) = before.get(stage) else { continue };
            // Sub-100us measurements are dominated by scheduling noise, and a
            // gate that fires on noise gets disabled by whoever it wakes up.
            if was < 100 {
                continue;
            }
            let ratio = *p50 as f64 / was as f64;
            if ratio > REGRESSION_LIMIT {
                println!(
                    "REGRESSION {}/{}: {:.2}ms vs {:.2}ms baseline ({:.0}% slower)",
                    m.fixture,
                    stage,
                    ms(*p50),
                    ms(was),
                    (ratio - 1.0) * 100.0
                );
                failed = true;
            }
        }
    }

    if !failed {
        println!("no regression against {}", path.display());
    }
    failed
}

/// Fail when four times the input costs much more than four times the time.
///
/// The check that matters most here: a constant-factor slowdown is a cost, but
/// superlinear growth is a hang waiting for a large enough command.
fn check_growth(fixtures: &[Fixture]) -> bool {
    let ctx = Ctx::default();
    let mut failed = false;

    println!("growth (1x → 4x → 16x)");
    for fixture in fixtures {
        let time_at = |scale: usize| -> f64 {
            let raw = fixture.raw.repeat(scale);
            let (p50, _) = percentiles_few(|| {
                std::hint::black_box(end_to_end(&raw, &ctx));
            });
            p50 as f64
        };

        let (one, four, sixteen) = (time_at(1).max(1.0), time_at(4), time_at(16));
        let (g4, g16) = (four / one, sixteen / four);
        let verdict = if g4 > GROWTH_LIMIT || g16 > GROWTH_LIMIT { "SUPERLINEAR" } else { "ok" };
        println!("  {:<24} 4x={g4:>5.1}  16x={g16:>5.1}  {verdict}", fixture.name);

        if verdict != "ok" {
            failed = true;
        }
    }
    println!();
    failed
}

/// Percentiles from a handful of samples, for measurements too slow to repeat
/// a hundred times.
fn percentiles_few(mut body: impl FnMut()) -> (u128, u128) {
    body();
    let mut times: Vec<Duration> = Vec::with_capacity(9);
    for _ in 0..9 {
        let start = Instant::now();
        body();
        times.push(start.elapsed());
    }
    times.sort();
    (times[4].as_micros(), times[8].as_micros())
}
