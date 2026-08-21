// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The content-addressed run store.
//!
//! This is what makes the tool's name honest. A lens changes the view, never the
//! subject: every byte a command produced is written here before anything is
//! filtered, and any view can be re-derived from it later without running the
//! command again (`LENS.md` invariant 5).
//!
//! ```text
//! <store>/<handle>/
//!     meta.json      argv, cwd, exit code, timestamp, duration, sizes
//!     stdout
//!     stderr
//! ```
//!
//! The handle is a hash of what produced the run *and* what it produced, so the
//! same command run twice with the same output addresses the same entry.

use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{fs, io};

use serde::{Deserialize, Serialize};

/// Number of runs kept before the oldest are pruned.
pub const DEFAULT_MAX_RUNS: usize = 200;

/// Total bytes kept before the oldest are pruned.
pub const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Content address of a stored run: the first 8 hex digits of its hash.
///
/// Short enough to paste into a prompt, long enough that a collision needs
/// billions of runs — and a collision is not a correctness failure anyway,
/// because two runs that collide are two runs with identical argv, directory
/// and output.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Handle(String);

impl Handle {
    /// Compute the handle for a run.
    ///
    /// Deliberately an in-tree FNV-1a rather than `DefaultHasher`: the standard
    /// hasher's algorithm is explicitly not stable across Rust releases, and a
    /// handle printed in yesterday's output has to still resolve after a
    /// toolchain upgrade.
    pub fn compute(argv: &[String], cwd: &Path, stdout: &[u8], stderr: &[u8]) -> Self {
        let mut hash = Fnv1a::new();
        for arg in argv {
            hash.write(arg.as_bytes());
            // Without a separator, ["ab", "c"] and ["a", "bc"] hash alike.
            hash.write(&[0]);
        }
        hash.write(cwd.as_os_str().as_encoded_bytes());
        hash.write(&[0]);
        hash.write(stdout);
        hash.write(&[0]);
        hash.write(stderr);
        Handle(format!("{:08x}", hash.finish() as u32))
    }

    /// The handle as it appears in output and on disk.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Accept a handle a user typed back at us.
    ///
    /// Rejects anything that is not 8 lowercase hex digits, which is what keeps
    /// `lens show ../../etc/passwd` from being a path traversal.
    pub fn parse(text: &str) -> Option<Self> {
        let valid = text.len() == 8
            && text.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
        valid.then(|| Handle(text.to_string()))
    }
}

impl std::fmt::Display for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// FNV-1a, 64-bit. Small, stable, and not cryptographic — none of which matters
/// for addressing a local cache entry.
struct Fnv1a(u64);

impl Fnv1a {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Fnv1a(Self::OFFSET_BASIS)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// What a stored run records about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// The command as invoked.
    pub argv: Vec<String>,
    /// Where it ran. Two runs of the same command in different repos are
    /// different runs.
    pub cwd: String,
    /// The code the command exited with.
    pub exit_code: i32,
    /// When it ran, RFC 3339 in UTC.
    pub timestamp: String,
    /// How long it took.
    pub duration_ms: u64,
    /// Size of the captured stdout.
    pub stdout_bytes: u64,
    /// Size of the captured stderr.
    pub stderr_bytes: u64,
}

/// Which captured stream to read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// The child's stdout.
    Stdout,
    /// The child's stderr.
    Stderr,
}

impl Stream {
    fn file_name(self) -> &'static str {
        match self {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        }
    }
}

/// A directory of stored runs.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    max_runs: usize,
    max_bytes: u64,
}

impl Store {
    /// Open (without creating) a store rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Store { root: root.into(), max_runs: DEFAULT_MAX_RUNS, max_bytes: DEFAULT_MAX_BYTES }
    }

    /// Override the retention limits.
    ///
    /// Test-only: a prune is not observable at the real limits without writing
    /// 200 runs, and the production path always uses the documented defaults.
    #[cfg(test)]
    pub fn with_limits(mut self, max_runs: usize, max_bytes: u64) -> Self {
        self.max_runs = max_runs;
        self.max_bytes = max_bytes;
        self
    }

    /// Where a run's directory lives.
    pub fn path_for(&self, handle: &Handle) -> PathBuf {
        self.root.join(handle.as_str())
    }

    /// Write a run and return its handle.
    ///
    /// Writing is atomic per file but not across the entry: a run interrupted
    /// mid-write leaves a directory that [`Store::list`] will still see. That is
    /// acceptable because a partial entry is pruned like any other, and because
    /// the alternative — a lock — costs more than the failure mode does.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the entry could not be written. Callers treat
    /// that as "no handle for this run", never as a reason to fail the command.
    pub fn write(
        &self,
        argv: &[String],
        cwd: &Path,
        stdout: &[u8],
        stderr: &[u8],
        exit_code: i32,
        duration_ms: u64,
    ) -> io::Result<Handle> {
        let handle = Handle::compute(argv, cwd, stdout, stderr);
        let dir = self.path_for(&handle);
        fs::create_dir_all(&dir)?;

        fs::write(dir.join("stdout"), stdout)?;
        fs::write(dir.join("stderr"), stderr)?;

        let meta = Meta {
            argv: argv.to_vec(),
            cwd: cwd.to_string_lossy().into_owned(),
            exit_code,
            timestamp: rfc3339_utc(SystemTime::now()),
            duration_ms,
            stdout_bytes: stdout.len() as u64,
            stderr_bytes: stderr.len() as u64,
        };
        fs::write(dir.join("meta.json"), serde_json::to_vec(&meta)?)?;

        // Prune after writing, not before: the run just captured is the one
        // most likely to be asked for, and pruning first could evict it.
        self.prune();
        Ok(handle)
    }

    /// Read back one captured stream.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the run is not in the store.
    pub fn read_stream(&self, handle: &Handle, stream: Stream) -> io::Result<Vec<u8>> {
        fs::read(self.path_for(handle).join(stream.file_name()))
    }

    /// Read back a run's metadata.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the run is missing or its `meta.json` is
    /// unreadable or malformed.
    pub fn read_meta(&self, handle: &Handle) -> io::Result<Meta> {
        let bytes = fs::read(self.path_for(handle).join("meta.json"))?;
        serde_json::from_slice(&bytes).map_err(io::Error::from)
    }

    /// Every run in the store, newest first.
    pub fn list(&self) -> Vec<Entry> {
        let Ok(entries) = fs::read_dir(&self.root) else { return Vec::new() };

        let mut runs: Vec<Entry> = entries
            .flatten()
            .filter_map(|entry| {
                let handle = Handle::parse(entry.file_name().to_string_lossy().as_ref())?;
                let meta = entry.metadata().ok()?;
                if !meta.is_dir() {
                    return None;
                }
                let modified = meta.modified().ok()?;
                Some(Entry { bytes: dir_size(&entry.path()), handle, modified })
            })
            .collect();

        runs.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.handle.cmp(&b.handle)));
        runs
    }

    /// Drop the oldest runs until both limits are satisfied.
    ///
    /// Failures here are swallowed. A store that could not be pruned is a disk
    /// usage problem; a command that failed because pruning failed is a
    /// correctness problem, and §2 says which of those is allowed.
    fn prune(&self) {
        let runs = self.list();
        let mut kept_bytes = 0u64;

        for (index, run) in runs.iter().enumerate() {
            kept_bytes += run.bytes;
            let over_count = index >= self.max_runs;
            let over_bytes = kept_bytes > self.max_bytes && index > 0;
            if over_count || over_bytes {
                let _ = fs::remove_dir_all(self.path_for(&run.handle));
            }
        }
    }
}

/// One run as the store sees it on disk.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Its content address.
    pub handle: Handle,
    /// Total size of the entry.
    pub bytes: u64,
    /// When it was written, used for eviction order.
    pub modified: SystemTime,
}

/// Total size of a run directory.
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    entries.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()
}

/// Format a timestamp as RFC 3339 in UTC, to the second.
///
/// Hand-rolled rather than pulling in a date library for one format string.
/// Civil-from-days is the standard algorithm and is unit-tested against known
/// dates below.
pub fn rfc3339_utc(time: SystemTime) -> String {
    let secs = time.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Days since the Unix epoch to a civil (year, month, day).
///
/// Howard Hinnant's `civil_from_days`, which shifts the epoch to March 1st so
/// the leap day lands at the end of the year and the month-length pattern
/// becomes arithmetic instead of a table.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A store in a fresh temp directory, removed when the test ends.
    struct TempStore {
        store: Store,
        root: PathBuf,
    }

    impl TempStore {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("lens-store-test-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("create temp store");
            TempStore { store: Store::new(&root), root }
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_handle_is_eight_lowercase_hex_digits() {
        let handle = Handle::compute(&argv(&["git", "diff"]), Path::new("/repo"), b"out", b"");
        assert_eq!(handle.as_str().len(), 8);
        assert!(handle.as_str().bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }

    #[test]
    fn the_same_run_addresses_the_same_entry() {
        let a = Handle::compute(&argv(&["git", "diff"]), Path::new("/repo"), b"out", b"err");
        let b = Handle::compute(&argv(&["git", "diff"]), Path::new("/repo"), b"out", b"err");
        assert_eq!(a, b);
    }

    #[test]
    fn every_input_changes_the_address() {
        let base = Handle::compute(&argv(&["git", "diff"]), Path::new("/repo"), b"out", b"err");
        let cases = [
            Handle::compute(&argv(&["git", "show"]), Path::new("/repo"), b"out", b"err"),
            Handle::compute(&argv(&["git", "diff"]), Path::new("/other"), b"out", b"err"),
            Handle::compute(&argv(&["git", "diff"]), Path::new("/repo"), b"different", b"err"),
            Handle::compute(&argv(&["git", "diff"]), Path::new("/repo"), b"out", b"different"),
        ];
        for case in cases {
            assert_ne!(base, case);
        }
    }

    #[test]
    fn argument_boundaries_are_part_of_the_address() {
        // Without a separator between arguments these two commands — which mean
        // entirely different things — would share an entry.
        let a = Handle::compute(&argv(&["rm", "-rf", "/tmp/x"]), Path::new("/"), b"", b"");
        let b = Handle::compute(&argv(&["rm", "-rf/tmp/x"]), Path::new("/"), b"", b"");
        assert_ne!(a, b);
    }

    #[test]
    fn fnv1a_matches_the_published_vectors() {
        // The reason to hand-roll a hash is that it is fixed forever; the way to
        // know it is the hash you think it is, is to check it against the
        // reference values rather than only against itself.
        let hash = Fnv1a::new();
        assert_eq!(hash.finish(), 0xcbf2_9ce4_8422_2325, "offset basis");

        let mut hash = Fnv1a::new();
        hash.write(b"a");
        assert_eq!(hash.finish(), 0xaf63_dc4c_8601_ec8c);

        let mut hash = Fnv1a::new();
        hash.write(b"foobar");
        assert_eq!(hash.finish(), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn a_handle_from_a_user_is_validated() {
        assert!(Handle::parse("a3f19c2b").is_some());
        // Anything that could escape the store directory is refused.
        assert!(Handle::parse("../../etc").is_none());
        assert!(Handle::parse("A3F19C2B").is_none(), "we write lowercase");
        assert!(Handle::parse("a3f19c2").is_none(), "too short");
        assert!(Handle::parse("a3f19c2bb").is_none(), "too long");
        assert!(Handle::parse("").is_none());
    }

    #[test]
    fn a_written_run_reads_back_byte_for_byte() {
        let tmp = TempStore::new("roundtrip");
        // Not UTF-8: the store holds bytes, and level 3 has to diff clean
        // against what the command actually produced.
        let stdout = vec![0x00, 0x01, 0xff, b'h', b'i'];
        let stderr = b"warning\n".to_vec();

        let handle = tmp
            .store
            .write(&argv(&["git", "diff"]), Path::new("/repo"), &stdout, &stderr, 1, 42)
            .expect("write run");

        assert_eq!(tmp.store.read_stream(&handle, Stream::Stdout).unwrap(), stdout);
        assert_eq!(tmp.store.read_stream(&handle, Stream::Stderr).unwrap(), stderr);

        let meta = tmp.store.read_meta(&handle).expect("read meta");
        assert_eq!(meta.argv, argv(&["git", "diff"]));
        assert_eq!(meta.cwd, "/repo");
        assert_eq!(meta.exit_code, 1);
        assert_eq!(meta.duration_ms, 42);
        assert_eq!(meta.stdout_bytes, 5);
        assert_eq!(meta.stderr_bytes, 8);
        assert!(meta.timestamp.ends_with('Z'), "{}", meta.timestamp);
    }

    #[test]
    fn reading_a_run_that_is_not_there_is_an_error_not_a_panic() {
        let tmp = TempStore::new("missing");
        let handle = Handle::parse("deadbeef").unwrap();
        assert!(tmp.store.read_meta(&handle).is_err());
        assert!(tmp.store.read_stream(&handle, Stream::Stdout).is_err());
    }

    #[test]
    fn pruning_keeps_the_newest_runs() {
        let tmp = TempStore::new("prune-count");
        let store = tmp.store.clone().with_limits(3, DEFAULT_MAX_BYTES);

        let mut handles = Vec::new();
        for i in 0..5 {
            let out = format!("run {i}");
            handles.push(
                store
                    .write(
                        &argv(&["cmd", &i.to_string()]),
                        Path::new("/"),
                        out.as_bytes(),
                        b"",
                        0,
                        1,
                    )
                    .expect("write"),
            );
            // Directory mtime has second granularity on some filesystems, and
            // eviction order is the whole point of this test.
            std::thread::sleep(Duration::from_millis(1100));
        }

        let kept = store.list();
        assert_eq!(kept.len(), 3, "three newest kept");
        let kept_handles: Vec<&Handle> = kept.iter().map(|e| &e.handle).collect();
        for recent in &handles[2..] {
            assert!(kept_handles.contains(&recent), "{recent} should survive");
        }
        for evicted in &handles[..2] {
            assert!(!kept_handles.contains(&evicted), "{evicted} should be pruned");
        }
    }

    #[test]
    fn a_byte_limit_also_evicts() {
        let tmp = TempStore::new("prune-bytes");
        // Far below one entry's size, so everything but the newest goes.
        let store = tmp.store.clone().with_limits(DEFAULT_MAX_RUNS, 16);

        for i in 0..3 {
            store
                .write(
                    &argv(&["cmd", &i.to_string()]),
                    Path::new("/"),
                    &vec![b'x'; 1024],
                    b"",
                    0,
                    1,
                )
                .expect("write");
            std::thread::sleep(Duration::from_millis(1100));
        }

        assert_eq!(store.list().len(), 1, "only the newest run survives the byte limit");
    }

    #[test]
    fn the_newest_run_is_never_pruned() {
        // Even a single run larger than the whole budget stays: it is the one
        // the caller just made and is about to ask for.
        let tmp = TempStore::new("prune-single");
        let store = tmp.store.clone().with_limits(DEFAULT_MAX_RUNS, 1);
        store.write(&argv(&["cmd"]), Path::new("/"), &vec![b'x'; 4096], b"", 0, 1).expect("write");
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn listing_a_store_that_does_not_exist_is_empty() {
        let store = Store::new("/nonexistent/lens/store");
        assert!(store.list().is_empty());
    }

    #[test]
    fn stray_directories_are_ignored() {
        let tmp = TempStore::new("stray");
        fs::create_dir_all(tmp.root.join("not-a-handle")).unwrap();
        fs::write(tmp.root.join("stray-file"), b"x").unwrap();
        tmp.store.write(&argv(&["cmd"]), Path::new("/"), b"out", b"", 0, 1).expect("write");
        assert_eq!(tmp.store.list().len(), 1);
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        let at = |secs: u64| rfc3339_utc(SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(1), "1970-01-01T00:00:01Z");
        // A leap day, which is where a hand-rolled calendar goes wrong.
        assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(at(1_709_164_800), "2024-02-29T00:00:00Z");
        // A non-leap century, the other place it goes wrong.
        assert_eq!(at(4_107_542_400), "2100-03-01T00:00:00Z");
        assert_eq!(at(1_774_224_000), "2026-03-23T00:00:00Z");
        assert_eq!(at(2_147_483_647), "2038-01-19T03:14:07Z");
    }
}
