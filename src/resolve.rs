// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Finding the real binary, and deciding whether to filter at all.
//!
//! Two questions, answered before anything is executed:
//!
//! 1. **Which binary is this?** Lens must never re-enter itself. `LENS.md`
//!    invariant 1 rules out PATH symlinks precisely so this stays simple, but a
//!    user who ignores that advice should get an honest command rather than a
//!    fork bomb.
//! 2. **Should the output be filtered?** (`LENS.md` §4.) Filtering is a service,
//!    not an entitlement: when it cannot help, or would break something,
//!    Lens gets out of the way. Every answer here is recorded, because
//!    "filtering didn't work" is a question the log has to be able to answer.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

/// Why a run was not filtered.
///
/// Recorded on every invocation (`LENS.md` §12), which is what makes a
/// passthrough diagnosable rather than mysterious.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PassthroughReason {
    /// `LENS_MODE=raw`.
    ModeRaw,
    /// The child may take over the terminal, so its stdio must not be captured.
    Interactive,
    /// argv asks for machine-readable output, which is already minimal and
    /// which a filter would corrupt.
    MachineReadableFlag,
    /// The command could not be found on PATH. The child will fail; that
    /// failure is the child's to report, verbatim.
    NotFound,
    /// Resolution landed back on the Lens binary itself.
    SelfReference,
}

impl PassthroughReason {
    /// Stable identifier for the log and for `lens plot`.
    pub fn as_str(self) -> &'static str {
        match self {
            PassthroughReason::ModeRaw => "mode_raw",
            PassthroughReason::Interactive => "interactive",
            PassthroughReason::MachineReadableFlag => "machine_readable_flag",
            PassthroughReason::NotFound => "not_found",
            PassthroughReason::SelfReference => "self_reference",
        }
    }
}

impl fmt::Display for PassthroughReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What to do with a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Capture the child's output for filtering.
    Capture {
        /// The resolved binary.
        program: PathBuf,
    },
    /// Replace this process with the child and let it own the terminal.
    Passthrough {
        /// Why, for the log.
        reason: PassthroughReason,
    },
}

/// Commands that take over the terminal no matter how they are invoked.
const ALWAYS_INTERACTIVE: &[&str] = &[
    "vi", "vim", "nvim", "emacs", "nano", "pico", "ed", "less", "more", "top", "htop", "btop",
    "ssh", "sftp", "telnet", "tmux", "screen", "man", "watch", "gdb", "lldb", "fzf",
];

/// Interpreters that open a REPL when given nothing to run.
///
/// `python script.py` is a batch job worth filtering; bare `python` is a
/// session. The difference is whether a non-flag argument is present.
const REPL_WITHOUT_SCRIPT: &[&str] =
    &["python", "python3", "node", "irb", "psql", "mysql", "sqlite3", "redis-cli"];

/// git subcommands that open an editor.
const GIT_EDITOR_SUBCOMMANDS: &[&str] = &["commit", "rebase", "merge", "citool", "gui"];

/// git subcommands where `-p`/`--patch` means "prompt me per hunk" rather than
/// "show the patch". `git log -p` is output; `git add -p` is a conversation.
const GIT_PATCH_INTERACTIVE: &[&str] = &["add", "checkout", "restore", "reset", "stash", "commit"];

/// git subcommands where `-i`/`--interactive` means what it says.
const GIT_INTERACTIVE_FLAG_SUBCOMMANDS: &[&str] = &["rebase", "add", "clean"];

/// Flags that produce machine-readable output (`LENS.md` §4).
///
/// Filtering these is worse than useless: the output is already minimal, and a
/// consumer is parsing it.
const MACHINE_READABLE_GIT: &[&str] =
    &["--porcelain", "-z", "--name-only", "--name-status", "--numstat", "--raw"];

/// The same, for any command.
const MACHINE_READABLE_GENERIC: &[&str] = &["--json", "--output=json", "--quiet", "-q"];

/// Prefixed forms — `--format=%H`, `--pretty=oneline`.
const MACHINE_READABLE_PREFIXES: &[&str] = &["--format", "--pretty"];

/// Decide what to do with `argv`.
///
/// `lens_exe` is this process's own binary, used to detect self-reference.
/// `stdin_is_tty` gates the "may prompt" heuristic: with no terminal attached
/// there is nobody to prompt, so a command that would have been interactive is
/// safe to capture.
///
/// When in doubt, this returns [`Plan::Passthrough`]. A missed filtering
/// opportunity costs tokens; a wrongly captured interactive command costs the
/// user their session.
pub fn plan(
    argv: &[String],
    mode_raw: bool,
    stdin_is_tty: bool,
    lens_exe: Option<&Path>,
    path_var: Option<&OsString>,
) -> Plan {
    debug_assert!(!argv.is_empty(), "cli::parse guarantees a non-empty argv");

    if mode_raw {
        return Plan::Passthrough { reason: PassthroughReason::ModeRaw };
    }
    if is_interactive(argv, stdin_is_tty) {
        return Plan::Passthrough { reason: PassthroughReason::Interactive };
    }
    if has_machine_readable_flag(argv) {
        return Plan::Passthrough { reason: PassthroughReason::MachineReadableFlag };
    }

    match find_program(&argv[0], path_var, lens_exe) {
        Found::Program(program) => Plan::Capture { program },
        Found::Missing => Plan::Passthrough { reason: PassthroughReason::NotFound },
        Found::Ourselves => Plan::Passthrough { reason: PassthroughReason::SelfReference },
    }
}

/// Outcome of a PATH search.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Found {
    Program(PathBuf),
    Missing,
    Ourselves,
}

/// Would this command take over the terminal?
fn is_interactive(argv: &[String], stdin_is_tty: bool) -> bool {
    let command = basename(&argv[0]);
    let args = &argv[1..];

    if ALWAYS_INTERACTIVE.contains(&command) {
        return true;
    }

    if REPL_WITHOUT_SCRIPT.contains(&command) {
        // A flag-only invocation (`python -q`) still lands in a REPL; a
        // positional argument means there is work to do and output to filter.
        return !args.iter().any(|a| !a.starts_with('-'));
    }

    if command == "git" {
        let subcommand = args.iter().find(|a| !a.starts_with('-')).map(String::as_str);
        let Some(subcommand) = subcommand else { return false };
        let has = |flags: &[&str]| args.iter().any(|a| flags.contains(&a.as_str()));

        // An editor-opening subcommand only prompts when there is a terminal to
        // prompt on. Without one — a pipeline, an agent's shell — git falls
        // back to failing or to the message it was given, which is capturable
        // output. This is what keeps `git commit -m` filtered in CI and safe
        // locally.
        if stdin_is_tty && GIT_EDITOR_SUBCOMMANDS.contains(&subcommand) {
            return true;
        }
        if GIT_PATCH_INTERACTIVE.contains(&subcommand) && has(&["-p", "--patch"]) {
            return true;
        }
        if GIT_INTERACTIVE_FLAG_SUBCOMMANDS.contains(&subcommand) && has(&["-i", "--interactive"]) {
            return true;
        }
    }

    false
}

/// Does argv ask for output meant to be parsed rather than read?
fn has_machine_readable_flag(argv: &[String]) -> bool {
    let command = basename(&argv[0]);
    let git = command == "git";

    argv[1..].iter().any(|arg| {
        if MACHINE_READABLE_GENERIC.contains(&arg.as_str()) {
            return true;
        }
        if git && MACHINE_READABLE_GIT.contains(&arg.as_str()) {
            return true;
        }
        if git && MACHINE_READABLE_PREFIXES.iter().any(|p| arg.starts_with(&format!("{p}="))) {
            return true;
        }
        false
    })
}

/// Locate `command` on PATH, refusing to resolve to Lens itself.
fn find_program(command: &str, path_var: Option<&OsString>, lens_exe: Option<&Path>) -> Found {
    let is_self = |candidate: &Path| -> bool {
        let Some(exe) = lens_exe else { return false };
        // Compare canonicalized paths: a symlink named `git` pointing at the
        // Lens binary is the case this exists to catch, and its name says
        // nothing useful.
        match (candidate.canonicalize(), exe.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    };

    // A path-qualified command bypasses PATH entirely, exactly as exec would.
    if command.contains('/') {
        let candidate = PathBuf::from(command);
        return if !is_executable(&candidate) {
            Found::Missing
        } else if is_self(&candidate) {
            Found::Ourselves
        } else {
            Found::Program(candidate)
        };
    }

    let Some(path) = path_var else { return Found::Missing };
    for dir in std::env::split_paths(path) {
        // An empty PATH entry means the current directory, which is a
        // historical footgun; skip it rather than reproduce it.
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(command);
        if !is_executable(&candidate) {
            continue;
        }
        return if is_self(&candidate) { Found::Ourselves } else { Found::Program(candidate) };
    }
    Found::Missing
}

/// Is this path a file we could execute?
fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The command name without its directory.
fn basename(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    fn reason(argv: &[String], mode_raw: bool, tty: bool) -> Option<PassthroughReason> {
        match plan(argv, mode_raw, tty, None, None) {
            Plan::Passthrough { reason } => Some(reason),
            Plan::Capture { .. } => None,
        }
    }

    #[test]
    fn raw_mode_wins_over_everything() {
        assert_eq!(reason(&argv(&["git", "diff"]), true, false), Some(PassthroughReason::ModeRaw));
    }

    #[test]
    fn known_interactive_commands_pass_through() {
        for command in ["vim", "less", "top", "ssh", "/usr/bin/vim"] {
            assert_eq!(
                reason(&argv(&[command]), false, false),
                Some(PassthroughReason::Interactive),
                "{command}"
            );
        }
    }

    #[test]
    fn git_editor_subcommands_are_interactive_only_with_a_terminal() {
        // With a TTY, `git commit` opens $EDITOR and must own the terminal.
        assert_eq!(
            reason(&argv(&["git", "commit"]), false, true),
            Some(PassthroughReason::Interactive)
        );
        // Without one — a pipeline, an agent — it runs to completion, so
        // capturing it is safe and useful.
        assert_ne!(
            reason(&argv(&["git", "commit", "-m", "x"]), false, false),
            Some(PassthroughReason::Interactive)
        );
    }

    #[test]
    fn interactive_flags_only_count_where_they_mean_that() {
        assert_eq!(
            reason(&argv(&["git", "add", "-p"]), false, false),
            Some(PassthroughReason::Interactive)
        );
        assert_eq!(
            reason(&argv(&["git", "rebase", "-i", "HEAD~3"]), false, false),
            Some(PassthroughReason::Interactive)
        );
        // `-p` to git log means "show the patch" — output, not a conversation.
        // Reading the flag without its subcommand would disable filtering for
        // one of the most worthwhile commands there is.
        assert_ne!(
            reason(&argv(&["git", "log", "-p"]), false, false),
            Some(PassthroughReason::Interactive)
        );
        assert_ne!(
            reason(&argv(&["git", "show", "--patch"]), false, false),
            Some(PassthroughReason::Interactive)
        );
        // `-i` means "ignore case" to grep and "in-place" to sed.
        assert_ne!(
            reason(&argv(&["grep", "-i", "needle"]), false, false),
            Some(PassthroughReason::Interactive)
        );
    }

    #[test]
    fn interpreters_are_interactive_only_without_a_script() {
        assert_eq!(reason(&argv(&["python3"]), false, false), Some(PassthroughReason::Interactive));
        assert_eq!(
            reason(&argv(&["python3", "-q"]), false, false),
            Some(PassthroughReason::Interactive)
        );
        // A script to run is output to filter.
        assert_ne!(
            reason(&argv(&["python3", "script.py"]), false, false),
            Some(PassthroughReason::Interactive)
        );
        assert_ne!(
            reason(&argv(&["node", "build.js"]), false, false),
            Some(PassthroughReason::Interactive)
        );
    }

    #[test]
    fn machine_readable_flags_pass_through() {
        for flags in [
            vec!["git", "status", "--porcelain"],
            vec!["git", "diff", "--name-only"],
            vec!["git", "log", "--pretty=oneline"],
            vec!["git", "log", "--format=%H"],
            vec!["git", "diff", "-z"],
            vec!["anything", "--json"],
            vec!["anything", "-q"],
        ] {
            assert_eq!(
                reason(&argv(&flags), false, false),
                Some(PassthroughReason::MachineReadableFlag),
                "{flags:?}"
            );
        }
    }

    #[test]
    fn git_specific_flags_do_not_leak_to_other_commands() {
        // `--raw` means something else to other tools; only git's list applies
        // to git.
        assert_ne!(
            reason(&argv(&["mytool", "--name-only"]), false, false),
            Some(PassthroughReason::MachineReadableFlag)
        );
    }

    #[test]
    fn a_prefix_match_needs_the_equals_sign() {
        // `git diff --format` with a separate value is still machine-readable,
        // but `--formatting` is not our flag at all.
        assert_ne!(
            reason(&argv(&["git", "log", "--formatting"]), false, false),
            Some(PassthroughReason::MachineReadableFlag)
        );
    }

    #[test]
    fn a_missing_command_passes_through_rather_than_erroring() {
        // The child's failure is the child's to report (invariant 6).
        let path = OsString::from("/nonexistent-dir");
        match plan(&argv(&["definitely-not-a-command"]), false, false, None, Some(&path)) {
            Plan::Passthrough { reason } => assert_eq!(reason, PassthroughReason::NotFound),
            other => panic!("expected passthrough, got {other:?}"),
        }
    }

    #[test]
    fn resolution_finds_a_real_binary_on_path() {
        let path = std::env::var_os("PATH").expect("PATH is set in the test environment");
        match plan(&argv(&["sh"]), false, false, None, Some(&path)) {
            Plan::Capture { program } => assert!(program.ends_with("sh"), "{program:?}"),
            other => panic!("expected capture, got {other:?}"),
        }
    }

    #[test]
    fn resolving_to_ourselves_passes_through() {
        // The fork-bomb guard: a `git` on PATH that is really the Lens binary.
        let exe = std::env::current_exe().expect("test binary path");
        let dir = exe.parent().expect("test binary has a parent").to_path_buf();
        let name = exe.file_name().expect("test binary has a name").to_string_lossy().to_string();
        let path = OsString::from(dir.as_os_str());
        match plan(&argv(&[&name]), false, false, Some(&exe), Some(&path)) {
            Plan::Passthrough { reason } => assert_eq!(reason, PassthroughReason::SelfReference),
            other => panic!("expected passthrough, got {other:?}"),
        }
    }

    #[test]
    fn path_qualified_commands_skip_the_path_search() {
        match plan(&argv(&["/bin/sh"]), false, false, None, None) {
            Plan::Capture { program } => assert_eq!(program, PathBuf::from("/bin/sh")),
            other => panic!("expected capture, got {other:?}"),
        }
    }

    #[test]
    fn reasons_have_stable_log_identifiers() {
        // These strings land in the command log and in `lens plot`; renaming one
        // breaks every saved query over the log.
        assert_eq!(PassthroughReason::ModeRaw.as_str(), "mode_raw");
        assert_eq!(PassthroughReason::Interactive.as_str(), "interactive");
        assert_eq!(PassthroughReason::MachineReadableFlag.as_str(), "machine_readable_flag");
        assert_eq!(PassthroughReason::NotFound.as_str(), "not_found");
        assert_eq!(PassthroughReason::SelfReference.as_str(), "self_reference");
    }
}
