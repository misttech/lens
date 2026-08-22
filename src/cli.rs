// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Argument splitting.
//!
//! Lens's command line is unusual in one way that rules out an off-the-shelf
//! parser: most of it is not ours. `lens cargo test --no-run -- --nocapture`
//! carries exactly one Lens token, and everything from `cargo` onward has to
//! reach the child byte-for-byte, including tokens that look like flags we
//! recognize.
//!
//! The rule: **everything after the first non-flag token belongs
//! to the child.** That token is either one of our subcommands or the name of a
//! command to run. Nothing after it is interpreted.

use std::fmt;

/// What this invocation is asking for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Run a command and filter its output.
    Run {
        /// The child's argv, starting with the command name. Never empty.
        argv: Vec<String>,
        /// Token budget for this invocation, from `--budget`.
        ///
        /// `None` here does not mean "no budget": `LENS_BUDGET` is read later,
        /// by the process that actually filters. The parser only reports what
        /// the command line said.
        budget: Option<usize>,
    },
    /// One of Lens's own subcommands, with its arguments unparsed.
    Subcommand {
        /// Which subcommand.
        name: Subcommand,
        /// Its arguments, verbatim.
        args: Vec<String>,
    },
    /// `lens --version`.
    Version,
    /// `lens`, `lens --help`.
    Help,
}

/// Lens's own subcommands.
///
/// A token matching one of these is never treated as a command to run, so
/// `lens show` cannot execute a binary named `show`. That collision is accepted
/// deliberately: the subcommand surface is small, fixed, and documented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subcommand {
    /// Re-render a stored run at a different level.
    Show,
    /// Filtering report for a past run.
    Explain,
    /// Aggregate the command log.
    Stats,
    /// Visualize the pipeline for a command or a stored run.
    Plot,
    /// List resolved lenses and where they came from.
    Lenses,
    /// Show the resolved configuration.
    Config,
    /// Tail the log.
    Logs,
}

impl Subcommand {
    /// The token that selects this subcommand.
    pub fn as_str(self) -> &'static str {
        match self {
            Subcommand::Show => "show",
            Subcommand::Explain => "explain",
            Subcommand::Stats => "stats",
            Subcommand::Plot => "plot",
            Subcommand::Lenses => "lenses",
            Subcommand::Config => "config",
            Subcommand::Logs => "logs",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        const ALL: &[Subcommand] = &[
            Subcommand::Show,
            Subcommand::Explain,
            Subcommand::Stats,
            Subcommand::Plot,
            Subcommand::Lenses,
            Subcommand::Config,
            Subcommand::Logs,
        ];
        ALL.iter().copied().find(|s| s.as_str() == token)
    }
}

impl fmt::Display for Subcommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A command line Lens cannot act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    /// A flag before the command name that Lens does not recognize.
    UnknownFlag(String),
    /// A flag that takes a value was given without one.
    FlagNeedsValue(String),
    /// `--budget` was given a value that is not a token count.
    InvalidBudget(String),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::UnknownFlag(flag) => write!(
                f,
                "unknown lens flag `{flag}`\n\
                 lens flags go before the command name; everything after it belongs to the child.\n\
                 try: lens --help"
            ),
            CliError::FlagNeedsValue(flag) => {
                write!(f, "`{flag}` needs a value, e.g. {flag} 2000")
            }
            CliError::InvalidBudget(value) => {
                write!(f, "`{value}` is not a token budget — expected a number")
            }
        }
    }
}

impl std::error::Error for CliError {}

/// Split a command line into Lens's part and the child's.
///
/// `args` is the argument list *without* argv[0].
///
/// # Errors
///
/// Returns [`CliError::UnknownFlag`] for an unrecognized flag appearing before
/// the command name. Flags appearing after it are the child's arguments and are
/// never inspected.
pub fn parse<I, S>(args: I) -> Result<Invocation, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut rest = args.into_iter().map(Into::into).peekable();
    let mut budget = None;

    // Lens's own flags, which may only appear before the command name. The set
    // is small on purpose: a flag added here is a token that can no longer
    // start a child command line without ambiguity.
    while rest.peek().is_some_and(|token| is_flag(token)) {
        let flag = rest.next().expect("peeked");
        match flag.as_str() {
            "-h" | "--help" => return Ok(Invocation::Help),
            "-V" | "--version" => return Ok(Invocation::Version),
            "--budget" => {
                let Some(value) = rest.next() else {
                    return Err(CliError::FlagNeedsValue("--budget".into()));
                };
                match value.parse::<usize>() {
                    Ok(n) => budget = Some(n),
                    Err(_) => return Err(CliError::InvalidBudget(value)),
                }
            }
            other => return Err(CliError::UnknownFlag(other.to_string())),
        }
    }

    let Some(first) = rest.next() else {
        return Ok(Invocation::Help);
    };

    if let Some(name) = Subcommand::from_token(&first) {
        return Ok(Invocation::Subcommand { name, args: rest.collect() });
    }

    // Everything from here is the child's, verbatim. A token like `--budget`
    // among them is the child's argument, not ours: Lens does not get to
    // reinterpret a command line it was asked to run.
    let mut argv = Vec::new();
    argv.push(first);
    argv.extend(rest);
    Ok(Invocation::Run { argv, budget })
}

/// Is this token a flag rather than a command name?
///
/// A bare `-` is a command-line convention for stdin, not a flag, and `--` is a
/// separator; neither can name a command, but neither is ours to consume.
fn is_flag(token: &str) -> bool {
    token.starts_with('-') && token != "-" && token != "--"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_argv(args: &[&str]) -> Vec<String> {
        match parse(args.to_vec()) {
            Ok(Invocation::Run { argv, .. }) => argv,
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn bare_invocation_is_help() {
        assert_eq!(parse(Vec::<String>::new()), Ok(Invocation::Help));
        assert_eq!(parse(vec!["--help"]), Ok(Invocation::Help));
        assert_eq!(parse(vec!["-h"]), Ok(Invocation::Help));
    }

    #[test]
    fn version_flag_is_recognized() {
        assert_eq!(parse(vec!["--version"]), Ok(Invocation::Version));
        assert_eq!(parse(vec!["-V"]), Ok(Invocation::Version));
    }

    #[test]
    fn first_non_flag_token_starts_the_child() {
        assert_eq!(run_argv(&["git", "diff"]), vec!["git", "diff"]);
        assert_eq!(run_argv(&["cargo", "test"]), vec!["cargo", "test"]);
    }

    #[test]
    fn child_flags_are_never_interpreted() {
        // The child's flags reach it untouched even when they collide with
        // names Lens uses elsewhere, and even when they are Lens's own.
        assert_eq!(
            run_argv(&["cargo", "test", "--no-run", "--", "--nocapture"]),
            vec!["cargo", "test", "--no-run", "--", "--nocapture"]
        );
        assert_eq!(run_argv(&["mytool", "--version"]), vec!["mytool", "--version"]);
        assert_eq!(run_argv(&["mytool", "--help"]), vec!["mytool", "--help"]);
        assert_eq!(run_argv(&["mytool", "--budget", "500"]), vec!["mytool", "--budget", "500"]);
    }

    #[test]
    fn subcommand_names_only_bind_in_first_position() {
        assert_eq!(
            parse(vec!["show", "a3f19c2b", "--level", "2"]),
            Ok(Invocation::Subcommand {
                name: Subcommand::Show,
                args: vec!["a3f19c2b".into(), "--level".into(), "2".into()],
            })
        );
        // `git show` is git's subcommand, not ours.
        assert_eq!(run_argv(&["git", "show", "HEAD"]), vec!["git", "show", "HEAD"]);
    }

    #[test]
    fn every_subcommand_token_round_trips() {
        for name in [
            Subcommand::Show,
            Subcommand::Explain,
            Subcommand::Stats,
            Subcommand::Plot,
            Subcommand::Lenses,
            Subcommand::Config,
            Subcommand::Logs,
        ] {
            assert_eq!(
                parse(vec![name.as_str()]),
                Ok(Invocation::Subcommand { name, args: vec![] }),
                "{name}"
            );
        }
    }

    #[test]
    fn a_budget_flag_before_the_command_is_ours() {
        match parse(vec!["--budget", "500", "git", "diff"]) {
            Ok(Invocation::Run { argv, budget }) => {
                assert_eq!(argv, vec!["git", "diff"]);
                assert_eq!(budget, Some(500));
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn a_budget_flag_needs_a_number() {
        assert_eq!(parse(vec!["--budget"]), Err(CliError::FlagNeedsValue("--budget".into())));
        assert_eq!(
            parse(vec!["--budget", "plenty", "true"]),
            Err(CliError::InvalidBudget("plenty".into()))
        );
    }

    #[test]
    fn unknown_lens_flag_is_an_error_not_a_command() {
        // Silently treating it as a command would try to execute `--not-a-flag`.
        assert_eq!(
            parse(vec!["--not-a-flag", "git", "diff"]),
            Err(CliError::UnknownFlag("--not-a-flag".into()))
        );
        assert!(
            CliError::UnknownFlag("--not-a-flag".into()).to_string().contains("before the command")
        );
    }

    #[test]
    fn a_lone_dash_names_a_command_not_a_flag() {
        // `-` is stdin by convention; it is not ours to consume, so it starts
        // the child's argv like any other non-flag token.
        assert_eq!(run_argv(&["-"]), vec!["-"]);
        assert_eq!(run_argv(&["--", "git"]), vec!["--", "git"]);
    }

    #[test]
    fn child_argv_is_never_empty_for_a_run() {
        // Downstream code indexes argv[0] to resolve the binary; the parser is
        // what guarantees that is safe.
        let argv = run_argv(&["ls"]);
        assert!(!argv.is_empty());
    }
}
