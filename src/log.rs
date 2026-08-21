// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The command log.
//!
//! Two distinct things share one file, and conflating them is how a log stops
//! being useful:
//!
//! * **Run records** — one per Lens invocation, including passthrough. This is
//!   what `lens stats` aggregates and what answers "why was my output mangled
//!   last Tuesday". A passthrough record carrying its reason is exactly what you
//!   need when someone reports that filtering "didn't work".
//! * **Events** — free-text diagnostics at a level.
//!
//! Two rules govern everything here, and both are invariants:
//!
//! * **Logging never touches the child's streams.** Nothing in this module
//!   writes to stdout or stderr.
//! * **Logging never fails the run.** A full disk, an unwritable
//!   directory, a rotation that could not take its lock — all of it is swallowed
//!   and the command still succeeds. [`Logger::init`] cannot return an error;
//!   the worst it can do is degrade to [`Level::Off`].

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::store::rfc3339_utc;

/// Rotate the current log once it exceeds this size.
pub const DEFAULT_MAX_SIZE_MB: u64 = 8;

/// Keep this many rotated generations.
pub const DEFAULT_MAX_FILES: u32 = 5;

/// Name of the live log file. Generations are this plus `.1`, `.2`, ...
const LOG_FILE: &str = "lens.log";

/// Verbosity, ordered from silent to loudest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Log nothing at all.
    Off,
    /// Something Lens could not do.
    Error,
    /// Something Lens did differently than asked.
    Warn,
    /// One line per run.
    #[default]
    Info,
    /// Internal decisions.
    Debug,
    /// Everything, including a bounded prefix of the child's output.
    Trace,
}

impl Level {
    /// Parse a level name, case-insensitively.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "off" => Some(Level::Off),
            "error" => Some(Level::Error),
            "warn" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            "trace" => Some(Level::Trace),
            _ => None,
        }
    }

    /// The name as it appears in a record.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One Lens invocation.
///
/// Written for **every** run, filtered or not. The fields that describe
/// filtering arrive with the stages that do it; what is here is what M2 can
/// honestly report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    /// Content address of the stored run, absent when nothing was captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// The command name, for grouping in `lens stats`.
    pub cmd: String,
    /// The full command line.
    ///
    /// The log cannot answer what was run without argv, so a command carrying a
    /// secret in its own arguments (`mysql -pPASSWORD`) writes that secret
    /// here. That is a deliberate trade, and it is the reason command *output*
    /// never appears at any level below trace, where it is capped at a short
    /// prefix.
    pub argv: Vec<String>,
    /// Where it ran.
    pub cwd: String,
    /// The code the command exited with.
    ///
    /// Absent for a passthrough run. Passthrough replaces this process with the
    /// child, so the record has to be written *before* the command runs and
    /// there is no outcome to report. Recording a placeholder would be worse
    /// than recording nothing: `lens stats` would count fabricated exit codes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// How long the child took. Absent for passthrough, for the same reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dur_ms: Option<u64>,
    /// Bytes the child wrote to stdout. Absent for passthrough.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out_bytes: Option<u64>,
    /// Bytes the child wrote to stderr. Absent for passthrough.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub err_bytes: Option<u64>,
    /// Lines the command produced, before filtering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_lines: Option<u64>,
    /// Lines that reached the caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out_lines: Option<u64>,
    /// Estimated tokens the command produced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_tok: Option<u64>,
    /// Estimated tokens that reached the caller. The only reduction figure this
    /// tool reports, and it is labelled as output tokens rather than as cost:
    /// prompt caching and extra turns both break that inference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out_tok: Option<u64>,
    /// Which view the caller got.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    /// Stages that ran, in order.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stages: Vec<String>,
    /// Whether the output was emitted unfiltered.
    pub passthrough: bool,
    /// Why, when it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A free-text diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    /// What happened.
    pub msg: String,
    /// Structured detail, flattened into the record.
    #[serde(flatten)]
    pub fields: std::collections::BTreeMap<String, String>,
}

/// A line in the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// When it was written, RFC 3339 UTC.
    pub t: String,
    /// At what level.
    pub lvl: Level,
    /// Which kind of record this is, and its payload.
    #[serde(flatten)]
    pub body: Body,
}

/// The two record kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Body {
    /// One invocation. Boxed because a run record is several times the size of
    /// an event, and every event would otherwise carry that footprint.
    Run(Box<RunRecord>),
    /// One diagnostic.
    Event(EventRecord),
}

/// How the log behaves.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory holding `lens.log` and its generations.
    pub dir: PathBuf,
    /// Records below this level are dropped.
    pub level: Level,
    /// Rotate once the live file passes this size.
    pub max_size_mb: u64,
    /// Keep this many generations.
    pub max_files: u32,
}

impl Config {
    /// A config with the documented defaults.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Config {
            dir: dir.into(),
            level: Level::default(),
            max_size_mb: DEFAULT_MAX_SIZE_MB,
            max_files: DEFAULT_MAX_FILES,
        }
    }
}

/// An open log.
///
/// A logger that could not open its file is not an error condition; it is a
/// logger that writes nothing. Every method on it stays callable.
#[derive(Debug)]
pub struct Logger {
    file: Option<File>,
    level: Level,
}

impl Logger {
    /// Open the log, rotating first if the live file is oversized.
    ///
    /// Never fails. An unwritable directory, a full disk, or a rotation that
    /// could not take its lock all produce a logger that silently discards,
    /// because a log problem may not become a command problem.
    pub fn init(config: &Config) -> Self {
        if config.level == Level::Off {
            return Logger { file: None, level: Level::Off };
        }

        let path = config.dir.join(LOG_FILE);
        if fs::create_dir_all(&config.dir).is_err() {
            return Logger { file: None, level: Level::Off };
        }

        rotate_if_needed(&path, config);

        // O_APPEND so concurrent writers interleave whole lines rather than
        // overwriting each other. No fsync anywhere: the latency budget is
        // ~10ms for the whole tool and a log is not worth a disk round-trip.
        let file = OpenOptions::new().create(true).append(true).open(&path).ok();
        Logger { file, level: config.level }
    }

    /// Is this level worth formatting a record for?
    pub fn enabled(&self, level: Level) -> bool {
        self.file.is_some() && self.level != Level::Off && level <= self.level
    }

    /// Record one invocation.
    pub fn run(&self, record: RunRecord) {
        self.write(Level::Info, Body::Run(Box::new(record)));
    }

    /// Record one diagnostic.
    pub fn event(&self, level: Level, msg: &str, fields: &[(&str, &str)]) {
        let fields = fields.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        self.write(level, Body::Event(EventRecord { msg: msg.to_string(), fields }));
    }

    fn write(&self, level: Level, body: Body) {
        if !self.enabled(level) {
            return;
        }
        let Some(mut file) = self.file.as_ref() else { return };

        let record = Record { t: rfc3339_utc(SystemTime::now()), lvl: level, body };
        let Ok(mut line) = serde_json::to_vec(&record) else { return };
        line.push(b'\n');

        // One write call per record, so a concurrent writer cannot interleave
        // inside a line. Errors are dropped on the floor by design.
        let _ = file.write_all(&line);
    }
}

/// Rotate the live log if it has outgrown `max_size_mb`.
///
/// Concurrency: several `lens` processes can run at once, so the rename
/// sequence is done under an advisory lock. If the lock cannot be taken the
/// rotation is skipped and the caller appends anyway: an oversized log beats a
/// blocked command or a lost write.
fn rotate_if_needed(path: &Path, config: &Config) {
    let Ok(meta) = fs::metadata(path) else { return };
    if meta.len() <= config.max_size_mb.saturating_mul(1024 * 1024) {
        return;
    }

    let Ok(lock_file) = File::open(path) else { return };
    if !crate::platform::try_lock_exclusive(&lock_file) {
        return;
    }

    // Re-check under the lock: another process may have rotated between our
    // stat and our lock, and rotating twice discards a generation of records.
    let still_oversized = fs::metadata(path)
        .map(|m| m.len() > config.max_size_mb.saturating_mul(1024 * 1024))
        .unwrap_or(false);

    if still_oversized {
        // Delete the generation that is about to fall off the end, then shift
        // the rest up. Descending order, so nothing overwrites a live file.
        let _ = fs::remove_file(generation(path, config.max_files));
        for gen_num in (1..config.max_files).rev() {
            let _ = fs::rename(generation(path, gen_num), generation(path, gen_num + 1));
        }
        let _ = fs::rename(path, generation(path, 1));
    }

    crate::platform::unlock(&lock_file);
}

/// Path of rotated generation `n`.
fn generation(path: &Path, n: u32) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// Read every record in the log, oldest generation first.
///
/// Unparseable lines are skipped rather than fatal: a log truncated by a full
/// disk should still yield the records that survived.
pub fn read_all(dir: &Path, max_files: u32) -> Vec<Record> {
    let mut records = Vec::new();
    let live = dir.join(LOG_FILE);

    // Oldest first, so the result reads chronologically.
    for n in (1..=max_files).rev() {
        read_into(&generation(&live, n), &mut records);
    }
    read_into(&live, &mut records);
    records
}

fn read_into(path: &Path, out: &mut Vec<Record>) {
    let Ok(text) = fs::read_to_string(path) else { return };
    out.extend(text.lines().filter_map(|line| serde_json::from_str(line).ok()));
}

/// What `lens stats` reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// Invocations counted.
    pub runs: u64,
    /// How many of them were emitted unfiltered.
    pub passthrough: u64,
    /// Total bytes the children produced.
    pub bytes: u64,
    /// Estimated tokens the commands produced.
    pub in_tok: u64,
    /// Estimated tokens that reached the caller.
    pub out_tok: u64,
    /// Per-command totals, sorted by count when rendered.
    pub by_command: std::collections::BTreeMap<String, u64>,
    /// Passthrough reasons and their counts.
    pub reasons: std::collections::BTreeMap<String, u64>,
}

/// Aggregate run records, optionally filtered.
///
/// `since` is a cutoff timestamp in the same RFC 3339 form the records carry,
/// which sorts lexicographically because the format is fixed-width UTC.
pub fn aggregate(records: &[Record], since: Option<&str>, cmd: Option<&str>) -> Stats {
    let mut stats = Stats::default();

    for record in records {
        let Body::Run(run) = &record.body else { continue };
        if since.is_some_and(|cutoff| record.t.as_str() < cutoff) {
            continue;
        }
        if cmd.is_some_and(|wanted| run.cmd != wanted) {
            continue;
        }

        stats.runs += 1;
        stats.bytes += run.out_bytes.unwrap_or(0) + run.err_bytes.unwrap_or(0);
        stats.in_tok += run.in_tok.unwrap_or(0);
        stats.out_tok += run.out_tok.unwrap_or(0);
        *stats.by_command.entry(run.cmd.clone()).or_default() += 1;
        if run.passthrough {
            stats.passthrough += 1;
            if let Some(reason) = &run.reason {
                *stats.reasons.entry(reason.clone()).or_default() += 1;
            }
        }
    }

    stats
}

/// Turn `7d`, `24h`, `30m` into a cutoff timestamp relative to `now`.
///
/// Returns `None` for a duration that does not parse, so a typo shows up as a
/// rejected flag rather than as a silently empty report.
pub fn since_cutoff(spec: &str, now: SystemTime) -> Option<String> {
    let (count, unit) = spec.split_at(spec.len().checked_sub(1)?);
    let count: u64 = count.parse().ok()?;
    let seconds = match unit {
        "m" => count * 60,
        "h" => count * 3600,
        "d" => count * 86_400,
        "w" => count * 604_800,
        _ => return None,
    };
    Some(rfc3339_utc(now - std::time::Duration::from_secs(seconds)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("lens-log-test-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create temp log dir");
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn a_run(cmd: &str, passthrough: bool) -> RunRecord {
        RunRecord {
            handle: Some("a3f19c2b".into()),
            cmd: cmd.into(),
            argv: vec![cmd.into(), "diff".into()],
            cwd: "/repo".into(),
            exit: (!passthrough).then_some(0),
            dur_ms: (!passthrough).then_some(8),
            out_bytes: (!passthrough).then_some(100),
            err_bytes: (!passthrough).then_some(20),
            in_lines: (!passthrough).then_some(12),
            out_lines: (!passthrough).then_some(4),
            in_tok: (!passthrough).then_some(300),
            out_tok: (!passthrough).then_some(90),
            level: (!passthrough).then_some(2),
            stages: if passthrough { vec![] } else { vec!["ansi".into()] },
            passthrough,
            reason: passthrough.then(|| "mode_raw".to_string()),
        }
    }

    #[test]
    fn levels_order_from_quiet_to_loud() {
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert!(Level::Debug < Level::Trace);
        assert!(Level::Off < Level::Error);
    }

    #[test]
    fn level_names_round_trip() {
        for level in
            [Level::Off, Level::Error, Level::Warn, Level::Info, Level::Debug, Level::Trace]
        {
            assert_eq!(Level::parse(level.as_str()), Some(level));
        }
        assert_eq!(Level::parse("WARN"), Some(Level::Warn));
        assert_eq!(Level::parse("verbose"), None);
    }

    #[test]
    fn a_run_record_round_trips_through_json() {
        let dir = TempDir::new("roundtrip");
        let logger = Logger::init(&Config::new(&dir.0));
        logger.run(a_run("git", false));
        drop(logger);

        let records = read_all(&dir.0, DEFAULT_MAX_FILES);
        assert_eq!(records.len(), 1);
        match &records[0].body {
            Body::Run(run) => assert_eq!(**run, a_run("git", false)),
            other => panic!("expected a run record, got {other:?}"),
        }
        assert!(records[0].t.ends_with('Z'));
    }

    #[test]
    fn the_record_shape_is_the_documented_one() {
        // This schema is a published contract: anything reading the log —
        // `lens stats`, a user's jq pipeline — depends on the field names.
        let dir = TempDir::new("shape");
        let logger = Logger::init(&Config::new(&dir.0));
        logger.run(a_run("git", false));
        drop(logger);

        let text = fs::read_to_string(dir.0.join(LOG_FILE)).unwrap();
        let value: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(value["type"], "run");
        assert_eq!(value["lvl"], "info");
        assert_eq!(value["cmd"], "git");
        assert_eq!(value["exit"], 0);
        assert_eq!(value["handle"], "a3f19c2b");
        assert!(value["t"].is_string());
    }

    #[test]
    fn a_passthrough_run_carries_its_reason() {
        // The record you need when someone reports that filtering did not work.
        let dir = TempDir::new("passthrough");
        let logger = Logger::init(&Config::new(&dir.0));
        logger.run(a_run("vim", true));
        drop(logger);

        let text = fs::read_to_string(dir.0.join(LOG_FILE)).unwrap();
        let value: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(value["passthrough"], true);
        assert_eq!(value["reason"], "mode_raw");
        // Nothing is invented about an outcome this record cannot know.
        assert!(value.get("exit").is_none());
        assert!(value.get("dur_ms").is_none());
    }

    #[test]
    fn events_carry_their_fields() {
        let dir = TempDir::new("event");
        let logger = Logger::init(&Config::new(&dir.0));
        logger.event(Level::Warn, "adapter parse failed", &[("cmd", "git"), ("err", "bad hunk")]);
        drop(logger);

        let text = fs::read_to_string(dir.0.join(LOG_FILE)).unwrap();
        let value: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(value["type"], "event");
        assert_eq!(value["lvl"], "warn");
        assert_eq!(value["msg"], "adapter parse failed");
        assert_eq!(value["err"], "bad hunk");
    }

    #[test]
    fn records_above_the_level_are_dropped() {
        let dir = TempDir::new("level");
        let mut config = Config::new(&dir.0);
        config.level = Level::Warn;
        let logger = Logger::init(&config);

        logger.event(Level::Error, "kept", &[]);
        logger.event(Level::Warn, "kept", &[]);
        logger.event(Level::Debug, "dropped", &[]);
        // A run record is info-level, so a warn-level log does not carry runs.
        logger.run(a_run("git", false));
        drop(logger);

        let records = read_all(&dir.0, DEFAULT_MAX_FILES);
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn an_off_logger_writes_nothing_at_all() {
        let dir = TempDir::new("off");
        let mut config = Config::new(&dir.0);
        config.level = Level::Off;
        let logger = Logger::init(&config);
        logger.run(a_run("git", false));
        logger.event(Level::Error, "not written", &[]);
        assert!(!dir.0.join(LOG_FILE).exists());
    }

    #[test]
    fn an_unwritable_directory_degrades_instead_of_failing() {
        // The whole point of the rule: this must not panic and must not
        // return an error, because the command it is logging is about to run.
        let config = Config::new("/proc/definitely/not/writable");
        let logger = Logger::init(&config);
        logger.run(a_run("git", false));
        logger.event(Level::Error, "swallowed", &[]);
        assert!(!logger.enabled(Level::Error));
    }

    #[test]
    fn rotation_shifts_generations_and_drops_the_oldest() {
        let dir = TempDir::new("rotate");
        let mut config = Config::new(&dir.0);
        config.max_size_mb = 0; // rotate on any non-empty file
        config.max_files = 3;

        // Five writes: each one rotates the previous line into .1 and shifts the
        // rest up, so the oldest two fall off the end.
        for i in 0..5 {
            let logger = Logger::init(&config);
            logger.event(Level::Info, &format!("line {i}"), &[]);
        }

        let live = dir.0.join(LOG_FILE);
        assert!(live.exists(), "a live log always exists");
        assert!(generation(&live, 3).exists(), "the last kept generation");
        assert!(!generation(&live, 4).exists(), "beyond max_files is deleted");

        // Oldest first, and only what fits in the retention window.
        let msgs: Vec<String> = read_all(&dir.0, config.max_files)
            .iter()
            .filter_map(|r| match &r.body {
                Body::Event(e) => Some(e.msg.clone()),
                Body::Run(_) => None,
            })
            .collect();
        assert_eq!(msgs, vec!["line 1", "line 2", "line 3", "line 4"]);
    }

    #[test]
    fn a_log_under_the_size_limit_is_not_rotated() {
        let dir = TempDir::new("no-rotate");
        let config = Config::new(&dir.0);
        for i in 0..3 {
            let logger = Logger::init(&config);
            logger.event(Level::Info, &format!("line {i}"), &[]);
        }
        assert!(!generation(&dir.0.join(LOG_FILE), 1).exists());
        assert_eq!(read_all(&dir.0, DEFAULT_MAX_FILES).len(), 3);
    }

    #[test]
    fn concurrent_writers_all_land_whole_lines() {
        // Several lens processes can run at once. Each record is one write to an
        // O_APPEND file, so lines interleave but never tear.
        let dir = TempDir::new("concurrent");
        let config = Config::new(&dir.0);

        std::thread::scope(|scope| {
            for writer in 0..4 {
                let config = config.clone();
                scope.spawn(move || {
                    let logger = Logger::init(&config);
                    for i in 0..25 {
                        logger.event(Level::Info, &format!("w{writer}-{i}"), &[]);
                    }
                });
            }
        });

        let text = fs::read_to_string(dir.0.join(LOG_FILE)).unwrap();
        assert_eq!(text.lines().count(), 100);
        // Every line parses: none was torn by another writer.
        assert_eq!(read_all(&dir.0, DEFAULT_MAX_FILES).len(), 100);
    }

    #[test]
    fn a_corrupt_line_does_not_lose_the_rest() {
        let dir = TempDir::new("corrupt");
        let logger = Logger::init(&Config::new(&dir.0));
        logger.run(a_run("git", false));
        drop(logger);

        let live = dir.0.join(LOG_FILE);
        let mut text = fs::read_to_string(&live).unwrap();
        text.push_str("{ this is not json\n");
        let logger = Logger::init(&Config::new(&dir.0));
        drop(logger);
        fs::write(&live, text).unwrap();

        let logger = Logger::init(&Config::new(&dir.0));
        logger.run(a_run("cargo", false));
        drop(logger);

        assert_eq!(read_all(&dir.0, DEFAULT_MAX_FILES).len(), 2);
    }

    #[test]
    fn aggregation_counts_runs_and_passthroughs() {
        let now = rfc3339_utc(SystemTime::now());
        let records = vec![
            Record {
                t: now.clone(),
                lvl: Level::Info,
                body: Body::Run(Box::new(a_run("git", false))),
            },
            Record {
                t: now.clone(),
                lvl: Level::Info,
                body: Body::Run(Box::new(a_run("git", true))),
            },
            Record {
                t: now.clone(),
                lvl: Level::Info,
                body: Body::Run(Box::new(a_run("cargo", false))),
            },
            Record {
                t: now.clone(),
                lvl: Level::Warn,
                body: Body::Event(EventRecord { msg: "x".into(), fields: Default::default() }),
            },
        ];

        let stats = aggregate(&records, None, None);
        assert_eq!(stats.runs, 3, "events are not runs");
        assert_eq!(stats.passthrough, 1);
        // The passthrough run reports no byte counts, so it contributes none.
        assert_eq!(stats.bytes, 240);
        assert_eq!(stats.by_command["git"], 2);
        assert_eq!(stats.by_command["cargo"], 1);
        assert_eq!(stats.reasons["mode_raw"], 1);

        let only_git = aggregate(&records, None, Some("git"));
        assert_eq!(only_git.runs, 2);
    }

    #[test]
    fn aggregation_respects_the_since_cutoff() {
        let old = Record {
            t: "2020-01-01T00:00:00Z".into(),
            lvl: Level::Info,
            body: Body::Run(Box::new(a_run("git", false))),
        };
        let new = Record {
            t: "2026-08-21T00:00:00Z".into(),
            lvl: Level::Info,
            body: Body::Run(Box::new(a_run("git", false))),
        };
        let stats = aggregate(&[old, new], Some("2026-01-01T00:00:00Z"), None);
        assert_eq!(stats.runs, 1);
    }

    #[test]
    fn since_specs_parse_the_documented_units() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        assert_eq!(since_cutoff("0d", now).unwrap(), rfc3339_utc(now));
        assert_eq!(
            since_cutoff("1d", now).unwrap(),
            rfc3339_utc(now - Duration::from_secs(86_400))
        );
        assert_eq!(since_cutoff("2h", now).unwrap(), rfc3339_utc(now - Duration::from_secs(7200)));
        assert_eq!(since_cutoff("30m", now).unwrap(), rfc3339_utc(now - Duration::from_secs(1800)));
        assert_eq!(
            since_cutoff("1w", now).unwrap(),
            rfc3339_utc(now - Duration::from_secs(604_800))
        );

        // A typo is rejected rather than silently reported as "no runs".
        assert_eq!(since_cutoff("7", now), None);
        assert_eq!(since_cutoff("7y", now), None);
        assert_eq!(since_cutoff("", now), None);
        assert_eq!(since_cutoff("d", now), None);
    }
}
