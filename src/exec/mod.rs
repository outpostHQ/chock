//! Every process a gate runs goes through here, with bounded capture, a deadline, and an error that
//! names which step failed.

pub mod budget;
pub mod fixed;
mod marked;
pub(crate) mod peak;
mod process;
mod tree;

use std::fmt;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use marked::last_line;
pub(crate) use marked::{marked, marked_at};
use process::Process;

/// The most output kept per stream. The rest is drained and dropped, since a full pipe would block
/// the child.
pub const MAX_CAPTURE: usize = 16 * 1024 * 1024;

/// How many chock gates deep this process is. The `test` gate runs the suite, which runs chock, so
/// the count stops the recursion.
pub const DEPTH: &str = "CHOCK_GATE_DEPTH";

/// What this process inherited, or zero at the top.
#[must_use]
pub fn depth() -> u32 {
    std::env::var(DEPTH)
        .ok()
        .and_then(|d| d.parse().ok())
        .unwrap_or(0)
}

/// The directories this process looks a program up in: its `PATH`.
#[must_use]
pub fn search_path() -> Option<std::ffi::OsString> {
    std::env::var_os("PATH")
}

/// Which step of running a tool failed, as distinct from the tool running and saying no.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Not on `PATH`, not executable, or the working directory is gone.
    Spawn,
    /// A pipe could not be drained.
    Capture,
    /// The child started but its exit could not be collected.
    Wait,
    /// The child passed its deadline. Any cleanup failure is in the reason.
    Hung,
    /// The child ran, and the network failed it on each try.
    Offline,
    /// The child's processes held more memory than the run allows.
    Memory,
}

impl Stage {
    /// What happened to the tool at this step, said before the tool's name.
    const fn says(self) -> &'static str {
        match self {
            Self::Spawn => "could not start",
            Self::Capture => "could not read the output of",
            Self::Wait => "could not collect the exit of",
            Self::Hung => "gave up waiting for",
            Self::Offline => "could not reach the network for",
            Self::Memory => "stopped",
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.says())
    }
}

/// A tool that could not be run, and at which step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecError {
    pub program: String,
    pub stage: Stage,
    pub reason: String,
    /// What a tool that was stopped had printed, where both streams were read whole.
    pub printed: Option<Box<Output>>,
}

impl ExecError {
    /// Whether the deadline was reached, rather than the tool failing to start.
    #[must_use]
    pub fn hung(&self) -> bool {
        self.stage == Stage::Hung
    }
}

#[cfg(test)]
impl ExecError {
    /// A tool that could not be run, with nothing it printed kept.
    pub(crate) fn of(program: &str, stage: Stage, reason: &str) -> Self {
        Self {
            program: program.to_string(),
            stage,
            reason: reason.to_string(),
            printed: None,
        }
    }
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}: {}", self.stage, self.program, self.reason)
    }
}

/// What a finished tool printed and how it exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// `None` when a signal killed the child.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// The capture holds only a prefix, so a gate must not read a total from it.
    pub truncated: bool,
}

#[cfg(test)]
impl Output {
    /// A tool's exit and both streams, captured whole.
    pub(crate) fn of(code: Option<i32>, stdout: &str, stderr: &str) -> Self {
        Self {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            truncated: false,
        }
    }
}

impl Output {
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// The line that best says why the tool failed: a marked error, else the last line it printed.
    #[must_use]
    pub fn why_it_failed(&self) -> &str {
        marked(&self.stderr)
            .or_else(|| last_line(&self.stderr))
            // A tool that writes its errors to stdout leaves stderr empty.
            .or_else(|| last_line(&self.stdout))
            .unwrap_or("no reason given")
    }

    /// The first error a compiler marked and its place, from stderr, then from stdout, where Kani
    /// writes it while stderr holds cargo's summary; else `why_it_failed`.
    #[must_use]
    pub fn why_it_stopped(&self) -> String {
        marked_at(&self.stderr)
            .or_else(|| marked_at(&self.stdout))
            .unwrap_or_else(|| self.why_it_failed().to_string())
    }

    /// What a signal that ended a process says: the kernel ends one with SIGKILL (exit 137) when
    /// memory runs out, which the tool's own words do not say.
    fn signal_note(&self) -> &'static str {
        let signalled = self.code.is_none() || self.code == Some(137);
        if signalled || self.stderr.contains("SIGKILL") {
            "a signal ended a process, most often SIGKILL at the memory limit; `CHOCK_MEMORY` sets \
             what a run plans for. "
        } else {
            ""
        }
    }

    /// The failure reason plus both streams' tails; a closing summary often omits what failed.
    #[must_use]
    pub fn failure_details(&self) -> String {
        let cut = if self.truncated {
            " (capture truncated)"
        } else {
            ""
        };
        // A marked error gets its place, which the tails may have lost.
        let why = marked_at(&self.stderr).unwrap_or_else(|| self.why_it_failed().to_string());
        format!(
            "{}{}{cut}{}{}",
            self.signal_note(),
            text_tail(&why, 1024),
            diagnostic("stdout", &self.stdout),
            diagnostic("stderr", &self.stderr)
        )
    }
}

const DIAGNOSTIC_BYTES: usize = 4096;

fn text_tail(text: &str, cap: usize) -> &str {
    let mut start = text.len().saturating_sub(cap);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text.get(start..).unwrap_or_default()
}

fn diagnostic(stream: &str, text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let clipped = if text.len() > DIAGNOSTIC_BYTES {
        "; earlier output omitted"
    } else {
        ""
    };
    format!(
        "\n{stream} (bounded output{clipped}):\n{}",
        text_tail(text, DIAGNOSTIC_BYTES)
    )
}

/// The text with terminal colour escapes removed, each from `\x1b` to the next letter.
#[must_use]
pub fn strip_colour(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        for c in chars.by_ref() {
            if c.is_ascii_alphabetic() {
                break;
            }
        }
    }
    out
}

/// Runs a program in `cwd` within the deadline, capturing both streams.
pub fn run(program: &str, args: &[&str], cwd: &Path) -> Result<Output, ExecError> {
    run_capped(program, args, cwd, MAX_CAPTURE)
}

/// `run` with a failure to run as a message; a tool that ran and said no is still `Ok`.
pub fn tool(root: &Path, program: &str, args: &[&str]) -> Result<Output, String> {
    run(program, args, root).map_err(|e| e.to_string())
}

/// Whether the tool objected, by exiting non-zero. A truncated objection is an error, since its
/// findings would be partial.
pub fn objected(out: &Output, tool: &str) -> Result<bool, String> {
    if out.success() {
        return Ok(false);
    }
    if out.truncated {
        return Err(format!(
            "{tool} printed more than chock keeps; the findings would be partial"
        ));
    }
    Ok(true)
}

/// `run` with the capture cap as a parameter, so a test can use a small one.
fn run_capped(program: &str, args: &[&str], cwd: &Path, cap: usize) -> Result<Output, ExecError> {
    run_full(program, args, cwd, cap, &[], &mut (), None)
}

/// `run` with extra environment variables, such as `RUSTDOCFLAGS`.
pub fn run_env(
    program: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> Result<Output, ExecError> {
    run_full(program, args, cwd, MAX_CAPTURE, env, &mut (), None)
}

/// How a program is started: `run_env`, or a test's stand-in with fixed answers.
pub type Start<'a> = &'a Starter<'a>;

/// What a `Start` points to, for a caller that builds one and holds it.
pub type Starter<'a> =
    dyn Fn(&str, &[&str], &Path, &[(&str, &str)]) -> Result<Output, ExecError> + 'a;

/// What a program printed, or the command with why it failed.
pub fn printed(
    start: Start,
    program: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> Result<String, String> {
    let out = start(program, args, cwd, env).map_err(|e| e.to_string())?;
    match out.success() {
        true => Ok(out.stdout),
        false => Err(format!(
            "`{program} {}` failed: {}",
            args.join(" "),
            out.failure_details()
        )),
    }
}

pub(crate) trait Watchdog {
    fn poll(&mut self) -> Result<(), String>;
    fn complete(&mut self) -> Result<(), String>;
    /// The step a refusal from `poll` stopped.
    fn stage(&self) -> Stage {
        Stage::Hung
    }
}

impl Watchdog for () {
    fn poll(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn complete(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// `run_paced` with the caller's watchdog polled as well.
#[cfg(target_os = "linux")]
pub(crate) fn run_watched(
    program: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
    watchdog: &mut dyn Watchdog,
    moving: Moving,
) -> Result<Output, ExecError> {
    let pace = (moving, deadline());
    run_full(program, args, cwd, MAX_CAPTURE, env, watchdog, Some(pace))
}

/// Whether an output line shows a tool still moving; each such line restarts a paced deadline.
pub type Moving = fn(&str) -> bool;

/// A paced run: what shows progress, and the longest wait for it, most often `deadline()`.
pub type Pace = (Moving, std::time::Duration);

/// `run_env` for a tool that may run as long as it shows progress: it is stopped only when no
/// line the pace's `Moving` accepts arrives within its wait.
pub fn run_paced(
    program: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
    pace: Pace,
) -> Result<Output, ExecError> {
    run_full(program, args, cwd, MAX_CAPTURE, env, &mut (), Some(pace))
}

/// The `Moving` of a tool with a deadline in all: no line restarts it.
fn never(_line: &str) -> bool {
    false
}

/// What restarts the deadline, the limit on the whole run, and the limit since the last progress:
/// a paced tool has no limit in all, only one since its last progress.
fn pacing(pace: Option<Pace>) -> (Moving, std::time::Duration, std::time::Duration) {
    match pace {
        Some((moving, idle)) => (moving, std::time::Duration::MAX, idle),
        None => (never, deadline(), std::time::Duration::MAX),
    }
}

/// The caller's watchdog, and a deadline that each line showing progress restarts.
struct Paced<'a> {
    inner: &'a mut dyn Watchdog,
    moved: std::sync::Arc<std::sync::atomic::AtomicU64>,
    seen: u64,
    since: std::time::Instant,
    limit: std::time::Duration,
}

impl<'a> Paced<'a> {
    fn new(
        inner: &'a mut dyn Watchdog,
        moved: std::sync::Arc<std::sync::atomic::AtomicU64>,
        limit: std::time::Duration,
    ) -> Self {
        Self {
            inner,
            moved,
            seen: 0,
            since: std::time::Instant::now(),
            limit,
        }
    }
}

impl Watchdog for Paced<'_> {
    fn poll(&mut self) -> Result<(), String> {
        self.inner.poll()?;
        let now = self.moved.load(std::sync::atomic::Ordering::Relaxed);
        if now != self.seen {
            self.seen = now;
            self.since = std::time::Instant::now();
        }
        match past(self.since.elapsed(), self.limit) {
            true => Err(format!(
                "no progress for {}s; cleanup requested",
                self.limit.as_secs()
            )),
            false => Ok(()),
        }
    }
    fn complete(&mut self) -> Result<(), String> {
        self.inner.complete()
    }
}

/// The first bytes of a line that `Moving` reads: room for colour codes, indent and a status word.
const LINE_START: usize = 64;

/// A pipe that counts, as its bytes pass, each line `moving` accepts.
struct Counted<R> {
    inner: R,
    start: Vec<u8>,
    moving: Moving,
    moved: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

fn counted<R>(
    pipe: Option<R>,
    moving: Moving,
    moved: &std::sync::Arc<std::sync::atomic::AtomicU64>,
) -> Option<Counted<R>> {
    pipe.map(|inner| Counted {
        inner,
        start: Vec::new(),
        moving,
        moved: std::sync::Arc::clone(moved),
    })
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        for &byte in buf.get(..n).unwrap_or_default() {
            self.feed(byte);
        }
        Ok(n)
    }
}

impl<R> Counted<R> {
    fn feed(&mut self, byte: u8) {
        if byte != b'\n' {
            if self.start.len() < LINE_START {
                self.start.push(byte);
            }
            return;
        }
        if (self.moving)(&String::from_utf8_lossy(&self.start)) {
            self.moved
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.start.clear();
    }
}

/// Long enough for a cold suite on a large workspace and no longer, since a hang is never recalled
/// and is paid in full on every run.
pub const TIMEOUT_SECS: u64 = 30 * 60;

/// The environment variable that overrides `TIMEOUT_SECS`.
pub const TIMEOUT: &str = "CHOCK_TIMEOUT";

/// The per-tool deadline: `CHOCK_TIMEOUT` seconds if set, else `TIMEOUT_SECS`.
#[must_use]
pub fn deadline() -> std::time::Duration {
    let secs = std::env::var(TIMEOUT)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

/// What git exports into a hook. Cleared from each spawn unless the caller names it, so a tool's
/// git calls do not act on the commit being made.
pub const GIT_STATE: [&str; 6] = [
    "GIT_INDEX_FILE",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
];

/// What cargo exports about the package it runs, cleared from each spawn so a tool does not read it
/// as its own context.
const LAUNCHING_PACKAGE: [&str; 8] = [
    "CARGO_CRATE_NAME",
    "CARGO_MANIFEST_DIR",
    "CARGO_MANIFEST_PATH",
    "CARGO_MANIFEST_LINKS",
    "CARGO_PRIMARY_PACKAGE",
    "CARGO_BIN_NAME",
    "CARGO_TARGET_TMPDIR",
    "CARGO_RUSTC_CURRENT_DIR",
];

fn describes_the_launching_package(name: &str) -> bool {
    name.starts_with("CARGO_PKG_")
        || name.starts_with("CARGO_BIN_EXE_")
        || LAUNCHING_PACKAGE.contains(&name)
}

trait ClearEach {
    fn env_remove_each(&mut self, names: &[&str]) -> &mut Self;
}

impl ClearEach for Command {
    fn env_remove_each(&mut self, names: &[&str]) -> &mut Self {
        for name in names {
            self.env_remove(name);
        }
        self
    }
}

/// How long a pipe may stay open after its process exits; a holder after that is a leftover child.
const DRAIN: std::time::Duration = std::time::Duration::from_secs(20);

/// Reads a pipe on its own thread and hands it back over a channel, so the read can be abandoned
/// when a grandchild keeps the pipe open.
fn captured(pipe: Option<impl std::io::Read + Send + 'static>, cap: usize) -> Capture {
    let (tx, rx) = std::sync::mpsc::channel();
    let tail = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let writing = std::sync::Arc::clone(&tail);
    std::thread::spawn(move || {
        let _ = tx.send(read_capped(pipe, cap, &writing));
    });
    Capture { reading: rx, tail }
}

struct Capture {
    reading: std::sync::mpsc::Receiver<Result<(String, bool), String>>,
    tail: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl Capture {
    fn diagnostic(&self, stream: &str) -> String {
        match self.tail.lock() {
            Ok(tail) => diagnostic(
                &format!("{stream} (partial capture)"),
                &String::from_utf8_lossy(&tail),
            ),
            Err(_) => format!("\n{stream}: diagnostic capture unavailable"),
        }
    }
}

/// A stream read to its end, and whether it was cut at the capture limit; or why it was not read.
type Drained = Result<(String, bool), (Stage, String)>;

/// A captured stream, or an error once `grace` passes with something still holding the pipe.
fn drained(
    reading: &std::sync::mpsc::Receiver<Result<(String, bool), String>>,
    which: &str,
    grace: std::time::Duration,
) -> Drained {
    reading
        .recv_timeout(grace)
        .map_err(|_| {
            (
                Stage::Capture,
                format!(
                    "it exited, and {}s later something it started still held its {which}, so the \
                 output cannot be read whole",
                    grace.as_secs()
                ),
            )
        })
        .and_then(|result| {
            result.map_err(|why| (Stage::Capture, format!("cannot read {which}: {why}")))
        })
}

fn reap_until(
    child: &mut std::process::Child,
    limit: std::time::Duration,
) -> Result<std::process::ExitStatus, String> {
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Err(error) => return Err(format!("cannot reap child: {error}")),
            Ok(None) => {}
        }
        if past(started.elapsed(), limit) {
            return Err("child did not exit within the cleanup deadline".to_string());
        }
        std::thread::sleep(POLL);
    }
}

fn wait_until(
    process: &mut Process,
    limit: std::time::Duration,
    watchdog: &mut dyn Watchdog,
) -> Result<(), (Stage, String)> {
    let started = std::time::Instant::now();
    loop {
        if observed_exit(process, watchdog)? {
            return Ok(());
        }
        if past(started.elapsed(), limit) {
            return Err((
                Stage::Hung,
                format!(
                    "still running after {}s; cleanup requested",
                    limit.as_secs()
                ),
            ));
        }
        std::thread::sleep(POLL);
    }
}

fn observed_exit(
    process: &mut Process,
    watchdog: &mut dyn Watchdog,
) -> Result<bool, (Stage, String)> {
    watchdog
        .poll()
        .map_err(|reason| (watchdog.stage(), reason))?;
    let exited = process
        .exited()
        .map_err(|error| (Stage::Wait, error.to_string()))?;
    if exited {
        watchdog
            .complete()
            .map_err(|reason| (Stage::Wait, reason))?;
    }
    Ok(exited)
}

const CLEANUP: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether `elapsed` has reached `limit`; apart from the clock, so a test can reach the boundary.
fn past(elapsed: std::time::Duration, limit: std::time::Duration) -> bool {
    elapsed >= limit
}

/// How often a running tool is polled: cheap over an hour, short enough not to delay a quick tool.
const POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// The program and arguments that start, with the courtesy prefix folded in.
#[must_use]
fn spawned<'a>(program: &'a str, args: &[&'a str], polite: &[&'a str]) -> (&'a str, Vec<&'a str>) {
    match polite.split_first() {
        // The wrapper runs with the real program as an argument; errors still name `program`.
        Some((first, flags)) => {
            let mut all = flags.to_vec();
            all.push(program);
            all.extend_from_slice(args);
            (first, all)
        }
        None => (program, args.to_vec()),
    }
}

/// rustc writes a crash report into its working directory, the tree a later gate reads; a
/// place the user chose for it still holds. The crash itself still reaches stderr.
fn crash_report(chosen: Option<&std::ffi::OsStr>) -> Option<(&'static str, &'static str)> {
    chosen.is_none().then_some(("RUSTC_ICE", "0"))
}

fn run_full(
    program: &str,
    args: &[&str],
    cwd: &Path,
    cap: usize,
    env: &[(&str, &str)],
    watchdog: &mut dyn Watchdog,
    pace: Option<Pace>,
) -> Result<Output, ExecError> {
    let fail = |stage: Stage, reason: String| ExecError {
        program: program.to_string(),
        stage,
        reason,
        printed: None,
    };

    let (spawn, argv) = spawned(program, args, crate::exec::budget::politely(program));

    let mut command = Command::new(spawn);
    command
        .args(&argv)
        .env_remove_each(&GIT_STATE)
        .envs(crash_report(std::env::var_os("RUSTC_ICE").as_deref()))
        .envs(env.iter().copied())
        .env(DEPTH, (depth() + 1).to_string())
        .envs(crate::exec::budget::caps(env))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let inherited = std::env::vars_os().map(|(name, _)| name);
    let asked = env.iter().map(|(name, _)| std::ffi::OsString::from(name));
    for name in inherited.chain(asked) {
        if name.to_str().is_some_and(describes_the_launching_package) {
            command.env_remove(name);
        }
    }
    let mut process =
        Process::spawn(&mut command).map_err(|e| fail(Stage::Spawn, e.to_string()))?;

    let moved = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (counts, whole, idle) = pacing(pace);
    // Each pipe drains on its own thread: reading one to EOF first deadlocks once the other fills.
    let reading_out = captured(counted(process.child.stdout.take(), counts, &moved), cap);
    let reading_err = captured(counted(process.child.stderr.take(), counts, &moved), cap);
    let mut paced = Paced::new(watchdog, moved, idle);
    let mut sampled = peak::Sampled::new(&mut paced, process.child.id());
    finish_capture(
        &mut process,
        program,
        &reading_out,
        &reading_err,
        whole,
        DRAIN,
        &mut sampled,
    )
}

fn finish_capture(
    process: &mut Process,
    program: &str,
    stdout: &Capture,
    stderr: &Capture,
    limit: std::time::Duration,
    grace: std::time::Duration,
    watchdog: &mut dyn Watchdog,
) -> Result<Output, ExecError> {
    let waited = wait_until(process, limit, watchdog);
    let early_cleanup = waited.as_ref().err().map(|_| process.finish());
    let started = std::time::Instant::now();
    let out = drained(&stdout.reading, "stdout", grace);
    let err = drained(
        &stderr.reading,
        "stderr",
        grace.saturating_sub(started.elapsed()),
    );
    // The bounded tails may hold only cargo's last lines, so the marked error line is quoted too.
    let said = err
        .as_ref()
        .ok()
        .and_then(|(text, _)| marked(text))
        .map(|line| format!("\nwhat it marked as the error: {line}"))
        .unwrap_or_default();
    let printed = stopped_streams(waited.is_err(), &out, &err);
    let fail = |(stage, reason): (Stage, String)| ExecError {
        program: program.to_string(),
        stage,
        reason: format!(
            "{reason}{said}{}{}",
            stdout.diagnostic("stdout"),
            stderr.diagnostic("stderr")
        ),
        printed,
    };
    let cleanup = early_cleanup.unwrap_or_else(|| process.finish());
    let captured = waited.and_then(|()| Ok((out?, err?)));
    let (status, ((stdout, out_cut), (stderr, err_cut))) =
        completed_capture(captured, cleanup).map_err(fail)?;
    Ok(Output {
        code: status.code(),
        stdout,
        stderr,
        truncated: out_cut || err_cut,
    })
}

/// A stopped tool's streams, read whole, so that a caller can keep all that it printed.
fn stopped_streams(stopped: bool, out: &Drained, err: &Drained) -> Option<Box<Output>> {
    let ((stdout, out_cut), (stderr, err_cut)) = (out.as_ref().ok()?, err.as_ref().ok()?);
    stopped.then(|| {
        Box::new(Output {
            code: None,
            stdout: stdout.clone(),
            stderr: stderr.clone(),
            truncated: *out_cut || *err_cut,
        })
    })
}

fn completed_capture<T>(
    result: Result<T, (Stage, String)>,
    cleanup: Result<std::process::ExitStatus, String>,
) -> Result<(std::process::ExitStatus, T), (Stage, String)> {
    match (result, cleanup) {
        (Ok(output), Ok(status)) => Ok((status, output)),
        (Err(failure), Ok(_)) => Err(failure),
        (Ok(_), Err(why)) => Err((Stage::Wait, format!("cleanup incomplete: {why}"))),
        (Err((stage, why)), Err(cleanup)) => {
            Err((stage, format!("{why}; cleanup incomplete: {cleanup}")))
        }
    }
}

/// Read to EOF keeping at most `cap` bytes. Reading continues past the cap on purpose: stopping
/// early leaves the child blocked on a pipe nobody drains.
fn read_capped<R: Read>(
    pipe: Option<R>,
    cap: usize,
    tail: &std::sync::Mutex<Vec<u8>>,
) -> Result<(String, bool), String> {
    let Some(mut reader) = pipe else {
        return Ok((String::new(), false));
    };
    let mut held: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        let read = next_chunk(&mut reader, &mut chunk)?;
        if read.is_empty() {
            break;
        }
        let room = cap.saturating_sub(held.len());
        held.extend(read.iter().take(room));
        truncated |= read.len() > room;
        store_tail(tail, read);
    }
    Ok((String::from_utf8_lossy_owned(held), truncated))
}

fn next_chunk<'a>(reader: &mut impl Read, chunk: &'a mut [u8]) -> Result<&'a [u8], String> {
    let n = read_chunk(reader, chunk)?;
    chunk
        .get(..n)
        .ok_or_else(|| "reader returned a length beyond its buffer".to_string())
}

fn read_chunk(reader: &mut impl Read, chunk: &mut [u8]) -> Result<usize, String> {
    loop {
        match reader.read(chunk) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result.map_err(|e| e.to_string()),
        }
    }
}

fn store_tail(tail: &std::sync::Mutex<Vec<u8>>, chunk: &[u8]) {
    if let Ok(mut tail) = tail.lock() {
        remember_tail(&mut tail, chunk);
    }
}

fn remember_tail(tail: &mut Vec<u8>, chunk: &[u8]) {
    let take = chunk.len().min(DIAGNOSTIC_BYTES);
    let retain = DIAGNOSTIC_BYTES - take;
    let remove = tail.len().saturating_sub(retain);
    tail.drain(..remove);
    tail.extend(chunk.iter().skip(chunk.len() - take));
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn each_stage_names_the_step_that_failed() {
        assert_eq!(Stage::Spawn.to_string(), "could not start");
        assert_eq!(Stage::Capture.to_string(), "could not read the output of");
        assert_eq!(Stage::Wait.to_string(), "could not collect the exit of");
        assert_eq!(Stage::Hung.to_string(), "gave up waiting for");
        assert_eq!(
            Stage::Offline.to_string(),
            "could not reach the network for"
        );
        assert_eq!(Stage::Memory.to_string(), "stopped");
    }

    #[test]
    fn a_process_a_signal_ended_is_said_to_be_most_often_at_the_memory_limit() {
        let details = |code, stderr| Output::of(code, "", stderr).failure_details();
        let memory = "most often SIGKILL at the memory limit";
        assert!(details(None, "").contains(memory));
        assert!(details(Some(137), "").contains(memory));
        let cargo = "error: test failed\n(signal: 9, SIGKILL: kill)\n";
        assert!(details(Some(101), cargo).contains(memory));
        assert!(!details(Some(1), "error: test failed\n").contains(memory));
    }

    fn said(stdout: &str, stderr: &str) -> Output {
        Output::of(Some(1), stdout, stderr)
    }

    #[test]
    fn printed_is_the_stdout_of_a_command_that_passed_and_names_one_that_did_not() {
        let cwd = Path::new(".");
        let passed: Start = &|_, _, _, _| {
            Ok(Output {
                code: Some(0),
                ..said("abc\n", "noise")
            })
        };
        assert_eq!(
            printed(passed, "git", &["status"], cwd, &[]),
            Ok("abc\n".to_string())
        );
        let failed: Start = &|_, _, _, _| Ok(said("", "fatal: no remote\n"));
        assert_eq!(
            printed(failed, "git", &["ls-remote", "origin"], cwd, &[]),
            Err("`git ls-remote origin` failed: fatal: no remote\nstderr (bounded output):\nfatal: no remote\n".to_string())
        );
        let absent: Start = &|program, _, _, _| {
            Err(ExecError {
                program: program.to_string(),
                stage: Stage::Spawn,
                reason: "not found".to_string(),
                printed: None,
            })
        };
        assert_eq!(
            printed(absent, "git", &[], cwd, &[]),
            Err("could not start git: not found".to_string())
        );
    }

    #[test]
    fn the_reason_a_command_failed_is_the_line_it_marked_and_never_the_first_one() {
        let build = "   Compiling proc-macro2 v1.0.107\n    Updating crates.io index\n\
                     error[E0432]: unresolved import `crate::gone`\n";
        assert_eq!(
            said("", build).why_it_failed(),
            "error[E0432]: unresolved import `crate::gone`"
        );
        assert_eq!(
            said("", "fatal: not a git repository\n").why_it_failed(),
            "fatal: not a git repository"
        );
    }

    #[test]
    fn a_tool_that_marks_its_error_on_stdout_stops_with_that_error_and_its_place() {
        let kani =
            "Compiling a v0.1.0\nerror[E0308]: mismatched types\n  --> src/a.rs:4:20\n   |\n";
        let summary = "error: could not compile `a` (lib) due to 1 previous error\n";
        assert_eq!(
            said(kani, summary).why_it_stopped(),
            "error[E0308]: mismatched types at src/a.rs:4:20"
        );
        // stderr's own marked error comes first, and a run that marks none keeps `why_it_failed`.
        let both = said(kani, "error[E0432]: unresolved import `x`\n");
        assert_eq!(both.why_it_stopped(), "error[E0432]: unresolved import `x`");
        assert_eq!(said("done\n", summary).why_it_stopped(), summary.trim_end());
    }

    #[test]
    fn the_failure_details_give_a_marked_error_the_place_the_tails_may_have_lost() {
        let warnings = "warning: unused\n".repeat(400);
        let stderr = format!("error[E0252]: `r` is defined twice\n --> tests/a.rs:3:5\n{warnings}");
        let details = said("", &stderr).failure_details();
        assert!(
            details.starts_with("error[E0252]: `r` is defined twice at tests/a.rs:3:5"),
            "{details}"
        );
        assert!(!details[40..].contains("tests/a.rs"), "{details}");
    }

    #[test]
    fn cargos_closing_summary_loses_to_the_diagnostic_that_caused_it() {
        let build = "   Compiling outpost-core v0.1.0\n\
                     error[E0599]: no method named `f` found for struct `Ctx`\n\
                     error: could not compile `outpost-cli` (bin \"outpost\") due to 1 previous error\n";
        assert_eq!(
            said("", build).why_it_failed(),
            "error[E0599]: no method named `f` found for struct `Ctx`"
        );
        // The summary alone is still better than nothing, and better than `Compiling`.
        let only_summary = "   Compiling outpost-core v0.1.0\n\
                            error: could not compile `outpost-cli` due to 1 previous error\n";
        assert_eq!(
            said("", only_summary).why_it_failed(),
            "error: could not compile `outpost-cli` due to 1 previous error"
        );
    }

    #[test]
    fn a_linker_that_names_itself_before_the_error_is_still_quoted() {
        let linked = "  Compiling app-log v0.1.0\n\
                      wild: error: Input file libmutest_runtime.rlib contains LLVM-IR, but linker \
                      plugin was not supplied\n\
                      error: could not compile `app-log` (lib test) due to 1 previous error\n";
        assert!(
            said("", linked).why_it_failed().starts_with("wild: error:"),
            "{}",
            said("", linked).why_it_failed()
        );
    }

    #[test]
    fn a_sentence_holding_the_word_error_is_not_read_as_a_tools_own() {
        assert_eq!(
            said("", "the build failed with error: see above\nfatal: nope\n").why_it_failed(),
            "fatal: nope"
        );
    }

    #[test]
    fn a_command_that_marks_no_error_is_quoted_from_the_last_thing_it_said() {
        assert_eq!(
            said(
                "",
                "   Compiling a\n   Compiling b\nlinking with `cc` did not work\n\n"
            )
            .why_it_failed(),
            "linking with `cc` did not work"
        );
    }

    /// cargo's JSON output puts its diagnostics on stdout.
    #[test]
    fn a_command_that_wrote_nothing_to_stderr_is_quoted_from_stdout() {
        assert_eq!(
            said("something went wrong\n", "").why_it_failed(),
            "something went wrong"
        );
        assert_eq!(said("", "").why_it_failed(), "no reason given");
    }

    #[test]
    #[cfg_attr(
        all(miri, windows),
        ignore = "Miri cannot give a command a variable on Windows"
    )]
    fn every_variable_git_exports_into_a_hook_is_cleared_and_nothing_else_is() {
        let mut command = Command::new("env");
        for name in GIT_STATE {
            command.env(name, "/somewhere/else");
        }
        command.env("PATH", "/usr/bin").env_remove_each(&GIT_STATE);
        // A removal shows up as a name with no value, so what survives is what the child inherits.
        let kept: Vec<String> = command
            .get_envs()
            .filter_map(|(name, value)| {
                value.map(|value| format!("{}={}", name.to_string_lossy(), value.to_string_lossy()))
            })
            .collect();
        assert_eq!(kept, vec!["PATH=/usr/bin".to_string()]);
    }

    /// anyhow's first run left a `rustc-ice-*.txt` in its tree; `typos` read it on the second.
    #[test]
    fn a_crash_report_goes_nowhere_unless_the_user_chose_a_place() {
        assert_eq!(crash_report(None), Some(("RUSTC_ICE", "0")));
        assert_eq!(crash_report(Some(std::ffi::OsStr::new("/var/ice"))), None);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn what_cargo_says_about_the_package_running_chock_never_reaches_a_tool() {
        for name in [
            "CARGO_PKG_NAME",
            "CARGO_PKG_VERSION_MAJOR",
            "CARGO_MANIFEST_DIR",
            "CARGO_CRATE_NAME",
            "CARGO_BIN_EXE_chock",
            "CARGO_PRIMARY_PACKAGE",
        ] {
            assert!(describes_the_launching_package(name), "{name}");
        }
        for name in [
            "CARGO",
            "CARGO_HOME",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_JOBS",
            "PATH",
        ] {
            assert!(!describes_the_launching_package(name), "{name}");
        }
        // Set explicitly so the removal is what this proves; `CARGO_HOME` is not the package's.
        let out = run_env(
            "sh",
            &[
                "-c",
                "printf '%s|%s' \"${CARGO_PKG_NAME-unset}\" \"${CARGO_HOME-unset}\"",
            ],
            &here(),
            &[("CARGO_PKG_NAME", "outer"), ("CARGO_HOME", "/kept")],
        )
        .unwrap();
        assert_eq!(out.stdout, "unset|/kept");
    }

    #[test]
    fn the_index_git_hands_a_hook_is_among_the_variables_cleared() {
        assert!(GIT_STATE.contains(&"GIT_INDEX_FILE"));
        assert!(GIT_STATE.contains(&"GIT_DIR"));
    }

    /// As with `bash -c 'sleep 600 & echo started'`, which exits while its child holds stdout.
    #[test]
    fn output_a_surviving_child_still_holds_is_given_up_on_rather_than_waited_for() {
        let (_tx, rx) = std::sync::mpsc::channel::<Result<(String, bool), String>>();
        let waited = std::time::Duration::from_millis(1);
        let (stage, why) = drained(&rx, "stdout", waited).unwrap_err();
        assert_eq!(stage, Stage::Capture);
        assert_eq!(
            why,
            "it exited, and 0s later something it started still held its stdout, so the output \
             cannot be read whole"
        );
    }

    #[test]
    fn output_the_reader_finished_is_handed_over() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok(("hello\n".to_string(), false))).unwrap();
        assert_eq!(
            drained(&rx, "stdout", std::time::Duration::from_secs(1)),
            Ok(("hello\n".to_string(), false))
        );
    }

    #[test]
    fn output_the_reader_had_to_cut_is_handed_over_marked_as_cut() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok(("part".to_string(), true))).unwrap();
        assert_eq!(
            drained(&rx, "stderr", std::time::Duration::from_secs(1)),
            Ok(("part".to_string(), true))
        );
    }

    fn here() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// What `yes WORD | head -c N` writes: whole `WORD\n` lines, then a cut-off tail.
    fn repeated(word: &str, bytes: usize) -> String {
        let line = format!("{word}\n");
        let mut out = line.repeat(bytes / line.len());
        out.push_str(&word[..bytes % line.len()]);
        out
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_successful_command_reports_its_stdout_and_a_zero_code() {
        let out = run("echo", &["hello"], &here()).unwrap();
        assert_eq!(out.code, Some(0));
        assert_eq!(out.stdout, "hello\n");
        assert!(out.success());
        assert!(!out.truncated);
    }

    /// The limit is passed in, since `deadline` reads chock's own environment, not the child's.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_tool_that_never_finishes_is_killed_rather_than_waited_on() {
        let started = std::time::Instant::now();
        let mut child = Process::spawn(Command::new("sleep").arg("600")).unwrap();
        let (stage, why) =
            wait_until(&mut child, std::time::Duration::from_millis(200), &mut ()).unwrap_err();
        let cleaned = child.finish();
        assert_eq!(stage, Stage::Hung);
        assert!(why.contains("still running after"), "{why}");
        assert!(cleaned.is_ok(), "{cleaned:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_timed_out_command_keeps_its_bounded_diagnostics_and_all_it_printed() {
        let mut child = Process::spawn(Command::new("sh")
            .args(["-c", "printf 'starting fixture\\n'; printf 'error: fixture stalled\\n' >&2; exec sleep 600"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())).unwrap();
        let stdout = captured(child.child.stdout.take(), 1024);
        let stderr = captured(child.child.stderr.take(), 1024);
        let why = finish_capture(
            &mut child,
            "fixture",
            &stdout,
            &stderr,
            std::time::Duration::from_millis(200),
            std::time::Duration::from_secs(1),
            &mut (),
        )
        .unwrap_err();
        assert_eq!(why.stage, Stage::Hung);
        assert!(why.reason.contains("starting fixture"), "{why}");
        assert!(why.reason.contains("fixture stalled"), "{why}");
        assert!(why.reason.contains("stdout"), "{why}");
        assert!(why.reason.contains("stderr"), "{why}");
        let printed = (why.printed.as_deref()).map(|it| (it.stdout.as_str(), it.stderr.as_str()));
        assert_eq!(
            printed,
            Some(("starting fixture\n", "error: fixture stalled\n"))
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_pipe_that_never_closes_still_exposes_its_partial_diagnostic() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let stdout = Capture {
            reading: rx,
            tail: std::sync::Arc::new(std::sync::Mutex::new(
                b"fixture still owns this pipe".to_vec(),
            )),
        };
        let stderr = captured(None::<std::io::Empty>, 10);
        let mut child = Process::spawn(Command::new("sh").args(["-c", "exit 0"])).unwrap();
        let why = finish_capture(
            &mut child,
            "fixture",
            &stdout,
            &stderr,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_millis(1),
            &mut (),
        )
        .unwrap_err();
        assert_eq!(why.stage, Stage::Capture);
        assert!(why.reason.contains("fixture still owns this pipe"), "{why}");
        assert_eq!(why.printed, None);
    }

    #[test]
    fn an_io_error_cannot_be_read_as_a_complete_output_prefix() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("fixture read failure"))
            }
        }
        let capture = captured(Some(Broken), 10);
        assert_eq!(
            drained(
                &capture.reading,
                "stdout",
                std::time::Duration::from_secs(1)
            ),
            Err((
                Stage::Capture,
                "cannot read stdout: fixture read failure".to_string()
            ))
        );
    }

    #[test]
    fn an_invalid_reader_length_is_refused_without_indexing_past_the_buffer() {
        struct Invalid;
        impl Read for Invalid {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                Ok(buf.len() + 1)
            }
        }
        assert_eq!(
            next_chunk(&mut Invalid, &mut [0; 4]),
            Err("reader returned a length beyond its buffer".to_string())
        );
    }

    #[test]
    fn diagnostic_tails_preserve_the_end_without_changing_the_parser_prefix() {
        let mut tail = Vec::new();
        remember_tail(&mut tail, &vec![b'x'; DIAGNOSTIC_BYTES]);
        remember_tail(&mut tail, b"done");
        let expected = [vec![b'x'; DIAGNOSTIC_BYTES - 4], b"done".to_vec()].concat();
        assert_eq!(tail, expected);
        remember_tail(&mut tail, &vec![b'y'; DIAGNOSTIC_BYTES + 1]);
        assert_eq!(tail, vec![b'y'; DIAGNOSTIC_BYTES]);
        assert_eq!(text_tail("aéé", 3), "é");
        assert_eq!(text_tail("hello", 0), "");
    }

    #[test]
    fn interrupted_reads_retry_and_long_failure_details_remain_bounded() {
        struct Interrupted(bool);
        impl Read for Interrupted {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                if std::mem::take(&mut self.0) {
                    Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
                } else {
                    Ok(0)
                }
            }
        }
        let held = std::sync::Mutex::new(Vec::new());
        assert_eq!(
            read_capped(Some(Interrupted(true)), 10, &held),
            Ok((String::new(), false))
        );
        let large = format!("{}last diagnostic", "x".repeat(DIAGNOSTIC_BYTES));
        let out = Output {
            code: Some(1),
            stdout: large,
            stderr: String::new(),
            truncated: true,
        };
        let why = out.failure_details();
        assert!(why.contains("capture truncated"), "{why}");
        assert!(why.contains("earlier output omitted"), "{why}");
        assert!(why.ends_with("last diagnostic"), "{why}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn interrupted_exit_observation_does_not_claim_the_child_finished() {
        use process::observed_child_exit;
        assert!(!observed_child_exit(Err(rustix::io::Errno::INTR)).unwrap());
        assert!(!observed_child_exit(Ok(false)).unwrap());
        assert!(observed_child_exit(Ok(true)).unwrap());
        assert_eq!(
            observed_child_exit(Err(rustix::io::Errno::CHILD))
                .unwrap_err()
                .raw_os_error(),
            Some(rustix::io::Errno::CHILD.raw_os_error())
        );
    }

    #[test]
    fn direct_capture_distinguishes_exact_capacity_overflow_and_absent_input() {
        let tail = std::sync::Mutex::new(Vec::new());
        assert_eq!(
            read_capped(Some(&b"abc"[..]), 3, &tail),
            Ok(("abc".into(), false))
        );
        assert_eq!(
            read_capped(Some(&b"abcd"[..]), 3, &tail),
            Ok(("abc".into(), true))
        );
        assert_eq!(
            read_capped(Some(&b"ab"[..]), 3, &tail),
            Ok(("ab".into(), false))
        );
        assert_eq!(
            read_capped(None::<std::io::Empty>, 0, &tail),
            Ok((String::new(), false))
        );
        let exact = "x".repeat(DIAGNOSTIC_BYTES);
        assert_eq!(
            diagnostic("stdout", &exact),
            format!("\nstdout (bounded output):\n{exact}")
        );
    }

    #[test]
    fn noninterrupted_read_errors_are_not_retried() {
        struct FailsOnce(bool);
        impl Read for FailsOnce {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                if std::mem::take(&mut self.0) {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "denied",
                    ))
                } else {
                    Ok(0)
                }
            }
        }
        assert_eq!(
            read_chunk(&mut FailsOnce(true), &mut [0; 1]),
            Err("denied".into())
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_watched_run_that_fails_quotes_the_error_its_tool_marked_above_the_tail() {
        struct NoRecords;
        impl Watchdog for NoRecords {
            fn poll(&mut self) -> Result<(), String> {
                Ok(())
            }
            fn complete(&mut self) -> Result<(), String> {
                Err("no progress records".into())
            }
        }
        let script = "printf 'error: internal compiler error: boom\\n' >&2; \
                      head -c 6000 /dev/zero | tr '\\0' x >&2";
        let failed =
            run_watched("sh", &["-c", script], &here(), &[], &mut NoRecords, never).unwrap_err();
        assert!(
            failed
                .reason
                .contains("what it marked as the error: error: internal compiler error: boom"),
            "{}",
            failed.reason
        );
    }

    fn stopping(line: &str) -> bool {
        !line.starts_with("SLOW")
    }

    fn whole_start(line: &str) -> bool {
        line.len() == LINE_START
    }

    #[test]
    fn a_counted_pipe_counts_each_finished_line_its_moving_accepts() {
        let moved = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let text = &b"PASS a\nSLOW b\nPA"[..];
        let mut pipe = counted(Some(text), stopping, &moved).unwrap();
        let mut held = String::new();
        pipe.read_to_string(&mut held).unwrap();
        assert_eq!(held, "PASS a\nSLOW b\nPA");
        assert_eq!(moved.load(std::sync::atomic::Ordering::Relaxed), 1);
        pipe.feed(b'\n');
        assert_eq!(moved.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert!(!never("PASS a"));
    }

    #[test]
    fn a_counted_pipe_keeps_only_the_start_of_a_long_line() {
        let moved = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let long = format!("{}\n", "x".repeat(LINE_START * 2));
        let mut pipe = counted(Some(std::io::empty()), whole_start, &moved).unwrap();
        for byte in long.bytes() {
            pipe.feed(byte);
        }
        assert_eq!(moved.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    struct Refusing;
    impl Watchdog for Refusing {
        fn poll(&mut self) -> Result<(), String> {
            Err("refused".into())
        }
        fn complete(&mut self) -> Result<(), String> {
            Err("incomplete".into())
        }
    }

    #[test]
    fn a_paced_deadline_restarts_on_progress_and_passes_the_inner_watchdog_on() {
        let moved = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut quiet = ();
        let hour = std::time::Duration::from_secs(3600);
        let mut paced = Paced::new(&mut quiet, std::sync::Arc::clone(&moved), hour);
        assert_eq!(paced.poll(), Ok(()));
        moved.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(paced.poll(), Ok(()));
        assert_eq!(paced.seen, 1);

        let short = std::time::Duration::from_millis(200);
        let mut paced = Paced::new(&mut quiet, std::sync::Arc::clone(&moved), short);
        std::thread::sleep(short + short / 2);
        moved.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(paced.poll(), Ok(()), "progress restarts the deadline");

        let mut paced = Paced::new(
            &mut quiet,
            std::sync::Arc::clone(&moved),
            std::time::Duration::ZERO,
        );
        assert_eq!(
            paced.poll(),
            Err("no progress for 0s; cleanup requested".to_string())
        );
        assert_eq!(paced.complete(), Ok(()));

        let mut refusing = Refusing;
        let mut paced = Paced::new(&mut refusing, moved, hour);
        assert_eq!(paced.poll(), Err("refused".to_string()));
        assert_eq!(paced.complete(), Err("incomplete".to_string()));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_paced_run_returns_what_its_tool_printed() {
        let out = run_paced(
            "sh",
            &["-c", "echo PASS; echo SLOW >&2"],
            &here(),
            &[],
            (stopping, deadline()),
        )
        .unwrap();
        assert_eq!(out.stdout, "PASS\n");
        assert_eq!(out.stderr, "SLOW\n");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn watchdog_completion_waits_for_the_process_and_the_recursion_depth_increases() {
        struct MustWait;
        impl Watchdog for MustWait {
            fn poll(&mut self) -> Result<(), String> {
                Ok(())
            }
            fn complete(&mut self) -> Result<(), String> {
                Err("completed too early".into())
            }
        }
        struct OverMemory;
        impl Watchdog for OverMemory {
            fn poll(&mut self) -> Result<(), String> {
                Err("held 9 MB".into())
            }
            fn complete(&mut self) -> Result<(), String> {
                Ok(())
            }
            fn stage(&self) -> Stage {
                Stage::Memory
            }
        }
        let mut child = Process::spawn(Command::new("sleep").arg("60")).unwrap();
        let stopped = observed_exit(&mut child, &mut OverMemory);
        assert_eq!(stopped, Err((Stage::Memory, "held 9 MB".to_string())));
        let status = observed_exit(&mut child, &mut MustWait);
        let cleanup = child.finish();
        assert_eq!(status, Ok(false));
        assert!(cleanup.is_ok());
        let out = run("sh", &["-c", "printf '%s' \"$CHOCK_GATE_DEPTH\""], &here()).unwrap();
        assert_eq!(out.stdout, (depth() + 1).to_string());
    }

    #[test]
    fn failure_details_preserve_test_names_beside_a_generic_error_summary() {
        let out = Output::of(
            Some(1),
            "FAIL [0.001s] fixture::cannot_read\n",
            "error: test run failed\n",
        );
        let why = out.failure_details();
        assert!(why.contains("fixture::cannot_read"), "{why}");
        assert!(why.contains("error: test run failed"), "{why}");
        assert!(why.contains("stdout"), "{why}");
        assert!(why.contains("stderr"), "{why}");
        assert_eq!(diagnostic("stdout", ""), "");
    }

    #[test]
    fn cleanup_failure_preserves_the_original_error_and_cannot_pass() {
        let failed = completed_capture::<()>(
            Err((Stage::Hung, "deadline elapsed".to_string())),
            Err("signal refused".to_string()),
        );
        assert_eq!(
            failed,
            Err((
                Stage::Hung,
                "deadline elapsed; cleanup incomplete: signal refused".to_string()
            ))
        );
        let otherwise_done = completed_capture(Ok("output"), Err("reap failed".to_string()));
        assert_eq!(
            otherwise_done,
            Err((Stage::Wait, "cleanup incomplete: reap failed".to_string()))
        );
    }

    /// One look after each `POLL`, for at most `turns` looks. The sleep comes first, so each line
    /// here runs in every call, however fast the child is.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn polled<T>(turns: u32, mut look: impl FnMut() -> Option<T>) -> Option<T> {
        (0..turns).find_map(|_| {
            std::thread::sleep(POLL);
            look()
        })
    }

    #[cfg(target_os = "linux")]
    struct Witness {
        pid: rustix::process::Pid,
        fd: rustix::fd::OwnedFd,
    }

    #[cfg(target_os = "linux")]
    impl Witness {
        fn open(pid: i32) -> Self {
            let pid = rustix::process::Pid::from_raw(pid).unwrap();
            let fd =
                rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).unwrap();
            Self { pid, fd }
        }

        fn stopped(&self) -> bool {
            let path = format!("/proc/{}/stat", self.pid.as_raw_pid());
            match std::fs::read_to_string(path) {
                Ok(stat) => stat.rsplit_once(')').is_some_and(|(_, rest)| {
                    matches!(rest.split_whitespace().next(), Some("Z" | "X"))
                }),
                Err(error) => error.kind() == std::io::ErrorKind::NotFound,
            }
        }

        fn await_exit(&self) -> bool {
            polled(100, || self.stopped().then_some(())).is_some()
        }
    }

    /// Each arm of `Witness::stopped` in one place, so none waits on how fast another test's child is.
    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_witness_reads_a_child_as_running_then_dead_then_gone() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let witness = Witness::open(i32::try_from(child.id()).unwrap());
        assert!(!witness.stopped());
        child.kill().unwrap();
        assert!(
            witness.await_exit(),
            "a killed child, not yet reaped, reads as stopped"
        );
        child.wait().unwrap();
        assert!(witness.stopped());
    }

    #[cfg(target_os = "linux")]
    impl Drop for Witness {
        fn drop(&mut self) {
            let _ = rustix::process::pidfd_send_signal(&self.fd, rustix::process::Signal::KILL);
        }
    }

    #[cfg(target_os = "linux")]
    fn ready_children(dir: &Path) -> Vec<Witness> {
        let ready = polled(150, || {
            std::fs::read_to_string(dir.join("ready"))
                .ok()
                .filter(|text| text.ends_with('\n'))
        })
        .unwrap_or_default();
        assert!(ready.ends_with('\n'), "fixture not ready");
        ready
            .split_whitespace()
            .map(|pid| Witness::open(pid.parse().unwrap()))
            .collect()
    }

    #[cfg(target_os = "linux")]
    fn tree_fixture(dir: &Path, script: &str) -> (Process, Capture, Capture) {
        let mut child = Process::spawn(
            Command::new("sh")
                .args(["-c", script])
                .current_dir(dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        let stdout = captured(child.child.stdout.take(), 1024);
        let stderr = captured(child.child.stderr.take(), 1024);
        (child, stdout, stderr)
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn timeout_kills_owned_grandchildren_but_not_another_group() {
        let (failure, stopped) = hung_beside_a_sentinel(
            "timeout-process-tree",
            "printf 'stdout marker\\n'; printf 'stderr marker\\n' >&2; sh -c 'sleep 600 & printf \"%s %s\\n\" \"$$\" \"$!\" > ready; wait' & wait",
        );
        assert_eq!(failure.stage, Stage::Hung);
        assert!(failure.reason.contains("stdout marker"), "{failure}");
        assert!(failure.reason.contains("stderr marker"), "{failure}");
        assert_eq!(stopped, [true, true]);
    }

    /// Runs `script` in a fixture that hangs past its timeout, beside a sentinel that must outlive
    /// it. Gives the failure, and whether each witness the script wrote has exited.
    #[cfg(target_os = "linux")]
    fn hung_beside_a_sentinel(name: &str, script: &str) -> (ExecError, Vec<bool>) {
        let dir = crate::testdir::make(name);
        let (mut child, stdout, stderr) = tree_fixture(&dir, script);
        let witnesses = ready_children(&dir);
        let mut sentinel = Process::spawn(Command::new("sleep").arg("600")).unwrap();
        let result = finish_capture(
            &mut child,
            "fixture",
            &stdout,
            &stderr,
            std::time::Duration::ZERO,
            std::time::Duration::from_secs(1),
            &mut (),
        );
        let stopped = witnesses.iter().map(Witness::await_exit).collect();
        assert!(
            !sentinel.exited().unwrap(),
            "the sentinel outside the tree was killed"
        );
        assert!(sentinel.finish().is_ok());
        (result.unwrap_err(), stopped)
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_stop_takes_a_process_in_a_group_of_its_own_and_spares_one_outside_the_tree() {
        let script = "setsid sleep 600 & printf '%s\\n' \"$!\" > ready; wait";
        let (failure, stopped) = hung_beside_a_sentinel("stop-own-group", script);
        assert_eq!(failure.stage, Stage::Hung);
        assert_eq!(stopped, [true]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn an_exited_leaders_pipe_holder_is_killed_without_claiming_complete_output() {
        let dir = crate::testdir::make("exited-pipe-holder");
        let (mut child, stdout, stderr) = tree_fixture(
            &dir,
            "printf 'before exit\\n'; sleep 600 & printf '%s\\n' \"$!\" > ready; exit 0",
        );
        let witnesses = ready_children(&dir);
        let result = finish_capture(
            &mut child,
            "fixture",
            &stdout,
            &stderr,
            std::time::Duration::from_secs(2),
            std::time::Duration::from_millis(50),
            &mut (),
        );
        let stopped: Vec<_> = witnesses.iter().map(Witness::await_exit).collect();
        let failure = result.unwrap_err();
        assert_eq!(failure.stage, Stage::Capture);
        assert!(failure.reason.contains("before exit"), "{failure}");
        assert_eq!(stopped, [true]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_successful_leaders_silent_descendant_is_also_killed() {
        let dir = crate::testdir::make("silent-descendant");
        let (mut child, stdout, stderr) = tree_fixture(
            &dir,
            "sleep 600 >/dev/null 2>&1 & printf '%s\\n' \"$!\" > ready; printf 'complete\\n'; exit 0",
        );
        let witnesses = ready_children(&dir);
        let result = finish_capture(
            &mut child,
            "fixture",
            &stdout,
            &stderr,
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(1),
            &mut (),
        );
        let stopped: Vec<_> = witnesses.iter().map(Witness::await_exit).collect();
        let output = result.unwrap();
        assert_eq!(output.code, Some(0));
        assert_eq!(output.stdout, "complete\n");
        assert_eq!(stopped, [true]);
        assert!(child.finish().is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn externally_reaped() -> Process {
        let child = Process::spawn(Command::new("sh").args(["-c", "exit 0"])).unwrap();
        rustix::process::waitpid(
            Some(rustix::process::Pid::from_child(&child.child)),
            rustix::process::WaitOptions::empty(),
        )
        .unwrap();
        child
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn an_external_reaper_invalidates_ownership_before_any_signal() {
        let mut child = externally_reaped();
        let failure =
            wait_until(&mut child, std::time::Duration::from_secs(1), &mut ()).unwrap_err();
        assert_eq!(failure.0, Stage::Wait);
        assert!(!child.owned);
        let reap = reap_until(&mut child.child, std::time::Duration::ZERO).unwrap_err();
        assert!(reap.starts_with("cannot reap child:"), "{reap}");
        assert_eq!(
            child.finish().unwrap_err(),
            "child ownership is no longer available for cleanup"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn finalization_refuses_a_child_reaped_elsewhere_and_does_not_retry() {
        let mut child = externally_reaped();
        assert_eq!(
            child.finish().unwrap_err(),
            "child was reaped elsewhere; refusing a stale process identity"
        );
        assert!(!child.owned);
        assert!(child.finalized);
        child.finalized = false;
        assert_eq!(
            child.finish().unwrap_err(),
            "child ownership is no longer available for cleanup"
        );
        assert!(child.finalized);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn reaping_is_bounded_and_drop_cleans_an_unfinished_owner() {
        let mut child = Process::spawn(Command::new("sleep").arg("600")).unwrap();
        let witness = Witness::open(i32::try_from(child.child.id()).unwrap());
        let result = reap_until(&mut child.child, std::time::Duration::ZERO);
        drop(child);
        let stopped = witness.await_exit();
        assert_eq!(
            result.unwrap_err(),
            "child did not exit within the cleanup deadline"
        );
        assert!(stopped);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_finished_owner_is_final_and_a_second_finish_is_refused() {
        let mut child = Process::spawn(Command::new("sleep").arg("600")).unwrap();
        let first = child.finish();
        assert!(first.is_ok(), "{first:?}");
        assert!(child.finalized);
        assert_eq!(
            child.finish().unwrap_err(),
            "child ownership is no longer available for cleanup"
        );
        assert!(child.finalized);
    }

    /// The pipe ends only when the tool does, so the reader sees its end only after a drop kills it.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn dropping_an_unfinished_owner_ends_its_tool() {
        let mut command = Command::new("sleep");
        command.arg("600").stdout(Stdio::piped());
        let mut child = Process::spawn(&mut command).unwrap();
        let mut out = child.child.stdout.take().unwrap();
        let (ended, seen) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = ended.send(std::io::Read::read_to_end(&mut out, &mut Vec::new()).is_ok());
        });
        drop(child);
        assert_eq!(
            seen.recv_timeout(std::time::Duration::from_secs(20)),
            Ok(true)
        );
    }

    /// A clock never lands on the limit, so only this catches `>=` narrowed to `>`.
    #[test]
    fn the_deadline_is_reached_at_the_limit_and_not_only_past_it() {
        let secs = std::time::Duration::from_secs;
        assert!(past(secs(1), secs(1)));
        assert!(past(secs(2), secs(1)));
        assert!(!past(std::time::Duration::from_millis(999), secs(1)));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_deadline_already_reached_kills_on_the_first_look() {
        let mut child = Process::spawn(Command::new("sleep").arg("600")).unwrap();
        let (stage, _) = wait_until(&mut child, std::time::Duration::ZERO, &mut ()).unwrap_err();
        let cleaned = child.finish();
        assert_eq!(stage, Stage::Hung);
        assert!(cleaned.is_ok(), "{cleaned:?}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_tool_that_finishes_inside_the_deadline_returns_its_exit() {
        let mut child = Process::spawn(Command::new("sh").args(["-c", "exit 5"])).unwrap();
        wait_until(&mut child, std::time::Duration::from_secs(30), &mut ()).unwrap();
        assert_eq!(child.finish().unwrap().code(), Some(5));
    }

    /// A variable, unlike a flag, also reaches the cargo that the tool spawns in turn.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_spawned_command_is_told_how_many_jobs_it_may_run() {
        let out = run("sh", &["-c", "echo ${CARGO_BUILD_JOBS:-unset}"], &here()).unwrap();
        assert_ne!(out.stdout.trim(), "unset");
        assert!(
            out.stdout.trim().parse::<usize>().is_ok_and(|n| n >= 1),
            "{}",
            out.stdout
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_failing_command_is_still_an_ok_result_carrying_its_code() {
        let out = run("sh", &["-c", "exit 3"], &here()).unwrap();
        assert_eq!(out.code, Some(3));
        assert!(!out.success());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_two_streams_are_kept_apart() {
        let out = run("sh", &["-c", "echo o; echo e >&2"], &here()).unwrap();
        assert_eq!(out.stdout, "o\n");
        assert_eq!(out.stderr, "e\n");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn arguments_reach_the_program_in_the_order_given() {
        let out = run("echo", &["one", "two", "three"], &here()).unwrap();
        assert_eq!(out.stdout, "one two three\n");
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_command_runs_in_the_directory_it_was_given() {
        let out = run("pwd", &[], Path::new("/")).unwrap();
        assert_eq!(out.stdout, "/\n");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn stdin_is_closed_so_a_reader_ends_instead_of_waiting_for_a_terminal() {
        let out = run("cat", &[], &here()).unwrap();
        assert_eq!(out.stdout, "");
        assert!(out.success());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_program_that_is_not_there_names_the_stage_that_failed() {
        let err = run("chock-no-such-program", &[], &here()).unwrap_err();
        assert_eq!(err.stage, Stage::Spawn);
        assert_eq!(err.program, "chock-no-such-program");
        assert_eq!(
            err.to_string(),
            format!("could not start chock-no-such-program: {}", err.reason)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn both_streams_flooding_at_once_finishes_rather_than_deadlocking() {
        let out = run(
            "sh",
            &[
                "-c",
                "yes abcdefgh | head -c 400000; yes zyxwvuts | head -c 400000 >&2",
            ],
            &here(),
        )
        .unwrap();
        assert_eq!(out.stdout, repeated("abcdefgh", 400_000));
        assert_eq!(out.stderr, repeated("zyxwvuts", 400_000));
        assert!(!out.truncated);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn output_past_the_cap_is_a_prefix_and_says_so() {
        let out = run_capped("sh", &["-c", "yes abcdefgh | head -c 40000"], &here(), 100).unwrap();
        assert_eq!(out.stdout, repeated("abcdefgh", 100));
        assert!(out.truncated);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn output_exactly_at_the_cap_is_not_called_truncated() {
        let out = run_capped("printf", &["%s", "0123456789"], &here(), 10).unwrap();
        assert_eq!(out.stdout, "0123456789");
        assert!(!out.truncated);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_flood_on_stderr_alone_still_marks_the_result_truncated() {
        let out = run_capped(
            "sh",
            &["-c", "yes abcdefgh | head -c 40000 >&2"],
            &here(),
            100,
        )
        .unwrap();
        assert_eq!(out.stdout, "");
        assert!(out.truncated);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[cfg_attr(miri, ignore = "Miri prints the code of an OS error twice")]
    fn a_group_of_only_zombies_is_settled_on_either_system_and_any_other_refusal_is_not() {
        use process::nothing_left;
        use rustix::io::Errno;
        assert_eq!(nothing_left(Ok(())), Ok(()));
        assert_eq!(nothing_left(Err(Errno::SRCH)), Ok(()));
        let refused = "cannot signal owned process group: Operation not permitted (os error 1)";
        let darwin = if cfg!(target_os = "macos") {
            Ok(())
        } else {
            Err(refused.to_string())
        };
        assert_eq!(nothing_left(Err(Errno::PERM)), darwin);
        assert!(
            nothing_left(Err(Errno::INVAL)).is_err(),
            "anything else is a cleanup that failed"
        );
    }

    /// The leader is kept unreaped until this signal, which is the case Darwin answers with EPERM.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_finished_tool_held_unreaped_is_terminated_cleanly() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);
        let mut process = Process::spawn(&mut command).unwrap();
        let exited = polled(500, || process.exited().unwrap().then_some(()));
        assert!(exited.is_some(), "the shell never exited");
        assert_eq!(process.terminate(), Ok(()));
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_signal_leaves_no_code_which_is_never_a_verdict() {
        let out = run("sh", &["-c", "kill -TERM $$"], &here()).unwrap();
        assert_eq!(out.code, None);
        assert!(!out.success());
    }
}
