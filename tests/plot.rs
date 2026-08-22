// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! End-to-end checks on `lens plot`, `lens lenses` and `lens config`.
//!
//! The property that makes plot honest: it prints the same
//! `ResolvedPipeline` the runner executes. Dry mode must never spawn the
//! command — `lens plot git push` is a question, not a push.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("lens-plot-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("store")).expect("create sandbox");
        fs::create_dir_all(root.join("logs")).expect("create sandbox");
        Sandbox { root }
    }

    fn lens(&self, args: &[&str]) -> Output {
        Command::new(lens_bin())
            .args(args)
            .env("LENS_STORE", self.root.join("store"))
            .env("LENS_LOG_DIR", self.root.join("logs"))
            .env("LENS_CONFIG", self.root.join("config.toml"))
            .output()
            .expect("run lens")
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

#[test]
fn plot_dry_does_not_spawn_the_command() {
    let sandbox = Sandbox::new("no-spawn");
    let marker = sandbox.root.join("spawned");
    let cmd = sandbox.root.join("would-run");
    fs::write(&cmd, format!("#!/bin/sh\necho spawned > {}\n", marker.display())).unwrap();
    #[cfg(unix)]
    {
        fs::set_permissions(&cmd, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let out = sandbox.lens(&["plot", cmd.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(!marker.exists(), "dry plot executed the command");
    let stdout = text(&out.stdout);
    assert!(stdout.contains("lens plot"), "{stdout}");
    assert!(stdout.contains("default"), "{stdout}");
}

#[test]
fn plot_json_matches_config_for_the_same_argv() {
    // One function, two callers: if these ever diverge, plot has started lying.
    let sandbox = Sandbox::new("same-pipeline");
    let plot = sandbox.lens(&["plot", "--format", "json", "git", "diff"]);
    let config = sandbox.lens(&["config", "git", "diff"]);
    assert!(plot.status.success(), "{}", text(&plot.stderr));
    assert!(config.status.success(), "{}", text(&config.stderr));

    let plot_v: serde_json::Value = serde_json::from_slice(&plot.stdout).unwrap();
    let config_v: serde_json::Value = serde_json::from_slice(&config.stdout).unwrap();
    assert_eq!(plot_v, config_v);
    assert_eq!(plot_v["lens"]["value"], "git-diff");
    assert_eq!(plot_v["budget"]["value"], 6000);
    assert_eq!(plot_v["adapter"]["value"], "git");
}

#[test]
fn plot_selects_git_over_default() {
    let sandbox = Sandbox::new("git-lens");
    let out = sandbox.lens(&["plot", "--format", "json", "git", "status"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["lens"]["value"], "git");
    assert_eq!(v["budget"]["value"], 4000);
}

#[test]
fn use_forces_a_lens_on_plot() {
    let sandbox = Sandbox::new("use-plot");
    let out = sandbox.lens(&["--use", "git-diff", "plot", "--format", "json", "cargo", "test"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["lens"]["value"], "git-diff");
}

#[test]
fn unknown_use_is_an_error_not_a_run() {
    let sandbox = Sandbox::new("bad-use");
    let out = sandbox.lens(&["--use", "no-such-lens", "true"]);
    assert!(!out.status.success());
    let stderr = text(&out.stderr);
    assert!(stderr.contains("no lens named"), "{stderr}");
    assert!(stderr.starts_with("lens:"), "{stderr}");
}

#[test]
fn plot_trace_reads_a_stored_run() {
    let sandbox = Sandbox::new("trace");
    let run = sandbox.lens(&["sh", "-c", "echo ok"]);
    assert!(run.status.success(), "{}", text(&run.stderr));
    let log = fs::read_to_string(sandbox.root.join("logs").join("lens.log")).unwrap();
    let handle = log
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .rfind(|r| r["type"] == "run")
        .and_then(|r| r["handle"].as_str().map(str::to_string))
        .expect("a handle for the run");

    let out = sandbox.lens(&["plot", "--handle", &handle]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("sh -c echo ok") || stdout.contains("raw"), "{stdout}");
    assert!(stdout.contains("tok"), "{stdout}");
}

#[test]
fn lenses_lists_the_builtins() {
    let sandbox = Sandbox::new("lenses");
    let out = sandbox.lens(&["lenses"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("default"), "{stdout}");
    assert!(stdout.contains("git-diff"), "{stdout}");
    assert!(stdout.contains("builtin"), "{stdout}");
}

#[test]
fn config_path_prints_the_override() {
    let sandbox = Sandbox::new("config-path");
    let out = sandbox.lens(&["config", "--path"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let printed = text(&out.stdout);
    assert!(printed.contains("config.toml"), "{printed}");
}
