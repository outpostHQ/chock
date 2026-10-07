//! `--global` installs the tools once per machine, `--local` writes what one project commits.
//! Sharing executables makes two projects comparable; sharing thresholds makes them incomparable.

use std::fmt;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::exec::Start;
use crate::gates;
use crate::project;
use crate::project::config::Config;
use crate::run::baseline::Baseline;
use crate::run::report::plural;
use crate::run::{self, Ctx, Group, Kind};
pub use crate::setup::adoption::{Decision, render_decisions};
use crate::setup::adoption::{adoption, decided};
use crate::setup::pins::{self, Pin};
use crate::setup::version;

/// chock's own files are the template, because chock runs its own gates on itself; a separate
/// template would drift.
const JUSTFILE: &str = include_str!("../../justfile");
const PIN_FILE: &str = include_str!("../../tool-versions.env");
const DENY: &str = include_str!("../../deny.toml");

/// The baselines are deliberately absent: they are the shared reference the gates measure against,
/// so they belong in git. `last-run.json` is this machine's cache of the last run and does not.
const IGNORES: [&str; 9] = [
    "/target",
    "lcov.info",
    // What `proof` leaves behind.
    "kani-list.json",
    ".chock/last-run.json",
    ".chock/run.lock",
    // Verdicts this machine may answer from. It names this machine's tool versions, so another
    // checkout reading it would match a key it has no right to.
    crate::run::verdicts::FILE,
    crate::edited::HEARTBEAT,
    // A compiler crash dump lands in the working directory, so a `git add -A` after one commits it.
    "rustc-ice-*.txt",
    // What `install_all` writes beside a file it will not overwrite. Not `*.chock`: `*` matches
    // the empty string, so that pattern would hide all of `.chock/`.
    "?*.chock",
];

/// What went wrong, as a value, so a test asserts the case and not the wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    UnknownOption(String),
    NotARustProject,
    Unreadable { path: String, reason: String },
    Unwritable { path: String, reason: String },
    NotInstalled(Vec<String>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOption(o) => write!(f, "unknown option `{o}`"),
            Self::NotARustProject => {
                write!(
                    f,
                    "no Cargo.toml here or in any parent — chock installs into a Rust project"
                )
            }
            Self::Unreadable { path, reason } => write!(f, "cannot read {path}: {reason}"),
            Self::Unwritable { path, reason } => write!(f, "cannot write {path}: {reason}"),
            Self::NotInstalled(crates) => {
                write!(f, "could not install: {}", crates.join(", "))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scope {
    pub global: bool,
    pub local: bool,
    /// Measure only what needs no compiler, skipping the instrumented builds.
    pub fast: bool,
    /// Print what the options are and do nothing.
    pub help: bool,
}

/// What `chock init --help` prints. It lists every option, because `--fast` saves an adopted
/// repository a full measurement it throws away.
pub const USAGE: &str = "\
chock init [--global] [--local] [--fast]

  --global   install the pinned tools, once per machine: this project's pins, or chock's own
  --local    write this project's hooks, justfile, pins and config
  --fast     measure only the gates that need no compiler; skips the builds
  --help     this text

  With neither --global nor --local, `chock init` does both. After you install a newer chock,
  run it again: it moves this project's pins to that chock and keeps the pins chock does not set.
";

/// No half named means both. `--fast` names neither half, so it leaves that choice alone.
pub fn scope_of(args: &[&str]) -> Result<Scope, Error> {
    let mut scope = Scope {
        global: false,
        local: false,
        fast: false,
        help: false,
    };
    for arg in args {
        match *arg {
            "--global" => scope.global = true,
            "--local" => scope.local = true,
            "--fast" => scope.fast = true,
            "--help" | "-h" => scope.help = true,
            other => return Err(Error::UnknownOption((*other).to_string())),
        }
    }
    if scope.help {
        return Ok(scope);
    }
    if !scope.global && !scope.local {
        scope.global = true;
        scope.local = true;
    }
    Ok(scope)
}

pub fn run(args: &[&str]) -> ExitCode {
    let scope = match scope_of(args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("chock init: {e}");
            return ExitCode::from(2);
        }
    };
    if scope.help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    // `--global` needs no project: a machine gets its tools before it has one.
    let root = project::find(&cwd);
    if scope.local {
        let Some(root) = &root else {
            eprintln!("chock init: {}", Error::NotARustProject);
            return ExitCode::from(2);
        };
        match write_local(root, scope.fast) {
            Ok(report) => print!("{report}"),
            Err(e) => {
                eprintln!("chock init: {e}");
                return ExitCode::from(2);
            }
        }
    }
    if !scope.global {
        return ExitCode::SUCCESS;
    }
    // Every pin is read and parsed before any install starts.
    let parsed = match pins_for(root.as_deref()) {
        Ok((source, text)) => pins::parse(&text).map_err(|e| format!("{source}: {e}")),
        Err(e) => Err(e.to_string()),
    };
    let parsed = match parsed {
        Ok(p) => p,
        Err(e) => {
            eprintln!("chock init: {e}");
            return ExitCode::from(2);
        }
    };
    // A dropped link is common on a laptop, and the next try often gets through.
    let patient = super::network::patiently(&crate::exec::run_env, &std::thread::sleep);
    if let Err(e) = install_global(&parsed, &*patient) {
        eprintln!("chock init: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

/// Where `--global` finds no pin file, it installs the pins this chock ships.
const SHIPPED: &str = "chock's own tool-versions.env";

/// The project's own pin file where it has one; else the pins that `--local` would write.
fn pins_for(root: Option<&Path>) -> Result<(String, String), Error> {
    let shipped = (SHIPPED.to_string(), PIN_FILE.to_string());
    let Some(path) = root.map(|r| r.join(project::PIN_FILE)) else {
        return Ok(shipped);
    };
    match fs::read_to_string(&path) {
        Ok(text) => Ok((path.display().to_string(), text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(shipped),
        Err(e) => Err(Error::Unreadable {
            path: path.display().to_string(),
            reason: e.to_string(),
        }),
    }
}

/// What happened to one file `init --local` was asked to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Written {
    Created(String),
    /// The file was there and already held this content.
    Unchanged(String),
    /// The file was there and held something else, so ours landed beside it under `kept`.
    Conflict {
        name: String,
        kept: String,
    },
}

impl fmt::Display for Written {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created(n) => write!(f, "  created   {n}"),
            Self::Unchanged(n) => write!(f, "  unchanged {n}"),
            Self::Conflict { name, kept } => {
                write!(
                    f,
                    "  conflict  {name} differs — wrote {kept}, merge it yourself"
                )
            }
        }
    }
}

pub(crate) fn unwritable(path: &Path, e: &std::io::Error) -> Error {
    Error::Unwritable {
        path: path.display().to_string(),
        reason: e.to_string(),
    }
}

/// The text of a file chock is about to add to, or empty where there is no file. Any other read
/// failure is an error, or chock would overwrite the file and lose what it held.
pub(crate) fn held_or_empty(path: &Path) -> Result<String, Error> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(Error::Unreadable {
            path: path.display().to_string(),
            reason: e.to_string(),
        }),
    }
}

fn free_name(base: &Path) -> PathBuf {
    let mut candidate = base.to_path_buf();
    let mut n = 1;
    while candidate.exists() || candidate.is_symlink() {
        candidate = PathBuf::from(format!("{}.{n}", base.display()));
        n += 1;
    }
    candidate
}

/// Never overwrites. `is_symlink` as well as `exists`: `exists` is false for a dangling link, and
/// the write would then go *through* it, outside the project.
pub(crate) fn install_file(root: &Path, name: &str, content: &str) -> Result<Written, Error> {
    let dst = root.join(name);
    if dst.exists() || dst.is_symlink() {
        let current = fs::read_to_string(&dst).unwrap_or_default();
        if current == content {
            return Ok(Written::Unchanged(name.to_string()));
        }
        // Reuse an earlier copy that holds this content, or each re-run adds a .1, .2, .3.
        let first = root.join(format!("{name}.chock"));
        let kept = if fs::read_to_string(&first).is_ok_and(|held| held == content) {
            first
        } else {
            let free = free_name(&first);
            crate::project::document::write(&free, content).map_err(|e| unwritable(&free, &e))?;
            free
        };
        let kept_name = kept
            .strip_prefix(root)
            .unwrap_or(&kept)
            .display()
            .to_string();
        return Ok(Written::Conflict {
            name: name.to_string(),
            kept: kept_name,
        });
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| unwritable(parent, &e))?;
    }
    crate::project::document::write(&dst, content).map_err(|e| unwritable(&dst, &e))?;
    Ok(Written::Created(name.to_string()))
}

/// chock's own copy pins no `CHOCK_VERSION` — `Cargo.toml` is the source of truth here and a
/// second copy drifts. An adopting project has none, so the pin is generated and `doctor` holds it.
#[must_use]
pub fn local_pin_file(chock_version: &str) -> String {
    format!(
        "{PIN_FILE}\n# The chock that wrote this project's baselines. doctor holds you to it.\nCHOCK_VERSION={chock_version}\n"
    )
}

/// Why a gate on chock's own hooks and config is on before anything measured it.
const WRITTEN_NOW: &str = "the hooks and the config that this command writes";

/// Keep the measurement beside the adoption decision: core failures and missing measurements remain
/// enabled, while measured quality debt may be left for the project to adopt deliberately.
pub fn judge(gate: &run::Gate, ctx: &Ctx) -> (Decision, Option<crate::run::report::GateReport>) {
    match gate.group {
        Group::OptIn => return (Decision::AskedFor, None),
        Group::Instrument => return (Decision::Reports, None),
        // Switched on without being measured: the config it reads is the one `init` is about to
        // write, so measuring it here reports it missing and then leaves the gate off.
        Group::Setup => return (Decision::On(WRITTEN_NOW.to_string()), None),
        Group::Gates | Group::Quality => {}
    }
    if let Some(why) = switched_on_unmeasured(gate, &ctx.root) {
        return (Decision::On(why.to_string()), None);
    }
    match gate.kind {
        // Measured, not run: a run takes a ratchet's first record, and `init` writes none.
        Kind::Ratchet { .. } | Kind::AnnotatedRatchet { .. } => match run::measure(gate, ctx) {
            Ok(series) => (
                Decision::On(format!("ratchet: {} today", plural(series.len(), "item"))),
                None,
            ),
            Err(reason) => (Decision::Unmeasurable(reason), None),
        },
        Kind::Debt { inspect, .. } => debt_adoption(gate, inspect(ctx)),
        Kind::Binary(_) => {
            let report = run::run_one(gate, ctx);
            (adoption(gate, &report), Some(report))
        }
    }
}

fn debt_adoption(
    gate: &run::Gate,
    inspection: Result<run::Inspection, String>,
) -> (Decision, Option<crate::run::report::GateReport>) {
    match inspection {
        Err(reason) => (Decision::Unmeasurable(reason), None),
        Ok(found) if found.blockers.is_empty() => (
            Decision::On(format!(
                "ratchet: {} today",
                plural(found.debt.len(), "finding")
            )),
            None,
        ),
        Ok(found) => {
            let report = run::outcome_report(gate, Ok(found.outcome()));
            (adoption(gate, &report), Some(report))
        }
    }
}

/// Why this gate is switched on without being measured, or `None` where it has to be. Shared with
/// `about_to_measure`, so what init says it will build is what init builds.
#[must_use]
fn switched_on_unmeasured(gate: &run::Gate, root: &Path) -> Option<&'static str> {
    // A gate keeping its own record writes its first one when a local run judges it. That is the
    // first `chock run`, the step init prints next; `init` writes no record.
    if gates::own_baseline(gate.name).is_some_and(|file| !root.join(file).exists()) {
        return Some("its first record is what the first `chock run` writes");
    }
    // The first `chock run` measures a ratchet anyway, so init does not build it a second time.
    if gate.builds && gate.counts_in().is_some() {
        return Some("a ratchet; the first `chock run` measures it and writes its record");
    }
    None
}

/// Runs every candidate against the tree, which is why `init --local` is not instant.
fn choose_gates(root: &Path, fast: bool) -> (Config, String) {
    let baseline = crate::project::document::read::<Baseline>(root)
        .ok()
        .flatten()
        .unwrap_or_else(|| Baseline::empty(env!("CARGO_PKG_VERSION")));
    // Read before anything is measured, so a project's own test command is the one init runs.
    let said = crate::project::document::read::<Config>(root)
        .ok()
        .flatten();
    let found = said
        .as_ref()
        .and_then(|set| set.coverage.clone())
        .or_else(|| coverage_command(root));
    let ctx = Ctx {
        coverage: found
            .clone()
            .map(crate::run::Coverage::from)
            .unwrap_or_else(|| crate::run::coverage_for(said.as_ref())),
        vcs: crate::project::vcs::holding(root, said.as_ref()),
        root: root.to_path_buf(),
        baseline,
        ..Ctx::from_config(said.as_ref())
    };
    let judged: Vec<(&str, Decision, Option<crate::run::report::GateReport>)> = gates::registry()
        .iter()
        .filter(|gate| measured(gate, fast))
        .map(|gate| {
            let (decision, report) = judge(gate, &ctx);
            (gate.name, decision, report)
        })
        .collect();
    // Written where a run writes it, so `chock explain <gate>` answers straight after `init`
    // instead of asking for a run that has already happened.
    let reports: Vec<crate::run::report::GateReport> =
        judged.iter().filter_map(|(_, _, r)| r.clone()).collect();
    crate::run::report::remember_or_say(&ctx.root, &reports, env!("CARGO_PKG_VERSION"));
    let rows: Vec<(&str, Decision)> = judged
        .iter()
        .map(|(name, decision, _)| (*name, decision.clone()))
        .collect();
    let (config, mut report) = decided(&rows, ctx.vcs.is_some(), found.as_deref());
    if let Some(argv) = &found {
        // chock cannot know another program's options, so it prints the one it chose: a fuller
        // mode can change the debt it counts.
        let _ = writeln!(
            report,
            "  coverage  {} — name its own options in {} if it takes any",
            argv.join(" "),
            crate::project::config::FILE
        );
    }
    (config, report)
}

/// Which gates `init` measures; `--fast` leaves out everything that needs a compiler.
fn measured(gate: &run::Gate, fast: bool) -> bool {
    !fast || !gate.builds
}

/// A project whose tests drive its own binary keeps a script that instruments every process and
/// merges the counters. One cargo command cannot, so the script is looked for rather than assumed.
fn coverage_command(root: &Path) -> Option<Vec<String>> {
    let found = COVERAGE_SCRIPTS
        .iter()
        .find(|name| is_program(&root.join(name)))?;
    Some(vec![
        (*found).to_string(),
        "--lcov".to_string(),
        run::LCOV.to_string(),
    ])
}

/// Where a project keeps that script. A project using another path names it in the config.
const COVERAGE_SCRIPTS: [&str; 2] = ["bin/coverage", "scripts/coverage"];

#[cfg(unix)]
fn is_program(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    fs::metadata(path).is_ok_and(|about| about.is_file() && about.permissions().mode() & 0o111 != 0)
}

/// Windows has no executable bit, so any file counts.
#[cfg(not(unix))]
fn is_program(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|about| about.is_file())
}

fn write_local(root: &Path, fast: bool) -> Result<String, Error> {
    write_local_with(root, &|at| {
        // Printed first: on a large workspace the measuring is the longest thing chock does.
        eprintln!("{}", about_to_measure(fast, at));
        choose_gates(at, fast)
    })
}

/// What the measuring will cost, said before it starts so a caller can choose `--fast` instead.
#[must_use]
fn about_to_measure(fast: bool, root: &Path) -> String {
    let counted = |building: bool| {
        gates::registry()
            .iter()
            .filter(|gate| measured(gate, fast))
            .filter(|gate| matches!(gate.group, Group::Gates | Group::Quality))
            .filter(|gate| switched_on_unmeasured(gate, root).is_none())
            .filter(|gate| gate.builds == building)
            .count()
    };
    let (builds, quick) = (counted(true), counted(false));
    if builds == 0 {
        return format!(
            "Measuring {} against this tree; none of them build it.",
            plural(quick, "gate")
        );
    }
    format!(
        "Measuring {} against this tree. {builds} of them build this project, which on a \
         large workspace is tens of minutes — `chock init --local --fast` switches on only the \
         {quick} that need no compiler.",
        plural(builds + quick, "gate")
    )
}

/// The chooser is injected because it runs every gate against the tree: a test calling the real
/// one would start the test suite from inside the test suite, which is a fork bomb, not a test.
fn write_local_with(
    root: &Path,
    choose: &dyn Fn(&Path) -> (Config, String),
) -> Result<String, Error> {
    let files = install_files(root)?;
    let ignores = add_ignores(root)?;
    let hooks = install_hooks(root)?;
    let editor = crate::setup::agents::install_editor_hook(root)?;
    let contract = crate::setup::agents::write_agent_contract(root)?;
    // Measuring decides the config, so a project that already has one is not measured.
    let (config, decisions) = match already_chosen(root) {
        Some(held) => (held, DECIDED.to_string()),
        None => choose(root),
    };
    let dir = root.join(".chock");
    fs::create_dir_all(&dir).map_err(|e| unwritable(&dir, &e))?;
    let config_path = root.join(crate::project::config::FILE);
    let config_note = keep_the_choices_already_made(&config_path, &config)?;
    Ok(format!(
        "chock init --local: {}\n{files}{config_note}{ignores}{hooks}{editor}{contract}\n\
         Gates measured against this tree:\n{decisions}\n\
         {} on. Next: `chock run` judges the tree and writes each gate's first record.\n{}",
        root.display(),
        plural(config.enabled.len(), "gate"),
        manifest_notes(root).concat()
    ))
}

/// The config a project already has, which is a decision and not a measurement. `None` where
/// there is none or it cannot be parsed, so chock measures instead.
fn already_chosen(root: &Path) -> Option<Config> {
    let path = root.join(crate::project::config::FILE);
    let text = fs::read_to_string(&path).ok()?;
    crate::project::document::parse::<Config>(&text, &path.display().to_string()).ok()
}

const DECIDED: &str = ".chock/config.json already says which checks are on, so nothing was \
measured. `chock enable GATE` switches one on, and `chock run` says where the tree stands.\n";

/// The one file holding decisions somebody made, not measurements chock took. Overwriting it
/// would switch off gates somebody turned on.
fn keep_the_choices_already_made(path: &Path, measured: &Config) -> Result<String, Error> {
    let Ok(text) = fs::read_to_string(path) else {
        crate::project::document::write(path, &measured.render())
            .map_err(|e| unwritable(path, &e))?;
        return Ok(format!("  created   {}\n", crate::project::config::FILE));
    };
    let Ok(held) = crate::project::document::parse::<Config>(&text, &path.display().to_string())
    else {
        return Ok(format!(
            "  note      {} is unreadable; left alone\n",
            crate::project::config::FILE
        ));
    };
    let offered: Vec<&str> = measured
        .enabled
        .iter()
        .map(String::as_str)
        .filter(|name| !held.is_on(name))
        .collect();
    if offered.is_empty() {
        return Ok(format!("  unchanged {}\n", crate::project::config::FILE));
    }
    Ok(format!(
        "  unchanged {}\n  note      {} also pass(es) now: chock enable {}\n",
        crate::project::config::FILE,
        offered.len(),
        offered.join(" ")
    ))
}

use crate::setup::hooks::install_hooks;

/// `outpost commit` screens a commit for keys but runs no user command, so chock's checks do not
/// gate it. This note says so, because silent hooks that nothing runs read as gated.
pub(crate) const OUTPOST_HAS_ONE_MOMENT: &str = "  note      outpost runs only a pre-commit \
hook, so chock's message check and its compiler-bound gates have no moment here. The editor hook \
answers on every edit, and `chock run` in CI is the backstop.\n";

/// The pin file, then the other files `init --local` writes, a line for each.
fn install_files(root: &Path) -> Result<String, Error> {
    let pin_file = local_pin_file(env!("CARGO_PKG_VERSION"));
    let pins = super::repin::install_pins(root, &pin_file)?;
    let files = install_all(root, &[("justfile", JUSTFILE), ("deny.toml", DENY)])?;
    Ok(format!("{pins}\n{files}"))
}

fn install_all(root: &Path, files: &[(&str, &str)]) -> Result<String, Error> {
    let mut out = String::new();
    for (name, content) in files {
        let _ = writeln!(out, "{}", install_file(root, name, content)?);
    }
    Ok(out)
}

/// Append what the gates produce to every ignore file the repositories holding this tree read.
/// Outpost reads only `.outpostignore`, so git's alone leaves an outpost clone tracking them.
fn add_ignores(root: &Path) -> Result<String, Error> {
    let mut report = String::new();
    for file in ignore_files(root) {
        let added = add_ignores_to(&root.join(file))?;
        // The name without its dot, which is what the other lines of the report are keyed by.
        let label = file.trim_start_matches('.');
        report.push_str(&format!("  {label:<9} {} added\n", plural(added, "line")));
    }
    Ok(report)
}

/// A directory no repository holds gets git's name, which is what a clone of it will read.
fn ignore_files(root: &Path) -> Vec<&'static str> {
    let held = crate::project::vcs::holders(root);
    match held.is_empty() {
        true => vec![".gitignore"],
        false => held.iter().map(|kind| kind.ignore_file()).collect(),
    }
}

fn add_ignores_to(path: &Path) -> Result<usize, Error> {
    let (text, added) = with_ignores(held_or_empty(path)?);
    if added > 0 {
        crate::project::document::write(path, &text).map_err(|e| unwritable(path, &e))?;
    }
    Ok(added)
}

/// `text` with each entry it does not already cover appended, and how many that was. A last line
/// without a newline would glue the first entry onto it, breaking it and the next run's check.
fn with_ignores(mut text: String) -> (String, usize) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let mut added = 0;
    for entry in IGNORES {
        if !covers(&text, entry) {
            text.push_str(entry);
            text.push('\n');
            added += 1;
        }
    }
    (text, added)
}

/// Whether `.gitignore` already covers `entry`. `/target`, `target`, `target/` and `/target/` are
/// the same rule here, so no equivalent spelling is added.
#[must_use]
fn covers(gitignore: &str, entry: &str) -> bool {
    let core = entry.trim_start_matches('/').trim_end_matches('/');
    gitignore.lines().map(str::trim).any(|line| {
        line.trim_start_matches('/').trim_end_matches('/') == core && !line.starts_with('#')
    })
}

/// What this manifest will cost a gate later. Reported, never fixed: the manifest is the project's.
#[must_use]
fn manifest_notes(root: &Path) -> Vec<String> {
    let Ok(manifest) = fs::read_to_string(root.join("Cargo.toml")) else {
        return Vec::new();
    };
    manifest_notes_for(&manifest)
}

/// Split from the read so the rules are testable without a filesystem.
#[must_use]
fn manifest_notes_for(manifest: &str) -> Vec<String> {
    let lines: Vec<&str> = manifest.lines().map(str::trim).collect();
    let has_section = |name: &str| {
        lines
            .iter()
            .any(|l| *l == name || l.starts_with(&format!("{name} ")))
    };
    let has_key = |name: &str| {
        lines
            .iter()
            .any(|l| l.starts_with(name) && l[name.len()..].trim_start().starts_with(['=', '.']))
    };
    if !has_section("[package]") {
        return Vec::new();
    }
    [
        (
            !has_section("[workspace]"),
            "no [workspace] table — cargo-udeps builds in a temp copy that would inherit whatever \
             workspace TMPDIR sits inside. Add an empty one.",
        ),
        (
            !manifest.contains("unsafe_code"),
            "no unsafe_code lint — add [lints.rust] unsafe_code = \"deny\" unless this crate needs \
             unsafe.",
        ),
        (
            !has_key("license") && !has_key("license-file"),
            "no license field — `just deps` fails until there is one. Add e.g. license = \
             \"MIT OR Apache-2.0\".",
        ),
    ]
    .into_iter()
    .filter(|(missing, _)| *missing)
    .map(|(_, message)| format!("  note      {message}\n"))
    .collect()
}

/// cargo-crap owns this baseline's format and reads it back; chock only records it. `false` means
/// nothing was recorded, and the caller reports that rather than a success.
pub fn record_crap(ctx: &Ctx, lower: bool, did: &str) -> bool {
    if let Err(why) = crate::gates::coverage::ensure(ctx) {
        return skipped(&why);
    }
    match merged_crap(ctx, lower) {
        Ok(merged) => write_crap(&ctx.root, &merged, did),
        Err(why) => skipped(&why),
    }
}

/// cargo-crap's report over the record. Paths are made relative first, because the record holds
/// relative paths and cargo-crap reports absolute ones.
fn merged_crap(ctx: &Ctx, lower: bool) -> Result<String, String> {
    let measured = crate::gates::coverage::crap::scores(ctx)?;
    let path = ctx.root.join(crate::gates::coverage::crap::baseline());
    let held = fs::read_to_string(path).unwrap_or_default();
    crap_kept_higher(held_unless(lower, &held), &measured, &ctx.root)
}

/// The record a recording holds scores to: none where `--lower` asks for what was measured.
fn held_unless(lower: bool, held: &str) -> &str {
    match lower {
        true => "",
        false => held,
    }
}

/// Raises and adds, as `Series::kept_higher` does: each function keeps the higher of its score
/// and the one `crap::floors` pairs it with in the record.
fn crap_kept_higher(held: &str, measured: &str, root: &Path) -> Result<String, String> {
    let floors = crate::gates::coverage::crap::floors(measured, held, root)?;
    // `floors` read `measured`, so it parses here too.
    let mut now: serde_json::Value = serde_json::from_str(measured).unwrap_or_default();
    raise_each(&mut now, &floors);
    serde_json::to_string_pretty(&now).map_err(|e| format!("cannot write the crap baseline: {e}"))
}

/// Each entry of the report that the record pairs with a score, held to that score.
fn raise_each(report: &mut serde_json::Value, floors: &std::collections::BTreeMap<usize, f64>) {
    let entries = report["entries"].as_array_mut().into_iter().flatten();
    for (at, entry) in entries.enumerate() {
        if let Some(was) = floors.get(&at) {
            raise(entry, *was);
        }
    }
}

/// The recorded score where it is the higher one: a function that improved keeps its floor until
/// `chock baseline --lower crap` records the gain.
fn raise(entry: &mut serde_json::Value, was: f64) {
    if entry["crap"].as_f64().is_some_and(|now| now < was) {
        entry["crap"] = serde_json::json!(was);
    }
}

/// Always `false`, so a caller says why and gives up in one line.
fn skipped(why: &str) -> bool {
    eprintln!("  SKIPPED   crap         {why}");
    false
}

/// Under `.chock/` with chock's other committed state; `data/` is the project's own namespace.
/// `did` is the caller's word for the write: `recorded` or `lowered`.
fn write_crap(root: &Path, json: &str, did: &str) -> bool {
    let file = crate::gates::coverage::crap::baseline();
    let path = root.join(&file);
    if let Some(dir) = path.parent()
        && let Err(e) = fs::create_dir_all(dir)
    {
        return skipped(&unwritable(dir, &e).to_string());
    }
    if let Err(e) =
        crate::project::document::write(&path, &crate::run::baseline::relativize(json, root))
    {
        return skipped(&unwritable(&path, &e).to_string());
    }
    println!("  {did:<9} crap         {file}");
    true
}

/// A setup line is a command and its arguments split on whitespace. Quoting is unsupported: no
/// pinned tool needs it, and splitting a quoted path wrongly would be worse than refusing.
fn setup_command(line: &str) -> Option<(&str, Vec<&str>)> {
    let mut parts = line.split_whitespace();
    let program = parts.next()?;
    Some((program, parts.collect()))
}

/// The second half of installing a tool that fetches the rest of itself. A failure here leaves the
/// tool on PATH but unusable, so it counts as the install failing rather than as a warning.
fn run_setup(pin: &Pin) -> bool {
    let Some(line) = &pin.setup else {
        return true;
    };
    let Some((program, args)) = setup_command(line) else {
        println!(
            "  FAILED    {} — {} pins an empty setup line",
            pin.crate_name, pin.key
        );
        return false;
    };
    match Command::new(program).args(&args).status() {
        Ok(s) if s.success() => {
            println!("  set up    {} — {line}", pin.crate_name);
            true
        }
        _ => {
            println!("  FAILED    {} — `{line}` did not finish", pin.crate_name);
            false
        }
    }
}

fn install_global(pins: &[Pin], start: Start) -> Result<(), Error> {
    let installed = crate::setup::doctor::installed_on_this_machine();
    // Asked per pin: the pin file installs cargo-binstall first, and every tool after it uses it.
    let mut failed: Vec<String> = pins
        .iter()
        .filter(|pin| !installs(pin, &installed, start))
        .map(|pin| pin.crate_name.clone())
        .collect();
    // Miri is no pin: it is a part of the `nightly` toolchain, so every machine gets it.
    let miri = crate::setup::miri::install(start, Path::new("."));
    failed.extend((!reported(&miri)).then(|| crate::setup::miri::NAME.to_string()));
    if failed.is_empty() {
        Ok(())
    } else {
        Err(Error::NotInstalled(failed))
    }
}

/// One pin: Outpost's fork of mutest-rs is built from its repository, any other tool is fetched.
fn installs(pin: &Pin, installed: &dyn Fn(&Pin) -> Option<String>, start: Start) -> bool {
    if !crate::setup::mutest::is_pin(pin, std::env::consts::OS) {
        return install_one(pin, installed, binstall_here());
    }
    println!("{}", crate::setup::mutest::starting());
    reported(&crate::setup::mutest::install(start, &std::env::temp_dir()))
}

/// Prints what an install that is no crate fetch did; `false` is one that failed.
fn reported(done: &Result<String, String>) -> bool {
    let (Ok(line) | Err(line)) = done;
    println!("{line}");
    done.is_ok()
}

/// Whether cargo says crates.io lacks the pin. That is not a failed install: a pin file can
/// name a Haskell binary or a Python tool beside the crates.
fn not_a_crate(said: &str) -> bool {
    said.contains("could not find") && said.contains("registry")
}

/// What installing this pin changes for every project on the machine. Whether a recorded number
/// moves depends on the release, which is why this is said rather than refused.
fn replacing(pin: &Pin, installed: Option<&str>) -> Option<String> {
    let have = installed?;
    if have == pin.want {
        return None;
    }
    Some(format!(
        "  replacing {} {have} with {}, which every project on this machine then measures with",
        pin.crate_name, pin.want
    ))
}

/// A pin not fetched here, and the line saying why: one built from its own checkout, a chock older
/// than this one, or one whose tool does not run on this system. None is a failed install.
fn not_fetched(pin: &Pin, os: &str) -> Option<String> {
    built_locally(pin)
        .or_else(|| older_chock(pin))
        .or_else(|| for_another_system(pin, os))
}

fn built_locally(pin: &Pin) -> Option<String> {
    let name = &pin.crate_name;
    pin.unpublished()
        .then(|| format!("  local     {name} — not on crates.io; build it from its checkout"))
}

/// A chock older than the one that runs, which installing would put in its place.
fn older_chock(pin: &Pin) -> Option<String> {
    let running = env!("CARGO_PKG_VERSION");
    let older = version::compare(&pin.want, running) == Some(std::cmp::Ordering::Less);
    (pin.crate_name == "chock" && older).then(|| {
        format!(
            "  kept      chock {running} — this project pins {}; `chock init --local` updates \
             its pins to this chock",
            pin.want
        )
    })
}

fn for_another_system(pin: &Pin, os: &str) -> Option<String> {
    let why = pin.elsewhere(os)?;
    Some(format!("  skipped   {} — {why}", pin.crate_name))
}

/// Whether `cargo binstall` answers here: it takes a tool's own prebuilt release, seconds where
/// compiling the crate takes minutes.
fn binstall_here() -> bool {
    Command::new("cargo")
        .args(["binstall", "-V"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Printed before each tool, because a compile takes minutes and silence reads as hung.
fn starting(pin: &Pin, binstall: bool) -> String {
    let how = match binstall {
        true => "its own prebuilt release where it has one, else compiled",
        false => "compiled from crates.io",
    };
    format!("  installing {} {} — {how}", pin.crate_name, pin.want)
}

/// What cargo is asked for one pin. binstall never takes a third-party build cache, only the
/// release the tool publishes itself, and falls back to compiling when there is none.
fn fetch_args(pin: &Pin, binstall: bool) -> Vec<String> {
    let at = format!("{}@{}", pin.crate_name, pin.want);
    let args = match binstall {
        true => vec![
            "binstall",
            "-y",
            "--locked",
            "--disable-strategies",
            "quick-install",
            &at,
        ],
        false => vec![
            "install",
            &pin.crate_name,
            "--version",
            &pin.want,
            "--locked",
        ],
    };
    args.into_iter().map(str::to_string).collect()
}

/// A binstall that fails for any reason falls back to the compile, whose errors `install_one`
/// already knows how to read.
fn fetch(pin: &Pin, binstall: bool) -> std::io::Result<std::process::Output> {
    let fetched = binstall.then(|| Command::new("cargo").args(fetch_args(pin, true)).output());
    match fetched {
        Some(Ok(out)) if out.status.success() => Ok(out),
        _ => Command::new("cargo").args(fetch_args(pin, false)).output(),
    }
}

/// One tool, fetched and then set up. A tool that is not on crates.io has no coordinate to fetch,
/// so it is reported rather than attempted, and that is not a failure.
fn install_one(pin: &Pin, installed: &dyn Fn(&Pin) -> Option<String>, binstall: bool) -> bool {
    if let Some(said) = not_fetched(pin, std::env::consts::OS) {
        println!("{said}");
        return true;
    }
    if let Some(said) = replacing(pin, installed(pin).as_deref()) {
        println!("{said}");
    }
    println!("{}", starting(pin, binstall));
    let Ok(out) = fetch(pin, binstall) else {
        println!("  FAILED    {} {}", pin.crate_name, pin.want);
        return false;
    };
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        if not_a_crate(&said) {
            println!(
                "  skipped   {} {} — crates.io does not have it, so chock did not install it",
                pin.crate_name, pin.want
            );
            return true;
        }
        print!("{said}");
        println!("  FAILED    {} {}", pin.crate_name, pin.want);
        return false;
    }
    println!("  installed {} {}", pin.crate_name, pin.want);
    run_setup(pin)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;

    #[test]
    fn help_selects_no_installation_and_keeps_both_aliases() {
        for argument in ["--help", "-h"] {
            assert_eq!(
                scope_of(&[argument]).unwrap(),
                Scope {
                    global: false,
                    local: false,
                    fast: false,
                    help: true
                }
            );
        }
    }

    #[test]
    fn direct_debt_adoption_never_adopts_a_correctness_blocker() {
        let gate = &crate::gates::cargo::modcheck::GATE;
        let debt = crate::run::report::Finding::at("src/orphan.rs", "orphan debt");
        let blocked = crate::run::report::Finding::at("src/lib.rs", "missing module");
        let (decision, report) = debt_adoption(
            gate,
            Ok(run::Inspection {
                debt: vec![debt.clone()],
                blockers: Vec::new(),
            }),
        );
        assert_eq!(decision, Decision::On("ratchet: 1 finding today".into()));
        assert!(report.is_none());
        let (_, report) = debt_adoption(
            gate,
            Ok(run::Inspection {
                debt: vec![debt.clone()],
                blockers: vec![blocked.clone()],
            }),
        );
        let report = report.unwrap();
        assert_eq!(report.verdict, crate::run::report::Verdict::Tripped);
        assert_eq!(report.findings, vec![blocked, debt]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn only_a_missing_file_reads_as_empty_and_one_that_cannot_be_read_is_an_error() {
        let dir = crate::testdir::make("init-held-or-empty");
        assert_eq!(held_or_empty(&dir.join("absent")), Ok(String::new()));
        fs::write(dir.join("held"), "kept\n").unwrap();
        assert_eq!(held_or_empty(&dir.join("held")), Ok("kept\n".to_string()));
        let binary = dir.join("binary");
        fs::write(&binary, [0xff, 0xfe, b'\n']).unwrap();
        let named = binary.display().to_string();
        assert!(
            matches!(held_or_empty(&binary), Err(Error::Unreadable { path, .. }) if path == named),
            "a file that is not UTF-8 read as text"
        );
    }

    /// A `.gitignore` chock cannot read was read as empty and rewritten holding only chock's lines.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gitignore_chock_cannot_read_is_left_byte_for_byte_as_it_was() {
        let dir = crate::testdir::make("init-ignores-unreadable");
        let path = dir.join(".gitignore");
        let theirs = [b'/', b't', 0xff, b'\n'];
        fs::write(&path, theirs).unwrap();
        assert!(
            matches!(add_ignores_to(&path), Err(Error::Unreadable { .. })),
            "it was read"
        );
        assert_eq!(fs::read(&path).unwrap(), theirs);
    }

    #[test]
    fn recording_the_crap_baseline_raises_a_score_and_never_lowers_one() {
        let held = r#"{"entries":[
          {"file":"src/a.rs","function":"high","line":1,"crap":20.0},
          {"file":"src/a.rs","function":"gone","line":9,"crap":5.0}]}"#;
        let measured = r#"{"entries":[
          {"file":"src/a.rs","function":"high","line":3,"crap":6.0},
          {"file":"src/a.rs","function":"new","line":7,"crap":4.0}]}"#;
        assert_eq!(
            scores(held, measured),
            [("high".to_string(), 20.0), ("new".to_string(), 4.0)]
        );
    }

    /// What `crap_kept_higher` records for a project at `/w/proj`: each function and its score.
    fn scores(held: &str, measured: &str) -> Vec<(String, f64)> {
        let merged = crap_kept_higher(held, measured, Path::new("/w/proj")).unwrap();
        let merged: serde_json::Value = serde_json::from_str(&merged).unwrap();
        let scored = |e: &serde_json::Value| {
            let function = e["function"].as_str().unwrap().to_string();
            (function, e["crap"].as_f64().unwrap())
        };
        merged["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(scored)
            .collect()
    }

    /// One key for the two once gave both the floor of the one the record listed last.
    #[test]
    fn two_functions_of_one_name_in_one_file_each_keep_their_own_floor() {
        let held = r#"{"entries":[
          {"file":"src/a.rs","function":"T::from","line":10,"crap":20.0},
          {"file":"src/a.rs","function":"T::from","line":50,"crap":3.0}]}"#;
        let measured = r#"{"entries":[
          {"file":"src/a.rs","function":"T::from","line":12,"crap":6.0},
          {"file":"src/a.rs","function":"T::from","line":55,"crap":3.5}]}"#;
        assert_eq!(
            scores(held, measured),
            [("T::from".to_string(), 20.0), ("T::from".to_string(), 3.5)]
        );
    }

    /// Keyed by file and function: two crates may each hold a `read`, and a key of the name alone
    /// would hand one function's floor to the other.
    #[test]
    fn a_score_is_keyed_by_the_file_and_the_function_together() {
        let held = r#"{"entries":[{"file":"src/a.rs","function":"read","line":1,"crap":20.0}]}"#;
        let measured = r#"{"entries":[{"file":"src/b.rs","function":"read","line":1,"crap":3.0}]}"#;
        assert_eq!(scores(held, measured), [("read".to_string(), 3.0)]);
    }

    #[test]
    fn a_report_naming_absolute_paths_is_matched_against_a_record_holding_relative_ones() {
        let held = r#"{"entries":[{"file":"src/a.rs","function":"f","line":1,"crap":20.0}]}"#;
        let absolute =
            r#"{"entries":[{"file":"/w/proj/src/a.rs","function":"f","line":1,"crap":6.0}]}"#;
        assert_eq!(scores(held, absolute), [("f".to_string(), 20.0)]);
    }

    /// Refusing an empty record would leave a project unable to record a baseline at all.
    #[test]
    fn a_first_recording_with_nothing_held_keeps_every_score_it_measured() {
        let measured = r#"{"entries":[{"file":"src/a.rs","function":"f","line":1,"crap":9.0}]}"#;
        assert_eq!(scores("", measured), [("f".to_string(), 9.0)]);
    }

    #[test]
    fn a_recording_asked_to_lower_holds_no_score_to_the_record() {
        let held = r#"{"entries":[{"file":"src/a.rs","function":"f","line":1,"crap":20.0}]}"#;
        let measured = r#"{"entries":[{"file":"src/a.rs","function":"f","line":1,"crap":6.0}]}"#;
        assert_eq!(held_unless(false, held), held);
        assert_eq!(
            scores(held_unless(true, held), measured),
            [("f".to_string(), 6.0)]
        );
    }

    #[test]
    fn a_report_that_is_not_cargo_craps_is_not_recorded() {
        for report in ["not json", "{}"] {
            let refused = crap_kept_higher("", report, Path::new("/w/proj")).unwrap_err();
            assert!(
                refused.starts_with("cargo-crap produced a report chock cannot read"),
                "{refused}"
            );
        }
    }

    /// A suite reaching its own server over HTTP, or driving its own binary, has to instrument each
    /// of those processes; `cargo llvm-cov nextest` sees none of them and reports the code untested.
    #[cfg(unix)]
    #[test]
    fn a_project_keeping_its_own_coverage_script_is_found_without_being_asked() {
        let dir = crate::testdir::make("init-coverage-found");
        write_program(&dir.join("bin/coverage"));
        assert_eq!(
            coverage_command(&dir),
            Some(vec![
                "bin/coverage".to_string(),
                "--lcov".to_string(),
                "{lcov}".to_string()
            ])
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn an_ordinary_project_is_left_on_the_command_cargo_can_run_itself() {
        let dir = crate::testdir::make("init-coverage-default");
        assert_eq!(coverage_command(&dir), None);
    }

    /// A directory carries the execute bit too, and `bin/coverage` being one is not a script chock
    /// can run: spawning it fails in a way that reads as the project having no coverage command.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_directory_wearing_the_execute_bit_is_not_the_script() {
        let dir = crate::testdir::make("init-coverage-directory");
        fs::create_dir_all(dir.join("bin/coverage")).unwrap();
        assert_eq!(coverage_command(&dir), None);
    }

    /// A file of that name nobody can execute is not the script, and running it would fail in a way
    /// that reads as the project having no coverage rather than as chock guessing wrongly.
    #[cfg(unix)]
    #[test]
    fn a_coverage_path_that_is_not_executable_is_not_taken_for_the_script() {
        let dir = crate::testdir::make("init-coverage-not-a-program");
        let path = dir.join("bin/coverage");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "notes about coverage").unwrap();
        assert_eq!(coverage_command(&dir), None);
    }

    #[cfg(unix)]
    fn write_program(path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }

    /// A machine gets its tools before it has a project, or before `--local` wrote its pins.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_global_install_with_no_pin_file_takes_the_pins_chock_ships() {
        let dir = crate::testdir::make("init-global-no-pins");
        let shipped = Ok((SHIPPED.to_string(), PIN_FILE.to_string()));
        assert_eq!(pins_for(None), shipped);
        assert_eq!(pins_for(Some(&dir)), shipped);
    }

    /// The project's own pins come first, and a pin file chock cannot read stops the install.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_global_install_takes_the_projects_pins_and_stops_at_ones_it_cannot_read() {
        let dir = crate::testdir::make("init-global-own-pins");
        let path = dir.join(project::PIN_FILE);
        fs::write(&path, "CARGO_NEXTEST_VERSION=0.9.1\n").unwrap();
        let own = (
            path.display().to_string(),
            "CARGO_NEXTEST_VERSION=0.9.1\n".to_string(),
        );
        assert_eq!(pins_for(Some(&dir)), Ok(own));
        let blocked = crate::testdir::make("init-global-pins-unreadable");
        fs::create_dir_all(blocked.join(project::PIN_FILE)).unwrap();
        assert!(matches!(
            pins_for(Some(&blocked)),
            Err(Error::Unreadable { .. })
        ));
    }

    /// Writing under `data/` once collided with a project's own file of the same name.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_recorded_baseline_lands_in_chocks_own_directory() {
        let dir = crate::testdir::make("init-crap-baseline");
        assert!(write_crap(&dir, "{\"entries\":[]}", "recorded"));
        assert_eq!(written(&dir), serde_json::json!({"entries": []}));
    }

    /// The record `write_crap` left under `dir`.
    fn written(dir: &Path) -> serde_json::Value {
        let held = fs::read_to_string(dir.join(crate::gates::coverage::crap::baseline())).unwrap();
        serde_json::from_str(&held).unwrap()
    }

    /// A path with an absolute root in it matches nothing when the baseline is read back on another
    /// machine, and cargo-crap then falls back to name-only matching.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_recorded_baseline_holds_paths_relative_to_the_project() {
        let dir = crate::testdir::make("init-crap-relative");
        let file = dir.join("src").join("a.rs").display().to_string();
        let json = serde_json::json!({"file": file});
        assert!(write_crap(&dir, &json.to_string(), "recorded"));
        assert_eq!(written(&dir), serde_json::json!({"file": "src/a.rs"}));
    }

    /// `.chock` occupied by a file leaves nowhere to put the baseline, and reporting that as a
    /// recorded baseline would leave the gate comparing against a file that was never written.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_with_no_room_for_the_baseline_records_nothing() {
        let dir = crate::testdir::make("init-crap-no-room");
        fs::write(dir.join(".chock"), "not a directory").unwrap();
        assert!(!write_crap(&dir, "{}", "recorded"));
    }

    #[cfg(unix)]
    #[test]
    fn a_baseline_that_cannot_be_written_records_nothing() {
        let dir = crate::testdir::make("init-crap-unwritable");
        let inside = dir.join(".chock");
        fs::create_dir_all(&inside).unwrap();
        read_only(&inside, true);
        let wrote = write_crap(&dir, "{}", "recorded");
        read_only(&inside, false);
        assert!(!wrote);
    }

    #[cfg(unix)]
    fn read_only(dir: &Path, yes: bool) {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = if yes { 0o555 } else { 0o755 };
        let mut perms = fs::metadata(dir).unwrap().permissions();
        perms.set_mode(mode);
        fs::set_permissions(dir, perms).unwrap();
    }

    #[test]
    fn a_setup_line_splits_into_a_command_and_its_arguments() {
        assert_eq!(
            setup_command("cargo kani --version"),
            Some(("cargo", vec!["kani", "--version"]))
        );
        assert_eq!(setup_command("kani"), Some(("kani", vec![])));
    }

    #[test]
    fn an_empty_setup_line_names_no_command_to_run() {
        assert_eq!(setup_command("   "), None);
    }

    fn version_pinned(crate_name: &str, want: &str) -> Pin {
        Pin {
            key: format!("{}_VERSION", crate_name.to_uppercase().replace('-', "_")),
            crate_name: crate_name.to_string(),
            command: crate_name.to_string(),
            want: want.to_string(),
            setup: None,
            systems: None,
        }
    }

    /// What one command gives back.
    type Answer = Result<crate::exec::Output, crate::exec::ExecError>;

    /// A machine where each command ends at `stage` with no output.
    fn no_answer(
        stage: crate::exec::Stage,
    ) -> impl Fn(&str, &[&str], &Path, &[(&str, &str)]) -> Answer {
        move |program: &str, _: &[&str], _: &Path, _: &[(&str, &str)]| {
            Err(crate::exec::ExecError {
                program: program.to_string(),
                stage,
                reason: "no answer".to_string(),
            })
        }
    }

    #[test]
    fn an_install_that_is_no_crate_fetch_fails_only_as_an_err() {
        assert!(reported(&Ok("  current   miri".to_string())));
        assert!(!reported(&Err("  FAILED    miri".to_string())));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_global_install_names_the_fork_and_miri_when_neither_was_installed() {
        let fork = version_pinned(crate::setup::mutest::CRATE, pins::UNPUBLISHED);
        assert!(crate::setup::mutest::is_pin(&fork, std::env::consts::OS));
        let unanswered = no_answer(crate::exec::Stage::Wait);
        let failed = Error::NotInstalled(vec!["cargo-mutest".into(), "miri".into()]);
        assert_eq!(install_global(&[fork], &unanswered), Err(failed));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_tool_built_from_its_checkout_and_a_machine_without_rustup_fail_no_install() {
        let local = version_pinned("outpost", pins::UNPUBLISHED);
        let no_rustup = no_answer(crate::exec::Stage::Spawn);
        assert_eq!(install_global(&[local], &no_rustup), Ok(()));
    }

    fn pinned(setup: Option<&str>) -> Pin {
        Pin {
            key: "EXAMPLE_VERSION".to_string(),
            crate_name: "example".to_string(),
            command: "example".to_string(),
            want: "1.0.0".to_string(),
            setup: setup.map(str::to_string),
            systems: None,
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_setup_command_that_cannot_run_fails_the_install() {
        assert!(!run_setup(&pinned(Some("chock-no-such-command-exists"))));
    }

    #[test]
    fn a_setup_line_naming_no_command_fails_rather_than_being_skipped() {
        assert!(!run_setup(&pinned(Some("   "))));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_setup_command_that_succeeds_leaves_the_install_good() {
        assert!(run_setup(&pinned(Some("true"))));
    }

    /// A command that starts and then exits non-zero is the case that separates "could not run"
    /// from "ran and refused"; without it the success check can be dropped and nothing notices.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_setup_command_that_runs_and_fails_is_not_a_successful_install() {
        assert!(!run_setup(&pinned(Some("false"))));
    }

    #[test]
    fn a_tool_with_no_setup_line_needs_no_second_step() {
        assert!(run_setup(&pinned(None)));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn an_instrument_is_never_switched_on_however_green_the_tree_is() {
        let dir = crate::testdir::make("init-decide-instrument");
        let ctx = Ctx::for_root(dir.to_path_buf(), Baseline::empty("0.1.0"));
        assert_eq!(
            judge(&crate::gates::tools::BSIZE, &ctx).0,
            Decision::Reports
        );
        assert_eq!(
            judge(&crate::gates::tools::MUTEST, &ctx).0,
            Decision::AskedFor
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_measurable_ratchet_is_on_from_the_start_because_its_baseline_is_this_tree() {
        let dir = crate::testdir::make("init-decide-ratchet");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "// a\n// b\n// c\nfn f() {}\n").unwrap();
        let ctx = Ctx::for_root(dir.to_path_buf(), Baseline::empty("0.1.0"));
        assert_eq!(
            judge(&crate::gates::text::slop::GATE, &ctx).0,
            Decision::On("ratchet: 1 item today".to_string())
        );
    }

    #[test]
    fn no_flag_means_both_halves() {
        assert_eq!(
            scope_of(&[]),
            Ok(Scope {
                global: true,
                local: true,
                fast: false,
                help: false,
            })
        );
    }

    #[test]
    fn one_flag_means_only_that_half() {
        assert_eq!(
            scope_of(&["--global"]),
            Ok(Scope {
                global: true,
                local: false,
                fast: false,
                help: false,
            })
        );
        assert_eq!(
            scope_of(&["--local"]),
            Ok(Scope {
                global: false,
                local: true,
                fast: false,
                help: false,
            })
        );
    }

    #[test]
    fn both_flags_together_mean_both() {
        assert_eq!(
            scope_of(&["--global", "--local"]),
            Ok(Scope {
                global: true,
                local: true,
                fast: false,
                help: false,
            })
        );
    }

    #[test]
    fn an_unknown_flag_is_refused_by_name_rather_than_ignored() {
        assert_eq!(
            scope_of(&["--globl"]),
            Err(Error::UnknownOption("--globl".to_string()))
        );
    }

    #[test]
    fn each_outcome_says_what_a_reader_has_to_do_about_it() {
        assert_eq!(
            Written::Created("justfile".into()).to_string(),
            "  created   justfile"
        );
        assert_eq!(
            Written::Unchanged("justfile".into()).to_string(),
            "  unchanged justfile"
        );
        assert_eq!(
            Written::Conflict {
                name: "justfile".into(),
                kept: "justfile.chock".into()
            }
            .to_string(),
            "  conflict  justfile differs — wrote justfile.chock, merge it yourself"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_that_is_not_there_is_created_with_our_content() {
        let dir = crate::testdir::make("init-create");
        let made = install_file(&dir, "justfile", "recipe:\n");
        assert_eq!(made, Ok(Written::Created("justfile".to_string())));
        let text = std::fs::read_to_string(dir.join("justfile")).unwrap();
        assert_eq!(text, "recipe:\n");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_already_holding_our_content_is_left_exactly_as_it_was() {
        let dir = crate::testdir::make("init-unchanged");
        std::fs::write(dir.join("justfile"), "recipe:\n").unwrap();
        let kept = install_file(&dir, "justfile", "recipe:\n");
        assert_eq!(kept, Ok(Written::Unchanged("justfile".to_string())));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_holding_something_else_keeps_its_content_and_ours_lands_beside_it() {
        let dir = crate::testdir::make("init-conflict");
        std::fs::write(dir.join("justfile"), "theirs\n").unwrap();
        assert_eq!(
            install_file(&dir, "justfile", "ours\n"),
            Ok(Written::Conflict {
                name: "justfile".to_string(),
                kept: "justfile.chock".to_string()
            })
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("justfile")).unwrap(),
            "theirs\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("justfile.chock")).unwrap(),
            "ours\n"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_second_conflict_does_not_overwrite_the_first_unmerged_copy() {
        let dir = crate::testdir::make("init-conflict-twice");
        std::fs::write(dir.join("justfile"), "theirs\n").unwrap();
        std::fs::write(dir.join("justfile.chock"), "first upgrade\n").unwrap();
        assert_eq!(
            install_file(&dir, "justfile", "second upgrade\n"),
            Ok(Written::Conflict {
                name: "justfile".to_string(),
                kept: "justfile.chock.1".to_string()
            })
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("justfile.chock")).unwrap(),
            "first upgrade\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("justfile.chock.1")).unwrap(),
            "second upgrade\n"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_third_conflict_counts_past_the_copies_already_there() {
        let dir = crate::testdir::make("init-conflict-thrice");
        std::fs::write(dir.join("justfile"), "theirs\n").unwrap();
        std::fs::write(dir.join("justfile.chock"), "first\n").unwrap();
        std::fs::write(dir.join("justfile.chock.1"), "second\n").unwrap();
        assert_eq!(
            install_file(&dir, "justfile", "third\n"),
            Ok(Written::Conflict {
                name: "justfile".to_string(),
                kept: "justfile.chock.2".to_string()
            })
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("justfile.chock.2")).unwrap(),
            "third\n"
        );
    }

    #[test]
    fn every_error_says_what_went_wrong_and_which_path() {
        assert_eq!(
            Error::UnknownOption("--globl".into()).to_string(),
            "unknown option `--globl`"
        );
        assert_eq!(
            Error::NotARustProject.to_string(),
            "no Cargo.toml here or in any parent — chock installs into a Rust project"
        );
        assert_eq!(
            Error::Unreadable {
                path: "a/b".into(),
                reason: "no such file".into()
            }
            .to_string(),
            "cannot read a/b: no such file"
        );
        assert_eq!(
            Error::Unwritable {
                path: "a/b".into(),
                reason: "read-only".into()
            }
            .to_string(),
            "cannot write a/b: read-only"
        );
        assert_eq!(
            Error::NotInstalled(vec!["cargo-deny".into(), "kani".into()]).to_string(),
            "could not install: cargo-deny, kani"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_conflict_copy_already_holding_our_content_is_reused_so_a_re_run_adds_nothing() {
        let dir = crate::testdir::make("init-conflict-idempotent");
        std::fs::write(dir.join("justfile"), "theirs\n").unwrap();
        assert_eq!(
            install_file(&dir, "justfile", "ours\n"),
            Ok(Written::Conflict {
                name: "justfile".to_string(),
                kept: "justfile.chock".to_string()
            })
        );
        assert_eq!(
            install_file(&dir, "justfile", "ours\n"),
            Ok(Written::Conflict {
                name: "justfile".to_string(),
                kept: "justfile.chock".to_string()
            })
        );
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
            .collect();
        names.sort();
        assert_eq!(names, ["justfile", "justfile.chock"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_the_projects_content_and_is_never_written_through() {
        let dir = crate::testdir::make("init-symlink");
        let outside = dir.join("outside-the-project");
        std::os::unix::fs::symlink(&outside, dir.join("justfile")).unwrap();
        assert_eq!(
            install_file(&dir, "justfile", "ours\n"),
            Ok(Written::Conflict {
                name: "justfile".to_string(),
                kept: "justfile.chock".to_string()
            })
        );
        assert!(
            !outside.exists(),
            "wrote through the link to {}",
            outside.display()
        );
    }

    fn git_repo(name: &str) -> crate::testdir::Scratch {
        let dir = crate::testdir::make(name);
        let _ = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status();
        dir
    }

    #[test]
    fn binstall_takes_the_tools_own_release_and_a_compile_takes_the_crate_at_its_pin() {
        let pin = &pins::parse("CARGO_DENY_VERSION=0.20.2\n").unwrap()[0];
        assert_eq!(
            fetch_args(pin, true),
            [
                "binstall",
                "-y",
                "--locked",
                "--disable-strategies",
                "quick-install",
                "cargo-deny@0.20.2"
            ]
        );
        assert_eq!(
            fetch_args(pin, false),
            ["install", "cargo-deny", "--version", "0.20.2", "--locked"]
        );
    }

    #[test]
    fn a_tool_built_from_a_checkout_or_made_for_another_system_is_not_fetched() {
        let parsed = pins::parse(
            "CARGO_ACL_VERSION=0.9.0\nCARGO_ACL_OS=linux\nOUTPOST_VERSION=0.0.0\nJUST_VERSION=1.58.0\n",
        )
        .unwrap();
        let (acl, outpost, just) = (&parsed[0], &parsed[1], &parsed[2]);
        assert_eq!(not_fetched(acl, "linux"), None);
        assert_eq!(
            not_fetched(acl, "macos").as_deref(),
            Some("  skipped   cargo-acl — runs only on linux, so macos has no use for it")
        );
        assert_eq!(
            not_fetched(outpost, "linux").as_deref(),
            Some("  local     outpost — not on crates.io; build it from its checkout")
        );
        assert_eq!(not_fetched(just, "windows"), None);
    }

    #[test]
    fn an_older_chock_pin_never_replaces_the_chock_that_runs() {
        let running = env!("CARGO_PKG_VERSION");
        let kept = format!(
            "  kept      chock {running} — this project pins 0.0.1; `chock init --local` updates its \
             pins to this chock"
        );
        let older = version_pinned("chock", "0.0.1");
        assert_eq!(not_fetched(&older, "linux"), Some(kept));
        assert_eq!(
            not_fetched(&version_pinned("chock", running), "linux"),
            None
        );
        assert_eq!(
            not_fetched(&version_pinned("cargo-deny", "0.0.1"), "linux"),
            None
        );
    }

    #[test]
    fn each_tool_is_announced_before_it_starts_with_how_it_will_arrive() {
        let pin = &pins::parse("CARGO_DENY_VERSION=0.20.2\n").unwrap()[0];
        assert_eq!(
            starting(pin, true),
            "  installing cargo-deny 0.20.2 — its own prebuilt release where it has one, else compiled"
        );
        assert_eq!(
            starting(pin, false),
            "  installing cargo-deny 0.20.2 — compiled from crates.io"
        );
    }

    #[test]
    fn a_pin_that_is_not_a_crate_is_not_a_failed_install() {
        assert!(not_a_crate(
            "error: could not find `shellcheck` in registry `crates-io` with version `=0.11.0`\n"
        ));
        assert!(not_a_crate(
            "error: could not find `ruff` in registry `crates-io`\n"
        ));
    }

    /// A crate that exists and would not build is a real failure, and reading it as a missing crate
    /// would report a tool as skipped while nothing installed it.
    #[test]
    fn a_crate_that_failed_to_build_is_still_a_failure() {
        assert!(!not_a_crate("error[E0432]: unresolved import `foo::bar`\n"));
        assert!(!not_a_crate(
            "error: failed to compile `cargo-deny v0.20.2`\n"
        ));
        assert!(!not_a_crate(""));
    }

    #[test]
    fn the_fast_measurement_leaves_out_every_gate_that_needs_a_compiler() {
        for gate in gates::registry() {
            assert_eq!(measured(gate, true), !gate.builds, "{}", gate.name);
            assert!(measured(gate, false), "{}", gate.name);
        }
    }

    #[test]
    fn fast_names_neither_half_so_it_leaves_that_choice_alone() {
        assert_eq!(
            scope_of(&["--fast"]).unwrap(),
            Scope {
                global: true,
                local: true,
                fast: true,
                help: false,
            }
        );
        assert_eq!(
            scope_of(&["--local", "--fast"]).unwrap(),
            Scope {
                global: false,
                local: true,
                fast: true,
                help: false,
            }
        );
        assert_eq!(
            scope_of(&["--local"]).unwrap(),
            Scope {
                global: false,
                local: true,
                fast: false,
                help: false,
            }
        );
    }

    /// The `--global`/`--local` split holds that sharing executables makes two projects comparable.
    /// That needs them to agree on the version, so a change is worth naming even when scores hold.
    #[test]
    fn replacing_a_version_another_project_pins_is_said_out_loud() {
        let pin = version_pinned("cargo-crap", "0.5.0");
        let said = replacing(&pin, Some("0.4.3")).unwrap();
        assert_eq!(
            said,
            "  replacing cargo-crap 0.4.3 with 0.5.0, which every project on this machine then measures with"
        );
    }

    #[test]
    fn installing_the_version_already_there_replaces_nothing() {
        assert_eq!(
            replacing(&version_pinned("cargo-crap", "0.5.0"), Some("0.5.0")),
            None
        );
    }

    #[test]
    fn installing_a_tool_this_machine_does_not_have_replaces_nothing() {
        assert_eq!(
            replacing(&version_pinned("cargo-crap", "0.5.0"), None),
            None
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_ratchet_that_builds_is_switched_on_rather_than_built_twice() {
        let dir = crate::testdir::make("init-ratchet-builds");
        let building: Vec<&str> = crate::gates::registry()
            .iter()
            .filter(|gate| matches!(gate.group, Group::Gates | Group::Quality))
            .filter(|gate| gate.builds && switched_on_unmeasured(gate, &dir).is_none())
            .map(|gate| gate.name)
            .collect();
        for spared in ["binsize", "codeslop", "coverage"] {
            assert!(
                !building.contains(&spared),
                "{spared} still builds: {building:?}"
            );
        }
        // A pass/fail gate is still run, because enabling it means it passes on this tree.
        assert!(building.contains(&"test"), "{building:?}");
        let expected = format!("{} of them build", building.len());
        let said = about_to_measure(false, &dir);
        assert!(said.contains(&expected), "wanted `{expected}` in: {said}");
    }

    /// init names the cost first, so a caller can choose `--fast`, which must leave out every check
    /// that builds.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn what_the_measuring_will_cost_is_said_before_it_starts() {
        let dir = crate::testdir::make("init-measuring-cost");
        let full = about_to_measure(false, &dir);
        assert!(full.contains("build this project"), "{full}");
        assert!(full.contains("--fast"), "{full}");
        let quick = about_to_measure(true, &dir);
        assert!(quick.contains("none of them build it"), "{quick}");
        // The whole point of `--fast`: fewer checks, and none of the ones that compile.
        assert!(!quick.contains("--fast"), "{quick}");
    }

    /// `crap` keeps its own record, and a local run that judges it with none writes the first one.
    /// `init` writes no record, so it switches `crap` on without judging it.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_that_keeps_its_own_baseline_is_on_before_that_baseline_exists() {
        let dir = crate::testdir::make("init-own-baseline");
        let file = crate::gates::own_baseline("crap").unwrap();
        assert_eq!(file, crate::gates::coverage::crap::baseline());
        let ctx = crate::run::Ctx::for_root(dir.to_path_buf(), Baseline::empty("0.1.0"));
        let (decision, report) = judge(&crate::gates::coverage::crap::GATE, &ctx);
        assert_eq!(
            decision,
            Decision::On("its first record is what the first `chock run` writes".to_string())
        );
        assert!(
            decision.enables(),
            "the first `chock run` would then skip it"
        );
        assert!(report.is_none(), "it was built rather than switched on");
        // With the file there the exemption is over. Asserted on the rule and never by judging
        // `crap` again: judging a binary gate runs it, and crap runs coverage, which runs this suite.
        fs::create_dir_all(dir.join(".chock")).unwrap();
        fs::write(dir.join(&file), "[]").unwrap();
        assert!(!crate::gates::own_baseline("crap").is_some_and(|held| !dir.join(held).exists()));
    }

    /// A gate about chock's own wiring reads the config `init` has not written yet, so measuring it
    /// reported it missing and then left the gate off — telling the user to run what they just ran.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_about_chocks_own_wiring_is_switched_on_without_being_measured() {
        let setup: Vec<&str> = crate::gates::registry()
            .iter()
            .filter(|gate| gate.group == crate::run::Group::Setup)
            .map(|gate| gate.name)
            .collect();
        assert_eq!(setup, vec!["wiring"]);
        let dir = crate::testdir::make("init-setup-group");
        let ctx = crate::run::Ctx::for_root(dir.to_path_buf(), Baseline::empty("0.1.0"));
        let (decision, report) = judge(&crate::gates::wiring::GATE, &ctx);
        assert_eq!(decision, Decision::On(WRITTEN_NOW.to_string()));
        assert!(decision.enables(), "it would be left out of the config");
        assert!(report.is_none(), "it was run after all");
    }

    /// Every gate that reads the repository has to be measurable while init is choosing, or init
    /// leaves it out of the config and the project never learns the check exists.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn init_chooses_the_gates_that_read_the_repository() {
        let dir = crate::testdir::make("init-chooses-repo-gates");
        let done = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(done.success(), "{done:?}");
        let manifest = "[package]\nname = \"chosen\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
        fs::write(dir.join("Cargo.toml"), manifest).unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        let (config, said) = choose_gates(&dir, true);
        for gate in ["hygiene", "commits"] {
            assert!(
                config.enabled.contains(gate),
                "{gate} was not chosen:\n{said}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn selection_preserves_the_coverage_script_without_running_it() {
        let dir = git_repo("init-select-coverage");
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.0.0\"\n[workspace]\n",
        )
        .unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/lib.rs"), "pub fn answer() -> u8 { 1 }\n").unwrap();
        write_program(&dir.join("bin/coverage"));
        let (config, report) = choose_gates(&dir, true);
        assert_eq!(
            config.coverage,
            Some(vec![
                "bin/coverage".to_string(),
                "--lcov".to_string(),
                "{lcov}".to_string(),
            ])
        );
        assert!(report.contains("coverage  bin/coverage"), "{report}");
        assert!(!dir.join("lcov.info").exists());
    }

    /// Stands in for the real chooser, which would run every gate — including the one that runs
    /// this suite.
    fn stub_choice(_root: &Path) -> (Config, String) {
        (
            Config::of(["slop"]),
            "  on        slop         ratchet\n".to_string(),
        )
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_project_that_already_says_which_checks_are_on_is_never_measured() {
        let dir = crate::testdir::make("init-already-chosen");
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
        fs::create_dir_all(dir.join(".chock")).unwrap();
        let held = Config::of(["slop", "nesting"]);
        crate::project::document::write(&dir.join(crate::project::config::FILE), &held.render())
            .unwrap();

        // The chooser is what runs every gate against the tree, so a chooser that is never called
        // is a tree that was never measured.
        let measured = std::cell::Cell::new(false);
        let never = |at: &Path| {
            measured.set(true);
            stub_choice(at)
        };
        let report = write_local_with(&dir, &never).unwrap();
        assert!(!measured.get(), "the tree was measured:\n{report}");
        assert!(
            report.contains("already says which checks are on"),
            "{report}"
        );
        let after = fs::read_to_string(dir.join(crate::project::config::FILE)).unwrap();
        assert_eq!(
            crate::project::document::parse::<Config>(&after, "config")
                .unwrap()
                .enabled,
            held.enabled
        );
    }

    /// A config chock cannot read is not a decision it can keep, so the tree is measured past it
    /// rather than the run stopping on a file somebody hand-edited.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_config_that_cannot_be_read_is_measured_past() {
        let dir = crate::testdir::make("init-unreadable-config");
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
        fs::create_dir_all(dir.join(".chock")).unwrap();
        fs::write(dir.join(crate::project::config::FILE), "{ not a config").unwrap();
        let measured = std::cell::Cell::new(false);
        let counted = |at: &Path| {
            measured.set(true);
            stub_choice(at)
        };
        let report = write_local_with(&dir, &counted).unwrap();
        assert!(measured.get(), "the tree was not measured:\n{report}");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn write_local_reports_every_file_it_wrote_and_the_baselines_it_could_not() {
        let dir = crate::testdir::make("init-local");
        let report = write_local_with(&dir, &stub_choice).unwrap();
        assert_eq!(
            report,
            format!(
                "chock init --local: {}\n\
                 \x20 created   tool-versions.env\n\
                 \x20 created   justfile\n\
                 \x20 created   deny.toml\n\
                 \x20 created   .chock/config.json\n\
                 \x20 gitignore 9 lines added\n\
                 \x20 editor    .claude/settings.json runs chock on every edit\n\
                 \x20 note      an editor session already open may not fire it until it reloads\n\
                 \x20 created   .chock/agents.md\n\
                 \n\
                 Gates measured against this tree:\n\
                 \x20 on        slop         ratchet\n\
                 \n\
                 1 gate on. Next: `chock run` judges the tree and writes each gate's first \
                 record.\n",
                dir.display()
            )
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("justfile")).unwrap(),
            JUSTFILE
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(project::PIN_FILE)).unwrap(),
            local_pin_file(env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn write_local_in_a_git_repository_reports_the_hooks_too() {
        let dir = git_repo("init-local-git");
        let report = write_local_with(&dir, &stub_choice).unwrap();
        assert!(report.contains("declared"), "{report}");
        // Declared, so nothing on disk: the hook a repository runs is whichever chock is installed.
        assert!(!dir.join(".chock/hooks/pre-commit").exists(), "{report}");
    }

    #[test]
    fn the_embedded_template_is_this_project_s_own_justfile() {
        assert!(
            JUSTFILE.contains("\ndoctor:"),
            "the template lost the doctor recipe: {JUSTFILE}"
        );
        assert!(
            PIN_FILE.contains("CARGO_NEXTEST_VERSION="),
            "the template lost the nextest pin: {PIN_FILE}"
        );
    }

    #[test]
    fn a_manifest_missing_both_a_workspace_table_and_a_license_is_told_about_both() {
        let notes = manifest_notes_for("[package]\nname = \"x\"\n");
        assert_eq!(
            notes
                .iter()
                .map(|n| n.split_whitespace().take(4).collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>(),
            [
                "note no [workspace] table",
                "note no unsafe_code lint",
                "note no license field"
            ]
        );
    }

    #[test]
    fn a_manifest_with_both_draws_no_note() {
        assert_eq!(
            manifest_notes_for(
                "[package]\nlicense = \"MIT\"\n\n[lints.rust]\nunsafe_code = \"deny\"\n\n[workspace]\n"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_inherited_license_counts_as_declared() {
        assert_eq!(
            manifest_notes_for(
                "[package]\nlicense.workspace = true\nunsafe_code = \"deny\"\n\n[workspace]\n"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_license_file_counts_as_a_license() {
        assert_eq!(
            manifest_notes_for(
                "[package]\nlicense-file = \"LICENSE\"\nunsafe_code = \"deny\"\n\n[workspace]\n"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_virtual_workspace_root_with_no_package_draws_no_note() {
        assert_eq!(
            manifest_notes_for("[workspace]\nmembers = [\"a\"]\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_pin_file_a_project_gets_pins_chock_itself_last() {
        let text = local_pin_file("0.3.1");
        let parsed = pins::parse(&text).unwrap();
        let last = parsed.last().unwrap();
        assert_eq!(
            (
                last.key.as_str(),
                last.crate_name.as_str(),
                last.want.as_str()
            ),
            ("CHOCK_VERSION", "chock", "0.3.1")
        );
    }

    #[test]
    fn chocks_own_pin_file_does_not_pin_chock() {
        let parsed = pins::parse(PIN_FILE).unwrap();
        assert!(
            !parsed.iter().any(|p| p.crate_name == "chock"),
            "Cargo.toml is the source of truth here; a second copy would drift"
        );
    }

    #[test]
    fn the_embedded_pin_file_parses_by_the_same_parser_doctor_uses() {
        let parsed = pins::parse(PIN_FILE).unwrap();
        let commands: Vec<&str> = parsed.iter().map(|p| p.command.as_str()).collect();
        assert_eq!(commands, pins::SHIPPED);
    }

    /// Outpost reads only `.outpostignore`, so a git-only wiring leaves an outpost clone tracking
    /// chock's scratch files. A directory no repository holds gets git's, which is what a clone reads.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn every_ignore_file_the_holding_repositories_read_is_written() {
        let dir = crate::testdir::make("init-ignore-files");
        assert_eq!(ignore_files(&dir), vec![".gitignore"]);
        crate::project::vcs::pretend_outpost_holds(&dir);
        assert_eq!(ignore_files(&dir), vec![".outpostignore"]);
        fs::write(dir.join(".git"), "gitdir: elsewhere\n").unwrap();
        assert_eq!(ignore_files(&dir), vec![".gitignore", ".outpostignore"]);
        let said = add_ignores(&dir).unwrap();
        for file in [".gitignore", ".outpostignore"] {
            let written = fs::read_to_string(dir.join(file)).unwrap();
            assert!(written.contains("lcov.info"), "{file}: {written}");
        }
        assert!(said.contains("outpostignore 9 lines added"), "{said}");
    }

    /// `*` matches the empty string in a gitignore, so `*.chock` spells `.chock` and hides the whole
    /// directory — the config a clone needs, and this repository's own `.chock/agents.md`.
    #[test]
    fn no_ignore_entry_collapses_to_chocks_own_directory() {
        for entry in IGNORES {
            let collapsed = entry.replace('*', "");
            assert_ne!(
                collapsed, ".chock",
                "{entry} would hide every untracked file under .chock/"
            );
        }
        assert!(IGNORES.contains(&"?*.chock"), "{IGNORES:?}");
    }

    /// The third case moves the slashes: nothing is appended, and the file must come back byte
    /// for byte, with no newline added.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gitignore_gains_only_the_entries_it_lacks_and_is_rewritten_only_when_it_gains_one() {
        let all = "/target\nlcov.info\nkani-list.json\n.chock/last-run.json\n\
                   .chock/run.lock\n.chock/verdicts.json\n.chock/last-edit\nrustc-ice-*.txt\n\
                   ?*.chock\n";
        let already = "target/\n/lcov.info\nkani-list.json\n.chock/last-run.json\n\
                       .chock/run.lock\n.chock/verdicts.json\n.chock/last-edit\n\
                       rustc-ice-*.txt\n?*.chock";
        for (name, before, added, want) in [
            ("init-ignores-new", None, 9, all),
            ("init-ignores-bare-last-line", Some("/target"), 8, all),
            ("init-ignores-covered", Some(already), 0, already),
        ] {
            let dir = crate::testdir::make(name);
            let path = dir.join(".gitignore");
            if let Some(text) = before {
                fs::write(&path, text).unwrap();
            }
            let got = add_ignores_to(&path).unwrap();
            let after = fs::read_to_string(&path).unwrap();
            assert_eq!((got, after.as_str()), (added, want), "{name}");
        }
    }
}
