// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! End-to-end checks on what the filtered view is allowed to be.
//!
//! Every test here runs the real binary against a real child, in a sandbox of
//! its own. The properties are the ones that make the tool's claim true rather
//! than merely plausible: a failing command always shows its failure, anything
//! left out is announced, and the raw view is byte-identical to what the command
//! produced.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("lens-filter-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("store")).expect("create sandbox");
        fs::create_dir_all(root.join("logs")).expect("create sandbox");
        Sandbox { root }
    }

    fn lens(&self, args: &[&str]) -> Output {
        self.lens_with(args, &[])
    }

    fn lens_with(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(lens_bin());
        cmd.args(args)
            .env("LENS_STORE", self.root.join("store"))
            .env("LENS_LOG_DIR", self.root.join("logs"));
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd.output().expect("run lens")
    }

    /// Run a script through Lens and return the handle of the stored run.
    fn run_script(&self, script: &str) -> (Output, String) {
        let out = self.lens(&["sh", "-c", script]);
        let text = fs::read_to_string(self.root.join("logs").join("lens.log")).unwrap_or_default();
        let handle = text
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .rfind(|r| r["type"] == "run")
            .and_then(|r| r["handle"].as_str().map(str::to_string))
            .expect("a handle for the run");
        (out, handle)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn lens_bin() -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("lens")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn has_marker(out: &str) -> bool {
    out.lines().any(|line| line.trim_start().starts_with("[lens:"))
}

/// Output with a great deal of churn and one line that matters.
const NOISY: &str = r#"
for i in $(seq 1 200); do echo "   Compiling crate_$i v1.0.0"; done
echo "warning: unused variable"
echo "warning: unused variable"
echo "warning: unused variable"
echo "the one line that matters"
"#;

#[test]
fn a_failing_command_always_shows_its_failure() {
    // The worst bug this tool can have: a filtered view of a failed command
    // that reads as success. Checked across levels and across both streams.
    let sandbox = Sandbox::new("failure-visible");

    let out = sandbox.lens(&[
        "sh",
        "-c",
        "for i in $(seq 1 100); do echo \"   Compiling c_$i v1.0\"; done; \
         echo 'error[E0308]: mismatched types' >&2; exit 1",
    ]);

    assert_eq!(out.status.code(), Some(1));
    let combined = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(combined.contains("E0308"), "the failure vanished:\n{combined}");
}

#[test]
fn a_failure_with_no_error_word_still_surfaces() {
    // Exit code 3 and nothing that looks like an error anywhere. The floor has
    // to rescue the tail, or the view says the command was fine.
    let sandbox = Sandbox::new("silent-failure");
    let out =
        sandbox.lens(&["sh", "-c", "echo 'doing a thing'; echo 'the last thing it said'; exit 3"]);

    assert_eq!(out.status.code(), Some(3));
    assert!(text(&out.stdout).contains("the last thing it said"), "{}", text(&out.stdout));
}

#[test]
fn the_last_line_of_a_long_output_is_not_lost() {
    // The naive-truncation trap: everything before it is noise, and the line
    // that matters is at the very end.
    let sandbox = Sandbox::new("last-line");
    let out = sandbox.lens(&["sh", "-c", NOISY]);
    assert!(text(&out.stdout).contains("the one line that matters"), "{}", text(&out.stdout));
}

#[test]
fn anything_left_out_is_announced() {
    // The central promise. A view shorter than the raw stream that says nothing
    // is indistinguishable from a command that produced less.
    let sandbox = Sandbox::new("announced");
    let (out, handle) = sandbox.run_script(NOISY);

    let filtered = text(&out.stdout);
    let raw = sandbox.lens(&["show", &handle, "--level", "3"]);
    assert!(filtered.len() < text(&raw.stdout).len(), "the view should be shorter");
    assert!(has_marker(&filtered), "and must say so:\n{filtered}");
}

#[test]
fn a_marker_names_a_handle_but_not_a_command() {
    // Both halves of the announcement: the marker names a handle that resolves,
    // and does not tell the reader to go and fetch everything with it. An agent
    // shown `lens show <handle> --level 3` followed it and pulled the entire raw
    // output, which costs more than not filtering at all.
    let sandbox = Sandbox::new("marker-handle");
    let (out, _) = sandbox.run_script(NOISY);

    let marker = text(&out.stdout)
        .lines()
        .find(|l| l.trim_start().starts_with("[lens:"))
        .expect("a marker")
        .to_string();

    let handle = marker
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        .find(|word| word.len() == 8 && word.bytes().all(|b| b.is_ascii_hexdigit()))
        .expect("a handle in the marker");

    assert!(!marker.contains("lens show"), "a marker offers, it does not instruct: {marker}");
    assert!(!marker.contains("--level"), "{marker}");

    let raw = sandbox.lens(&["show", handle, "--level", "3"]);
    assert_eq!(raw.status.code(), Some(0));
    assert!(text(&raw.stdout).contains("Compiling crate_200"), "the handle resolves to everything");
}

#[test]
fn the_raw_view_is_byte_identical_to_the_command() {
    // What makes the store's promise checkable, and the reason level 3 does not
    // go through the pipeline at all.
    let sandbox = Sandbox::new("raw-identical");
    let script = "printf 'a\\n\\nb\\n'; printf '\\001\\377 binary\\n'";
    let (_, handle) = sandbox.run_script(script);

    let raw = sandbox.lens(&["show", &handle, "--level", "3"]);
    let direct = Command::new("sh").args(["-c", script]).output().expect("run directly");
    assert_eq!(raw.stdout, direct.stdout);
}

#[test]
fn output_that_needs_no_filtering_is_unchanged() {
    // Filtering is a service, not a tax. Output with nothing to remove comes
    // back exactly as written, with no marker to teach readers to ignore.
    let sandbox = Sandbox::new("untouched");
    let out = sandbox.lens(&["sh", "-c", "printf 'one\\ntwo\\nthree\\n'"]);
    assert_eq!(text(&out.stdout), "one\ntwo\nthree\n");
    assert!(!has_marker(&text(&out.stdout)));
}

#[test]
fn file_line_references_still_resolve_after_filtering() {
    // Line addressability: a `file:line` in the view has to mean the same line
    // it meant in the raw stream. Nothing renumbers, so the reference survives
    // verbatim alongside the source excerpt it points at.
    let sandbox = Sandbox::new("addressability");
    let out = sandbox.lens(&[
        "sh",
        "-c",
        "for i in $(seq 1 50); do echo \"   Compiling c_$i v1.0\"; done; \
         printf 'error[E0308]: mismatched types\\n  --> src/main.rs:42:9\\n   |\\n42 |     let x: u8 = 1;\\n' >&2; \
         exit 1",
    ]);

    let err = text(&out.stderr);
    assert!(err.contains("src/main.rs:42:9"), "{err}");
    assert!(err.contains("42 |     let x: u8 = 1;"), "the excerpt stays with its message: {err}");
}

#[test]
fn the_filtered_levels_narrow() {
    // Levels 1 and 2 are subsets of the raw stream and of each other, so asking
    // for less detail means less output.
    //
    // Level 0 is deliberately excluded: it is a different shape rather than a
    // smaller subset — counts instead of content — and on output with nothing
    // worth showing, a two-line summary is legitimately longer than level 1's
    // single marker. It is still bounded by the raw view, which is asserted
    // separately below.
    let sandbox = Sandbox::new("levels");
    let (_, handle) = sandbox.run_script(NOISY);

    let size = |level: &str| sandbox.lens(&["show", &handle, "--level", level]).stdout.len();

    assert!(size("2") <= size("3"), "level 2 showed more than raw");
    assert!(size("1") <= size("2"), "level 1 showed more than level 2");
    assert!(size("0") <= size("3"), "level 0 showed more than raw");
}

#[test]
fn a_stream_the_command_never_wrote_to_stays_empty() {
    // Rendering "0 lines · 0 failing" onto stderr for a command that succeeded
    // quietly would be output the caller has to filter, which is backwards.
    let sandbox = Sandbox::new("empty-stderr");
    let (_, handle) = sandbox.run_script("echo only-stdout");
    for level in ["0", "1", "2", "3"] {
        let out = sandbox.lens(&["show", &handle, "--level", level]);
        assert!(out.stderr.is_empty(), "level {level} invented stderr: {:?}", text(&out.stderr));
    }
}

#[test]
fn every_filtered_level_announces_what_it_left_out() {
    let sandbox = Sandbox::new("levels-announce");
    let (_, handle) = sandbox.run_script(NOISY);

    for level in ["0", "1", "2"] {
        let out = sandbox.lens(&["show", &handle, "--level", level]);
        assert!(has_marker(&text(&out.stdout)), "level {level} said nothing about the rest");
    }
}

#[test]
fn show_reports_the_stored_runs_exit_code_at_every_level() {
    let sandbox = Sandbox::new("show-exit");
    let (_, handle) = sandbox.run_script("echo out; exit 7");
    for level in ["0", "1", "2", "3"] {
        let out = sandbox.lens(&["show", &handle, "--level", level]);
        assert_eq!(out.status.code(), Some(7), "level {level}");
    }
}

#[test]
fn an_unknown_level_is_refused() {
    let sandbox = Sandbox::new("bad-level");
    let (_, handle) = sandbox.run_script("echo x");
    let out = sandbox.lens(&["show", &handle, "--level", "9"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(text(&out.stderr).contains("--level"));
}

#[test]
fn raw_mode_bypasses_filtering_entirely() {
    let sandbox = Sandbox::new("raw-mode");
    let script = "for i in $(seq 1 50); do echo \"   Compiling c_$i v1.0\"; done";
    let out = sandbox.lens_with(&["sh", "-c", script], &[("LENS_MODE", "raw")]);
    let direct = Command::new("sh").args(["-c", script]).output().expect("run directly");
    assert_eq!(out.stdout, direct.stdout);
    assert!(!has_marker(&text(&out.stdout)));
}

#[test]
fn the_run_record_reports_what_filtering_did() {
    // `lens stats` reduction comes from these, so they have to describe the
    // view the caller received rather than what was captured.
    let sandbox = Sandbox::new("record-counts");
    sandbox.run_script(NOISY);

    let log = fs::read_to_string(sandbox.root.join("logs").join("lens.log")).unwrap();
    let record: serde_json::Value = log
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .find(|r: &serde_json::Value| r["type"] == "run")
        .expect("a run record");

    let in_lines = record["in_lines"].as_u64().expect("in_lines");
    let out_lines = record["out_lines"].as_u64().expect("out_lines");
    assert!(out_lines < in_lines, "{out_lines} of {in_lines} lines kept");
    assert!(record["in_tok"].as_u64().unwrap() > record["out_tok"].as_u64().unwrap());
    assert_eq!(record["level"], 2);
    assert_eq!(record["stages"][0], "ansi");
}

#[test]
fn stats_reports_reduction_in_output_tokens() {
    let sandbox = Sandbox::new("stats-reduction");
    sandbox.run_script(NOISY);

    let out = sandbox.lens(&["stats"]);
    let report = text(&out.stdout);
    assert!(report.contains("reduction"), "{report}");
    // Labelled as output tokens, never as a bill: prompt caching and extra
    // turns both break that inference.
    assert!(report.contains("output tokens only"), "{report}");
}

#[test]
fn ansi_escapes_do_not_reach_the_view() {
    let sandbox = Sandbox::new("ansi");
    let out = sandbox.lens(&["sh", "-c", "printf '\\033[31merror: red\\033[0m\\n'; exit 1"]);
    assert!(text(&out.stdout).contains("error: red"));
    assert!(!text(&out.stdout).contains('\u{1b}'), "an escape survived");
}

#[test]
fn repeated_lines_collapse_with_a_count() {
    let sandbox = Sandbox::new("dedupe");
    let out = sandbox.lens(&["sh", "-c", "for i in $(seq 1 50); do echo 'warning: same'; done"]);
    let view = text(&out.stdout);
    assert_eq!(view.matches("warning: same").count(), 1, "{view}");
    assert!(view.contains("×50"), "the count is what replaces the copies: {view}");
}

#[test]
fn empty_output_stays_empty() {
    let sandbox = Sandbox::new("empty");
    let out = sandbox.lens(&["sh", "-c", "true"]);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

#[test]
fn a_budget_drops_ordinary_output_and_keeps_the_failure() {
    let sandbox = Sandbox::new("budget-keeps-error");
    // Blank lines make one block per line, so the failure floor (which force-
    // keeps the tail of a failing stdout) cannot rescue the whole stream.
    let script = r#"
for i in $(seq 1 80); do printf 'ordinary line %s of filler text for the budget\n\n' "$i"; done
echo 'error[E0308]: mismatched types' >&2
exit 1
"#;
    let out = sandbox.lens(&["--budget", "40", "sh", "-c", script]);
    assert_eq!(out.status.code(), Some(1));
    let combined = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(combined.contains("E0308"), "the failure vanished:\n{combined}");
    assert!(has_marker(&combined), "the budget has to announce what it dropped:\n{combined}");
    assert!(
        text(&out.stdout).matches("ordinary line").count() < 80,
        "the budget left every ordinary line in place:\n{}",
        text(&out.stdout)
    );
}

#[test]
fn debug_report_arrives_after_the_child() {
    let sandbox = Sandbox::new("debug-report");
    let out = sandbox.lens_with(
        &["sh", "-c", "echo 'hello from the child'; echo 'error: boom' >&2; exit 1"],
        &[("LENS_DEBUG", "1")],
    );
    let stdout = text(&out.stdout);
    let stderr = text(&out.stderr);
    assert!(stdout.contains("hello from the child"), "{stdout}");
    assert!(stderr.contains("error: boom"), "{stderr}");
    let report_at = stderr.find("lens report").unwrap_or_else(|| panic!("no report in {stderr}"));
    let child_at = stderr.find("error: boom").unwrap();
    assert!(child_at < report_at, "the report mixed into the child's stderr:\n{stderr}");
    assert!(stderr.contains("removed by stage") || stderr.contains("preserved"), "{stderr}");
}

#[test]
fn explain_prints_the_report_for_a_stored_run() {
    let sandbox = Sandbox::new("explain");
    let (_, handle) = sandbox.run_script(NOISY);
    let out = sandbox.lens(&["explain", &handle]);
    assert_eq!(out.status.code(), Some(0));
    let report = text(&out.stdout);
    assert!(report.contains("lens report"), "{report}");
    assert!(report.contains(&handle), "{report}");
    assert!(report.contains("removed by stage"), "{report}");
}
