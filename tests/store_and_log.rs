// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! End-to-end checks on the store and the command log.
//!
//! Every test here isolates its state through `LENS_STORE` and `LENS_LOG_DIR`
//! pointed at a temp directory. Nothing in this file may touch a developer's
//! real cache or logs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A temp directory holding one test's store and log.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("lens-e2e-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("store")).expect("create sandbox");
        fs::create_dir_all(root.join("logs")).expect("create sandbox");
        Sandbox { root }
    }

    fn store(&self) -> PathBuf {
        self.root.join("store")
    }

    fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Run `lens <args>` against this sandbox.
    fn lens(&self, args: &[&str]) -> Output {
        self.lens_with(args, &[])
    }

    /// The same, with extra environment.
    fn lens_with(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(lens_bin());
        cmd.args(args)
            .env("LENS_STORE", self.store())
            .env("LENS_LOG_DIR", self.logs())
            .env("LENS_CONFIG", self.root.join("config.toml"));
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd.output().expect("run lens")
    }

    /// Run `lens <args>` with raw `OsStr` arguments — for arguments that are
    /// not valid UTF-8 and so cannot go through [`Sandbox::lens`].
    #[cfg(unix)]
    fn lens_os(&self, args: &[&std::ffi::OsStr]) -> Output {
        Command::new(lens_bin())
            .args(args)
            .env("LENS_STORE", self.store())
            .env("LENS_LOG_DIR", self.logs())
            .output()
            .expect("run lens")
    }

    /// Every run directory in the store.
    fn runs(&self) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(self.store()) else { return Vec::new() };
        entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()
    }

    /// Every log line, parsed.
    fn records(&self) -> Vec<serde_json::Value> {
        let Ok(text) = fs::read_to_string(self.logs().join("lens.log")) else { return Vec::new() };
        text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
    }

    /// Just the run records.
    fn run_records(&self) -> Vec<serde_json::Value> {
        self.records().into_iter().filter(|r| r["type"] == "run").collect()
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

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

#[test]
fn a_captured_run_is_stored_byte_for_byte() {
    // Nothing is permanently lost. The store is what makes that
    // true, so what lands in it has to be exactly what the command produced.
    let sandbox = Sandbox::new("stored");
    let out = sandbox.lens(&["sh", "-c", "printf 'one\\ntwo\\n'; printf 'bad\\n' >&2; exit 2"]);
    assert_eq!(out.status.code(), Some(2));

    let runs = sandbox.runs();
    assert_eq!(runs.len(), 1, "one run captured");
    let run = &runs[0];

    assert_eq!(read(&run.join("stdout")), b"one\ntwo\n");
    assert_eq!(read(&run.join("stderr")), b"bad\n");
    // What the caller saw and what was stored are the same bytes.
    assert_eq!(read(&run.join("stdout")), out.stdout);
    assert_eq!(read(&run.join("stderr")), out.stderr);

    let meta: serde_json::Value = serde_json::from_slice(&read(&run.join("meta.json"))).unwrap();
    assert_eq!(meta["exit_code"], 2);
    assert_eq!(meta["stdout_bytes"], 8);
    assert_eq!(meta["stderr_bytes"], 4);
    assert_eq!(meta["argv"][0], "sh");
    assert!(meta["timestamp"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn binary_output_is_stored_without_reinterpretation() {
    let sandbox = Sandbox::new("binary");
    sandbox.lens(&["sh", "-c", r"printf '\001\377\000z'"]);
    let run = &sandbox.runs()[0];
    assert_eq!(read(&run.join("stdout")), vec![0x01, 0xff, 0x00, b'z']);
}

#[test]
fn the_same_run_twice_addresses_one_entry() {
    // Content addressing: identical argv, directory and output is the same run,
    // so a repeated command does not grow the store.
    let sandbox = Sandbox::new("dedupe");
    sandbox.lens(&["sh", "-c", "echo stable"]);
    sandbox.lens(&["sh", "-c", "echo stable"]);
    assert_eq!(sandbox.runs().len(), 1);
}

#[test]
fn different_output_is_a_different_run() {
    let sandbox = Sandbox::new("distinct");
    sandbox.lens(&["sh", "-c", "echo one"]);
    sandbox.lens(&["sh", "-c", "echo two"]);
    assert_eq!(sandbox.runs().len(), 2);
}

#[test]
fn every_invocation_is_recorded_including_passthrough() {
    // A passthrough record carrying its reason is exactly what answers
    // "filtering didn't work" — so it must exist, and it must say why.
    let sandbox = Sandbox::new("recorded");
    sandbox.lens(&["sh", "-c", "echo captured"]);
    sandbox.lens_with(&["sh", "-c", "echo raw"], &[("LENS_MODE", "raw")]);
    sandbox.lens(&["git", "status", "--porcelain"]);

    let records = sandbox.run_records();
    assert_eq!(records.len(), 3, "one record per invocation");

    let captured = &records[0];
    assert_eq!(captured["passthrough"], false);
    assert_eq!(captured["exit"], 0);
    assert!(captured["handle"].is_string(), "a captured run has a handle");
    assert_eq!(captured["out_bytes"], 9);

    let raw = &records[1];
    assert_eq!(raw["passthrough"], true);
    assert_eq!(raw["reason"], "mode_raw");
    // Passthrough execs the child, so this process never learns the outcome.
    // Recording a placeholder would put fabricated exit codes into `lens stats`.
    assert!(raw.get("exit").is_none());
    assert!(raw.get("handle").is_none());

    let porcelain = &records[2];
    assert_eq!(porcelain["passthrough"], true);
    assert_eq!(porcelain["reason"], "machine_readable_flag");
}

#[test]
fn a_passthrough_run_stores_nothing() {
    // Nothing was captured, so there is nothing to store — and inventing an
    // entry would mean a handle that resolves to output Lens never saw.
    let sandbox = Sandbox::new("passthrough-store");
    sandbox.lens_with(&["sh", "-c", "echo raw"], &[("LENS_MODE", "raw")]);
    assert!(sandbox.runs().is_empty());
}

#[test]
fn the_run_record_carries_the_handle_of_its_stored_run() {
    let sandbox = Sandbox::new("handle-link");
    sandbox.lens(&["sh", "-c", "echo linked"]);

    let handle = sandbox.run_records()[0]["handle"].as_str().unwrap().to_string();
    let stored = sandbox.store().join(&handle);
    assert!(stored.is_dir(), "the handle in the log resolves in the store");
    assert_eq!(read(&stored.join("stdout")), b"linked\n");
}

#[test]
fn an_unwritable_log_does_not_fail_the_command() {
    // The command is the product; the log is bookkeeping.
    let sandbox = Sandbox::new("unwritable-log");
    let out = sandbox
        .lens_with(&["sh", "-c", "echo fine"], &[("LENS_LOG_DIR", "/proc/nope/not/writable")]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "fine\n");
}

#[test]
fn an_unwritable_store_does_not_fail_the_command() {
    let sandbox = Sandbox::new("unwritable-store");
    let out =
        sandbox.lens_with(&["sh", "-c", "echo fine"], &[("LENS_STORE", "/proc/nope/not/writable")]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "fine\n");

    // ...but it is reported, because a run that cannot be re-derived is a
    // degraded run even though the command succeeded.
    let records = sandbox.records();
    let warnings = records.iter().filter(|r| r["lvl"] == "warn").count();
    assert!(warnings > 0, "the failure is logged even though it is swallowed");
}

#[test]
fn logging_never_touches_the_childs_streams() {
    // Debug logging is on and the log still contributes nothing to
    // what the caller reads.
    let sandbox = Sandbox::new("stream-purity");
    let out = sandbox.lens_with(&["sh", "-c", "echo only-this"], &[("LENS_LOG", "trace")]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "only-this\n");
    assert_eq!(out.stderr, b"");
    assert!(!sandbox.records().is_empty(), "trace logging was actually on");
}

#[test]
fn the_log_never_carries_command_output() {
    // Command output can contain secrets, and the log is not the store.
    //
    // The secret is constructed by the command rather than written in it, so
    // that finding it in the log means output leaked — argv is logged on
    // purpose and would otherwise make this test pass for the wrong reason.
    let sandbox = Sandbox::new("no-secrets");
    sandbox.lens_with(
        &[
            "sh",
            "-c",
            r"printf 'hunter%d-topsecret
' 2",
        ],
        &[("LENS_LOG", "debug")],
    );

    let text = fs::read_to_string(sandbox.logs().join("lens.log")).unwrap();
    assert!(text.contains("printf"), "the command itself is logged");
    assert!(!text.contains("hunter2-topsecret"), "its output is not");
}

#[test]
fn log_off_writes_nothing() {
    let sandbox = Sandbox::new("log-off");
    let out = sandbox.lens_with(&["sh", "-c", "echo quiet"], &[("LENS_LOG", "off")]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "quiet\n");
    assert!(sandbox.records().is_empty());
    // The run is still stored: the log is off, not the tool.
    assert_eq!(sandbox.runs().len(), 1);
}

#[test]
fn an_unparseable_log_level_falls_back_rather_than_failing() {
    let sandbox = Sandbox::new("bad-level");
    let out = sandbox.lens_with(&["sh", "-c", "echo fine"], &[("LENS_LOG", "verbose")]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(sandbox.run_records().len(), 1, "fell back to the default level");
}

#[test]
fn stats_aggregates_the_log() {
    let sandbox = Sandbox::new("stats");
    sandbox.lens(&["sh", "-c", "echo a"]);
    sandbox.lens(&["sh", "-c", "echo b"]);
    sandbox.lens_with(&["sh", "-c", "echo c"], &[("LENS_MODE", "raw")]);

    let out = sandbox.lens(&["stats"]);
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("runs"), "{report}");
    assert!(report.contains("passthrough"), "{report}");
    assert!(report.contains("mode_raw"), "the reason breakdown: {report}");
    assert!(report.contains("sh"), "grouped by command: {report}");
}

#[test]
fn stats_filters_by_command_and_window() {
    let sandbox = Sandbox::new("stats-filter");
    sandbox.lens(&["sh", "-c", "echo a"]);

    let out = sandbox.lens(&["stats", "--cmd", "cargo"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("no runs recorded"));

    let out = sandbox.lens(&["stats", "--since", "1d", "--cmd", "sh"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("runs"));

    // A typo is rejected loudly rather than reported as an empty window.
    let out = sandbox.lens(&["stats", "--since", "7"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unrecognized duration"));
}

#[test]
fn stats_on_an_empty_log_says_so() {
    let sandbox = Sandbox::new("stats-empty");
    let out = sandbox.lens(&["stats"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("no runs recorded"));
}

#[test]
fn logs_tails_the_records() {
    let sandbox = Sandbox::new("logs");
    for i in 0..5 {
        sandbox.lens(&["sh", "-c", &format!("echo line{i}")]);
    }

    let out = sandbox.lens(&["logs", "--tail", "2"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.lines().count(), 2);
    // Newest last, and each line is a whole record.
    for line in text.lines() {
        let value: serde_json::Value = serde_json::from_str(line).expect("a parseable record");
        assert!(value["t"].is_string());
    }
}

#[test]
fn logs_filters_by_level() {
    let sandbox = Sandbox::new("logs-level");
    sandbox.lens(&["sh", "-c", "echo a"]);
    // Run records are info; asking for warn and above excludes them.
    let out = sandbox.lens(&["logs", "--level", "warn"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "");

    let out = sandbox.lens(&["logs", "--level", "info"]);
    assert!(!String::from_utf8_lossy(&out.stdout).trim().is_empty());
}

#[test]
fn lens_subcommands_do_not_record_runs() {
    // `lens stats` is not a command Lens ran for the user; counting it would
    // make the log a record of itself.
    let sandbox = Sandbox::new("subcommand-noise");
    sandbox.lens(&["sh", "-c", "echo a"]);
    sandbox.lens(&["stats"]);
    sandbox.lens(&["logs"]);
    assert_eq!(sandbox.run_records().len(), 1);
}

#[test]
fn show_re_emits_a_stored_run_without_re_running_it() {
    // The store's reason to exist: a view can be re-derived from a handle, and
    // deriving it does not execute anything. The marker file proves that — if
    // `show` had re-run the command, there would be two of them.
    let sandbox = Sandbox::new("show");
    let marker = sandbox.root.join("ran");
    let script =
        format!("echo >> {}; printf 'out\\n'; printf 'err\\n' >&2; exit 3", marker.display());

    let run = sandbox.lens(&["sh", "-c", &script]);
    assert_eq!(run.status.code(), Some(3));
    let handle = sandbox.run_records()[0]["handle"].as_str().unwrap().to_string();

    let shown = sandbox.lens(&["show", &handle]);
    assert_eq!(shown.stdout, run.stdout, "byte-identical to what the command produced");
    assert_eq!(shown.stderr, run.stderr);
    assert_eq!(shown.status.code(), Some(3), "the stored run's exit code is reported");

    let ran = fs::read_to_string(&marker).unwrap();
    assert_eq!(ran.lines().count(), 1, "the command ran once, not twice");
}

#[test]
fn show_rejects_a_handle_that_could_escape_the_store() {
    let sandbox = Sandbox::new("show-traversal");
    let out = sandbox.lens(&["show", "../../etc/passwd"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a handle"));
    assert!(out.stdout.is_empty());
}

#[test]
fn show_says_so_when_a_run_is_not_there() {
    let sandbox = Sandbox::new("show-missing");
    let out = sandbox.lens(&["show", "deadbeef"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no run"));
}

#[test]
fn show_level_three_is_the_stored_bytes() {
    // The store's promise, checked at the level that claims to be raw: no
    // parse, no re-render, the bytes as the command wrote them.
    let sandbox = Sandbox::new("show-level");
    sandbox.lens(&["sh", "-c", "printf 'x\ny\n'"]);
    let handle = sandbox.run_records()[0]["handle"].as_str().unwrap().to_string();

    let out = sandbox.lens(&["show", &handle, "--level", "3"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, b"x\ny\n");
}

#[cfg(unix)]
#[test]
fn a_non_utf8_argument_runs_instead_of_panicking() {
    // std::env::args() panics on the first non-UTF-8 argument; Lens has to
    // resolve that doubt toward running the command, not toward a crash.
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let sandbox = Sandbox::new("non-utf8-arg");
    let bad = OsStr::from_bytes(b"\xffbad\xfe");
    let out = sandbox.lens_os(&["/bin/echo".as_ref(), bad]);

    assert_eq!(out.status.code(), Some(0), "no panic, no signal death");
    let mut expected = bad.as_bytes().to_vec();
    expected.push(b'\n');
    assert_eq!(out.stdout, expected, "the raw byte argument reached the child untouched");
}

#[test]
fn show_treats_a_missing_meta_as_failed_not_succeeded() {
    // meta.json can be absent from a run interrupted mid-write (store.rs
    // documents the possibility). Defaulting the exit code to 0 there would
    // report success for a run whose fate was never recorded — exactly the
    // lie this tool exists to prevent.
    let sandbox = Sandbox::new("show-no-meta");
    let run = sandbox.lens(&["sh", "-c", "printf 'kept\\n'"]);
    assert_eq!(run.status.code(), Some(0));
    let handle = sandbox.run_records()[0]["handle"].as_str().unwrap().to_string();

    let run_dir = sandbox.store().join(&handle);
    fs::remove_file(run_dir.join("meta.json")).expect("remove meta.json");

    let out = sandbox.lens(&["show", &handle]);
    assert_ne!(out.status.code(), Some(0), "an unrecorded fate is never reported as success");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no recorded exit code"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"kept\n", "the stored content is still retrievable without meta.json");
}
