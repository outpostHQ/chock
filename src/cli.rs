//! What each command does, kept out of `main` so every branch is reachable from a test.

mod args;
mod tree;
use args::split_json;
pub use args::{Command, parse};

use std::path::PathBuf;
use std::process::ExitCode;

use crate::gates::{self, repo::commits};
use crate::project::{
    self,
    config::{Config, Stage},
    document, vcs,
};
use crate::run::{self, Ctx, baseline::Baseline, report::LAST_RUN, report::Run, report::Verdict};
use crate::setup::{doctor, init, pins};
use crate::usage::USAGE;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What `enable`, `disable` and `stage` change for each gate they name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change<'a> {
    On,
    /// Off, with the reason the config records in place of the default reason.
    Off(Option<&'a str>),
    Stage(Stage),
}

impl Change<'_> {
    /// Makes the change to one gate, and says what it set or that the gate had it already.
    fn apply(self, config: &mut Config, gate: &run::Gate) -> String {
        let (changed, done) = match self {
            Change::On => (config.enable(gate.name), "on".to_string()),
            Change::Off(why) => (config.disable(gate.name, why), "off".to_string()),
            Change::Stage(stage) => (
                config.place(gate.name, gate.builds, stage),
                format!("runs at {}", stage.name()),
            ),
        };
        if changed {
            done
        } else {
            format!("already {done}")
        }
    }

    /// The line after the change: where to look next, or that nothing enforces the gate.
    fn advice(self, names: &[&str]) -> String {
        match self {
            Change::On => format!("`chock run {}` shows the findings now.\n", names.join(" ")),
            Change::Stage(Stage::Manual) => {
                "No hook and no CI job runs a manual gate; only `chock run` does.\n".to_string()
            }
            Change::Off(_) | Change::Stage(_) => String::new(),
        }
    }
}

pub fn dispatch(args: &[&str]) -> ExitCode {
    match parse(args) {
        Command::Run {
            names,
            json,
            fast,
            ci,
            no_cache,
            miri_part,
            skip,
        } => {
            let how = How {
                json,
                no_cache,
                hook: false,
                miri_part,
                skip: &skip,
            };
            ExitCode::from(run_gates_code(&names, tier_for(fast, ci), how))
        }
        Command::CacheClear => clear_cache(),
        Command::Gates { json } => list_gates(json),
        Command::Configure { names, change } => configure(&names, change),
        Command::Explain { name, json } => explain_or_owe(name, json),
        Command::Baseline(names) => record_baseline(&names),
        Command::Doctor { json } => run_doctor(json),
        Command::Survey { json } => run_survey(json),
        Command::Message(file) => check_message(file),
        Command::Hook(args) => run_hook(args),
        Command::Edited(rest) => run_edited(rest),
        Command::Tree(name, rest) => tree::reads(name, rest),
        Command::Init(rest) => init::run(rest),
        Command::Print(text) => {
            emit(text);
            ExitCode::SUCCESS
        }
        Command::Usage(problem) => usage(&problem),
    }
}

/// Ignores a closed pipe, as in `chock gates --json | head`, where `println!` would panic.
fn emit(text: &str) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}

fn usage(problem: &str) -> ExitCode {
    eprintln!("chock: {problem}\n\n{USAGE}");
    ExitCode::from(2)
}

fn cannot_run(message: &str) -> ExitCode {
    ExitCode::from(refused(message))
}

/// The refusal as a `u8`, for callers that return a code.
fn refused(message: &str) -> u8 {
    eprintln!("chock: {message}");
    2
}

/// Every command except `edited` and the ones `tree` runs needs the project root.
fn root() -> Result<PathBuf, String> {
    project::here()
}

/// The config, or none off a project or before `init`. A file that does not read is an error,
/// so no command takes the defaults in its place.
fn configured() -> Result<Option<Config>, String> {
    root()
        .ok()
        .map(|root| document::read::<Config>(&root))
        .transpose()
        .map(Option::flatten)
        .map_err(|e| e.to_string())
}

fn context(ci: bool) -> Result<(Ctx, Option<Config>), String> {
    let root = root()?;
    let baseline = document::read::<Baseline>(&root)
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| Baseline::empty(VERSION));
    let config = document::read::<Config>(&root).map_err(|e| e.to_string())?;
    // A literal for the root: Outpost's taint scan cannot follow it through a constructor.
    let ctx = Ctx {
        vcs: vcs::holding(&root, config.as_ref()),
        root,
        baseline,
        ci,
        ..Ctx::from_config(config.as_ref())
    };
    crate::exec::budget::prime(ctx.jobs);
    Ok((ctx, config))
}

/// What git asked a hook for, kept apart from running it so a test needs no build.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Asked<'a> {
    /// Every check a commit can afford to wait for.
    Committing,
    /// Everything, the compiler-bound checks included.
    Pushing,
    Message(&'a str),
    /// The message git is composing, named by git rather than handed over.
    MessageGitIsComposing,
    Unnamed,
    Unknown(&'a str),
}

/// Git 2.55 passes a declared `commit-msg` no arguments and `pre-push` the remote and its URL.
/// Only the hook name decides.
pub(crate) fn asked<'a>(args: &[&'a str]) -> Asked<'a> {
    match args {
        ["pre-commit", ..] => Asked::Committing,
        ["pre-push", ..] => Asked::Pushing,
        ["commit-msg", file, ..] => Asked::Message(file),
        ["commit-msg"] => Asked::MessageGitIsComposing,
        [] => Asked::Unnamed,
        [name, ..] => Asked::Unknown(name),
    }
}

/// Which layer asks. Each runs the gates staged at it or before it, and `chock run` runs them all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    Committing,
    Pushing,
    /// Everything except checks whose tool CI cannot install. `fast` is CI's commit-set job.
    Ci {
        fast: bool,
    },
    Everything,
}

impl Tier {
    /// Only CI lacks the tools that `local_only` names, in both of its jobs.
    fn meets_an_absent_tool(self) -> bool {
        matches!(self, Tier::Ci { .. })
    }

    /// The last stage this tier runs. CI's fast job mirrors the commit.
    fn reach(self) -> Stage {
        match self {
            Tier::Committing | Tier::Ci { fast: true } => Stage::Commit,
            Tier::Pushing => Stage::Push,
            Tier::Ci { fast: false } => Stage::Ci,
            Tier::Everything => Stage::Manual,
        }
    }
}

/// `--fast` selects the commit set. A separate fn, so a test can assert the mapping.
#[must_use]
pub(crate) fn tier_for(fast: bool, ci: bool) -> Tier {
    match (fast, ci) {
        (fast, true) => Tier::Ci { fast },
        (true, false) => Tier::Committing,
        (false, false) => Tier::Everything,
    }
}

/// Runs the gates for one hook, then one line that says where to look.
fn run_hook(args: &[&str]) -> ExitCode {
    let (args, message) = match in_project(args) {
        Ok(split) => split,
        Err(why) => return cannot_run(&why),
    };
    match asked(&args) {
        Asked::Committing => hooked(Tier::Committing, "pre-commit", EXPLAIN),
        Asked::Pushing => hooked(Tier::Pushing, "pre-push", NO_VERIFY),
        Asked::Message(file) => check_message(message.as_deref().unwrap_or(file)),
        Asked::MessageGitIsComposing => {
            // A hook runs in the tree it gates, so the current directory is the fallback root.
            let here = root().unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());
            match vcs::message_being_composed(&here) {
                Some(path) => check_message(&path.to_string_lossy()),
                None => cannot_run(
                    "chock hook commit-msg was given no file and git names no message being \
                     composed, so there is nothing to check",
                ),
            }
        }
        Asked::Unnamed => {
            cannot_run("chock hook needs a hook name: pre-commit, pre-push or commit-msg")
        }
        Asked::Unknown(name) => cannot_run(&format!("chock hook does not run `{name}`")),
    }
}

/// The hook's arguments once it has moved into the nearest `<ancestor>/<dir>` holding a
/// `Cargo.toml`, and the message file made whole first, since git names it from the top.
fn in_project<'a>(args: &[&'a str]) -> Result<(Vec<&'a str>, Option<String>), String> {
    let (args, project) = project_named(args)?;
    let Some(dir) = project else {
        return Ok((args, None));
    };
    let message = match asked(&args) {
        Asked::Message(file) => std::path::absolute(file).ok(),
        _ => None,
    };
    let here = std::env::current_dir().map_err(|e| format!("chock hook: {e}"))?;
    let found = here
        .ancestors()
        .map(|at| at.join(dir))
        .find(|at| at.join("Cargo.toml").is_file())
        .ok_or_else(|| {
            format!(
                "chock hook --project {dir}: no Cargo.toml there, from {}",
                here.display()
            )
        })?;
    std::env::set_current_dir(&found)
        .map_err(|e| format!("cannot enter {}: {e}", found.display()))?;
    Ok((args, message.map(|path| path.display().to_string())))
}

/// A hook's arguments without `--project <dir>`, which a hook declared at the top of a repository
/// passes for a project below it, and that directory.
pub(crate) fn project_named<'a>(
    args: &[&'a str],
) -> Result<(Vec<&'a str>, Option<&'a str>), String> {
    let (mut rest, mut project) = (Vec::new(), None);
    let mut each = args.iter().copied();
    while let Some(arg) = each.next() {
        match arg {
            "--project" => {
                project = Some(
                    each.next()
                        .ok_or("chock hook --project needs a directory")?,
                );
            }
            _ => rest.push(arg),
        }
    }
    Ok((rest, project))
}

fn hooked(tier: Tier, when: &str, fix: &str) -> ExitCode {
    let how = How {
        hook: true,
        ..How::default()
    };
    gated(run_gates_code(&[], tier, how), when, fix)
}

const EXPLAIN: &str = "Run `chock explain <gate>` for the findings of one.";
const NO_VERIFY: &str = "Fix it, or push with --no-verify and say why.";

/// Git blocks on any non-zero code, so keep chock's code: 2 still means nothing was measured.
fn gated(code: u8, when: &str, fix: &str) -> ExitCode {
    if code != 0 {
        eprintln!("{when}: see the line above. {fix}");
    }
    ExitCode::from(code)
}

/// How a run was asked for, beyond the gates it names and the tier that asks.
#[derive(Debug, Clone, Copy, Default)]
struct How<'a> {
    json: bool,
    /// A git hook asked, for which no gate staged is not a refusal.
    hook: bool,
    /// Every gate is judged again; no kept verdict answers.
    no_cache: bool,
    /// The part of the Miri suite this run takes; `None` is all of it.
    miri_part: Option<crate::gates::tools::miri::Part>,
    /// Gates left out, though the tier and the config would run them.
    skip: &'a [&'a str],
}

fn run_gates_code(names: &[&str], tier: Tier, how: How<'_>) -> u8 {
    let (ctx, config) = match context(tier.meets_an_absent_tool()) {
        Ok((ctx, config)) => (
            Ctx {
                no_cache: how.no_cache,
                miri_part: how.miri_part,
                ..ctx
            },
            config,
        ),
        Err(e) => return refused(&e),
    };
    // Two runs in one tree would write the same coverage report and run the same suite.
    let _held = match crate::run::lock::take(&ctx.root) {
        Ok(held) => held,
        Err(e) => return refused(&e),
    };
    let chosen = match skipping(selection(names, tier, config.as_ref()), how.skip) {
        Ok(gates) => gates,
        Err(e) => return refused(&e),
    };
    if chosen.is_empty() {
        return no_work(how.hook);
    }
    let result = run::run_all(&chosen, &ctx, VERSION, &|done| {
        eprintln!("{}", done.progress())
    });
    emit_run(&result, how.json);
    run::report::settle(&ctx.root, &ctx.baseline, &result.gates, VERSION);
    let code = result.verdict().code();
    crate::run::evidence::record(&ctx.root, &invocation(names), code);
    code
}

/// `chock cache clear`. It takes the run lock, since a run in this tree keeps a verdict as each
/// gate ends.
fn clear_cache() -> ExitCode {
    let kept = match cleared() {
        Ok(kept) => kept,
        Err(e) => return cannot_run(&e),
    };
    emit(&format!(
        "chock: removed {kept} kept verdict(s); the next run judges every gate again\n"
    ));
    ExitCode::SUCCESS
}

/// The count of kept verdicts removed, with the run lock held.
fn cleared() -> Result<usize, String> {
    let root = root()?;
    let _held = crate::run::lock::take(&root)?;
    crate::run::verdicts::clear(&root)
        .map_err(|e| format!("cannot remove {}: {e}", crate::run::verdicts::FILE))
}

fn emit_run(result: &Run, json: bool) {
    emit(&if json {
        result.render_json()
    } else {
        result.render()
    });
}

fn no_work(hook: bool) -> u8 {
    if hook {
        eprintln!("chock: no gate is assigned to this hook; nothing was measured");
        return 0;
    }
    refused(
        "no gate is left after --fast/--ci, --skip and the configured `stage`; nothing was measured",
    )
}

/// The gates left after `--skip`; a name that is not a gate is refused like any other typo.
fn skipping(
    kept: Result<Vec<&'static run::Gate>, String>,
    skip: &[&str],
) -> Result<Vec<&'static run::Gate>, String> {
    let kept = kept?;
    if let Some(name) = skip.iter().find(|name| gates::find(name).is_none()) {
        return Err(unknown_gate(name));
    }
    Ok(kept
        .into_iter()
        .filter(|gate| !skip.contains(&gate.name))
        .collect())
}

/// Refuses when a filter drops a gate the caller named.
fn selection(
    names: &[&str],
    tier: Tier,
    config: Option<&Config>,
) -> Result<Vec<&'static run::Gate>, String> {
    let (kept, elsewhere) = here(for_tier(select(names, config)?, tier, config), OS);
    elsewhere.iter().for_each(|said| eprintln!("{said}"));
    let excluded: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| !kept.iter().any(|gate| gate.name == *name))
        .collect();
    if !excluded.is_empty() {
        return Err(format!(
            "--fast/--ci, the configured `stage` or this system leaves out what you named: {}; run them without that filter, or on a system their tool runs on",
            excluded.join(", ")
        ));
    }
    Ok(kept)
}

const OS: &str = std::env::consts::OS;

/// The gates `os` can run, and a line naming the rest, which a Linux run enforces.
fn here(chosen: Vec<&'static run::Gate>, os: &str) -> (Vec<&'static run::Gate>, Option<String>) {
    let (kept, elsewhere): (Vec<_>, Vec<_>) = chosen
        .into_iter()
        .partition(|gate| gates::runs_on(gate.name, os));
    let names: Vec<&str> = elsewhere.iter().map(|gate| gate.name).collect();
    let said = format!(
        "chock: left out on {os}, where its tool does not run: {}; a Linux run enforces it",
        names.join(", ")
    );
    (kept, (!names.is_empty()).then_some(said))
}

/// Writes nothing: a survey is pointed at trees chock does not own.
fn run_survey(json: bool) -> ExitCode {
    let (ctx, _config) = match context(false) {
        Ok(pair) => pair,
        Err(e) => return ExitCode::from(refused(&e)),
    };
    let result = run::survey_all(&gates::surveyable(), &ctx, VERSION);
    emit_run(&result, json);
    ExitCode::from(result.verdict().code())
}

/// The gates this tier runs: those staged at it or before it, less the tools CI cannot install.
#[must_use]
fn for_tier(
    kept: Vec<&'static run::Gate>,
    tier: Tier,
    config: Option<&Config>,
) -> Vec<&'static run::Gate> {
    let absent = config
        .and_then(|set| set.local_only.as_ref())
        .filter(|_| tier.meets_an_absent_tool());
    kept.into_iter()
        .filter(|gate| stage_of(gate, config) <= tier.reach())
        .filter(|gate| !absent.is_some_and(|names| names.iter().any(|name| name == gate.name)))
        .collect()
}

fn stage_of(gate: &run::Gate, config: Option<&Config>) -> Stage {
    config.map_or(Stage::default_for(gate.builds), |set| {
        set.stage_of(gate.name, gate.builds)
    })
}

/// A name that is not a gate is a typo, refused rather than run as an empty set.
fn select(names: &[&str], config: Option<&Config>) -> Result<Vec<&'static run::Gate>, String> {
    config.map_or(Ok(()), held_where_touched)?;
    if names.is_empty() {
        return all_on(config);
    }
    names
        .iter()
        .map(|name| gates::find(name).ok_or_else(|| unknown_gate(name)))
        .collect()
}

/// With no name given: the gates the config switches on, or the enforced ones where there is none.
fn all_on(config: Option<&Config>) -> Result<Vec<&'static run::Gate>, String> {
    match config {
        Some(config) => enabled_by(config),
        None => Ok(gates::enforced()),
    }
}

fn unknown_gate(name: &str) -> String {
    let known: Vec<&str> = gates::registry().iter().map(|g| g.name).collect();
    format!("no gate named `{name}`. There is: {}", known.join(", "))
}

/// Registry order, not config order, so a cheap failure always reports first.
fn enabled_by(config: &Config) -> Result<Vec<&'static run::Gate>, String> {
    let known: Vec<&str> = gates::registry().iter().map(|g| g.name).collect();
    let stale = config.unknown(&known);
    if !stale.is_empty() {
        return Err(format!(
            "{} names what is no gate: {}",
            project::config::FILE,
            stale.join(", ")
        ));
    }
    Ok(gates::registry()
        .iter()
        .filter(|gate| config.is_on(gate.name))
        .collect())
}

/// Refuses a config that lists under `clean_when_touched` a gate with no number for each file:
/// the key would do nothing for that gate, and say nothing.
fn held_where_touched(config: &Config) -> Result<(), String> {
    let unheld = config.unheld_where_touched(&gates::holds_each_file);
    if unheld.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} lists under `clean_when_touched` what keeps no number for each file: {}",
        project::config::FILE,
        unheld.join(", ")
    ))
}

/// Changes each named gate in the config, says what changed, and writes the config back. It
/// refuses an unknown name before it writes.
fn reconfigure(names: &[&str], change: Change) -> Result<(), ExitCode> {
    let root = root().map_err(|e| cannot_run(&e))?;
    let mut config = document::read::<Config>(&root)
        .map_err(|e| cannot_run(&e.to_string()))?
        .ok_or_else(|| {
            cannot_run(&format!(
                "no {} — run `chock init --local` first",
                project::config::FILE
            ))
        })?;
    let named = names
        .iter()
        .map(|name| gates::find(name).ok_or_else(|| cannot_run(&unknown_gate(name))))
        .collect::<Result<Vec<_>, _>>()?;
    for gate in named {
        let done = change.apply(&mut config, gate);
        emit(&format!("  {:<12} {done}\n", gate.name));
    }
    let path = root.join(project::config::FILE);
    let was = std::fs::read_to_string(&path).unwrap_or_default();
    document::write(&path, &document::rewrite(&was, &config))
        .map_err(|e| cannot_run(&format!("cannot write {}: {e}", path.display())))
}

/// Switches gates on or off, or moves them to a stage, then says what to do next.
fn configure(names: &[&str], change: Change) -> ExitCode {
    if let Err(code) = reconfigure(names, change) {
        return code;
    }
    emit(&change.advice(names));
    ExitCode::SUCCESS
}

/// Checks a commit message before the commit exists, while a fix costs nothing.
fn check_message(file: &str) -> ExitCode {
    let (stored, limits) = match message_and_limits(file) {
        Ok(read) => read,
        Err(e) => return cannot_run(&e),
    };
    let found = commits::faults("this message", stored.trim(), limits);
    if found.is_empty() {
        return ExitCode::SUCCESS;
    }
    for finding in &found {
        eprintln!("  {}", finding.render());
    }
    ExitCode::from(1)
}

/// The message as git stores it, and the limits the project sets for the commit-msg hook.
fn message_and_limits(file: &str) -> Result<(String, commits::Limits), String> {
    let message = std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?;
    // Drop the comment lines git strips, so the check reads the stored message.
    let stored: String = message
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let limits = commits::Limits::of(configured()?.and_then(|set| set.message));
    Ok((stored, limits))
}

/// Returns `false` when it records nothing, so the caller can say so. `crap` keeps its own file.
fn record_one(gate: &run::Gate, ctx: &run::Ctx, into: &mut Baseline, lower: bool) -> bool {
    if gate.name == crate::gates::coverage::crap::GATE.name {
        return crate::setup::init::record_crap(ctx, lower, kept_as(lower));
    }
    let read = match run::measured(gate, ctx) {
        Ok(read) => read,
        Err(reason) => {
            eprintln!("{}", run::report::skipped(gate.name, &reason));
            return false;
        }
    };
    eprint!("{}", read.notes(gate.name));
    let series = read.series;
    if let Err(why) = run::portable(&series) {
        eprintln!("{}", run::report::skipped(gate.name, &why));
        return false;
    }
    emit(&format!(
        "  {:<9} {:<12} {}\n",
        kept_as(lower),
        gate.name,
        run::report::plural(series.len(), "item")
    ));
    into.keep(gate.name, unit_of(gate), series, lower);
    true
}

fn kept_as(lower: bool) -> &'static str {
    if lower { "lowered" } else { "recorded" }
}

/// `Baseline::record` needs a unit; a pass/fail check gives its kind, which no ratchet uses.
fn unit_of(gate: &run::Gate) -> &'static str {
    gate.counts_in().unwrap_or("pass or fail")
}

/// The command as typed, so a reader of the record can re-run it.
fn invocation(names: &[&str]) -> String {
    let mut spelled = "chock run".to_string();
    for name in names {
        spelled.push(' ');
        spelled.push_str(name);
    }
    spelled
}

fn list_gates(json: bool) -> ExitCode {
    // Off a project or with no config, list the default set as on, as a run would.
    match configured() {
        Ok(config) => listed(json, config.as_ref()),
        Err(e) => cannot_run(&e),
    }
}

/// Each gate with its state, group, kind, stage and what it checks.
fn listed(json: bool, config: Option<&Config>) -> ExitCode {
    if json {
        let rows: Vec<serde_json::Value> = gates::registry()
            .iter()
            .map(|gate| {
                serde_json::json!({
                    "gate": gate.name,
                    "about": gate.about,
                    "group": format!("{:?}", gate.group).to_lowercase(),
                    "ratchet": gate.counts_in().is_some(),
                    "enabled": switched_on(gate, config),
                    "stage": stage_of(gate, config).name(),
                    "rerun": run::rerun(gate.name),
                })
            })
            .collect();
        let doc = serde_json::json!({ "chock": VERSION, "gates": rows });
        emit(&format!(
            "{}\n",
            serde_json::to_string_pretty(&doc).unwrap_or_default()
        ));
        return ExitCode::SUCCESS;
    }
    for gate in gates::registry() {
        let kind = if gate.counts_in().is_some() {
            "ratchet"
        } else {
            "pass/fail"
        };
        emit(&format!(
            "  {:<4} {:<12} {:<10} {:<9} {:<6} {}\n",
            if switched_on(gate, config) {
                "on"
            } else {
                "off"
            },
            gate.name,
            format!("{:?}", gate.group).to_lowercase(),
            kind,
            stage_of(gate, config).name(),
            gate.about
        ));
    }
    ExitCode::SUCCESS
}

/// Whether `chock run` would include this gate. Without a config, a run uses the enforced set.
fn switched_on(gate: &run::Gate, config: Option<&Config>) -> bool {
    match config {
        Some(config) => config.is_on(gate.name),
        None => gates::enforced().iter().any(|held| held.name == gate.name),
    }
}

fn explain_or_owe(name: Option<&str>, json: bool) -> ExitCode {
    let Some(name) = name else {
        return match root().and_then(|root| run::debt::shown(&root, json)) {
            Ok(said) => {
                emit(&said);
                ExitCode::SUCCESS
            }
            Err(e) => cannot_run(&e),
        };
    };
    explain(name)
}

fn explain(name: &str) -> ExitCode {
    if gates::find(name).is_none() {
        return cannot_run(&unknown_gate(name));
    }
    let root = match root() {
        Ok(r) => r,
        Err(e) => return cannot_run(&e),
    };
    let path = root.join(LAST_RUN);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return cannot_run(&format!(
            "no record of a run at {} — run `chock run` first",
            path.display()
        ));
    };
    let Ok(last) = serde_json::from_str::<Run>(&text) else {
        return cannot_run(&format!("{} is not a chock run record", path.display()));
    };
    let Some(report) = last.gates.iter().find(|g| g.gate == name) else {
        return cannot_run(&format!("the last run did not include `{name}`"));
    };
    emit(&format!("{}\n", report.summary()));
    // How long ago this gate ran, so nobody reads an old report as a fresh pass.
    if let Some(ago) = ago(report.ran_at, std::time::SystemTime::now()) {
        emit(&format!("  measured {ago}\n"));
    }
    for line in report.explained(&root) {
        emit(&format!("  {line}\n"));
    }
    emit(&crate::edited::told(&crate::edited::behind_held(
        &root, report,
    )));
    emit(&format!("\nre-run with: {}\n", report.rerun));
    ExitCode::from(report.verdict.code())
}

/// `now` is a parameter so a test can fix it. A record from a clock that is ahead gives `None`.
fn ago(ran_at: Option<u64>, now: std::time::SystemTime) -> Option<String> {
    let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(ran_at?);
    Some(crate::setup::doctor::how_long(now.duration_since(at).ok()?))
}

/// Records without judging: a baseline from a failing tree is debt accepted in a readable commit.
fn record_baseline(asked: &[&str]) -> ExitCode {
    let lower = asked.contains(&"--lower");
    let names: Vec<&str> = asked.iter().copied().filter(|a| *a != "--lower").collect();
    let (ctx, config) = match context(false) {
        // A record holds the whole tree, never one change.
        Ok((ctx, config)) => (run::Ctx { whole: true, ..ctx }, config),
        Err(e) => return cannot_run(&e),
    };
    // Recording measures the tree too, so it takes the same lock a run does.
    let _held = match crate::run::lock::take(&ctx.root) {
        Ok(held) => held,
        Err(e) => return cannot_run(&e),
    };
    let chosen: Vec<&run::Gate> = if names.is_empty() {
        recordable(config.as_ref())
    } else {
        match select(&names, config.as_ref()) {
            Ok(gates) => gates,
            Err(e) => return cannot_run(&e),
        }
    };
    let mut recorded = ctx.baseline.clone();
    let mut failed = Vec::new();
    for gate in &chosen {
        if !record_one(gate, &ctx, &mut recorded, lower) {
            failed.push(gate.name);
        }
    }
    recorded.chock = VERSION.to_string();
    let dir = ctx.root.join(".chock");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return cannot_run(&format!("cannot create {}: {e}", dir.display()));
    }
    let path = ctx.root.join(crate::run::baseline::FILE);
    if let Err(e) = document::write(&path, &document::render(&recorded)) {
        return cannot_run(&format!("cannot write {}: {e}", path.display()));
    }
    emit(&format!("wrote {}\n", crate::run::baseline::FILE));
    if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "chock: could not measure {}: {}",
            run::report::plural(failed.len(), "gate"),
            failed.join(", ")
        );
        ExitCode::from(2)
    }
}

/// The enabled gates that record a number. `crap` records one but is not a `Kind::Ratchet`.
fn recordable(config: Option<&Config>) -> Vec<&'static run::Gate> {
    let mut gates = gates::ratchets();
    gates.push(&crate::gates::coverage::crap::GATE);
    if let Some(config) = config {
        gates.retain(|gate| config.is_on(gate.name));
    }
    gates
}

fn run_doctor(json: bool) -> ExitCode {
    let root = match root() {
        Ok(r) => r,
        Err(e) => return cannot_run(&e),
    };
    let pin_file = root.join(project::PIN_FILE);
    let text = match std::fs::read_to_string(&pin_file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("chock: cannot read {}: {e}", pin_file.display());
            eprintln!("       run `chock init --local` to write one");
            return ExitCode::from(2);
        }
    };
    let (pins, extra) = match pins::parse(&text)
        .and_then(|pins| pins::additional(&text).map(|extra| (pins, extra)))
    {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("chock: {}: {e}", pin_file.display());
            return ExitCode::from(2);
        }
    };
    let rows = doctor::gathered(&root, &pins, &extra);
    if json {
        emit(&format!("{}\n", doctor::render_json(&rows, VERSION)));
    } else {
        emit(&doctor::render(&rows));
    }
    ExitCode::from(doctor::verdict(&rows).code())
}

/// Checks single files for an edit hook. No baseline and no build: each rule reads only the file.
fn run_edited(args: &[&str]) -> ExitCode {
    let (rest, json) = split_json(args);
    let paths = match named(&rest) {
        Ok(paths) if paths.is_empty() => return ExitCode::SUCCESS,
        Ok(paths) => paths,
        Err(code) => return code,
    };
    let hook = rest.first() == Some(&"--hook");
    let mut found = Vec::new();
    for path in &paths {
        let root = crate::edited::project_of(path);
        // A hook skips a file outside an adopted project; a call by hand checks it.
        if hook && !root.as_deref().is_some_and(crate::edited::adopted) {
            continue;
        }
        // Record each firing, so `doctor` can tell "never fired" from "found nothing".
        if let Some(root) = root.as_deref().filter(|root| crate::edited::adopted(root)) {
            crate::edited::note_firing(root);
        }
        let rules = crate::edited::rules_for(root.as_deref(), path);
        match std::fs::read_to_string(path) {
            Ok(src) => found.extend(crate::edited::faults(path, &src, &rules)),
            // A file the hook named and chock cannot open is the hook's problem, not the file's.
            Err(e) => return cannot_run(&format!("cannot read {path}: {e}")),
        }
    }
    answered(&found, json, hook)
}

/// A hook's findings go to stderr with exit 2, the only output Claude Code gives the model.
fn answered(found: &[crate::run::report::Finding], json: bool, hook: bool) -> ExitCode {
    if json {
        let report = crate::run::report::Run::new(VERSION, vec![crate::edited::report(found)]);
        emit(&format!("{}\n", report.render_json()));
    } else if hook && !found.is_empty() {
        eprint!("{}", crate::edited::told(found));
        return ExitCode::from(2);
    } else {
        emit(&crate::edited::told(found));
    }
    if found.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(Verdict::Tripped.code())
    }
}

/// The files to read. `--hook` reads the editor's tool JSON from stdin, so a hook needs no `jq`.
fn named(rest: &[&str]) -> Result<Vec<String>, ExitCode> {
    if rest.first() != Some(&"--hook") {
        return match rest {
            [] => Err(usage("edited takes one or more paths, or --hook")),
            paths => Ok(paths.iter().map(|p| (*p).to_string()).collect()),
        };
    }
    let mut tool_json = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut tool_json).is_err() {
        return Err(cannot_run("cannot read the hook's JSON from stdin"));
    }
    // A write chock has no rule for is not a failure of the write; a hook that named no file is.
    match crate::edited::edited_path(&tool_json) {
        Ok(path) => Ok(path.into_iter().collect()),
        Err(why) => Err(cannot_run(&why)),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::run::{coverage_for, runner_for};

    /// A shared unit would let a gate that changed kind raise the old numbers.
    #[test]
    fn a_pass_fail_check_and_a_ratchet_never_name_the_same_unit() {
        assert_eq!(
            unit_of(&crate::gates::metrics::assertions::GATE),
            "length assertion(s)"
        );
        assert_eq!(unit_of(&crate::gates::tools::TEST), "pass or fail");
        assert_eq!(
            unit_of(&crate::gates::cargo::modcheck::GATE),
            "orphan module file(s)"
        );
    }

    #[test]
    fn a_kept_report_says_how_long_ago_it_was_measured() {
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        assert_eq!(
            ago(Some(1_000_000), at + std::time::Duration::from_secs(5)),
            Some("just now".to_string())
        );
        assert_eq!(
            ago(Some(1_000_000), at + std::time::Duration::from_secs(1_800)),
            Some("30 minutes ago".to_string())
        );
        // No stamp, or a clock behind the record, gives `None`.
        assert_eq!(ago(None, at), None);
        assert_eq!(
            ago(Some(1_000_000), at - std::time::Duration::from_secs(60)),
            None
        );
    }

    #[test]
    fn a_project_that_names_its_own_coverage_command_gets_that_one() {
        let config = Config::of(["coverage"]).with_coverage(["bin/coverage", "--lcov", "{lcov}"]);
        assert_eq!(
            coverage_for(Some(&config)).argv,
            vec![
                "bin/coverage".to_string(),
                "--lcov".to_string(),
                "{lcov}".to_string()
            ]
        );
        assert_eq!(coverage_for(None).argv, crate::run::default_coverage().argv);
    }

    #[test]
    fn a_target_and_profile_reach_the_runner_in_its_own_spelling_and_default_coverage_refuses() {
        let config = Config {
            target: Some("wasm32-wasip1".to_string()),
            profile: Some("dist".to_string()),
            ..Config::of(["test", "coverage"])
        };
        let mut runner = crate::run::default_runner().argv;
        runner.extend(["--target", "wasm32-wasip1", "--cargo-profile", "dist"].map(String::from));
        assert_eq!(runner_for(Some(&config)).argv, runner);
        let refused = coverage_for(Some(&config));
        let why = refused
            .report
            .get()
            .and_then(|report| report.as_ref().err());
        assert_eq!(
            why.map(String::as_str),
            Some(
                "the default coverage command cannot be told the configured target or profile; \
                 name the command that writes the report in `coverage`"
            )
        );
        let named = Config {
            coverage: Some(vec!["bin/coverage-rust".to_string(), "{lcov}".to_string()]),
            ..config
        };
        assert!(coverage_for(Some(&named)).report.get().is_none());
    }

    #[test]
    fn default_compiler_commands_receive_features_without_rewriting_custom_commands() {
        let config = Config {
            features: Some(vec!["testkit".to_string()]),
            no_default_features: Some(true),
            ..Config::of(["test", "coverage"])
        };
        let flags = ["--no-default-features", "--features", "testkit"];
        let mut runner = crate::run::default_runner().argv;
        runner.extend(flags.map(String::from));
        assert_eq!(runner_for(Some(&config)).argv, runner);
        let mut coverage = crate::run::default_coverage().argv;
        coverage.extend(flags.map(String::from));
        assert_eq!(coverage_for(Some(&config)).argv, coverage);
        let custom = Config {
            runner: Some(vec!["bin/test-rust".to_string()]),
            coverage: Some(vec!["bin/coverage-rust".to_string(), "{lcov}".to_string()]),
            ..config
        };
        assert_eq!(runner_for(Some(&custom)).argv, ["bin/test-rust"]);
        assert_eq!(
            coverage_for(Some(&custom)).argv,
            ["bin/coverage-rust", "{lcov}"]
        );
        assert_eq!(
            coverage_for(Some(&Config {
                coverage: Some(Vec::new()),
                ..custom
            }))
            .argv,
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_project_that_names_no_runner_gets_the_default_one() {
        assert_eq!(runner_for(None), crate::run::default_runner());
        assert_eq!(
            runner_for(Some(&Config::of(["test"]))),
            crate::run::default_runner()
        );
    }

    #[test]
    fn a_project_that_names_its_own_runner_gets_that_command() {
        let config = Config {
            runner: Some(vec!["bin/test-rust".to_string(), "--tap".to_string()]),
            ..Config::of(["test"])
        };
        assert_eq!(
            runner_for(Some(&config)),
            crate::run::Runner {
                argv: vec!["bin/test-rust".to_string(), "--tap".to_string()],
                tools: Vec::new(),
            }
        );
    }

    /// Falling back here would run `cargo nextest` and report the answer as the project's own.
    #[test]
    fn a_runner_naming_no_command_is_carried_through_rather_than_replaced() {
        let config = Config {
            runner: Some(Vec::new()),
            ..Config::of(["test"])
        };
        assert_eq!(runner_for(Some(&config)).argv, Vec::<String>::new());
    }

    #[test]
    fn explain_takes_one_gate_or_none_for_all_the_debt() {
        let asked = |name, json| Command::Explain { name, json };
        assert_eq!(parse(&["explain", "crap"]), asked(Some("crap"), false));
        assert_eq!(parse(&["explain"]), asked(None, false));
        assert_eq!(parse(&["explain", "--json"]), asked(None, true));
        assert_eq!(
            parse(&["explain", "a", "b"]),
            Command::Usage("explain takes one gate, or none for all the debt".to_string())
        );
    }

    #[test]
    fn the_usage_text_lists_every_subcommand_that_parses() {
        for name in [
            "run", "gates", "enable", "disable", "explain", "baseline", "cache", "doctor",
            "message", "edited", "slop", "lean", "oracle", "sweep", "moved", "init",
        ] {
            assert!(
                USAGE.contains(&format!("chock {name}")),
                "{name} missing from usage"
            );
        }
    }

    #[test]
    fn a_project_that_has_not_run_init_gets_the_enforced_set() {
        assert_eq!(select(&[], None).unwrap().len(), gates::enforced().len());
    }

    #[test]
    fn a_configured_project_runs_exactly_what_it_switched_on() {
        let config = Config::of(["slop", "lint"]);
        let chosen = select(&[], Some(&config)).unwrap();
        assert_eq!(
            chosen.iter().map(|g| g.name).collect::<Vec<_>>(),
            vec!["lint", "slop"]
        );
    }

    #[test]
    fn a_named_gate_runs_even_when_the_config_leaves_it_off() {
        let config = Config::of(["lint"]);
        let chosen = select(&["mutest"], Some(&config)).unwrap();
        assert_eq!(
            chosen.iter().map(|g| g.name).collect::<Vec<_>>(),
            vec!["mutest"]
        );
    }

    #[test]
    fn a_config_naming_a_gate_that_no_longer_exists_is_refused_not_skipped() {
        let config = Config::of(["lint", "codeslope"]);
        let err = select(&[], Some(&config)).unwrap_err();
        assert!(err.contains("codeslope"), "{err}");
        assert!(err.contains(project::config::FILE), "{err}");
    }

    #[test]
    fn skip_leaves_out_the_gates_it_names_and_refuses_a_name_that_is_no_gate() {
        let kept = || select(&["test", "miri", "mutest"], None);
        let left = skipping(kept(), &["miri", "mutest"]).unwrap();
        assert_eq!(names_of(&left), ["test"]);
        let err = skipping(kept(), &["mirri"]).unwrap_err();
        assert!(err.starts_with("no gate named `mirri`"), "{err}");
        let refused = skipping(Err("no gate named `x`".to_string()), &[]).unwrap_err();
        assert_eq!(refused, "no gate named `x`");
    }

    /// `binsize` keeps one number for the project and `sort` one for each package.
    #[test]
    fn a_config_holding_where_touched_a_gate_with_no_number_for_each_file_is_refused() {
        let mut config = Config::of(["nesting", "binsize", "sort"]);
        config.clean_when_touched = Some(vec!["nesting".to_string()]);
        let mut chosen = names_of(&select(&[], Some(&config)).unwrap());
        chosen.sort_unstable();
        assert_eq!(chosen, ["binsize", "nesting", "sort"]);
        config.clean_when_touched =
            Some(["nesting", "binsize", "sort"].map(str::to_string).to_vec());
        let refusal = format!(
            "{} lists under `clean_when_touched` what keeps no number for each file: binsize, sort",
            project::config::FILE
        );
        assert_eq!(select(&[], Some(&config)).unwrap_err(), refusal);
        // A named run reads the same config, so it is refused alike.
        assert_eq!(select(&["nesting"], Some(&config)).unwrap_err(), refusal);
    }

    #[test]
    fn an_empty_config_runs_nothing_rather_than_quietly_running_everything() {
        let config = Config::of(Vec::<String>::new());
        assert!(select(&[], Some(&config)).unwrap().is_empty());
    }

    #[test]
    fn an_unknown_gate_name_is_refused_and_lists_the_real_ones() {
        let err = select(&["nope"], None).unwrap_err();
        assert!(err.starts_with("no gate named `nope`"));
        assert!(err.contains("slop"));
    }

    #[test]
    fn every_gate_in_the_registry_can_be_asked_for_by_name() {
        for gate in gates::registry() {
            assert!(
                select(&[gate.name], None).is_ok(),
                "{} unselectable",
                gate.name
            );
        }
    }

    #[test]
    fn recording_covers_every_ratchet_before_init_and_only_the_chosen_ones_after() {
        let names: Vec<&str> = recordable(None).iter().map(|g| g.name).collect();
        for gate in gates::ratchets() {
            assert!(
                names.contains(&gate.name),
                "{} is not recordable",
                gate.name
            );
        }
        // `crap` is not a ratchet, but it records a number.
        assert!(names.contains(&crate::gates::coverage::crap::GATE.name));
        let config = Config::of(["slop"]);
        assert_eq!(
            recordable(Some(&config))
                .iter()
                .map(|g| g.name)
                .collect::<Vec<_>>(),
            vec!["slop"]
        );
    }

    #[test]
    fn a_run_of_the_enabled_gates_is_recorded_as_the_command_with_no_names() {
        assert_eq!(invocation(&[]), "chock run");
        assert_eq!(invocation(&["slop", "lint"]), "chock run slop lint");
    }

    #[test]
    fn a_gate_whose_tool_cannot_run_here_is_left_out_and_named_never_passed() {
        let chosen = || {
            ["test", "acl", "proof"]
                .map(|name| gates::find(name).unwrap())
                .to_vec()
        };
        let names = |kept: &[&run::Gate]| kept.iter().map(|gate| gate.name).collect::<Vec<_>>();
        let (kept, said) = here(chosen(), "linux");
        assert_eq!((names(&kept), said), (vec!["test", "acl", "proof"], None));
        let (kept, said) = here(chosen(), "windows");
        assert_eq!(names(&kept), ["test"]);
        assert_eq!(
            said.as_deref(),
            Some(
                "chock: left out on windows, where its tool does not run: acl, proof; a Linux run enforces it"
            )
        );
    }

    /// Each change says what it set, or that the gate had it already, and what to do next.
    #[test]
    fn a_change_says_what_it_set_and_what_comes_next() {
        let binsize = gates::find("binsize").unwrap();
        let mut config = Config::of(["binsize"]);
        let ci = Change::Stage(Stage::Ci);
        assert_eq!(ci.apply(&mut config, binsize), "runs at ci");
        assert_eq!(ci.apply(&mut config, binsize), "already runs at ci");
        assert_eq!(Change::On.apply(&mut config, binsize), "already on");
        assert_eq!(Change::Off(None).apply(&mut config, binsize), "off");
        assert_eq!(
            Change::On.advice(&["binsize", "crap"]),
            "`chock run binsize crap` shows the findings now.\n"
        );
        assert!(
            Change::Stage(Stage::Manual)
                .advice(&[])
                .contains("only `chock run`")
        );
        assert_eq!(ci.advice(&[]), "");
        assert_eq!(Change::Off(None).advice(&[]), "");
    }

    fn names_of(gates: &[&'static run::Gate]) -> Vec<&'static str> {
        gates.iter().map(|gate| gate.name).collect()
    }

    fn staged(names: &[&str], stage: Stage) -> Config {
        Config {
            stage: names.iter().map(|name| (name.to_string(), stage)).collect(),
            ..Config::of(names.iter().copied())
        }
    }

    /// A push stage moves when a check runs, never whether: `pre-push` and CI still run it.
    #[test]
    fn a_check_deferred_to_the_push_is_left_out_of_what_a_commit_waits_for() {
        let every = gates::enforced();
        let cheap = for_tier(every.clone(), Tier::Committing, None);
        assert!(cheap.iter().all(|gate| !gate.builds), "a compile snuck in");
        let named = names_of(&cheap);
        assert!(named.contains(&"slop"), "{named:?}");

        let config = staged(&["slop"], Stage::Push);
        let left = names_of(&for_tier(every, Tier::Committing, Some(&config)));
        assert!(!left.contains(&"slop"), "{left:?}");
        // The rest of the commit set stays the same.
        let expected: Vec<&str> = named
            .iter()
            .copied()
            .filter(|name| *name != "slop")
            .collect();
        assert_eq!(left, expected);
    }

    #[test]
    fn the_fast_flag_asks_for_exactly_what_a_commit_waits_for() {
        assert_eq!(tier_for(true, false), Tier::Committing);
        assert_eq!(tier_for(false, false), Tier::Everything);
    }

    /// Both CI jobs lack the `local_only` tools, so `--ci` must reach the commit set too.
    #[test]
    fn the_ci_flag_reaches_both_of_cis_jobs() {
        assert_eq!(tier_for(false, true), Tier::Ci { fast: false });
        assert_eq!(tier_for(true, true), Tier::Ci { fast: true });
    }

    /// The hooks still run `local_only` checks, so the option cannot hide a red check.
    #[test]
    fn a_check_ci_cannot_run_is_still_run_by_the_hooks() {
        let every = gates::enforced();
        // `slop`, because outpost gates are opt-in and `enforced` does not hold them.
        let config = Config {
            local_only: Some(vec!["slop".to_string()]),
            ..Config::of(["slop"])
        };
        for tier in [Tier::Ci { fast: false }, Tier::Ci { fast: true }] {
            let named = names_of(&for_tier(every.clone(), tier, Some(&config)));
            assert!(!named.contains(&"slop"), "{tier:?}: {named:?}");
        }
        for tier in [Tier::Everything, Tier::Pushing, Tier::Committing] {
            let named = names_of(&for_tier(every.clone(), tier, Some(&config)));
            assert!(named.contains(&"slop"), "{tier:?}: {named:?}");
        }
    }

    /// CI's fast job mirrors the commit, so only its full job runs a gate staged at CI.
    #[test]
    fn a_check_deferred_to_ci_is_run_by_ci_and_by_no_hook() {
        let every = gates::enforced();
        let config = staged(&["slop"], Stage::Ci);
        let named = names_of(&for_tier(
            every.clone(),
            Tier::Ci { fast: false },
            Some(&config),
        ));
        assert!(named.contains(&"slop"), "{named:?}");
        for tier in [Tier::Committing, Tier::Pushing, Tier::Ci { fast: true }] {
            let named = names_of(&for_tier(every.clone(), tier, Some(&config)));
            assert!(!named.contains(&"slop"), "{tier:?}: {named:?}");
        }
    }

    #[test]
    fn a_check_moved_to_ci_is_out_of_both_hooks_and_still_in_what_chock_run_runs() {
        let every = gates::enforced();
        let config = staged(&["binsize", "duplication"], Stage::Ci);
        let pushing = names_of(&for_tier(every.clone(), Tier::Pushing, Some(&config)));
        assert!(!pushing.contains(&"binsize"), "{pushing:?}");
        // A stage moves only the gates it names.
        assert!(pushing.contains(&"coverage"), "{pushing:?}");

        // A gate named for CI is out of the commit as well, compiler or not.
        let committing = names_of(&for_tier(every.clone(), Tier::Committing, Some(&config)));
        assert!(!committing.contains(&"duplication"), "{committing:?}");
        assert!(committing.contains(&"slop"), "{committing:?}");

        // `chock run` runs every gate, whatever its stage.
        let all = names_of(&for_tier(every, Tier::Everything, Some(&config)));
        assert!(
            all.contains(&"binsize") && all.contains(&"duplication"),
            "{all:?}"
        );
    }

    #[test]
    fn a_manual_check_is_run_by_chock_run_alone() {
        let every = gates::enforced();
        let config = staged(&["binsize"], Stage::Manual);
        for tier in [Tier::Committing, Tier::Pushing, Tier::Ci { fast: false }] {
            let named = names_of(&for_tier(every.clone(), tier, Some(&config)));
            assert!(!named.contains(&"binsize"), "{tier:?}: {named:?}");
        }
        let all = names_of(&for_tier(every, Tier::Everything, Some(&config)));
        assert!(all.contains(&"binsize"), "{all:?}");
    }

    /// The default puts a compile at the push; a project may still want one before each commit.
    #[test]
    fn a_check_that_compiles_can_be_staged_at_the_commit() {
        let every = gates::enforced();
        assert!(gates::find("lint").is_some_and(|gate| gate.builds));
        let config = staged(&["lint"], Stage::Commit);
        let named = names_of(&for_tier(every, Tier::Committing, Some(&config)));
        assert!(named.contains(&"lint"), "{named:?}");
    }

    #[test]
    fn a_pre_push_hook_is_recognised_through_the_remote_git_appends() {
        assert_eq!(
            asked(&[
                "pre-push",
                "origin",
                "https://github.com/outpostHQ/chock.git"
            ]),
            Asked::Pushing
        );
        assert_eq!(asked(&["pre-commit", "origin"]), Asked::Committing);
    }

    #[test]
    fn a_hook_declared_above_its_project_names_the_project_wherever_git_puts_its_own_arguments() {
        let push = ["pre-push", "--project", "crates/a b", "origin", "url"];
        assert_eq!(
            project_named(&push).unwrap(),
            (vec!["pre-push", "origin", "url"], Some("crates/a b"))
        );
        assert_eq!(
            project_named(&["commit-msg", "--project", "x", "MSG"]).unwrap(),
            (vec!["commit-msg", "MSG"], Some("x"))
        );
        assert_eq!(
            project_named(&["pre-commit"]).unwrap(),
            (vec!["pre-commit"], None)
        );
        assert!(project_named(&["pre-commit", "--project"]).is_err());
    }

    #[test]
    fn a_commit_msg_hook_with_no_file_asks_git_which_message_it_is_composing() {
        assert_eq!(asked(&["commit-msg"]), Asked::MessageGitIsComposing);
        assert_eq!(
            asked(&["commit-msg", ".git/COMMIT_EDITMSG"]),
            Asked::Message(".git/COMMIT_EDITMSG")
        );
    }

    #[test]
    fn a_hook_chock_does_not_run_is_named_back_and_an_empty_one_asks_for_a_name() {
        assert_eq!(asked(&["post-merge"]), Asked::Unknown("post-merge"));
        assert_eq!(asked(&[]), Asked::Unnamed);
    }
}
