// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! End-to-end checks on the git adapter.
//!
//! Structure is opt-in: `--use git-diff` forces it onto a script that prints
//! git-shaped output. Parse failure must not fail the command — generic runs
//! instead, and the view still contains what the child wrote.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("lens-git-{}-{name}", std::process::id()));
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
fn a_hunk_header_survives_filtering() {
    let sandbox = Sandbox::new("hunk");
    let script = "printf '%s\\n' \
'diff --git a/src/lib.rs b/src/lib.rs' \
'--- a/src/lib.rs' \
'+++ b/src/lib.rs' \
'@@ -12,7 +12,8 @@ pub fn parse()' \
' keep' \
'-old' \
'+new'";
    let out = sandbox.lens(&["--use", "git-diff", "sh", "-c", script]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("@@ -12,7 +12,8 @@ pub fn parse()"), "{stdout}");
}

#[test]
fn unparseable_git_output_falls_back_to_generic() {
    let sandbox = Sandbox::new("fallback");
    let out =
        sandbox.lens(&["--use", "git-diff", "sh", "-c", "printf 'hello from git-diff lens\\n'"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("hello from git-diff lens"));
    let log = fs::read_to_string(sandbox.root.join("logs").join("lens.log")).unwrap_or_default();
    assert!(log.contains("adapter parse failed") || log.contains("falling back"), "{log}");
}

#[test]
fn plot_names_the_git_adapter_for_git_diff() {
    let sandbox = Sandbox::new("plot-adapter");
    let out = sandbox.lens(&["plot", "--format", "json", "git", "diff"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["adapter"]["value"], "git");
    assert_eq!(v["lens"]["value"], "git-diff");
}
