//! The `tla` gate: each TLA+ cfg checks under TLC as it expects, each invariant a model lists is
//! refuted by a bad variant, Apalache proves each inductive invariant, and TLAPS each theorem.

mod apalache;
mod check;
mod engines;
pub(crate) mod text;
mod tlaps;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::Run;
use crate::exec;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind};
use text::uncommented;

pub const GATE: Gate = Gate {
    name: "tla",
    about: "each TLA+ model passes TLC, each bad variant of it fails, Apalache proves IndInv, and TLAPS each theorem",
    group: Group::OptIn,
    builds: true,
    // The tree holds the models and the pins of both jars; Java is the one thing outside it.
    reads: Some(crate::run::verdicts::Reads::tree_and(&["java"]).versioned("tla-v2")),
    kind: Kind::Binary(check::check),
};

const DIR: &str = "proofs/tla";
const SCRATCH: &str = "target/test-scratch/chock/tla";
const WORKERS: u32 = 2;
const TIMEOUT_S: u64 = 300;
const INDUCTIVE: &str = "IndInv";

/// What a cfg must do under TLC: from its `\* expect:` comment, else from its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
enum Expect {
    Pass,
    Deadlock,
    Violation,
}

/// The names a cfg gives TLC, comments left out.
#[derive(Debug, Default, PartialEq, Eq)]
struct Listed {
    invariants: Vec<String>,
    properties: Vec<String>,
    /// `CHECK_DEADLOCK FALSE`: TLC does not check the cfg for a deadlock.
    no_deadlock: bool,
}

/// One cfg, the directory that holds it, and the model it checks.
#[derive(Debug)]
struct Cfg {
    dir: PathBuf,
    stem: String,
    model: String,
    expect: Expect,
    listed: Listed,
}

impl Cfg {
    fn file(&self) -> String {
        format!("{}.cfg", self.stem)
    }

    fn is_own(&self) -> bool {
        self.stem == self.model
    }
}

/// What TLC said about one cfg.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Said {
    Passed,
    Deadlock,
    /// A safety violation, with the invariant TLC names where it names one.
    Violated(Option<String>),
    Temporal,
    TimedOut(u64),
    /// TLC stopped without a verdict, such as on a parse or config error; its first error line.
    Broken(String),
}

impl Said {
    fn of(out: &exec::Output) -> Self {
        let text = format!("{}\n{}", out.stdout, out.stderr);
        match out.code {
            Some(0) => Self::Passed,
            Some(11) => Self::Deadlock,
            Some(12) => Self::Violated(violated(&text)),
            Some(13) => Self::Temporal,
            code => Self::Broken(first_error(&text, code)),
        }
    }

    fn describe(&self, by: Engine) -> String {
        let engine = by.name();
        match (self, by) {
            (Self::Passed, Engine::Apalache) => {
                "Apalache finds no violation within `tla.length` steps of Init".into()
            }
            (Self::Violated(None), Engine::Apalache) => {
                "Apalache finds an invariant violated, and its log does not name it".into()
            }
            (Self::Passed, _) => format!("{engine} finds no violation"),
            (Self::Deadlock, _) => format!("{engine} reaches a deadlock"),
            (Self::Violated(Some(name)), _) => format!("{engine} finds {name} violated"),
            (Self::Violated(None), _) => format!("{engine} finds a safety property violated"),
            (Self::Temporal, _) => format!("{engine} finds a temporal property violated"),
            (Self::TimedOut(secs), _) => format!(
                "{engine} ran past its {secs} s limit; raise `tla.timeouts` for it or shrink its constants"
            ),
            (Self::Broken(line), _) => format!("{engine} could not check it: {line}"),
        }
    }
}

/// The engine that gives a cfg its verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Engine {
    Tlc,
    Apalache,
}

impl Engine {
    fn name(self) -> &'static str {
        match self {
            Self::Tlc => "TLC",
            Self::Apalache => "Apalache",
        }
    }
}

const SECTIONS: [&str; 18] = [
    "CONSTANT",
    "CONSTANTS",
    "INIT",
    "NEXT",
    "SPECIFICATION",
    "INVARIANT",
    "INVARIANTS",
    "PROPERTY",
    "PROPERTIES",
    "CONSTRAINT",
    "CONSTRAINTS",
    "ACTION_CONSTRAINT",
    "ACTION_CONSTRAINTS",
    "SYMMETRY",
    "VIEW",
    "CHECK_DEADLOCK",
    "POSTCONDITION",
    "ALIAS",
];

fn listed(cfg: &str) -> Listed {
    let mut found = Listed::default();
    let mut section = "";
    for word in uncommented(cfg).split_whitespace() {
        if SECTIONS.contains(&word) {
            section = word;
        } else if matches!(section, "INVARIANT" | "INVARIANTS") {
            found.invariants.push(word.to_string());
        } else if matches!(section, "PROPERTY" | "PROPERTIES") {
            found.properties.push(word.to_string());
        } else if section == "CHECK_DEADLOCK" {
            found.no_deadlock = word == "FALSE";
        }
    }
    found
}

/// The `\* expect: <result>` comment of a cfg, on a line of its own or at the end of one.
fn expected(cfg: &str) -> Option<Result<Expect, String>> {
    cfg.lines().find_map(|line| {
        let (_, comment) = line.split_once("\\*")?;
        let said = comment.trim().strip_prefix("expect:")?;
        Some(match said.split_whitespace().next().unwrap_or("") {
            "pass" => Ok(Expect::Pass),
            "deadlock" => Ok(Expect::Deadlock),
            "violation" => Ok(Expect::Violation),
            other => Err(other.to_string()),
        })
    })
}

/// Without a comment, `Foo.cfg` and `Foo_live.cfg` must pass and any other `Foo_*.cfg` must fail.
fn by_name(stem: &str, model: &str) -> Expect {
    match stem.strip_prefix(model) {
        Some("" | "_live") => Expect::Pass,
        _ => Expect::Violation,
    }
}

/// The model of a cfg: the longest model name that is its stem, or starts it before a `_`.
fn model_of<'a>(stem: &str, models: &'a [String]) -> Option<&'a String> {
    let starts = |model: &&String| {
        stem.strip_prefix(model.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('_'))
    };
    models.iter().filter(starts).max_by_key(|model| model.len())
}

/// Every cfg under the model directories. A cfg with no model, or with an `expect` word chock does
/// not know, is a finding rather than a cfg.
fn discovered(root: &Path, dirs: &[String]) -> (Vec<Cfg>, Vec<Finding>) {
    let mut holders = Vec::new();
    for dir in dirs {
        holders_under(&root.join(dir), ".cfg", 4, &mut holders);
    }
    let mut cfgs = Vec::new();
    let mut findings = Vec::new();
    for dir in holders {
        let names = names_in(&dir);
        let stems = |ext: &str| -> Vec<String> {
            let tail = format!(".{ext}");
            names
                .iter()
                .filter_map(|name| name.strip_suffix(&tail))
                .map(String::from)
                .collect()
        };
        let models = stems("tla");
        for read in stems("cfg")
            .into_iter()
            .map(|stem| cfg_at(root, &dir, stem, &models))
        {
            match read {
                Ok(cfg) => cfgs.push(cfg),
                Err(finding) => findings.push(*finding),
            }
        }
    }
    (cfgs, findings)
}

const NO_MODEL: &str =
    "names no model: no .tla file beside it is its name, or a start of it before _";

fn cfg_at(root: &Path, dir: &Path, stem: String, models: &[String]) -> Result<Cfg, Box<Finding>> {
    let path = dir.join(format!("{stem}.cfg"));
    let file = relative(root, &path);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let model = model_of(&stem, models).ok_or_else(|| Box::new(Finding::at(&file, NO_MODEL)))?;
    let expect = match expected(&text) {
        Some(Ok(expect)) => expect,
        None => by_name(&stem, model),
        Some(Err(word)) => {
            let why = format!("`expect: {word}` is not pass, deadlock or violation");
            return Err(Box::new(Finding::at(&file, &why)));
        }
    };
    Ok(Cfg {
        dir: dir.to_path_buf(),
        model: model.clone(),
        stem,
        expect,
        listed: listed(&text),
    })
}

/// `dir` and the directories below it that hold a file ending in `ext`, in name order.
fn holders_under(dir: &Path, ext: &str, depth: u32, found: &mut Vec<PathBuf>) {
    let names = names_in(dir);
    if names.iter().any(|name| name.ends_with(ext)) {
        found.push(dir.to_path_buf());
    }
    let below = names
        .iter()
        .filter(|name| !name.starts_with('.') && *name != "states");
    for name in below.filter(|name| crate::project::descends_into(&dir.join(name))) {
        if depth > 0 {
            holders_under(&dir.join(name), ext, depth - 1, found);
        }
    }
}

fn names_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

fn relative(root: &Path, path: &Path) -> String {
    let path = path.strip_prefix(root).unwrap_or(path);
    path.to_string_lossy().replace('\\', "/")
}

/// The invariant a TLC safety violation names, from `Error: Invariant Safe is violated.`
fn violated(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (_, rest) = line.split_once("Invariant ")?;
        let (name, tail) = rest.split_once(' ')?;
        tail.starts_with("is violated").then(|| name.to_string())
    })
}

/// The first line that names an error, and the line after it, which says where; else the last.
pub(super) fn first_error(text: &str, code: Option<i32>) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let at = lines
        .iter()
        .position(|line| line.to_lowercase().contains("error"));
    let said = match at {
        Some(at) => lines[at..]
            .iter()
            .take(2)
            .copied()
            .collect::<Vec<_>>()
            .join(" "),
        None => lines.last().copied().unwrap_or("no output").to_string(),
    };
    let exit = code.map_or("a signal".to_string(), |code| format!("exit {code}"));
    let said: String = said.chars().take(240).collect();
    format!("{exit}: {said}")
}

/// Whether the engine's verdict is the one the cfg expects; `Some` says why not.
fn judged(by: Engine, cfg: &Cfg, said: &Said) -> Option<String> {
    let type_only = |name: &str| name.starts_with("Type");
    match (cfg.expect, said) {
        (Expect::Pass, Said::Passed) | (Expect::Deadlock, Said::Deadlock) => None,
        (Expect::Violation, Said::Violated(Some(name)))
            if cfg.listed.invariants.contains(name) && !type_only(name) =>
        {
            None
        }
        (Expect::Violation, Said::Temporal) if !cfg.listed.properties.is_empty() => None,
        (_, Said::TimedOut(_) | Said::Broken(_)) => Some(said.describe(by)),
        (Expect::Pass, _) => Some(format!("must pass, and {}", said.describe(by))),
        (Expect::Deadlock, _) => Some(format!(
            "must deadlock, as its `expect: deadlock` says, and {}",
            said.describe(by)
        )),
        (Expect::Violation, Said::Violated(Some(name))) if type_only(name) => Some(format!(
            "only {name} is violated: the variant is ill-typed, which refutes nothing"
        )),
        (Expect::Violation, Said::Deadlock) => Some(
            "deadlocks instead of violating an invariant it lists; if that is its point, \
             write `\\* expect: deadlock` in it"
                .into(),
        ),
        (Expect::Violation, Said::Passed) => {
            let hint = match by {
                Engine::Tlc => "; if it is a good model, write `\\* expect: pass` in it",
                Engine::Apalache => "",
            };
            let (model, engine, why) = (&cfg.model, by.name(), said.describe(by));
            Some(format!(
                "is a bad variant of {model}, so {engine} must refute it, and {why}{hint}"
            ))
        }
        (Expect::Violation, _) => Some(format!(
            "must violate an invariant or property it lists, and {}",
            said.describe(by)
        )),
    }
}

/// Definitions that are only a conjunction of names, such as `Safe == NoLoss /\ NoTear`, so a
/// variant that refutes each part covers the whole.
fn conjunctions(tla: &str) -> BTreeMap<String, Vec<String>> {
    let mut found = BTreeMap::new();
    let mut open: Option<(String, String)> = None;
    let is_name =
        |text: &str| !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_');
    let mut close = |open: Option<(String, String)>| {
        let Some((name, body)) = open else {
            return;
        };
        let parts: Vec<String> = body
            .split("/\\")
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(String::from)
            .collect();
        if !parts.is_empty() && parts.iter().all(|part| is_name(part)) {
            found.insert(name, parts);
        }
    };
    for line in uncommented(tla).lines() {
        if line.starts_with(char::is_whitespace) {
            if let Some((_, body)) = &mut open {
                body.push(' ');
                body.push_str(line);
            }
            continue;
        }
        close(open.take());
        open = line
            .split_once("==")
            .filter(|(head, _)| is_name(head.trim()))
            .map(|(head, body)| (head.trim().to_string(), body.to_string()));
    }
    close(open);
    found
}

/// The parts of `name` that no variant refutes, a conjunction read as its parts.
fn unrefuted(
    name: &str,
    hits: &BTreeSet<String>,
    parts: &BTreeMap<String, Vec<String>>,
    seen: &mut Vec<String>,
) -> Vec<String> {
    if hits.contains(name) || name.starts_with("Type") {
        return Vec::new();
    }
    match parts.get(name) {
        Some(each) if !seen.iter().any(|held| held == name) => {
            seen.push(name.to_string());
            each.iter()
                .flat_map(|part| unrefuted(part, hits, parts, seen))
                .collect()
        }
        _ => vec![name.to_string()],
    }
}

/// Each model needs a `_reach` cfg, and each invariant its own cfg lists, but its type and its
/// inductive invariant, must be violated by a variant, as `said` or `also` (runs alone) names.
fn uncovered(
    root: &Path,
    cfgs: &[Cfg],
    said: &[Said],
    also: &[Ask],
    inductive: &dyn Fn(&str) -> String,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for own in cfgs.iter().filter(|cfg| cfg.is_own()) {
        let file = relative(root, &own.dir.join(own.file()));
        let reach = format!("{}_reach", own.model);
        let reached = cfgs
            .iter()
            .any(|cfg| cfg.dir == own.dir && cfg.stem == reach);
        if !reached {
            let why = format!(
                "has no {reach}.cfg, so nothing shows that the states it guards are ever reached"
            );
            findings.push(Finding::at(&file, &why).item(&own.model));
        }
        let skip = inductive(&own.model);
        let hits = refuted_of(own, cfgs, said, also);
        for name in missing(own, &hits, &parts_of(own), &skip) {
            let why = format!(
                "lists {name}, and no bad variant of {} violates it",
                own.model
            );
            findings.push(Finding::at(&file, &why).item(&name));
        }
    }
    findings
}

/// Whether `cfg` is a bad variant of the model whose own cfg is `own`.
fn bad_of(own: &Cfg, cfg: &Cfg) -> bool {
    cfg.dir == own.dir && cfg.model == own.model && cfg.expect == Expect::Violation
}

/// Each conjunction of the module of `own`, by its parts.
fn parts_of(own: &Cfg) -> BTreeMap<String, Vec<String>> {
    let text = std::fs::read_to_string(own.dir.join(format!("{}.tla", own.model)));
    conjunctions(&text.unwrap_or_default())
}

/// The invariants the bad variants of `own` violate: one that `said` names for each variant, and
/// each that `also` gives one.
fn refuted_of(own: &Cfg, cfgs: &[Cfg], said: &[Said], also: &[Ask]) -> BTreeSet<String> {
    let named = cfgs.iter().zip(said).filter(|(cfg, _)| bad_of(own, cfg));
    let named = named.filter_map(|(_, said)| match said {
        Said::Violated(Some(name)) => Some(name.clone()),
        _ => None,
    });
    let probed = also.iter().filter(|(cfg, _)| bad_of(own, cfg));
    named.chain(probed.map(|(_, name)| name.clone())).collect()
}

/// The invariants of `own`, but `skip`, that are not in `hits`, a conjunction read as its `parts`.
fn missing(
    own: &Cfg,
    hits: &BTreeSet<String>,
    parts: &BTreeMap<String, Vec<String>>,
    skip: &str,
) -> BTreeSet<String> {
    let listed = own.listed.invariants.iter().filter(|name| *name != skip);
    listed
        .flat_map(|name| unrefuted(name, hits, parts, &mut Vec::new()))
        .collect()
}

/// A cfg and an invariant: one to check alone on it, or one it violates.
type Ask<'a> = (&'a Cfg, String);

/// Apalache names only the first invariant that a variant violates. So for each model it checks
/// alone, each invariant that no variant named goes alone to each violated variant that checks it.
fn unnamed<'a>(
    cfgs: &'a [Cfg],
    said: &[Said],
    split: &engines::Split,
    inductive: &dyn Fn(&str) -> String,
) -> Vec<Ask<'a>> {
    let mut asks = Vec::new();
    let only_apalache = |cfg: &&Cfg| cfg.is_own() && split.engine(cfg) == Engine::Apalache;
    for own in cfgs.iter().filter(only_apalache) {
        let parts = parts_of(own);
        let hits = refuted_of(own, cfgs, said, &[]);
        for name in missing(own, &hits, &parts, &inductive(&own.model)) {
            let checks = |cfg: &Cfg| {
                let mut listed = cfg.listed.invariants.iter();
                listed.any(|held| reaches(held, &name, &parts, &mut Vec::new()))
            };
            let violated = |said: &Said| matches!(said, Said::Violated(_));
            let variants = cfgs.iter().zip(said);
            let variants =
                variants.filter(|(cfg, said)| bad_of(own, cfg) && violated(said) && checks(cfg));
            asks.extend(variants.map(|(cfg, _)| (cfg, name.clone())));
        }
    }
    asks
}

/// Whether a check of `held` checks `name`: `held` is `name`, or a conjunction with it in its parts.
fn reaches(
    held: &str,
    name: &str,
    parts: &BTreeMap<String, Vec<String>>,
    seen: &mut Vec<String>,
) -> bool {
    if held == name {
        return true;
    }
    if seen.iter().any(|it| it == held) {
        return false;
    }
    seen.push(held.to_string());
    let each = parts.get(held).map(Vec::as_slice).unwrap_or_default();
    each.iter().any(|part| reaches(part, name, parts, seen))
}

/// Runs each (cfg index, invariant) of `asks` alone under Apalache, within `tla.length` steps of
/// `Init`. Gives those it finds violated, and a finding for each run that cannot decide.
fn alone<'a>(
    ctx: &Ctx,
    java: &str,
    asks: Vec<Ask<'a>>,
    lanes: usize,
) -> Result<(Vec<Ask<'a>>, Vec<Finding>), String> {
    let (mut also, mut findings) = (Vec::new(), Vec::new());
    if asks.is_empty() {
        return Ok((also, findings));
    }
    let jar = apalache::jar(ctx)?;
    let scratch = ctx.root.join(SCRATCH);
    let length = format!("--length={}", ctx.tla.length.unwrap_or(0));
    let runs = asks.iter().enumerate().map(|(at, (cfg, name))| {
        let out = scratch.join("apalache").join(format!("alone-{at}"));
        let args = vec![length.clone(), format!("--inv={name}")];
        apalache::checking(&jar, &ctx.tla, cfg, &out, args)
    });
    let mut runs: Vec<Run> = runs.collect();
    with_tmp(&mut runs, &scratch)?;
    let outs = super::run_all(&ctx.tools, java, &runs, lanes);
    super::cleared(&scratch)?;
    for (((cfg, name), run), out) in asks.into_iter().zip(&runs).zip(outs) {
        match apalache::said(out, run.limit)? {
            Said::Violated(_) => also.push((cfg, name)),
            Said::Passed => {}
            other => {
                let file = relative(&ctx.root, &cfg.dir.join(cfg.file()));
                let why = format!(
                    "checks {name} alone, and {}",
                    other.describe(Engine::Apalache)
                );
                findings.push(Finding::at(&file, &why).item(&cfg.model));
            }
        }
    }
    Ok((also, findings))
}

/// A Java runtime: `JAVA_HOME`'s, else the one on `PATH`. Both TLC and Apalache run on it.
fn java(ctx: &Ctx) -> Result<String, String> {
    java_in(ctx, java_home())
}

/// The Java home this process names: its `JAVA_HOME`. `doctor` checks the same one.
#[must_use]
pub fn java_home() -> Option<std::ffi::OsString> {
    std::env::var_os("JAVA_HOME")
}

fn java_in(ctx: &Ctx, home: Option<std::ffi::OsString>) -> Result<String, String> {
    let java = java_program(home);
    let runs = ctx.tools.run(&java, &["-version"], &ctx.root);
    let runs = runs.is_ok_and(|out| out.success());
    runs.then_some(java)
        .ok_or_else(|| crate::gates::fixes::not_installed("java"))
}

/// `bin/java` under `home`, a `JAVA_HOME`, where it holds one; else the `java` on `PATH`.
/// `doctor` checks the same program.
#[must_use]
pub fn java_program(home: Option<std::ffi::OsString>) -> String {
    let program = if cfg!(windows) { "java.exe" } else { "java" };
    let found = home
        .map(|home| PathBuf::from(home).join("bin").join(program))
        .filter(|path| path.is_file());
    found.map_or_else(|| "java".into(), |path| path.display().to_string())
}

/// Gives each run its own Java temporary directory: TLC unpacks its standard modules there, and
/// runs that share one collide.
fn with_tmp(runs: &mut [Run], scratch: &Path) -> Result<(), String> {
    for (at, run) in runs.iter_mut().enumerate() {
        let tmp = scratch.join(format!("java-tmp-{at}"));
        super::made(&tmp).map_err(|e| format!("cannot make {}: {e}", tmp.display()))?;
        run.argv
            .insert(0, format!("-Djava.io.tmpdir={}", tmp.display()));
    }
    Ok(())
}

/// The cfgs under `dirs` that `--model` asks for, and what is wrong with the others found there.
fn asked(ctx: &Ctx, dirs: &[String]) -> Result<(Vec<Cfg>, Vec<Finding>), String> {
    let (cfgs, findings) = discovered(&ctx.root, dirs);
    if cfgs.is_empty() && findings.is_empty() {
        let dirs = dirs.join(", ");
        return Err(format!(
            "no .cfg file under {dirs}, so TLC would check nothing"
        ));
    }
    let asks = |cfg: &Cfg| ctx.scope.asks_model(&cfg.model);
    Ok((cfgs.into_iter().filter(asks).collect(), findings))
}

fn limit_of(settings: &crate::project::config::Tla, stem: &str, model: &str) -> u64 {
    let named = settings
        .timeouts
        .get(stem)
        .or_else(|| settings.timeouts.get(model));
    named
        .copied()
        .unwrap_or(settings.timeout_s.unwrap_or(TIMEOUT_S))
}

/// The flag that stops TLC 1.8 and later from writing a trace spec next to the model at each
/// violation. An older TLC writes none, and refuses the flag.
fn no_trace_spec(version: &str) -> Option<&'static str> {
    let mut parts = version.split('.').map(|part| part.parse().unwrap_or(0_u32));
    let at = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    (at >= (1, 8)).then_some("-noGenerateSpecTE")
}

/// The pinned TLC: its jar, and the flag that keeps its trace spec out of the model directory.
struct Tlc<'a> {
    jar: &'a Path,
    traces: Option<&'static str>,
}

/// One TLC run. `-metadir` and `-cleanup` keep its states out of the model directory, and the
/// `traces` flag of `tool` keeps its trace spec out.
fn tlc(tool: &Tlc, cfg: &Cfg, meta: &Path, workers: u32, limit: u64) -> Run {
    let (jar, meta) = (tool.jar.to_string_lossy(), meta.to_string_lossy());
    let (workers, file, model) = (
        workers.to_string(),
        cfg.file(),
        format!("{}.tla", cfg.model),
    );
    let head = [
        "-XX:+UseParallelGC",
        "-Xmx2g",
        "-cp",
        &jar,
        "tlc2.TLC",
        "-workers",
        &workers,
        "-cleanup",
        "-metadir",
        &meta,
    ];
    let argv = head
        .into_iter()
        .chain(tool.traces)
        .chain(["-config", &file, &model]);
    Run {
        dir: cfg.dir.clone(),
        argv: argv.map(str::to_string).collect(),
        limit,
    }
}

fn read_tlc(out: Result<exec::Output, exec::ExecError>, limit: u64) -> Result<Said, String> {
    match out {
        Ok(out) => Ok(Said::of(&out)),
        Err(e) if e.hung() => Ok(Said::TimedOut(limit)),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::check::{check, lanes};
    use super::*;
    use crate::run::Outcome;

    pub(super) fn cfg(stem: &str, model: &str, text: &str) -> Cfg {
        Cfg {
            dir: PathBuf::from("m"),
            stem: stem.into(),
            model: model.into(),
            expect: expected(text).map_or_else(|| by_name(stem, model), Result::unwrap),
            listed: listed(text),
        }
    }

    #[test]
    fn a_cfg_lists_its_names_and_its_comments_say_nothing() {
        let text = "CONSTANTS\n  N = 3\nINIT Init \\* not a name\nNEXT Next\n(* INVARIANT Gone *)\n\
                    INVARIANT TypeOK Safe\nINVARIANTS IndInv\nPROPERTY Live\nCHECK_DEADLOCK FALSE\n";
        let held = listed(text);
        assert_eq!(held.invariants, ["TypeOK", "Safe", "IndInv"]);
        assert_eq!(held.properties, ["Live"]);
        assert!(held.no_deadlock);
        assert!(!listed("CHECK_DEADLOCK TRUE\n").no_deadlock);
    }

    #[test]
    fn the_expect_comment_rules_and_without_it_the_name_does() {
        assert_eq!(
            expected("NEXT NextPrune \\* expect: pass\n"),
            Some(Ok(Expect::Pass))
        );
        assert_eq!(
            expected("\\* expect: deadlock\nNEXT N\n"),
            Some(Ok(Expect::Deadlock))
        );
        assert_eq!(expected("\\* expect: maybe\n"), Some(Err("maybe".into())));
        assert_eq!(expected("\\* expected nothing\n"), None);
        assert_eq!(by_name("Lock", "Lock"), Expect::Pass);
        assert_eq!(by_name("Lock_live", "Lock"), Expect::Pass);
        assert_eq!(by_name("Lock_reach", "Lock"), Expect::Violation);
        assert_eq!(by_name("Lock_no_fence", "Lock"), Expect::Violation);
        let models = ["Lock".to_string(), "Lock_Two".to_string()];
        assert_eq!(
            model_of("Lock_Two_bad", &models).map(String::as_str),
            Some("Lock_Two")
        );
        assert_eq!(
            model_of("Lock_bad", &models).map(String::as_str),
            Some("Lock")
        );
        assert_eq!(model_of("Locks", &models), None);
    }

    #[test]
    fn only_a_listed_invariant_that_is_not_a_type_refutes_a_variant() {
        let bad = cfg("M_bad", "M", "INVARIANT TypeOK Safe\n");
        assert_eq!(
            judged(Engine::Tlc, &bad, &Said::Violated(Some("Safe".into()))),
            None
        );
        assert!(
            judged(Engine::Tlc, &bad, &Said::Violated(Some("TypeOK".into())))
                .unwrap()
                .contains("ill-typed")
        );
        assert!(
            judged(Engine::Tlc, &bad, &Said::Passed)
                .unwrap()
                .contains("expect: pass")
        );
        assert!(
            judged(Engine::Tlc, &bad, &Said::Deadlock)
                .unwrap()
                .contains("expect: deadlock")
        );
        assert!(judged(Engine::Tlc, &bad, &Said::Temporal).is_some());
        let broken = Said::Broken("exit 150: ***Parse Error***".into());
        assert!(
            judged(Engine::Tlc, &bad, &broken)
                .unwrap()
                .contains("could not check")
        );
        let live = cfg("M_stall", "M", "PROPERTY Live\n");
        assert_eq!(judged(Engine::Tlc, &live, &Said::Temporal), None);
        let good = cfg("M", "M", "INVARIANT Safe\n");
        assert_eq!(judged(Engine::Tlc, &good, &Said::Passed), None);
        assert!(
            judged(Engine::Tlc, &good, &Said::Violated(Some("Safe".into())))
                .unwrap()
                .starts_with("must pass")
        );
        let stuck = cfg("M_end", "M", "\\* expect: deadlock\n");
        assert_eq!(judged(Engine::Tlc, &stuck, &Said::Deadlock), None);
    }

    #[test]
    fn tlc_output_reads_as_a_verdict_by_its_exit_code() {
        let out = |code, stdout: &str| exec::Output {
            code,
            stdout: stdout.into(),
            stderr: String::new(),
            truncated: false,
        };
        let safety = out(
            Some(12),
            "Error: Invariant Safe is violated.\nThe behavior up to this point is:",
        );
        assert_eq!(Said::of(&safety), Said::Violated(Some("Safe".into())));
        assert_eq!(
            Said::of(&out(Some(11), "Error: Deadlock reached.")),
            Said::Deadlock
        );
        let parse = out(
            Some(150),
            "Parsing file M.tla\n***Parse Error***\nEncountered \"==\" at line 4",
        );
        assert_eq!(
            Said::of(&parse),
            Said::Broken("exit 150: ***Parse Error*** Encountered \"==\" at line 4".into())
        );
        assert_eq!(
            Said::of(&out(None, "")),
            Said::Broken("a signal: no output".into())
        );
    }

    #[test]
    fn a_conjunction_is_covered_when_each_part_is_refuted() {
        let tla = "---- MODULE M ----\nSafe == NoLoss /\\ NoTear\nIndInv ==\n  /\\ TypeOK\n  /\\ x > 0\n\
                   Both ==\n  /\\ Safe \\* both parts\n  /\\ Other\n====\n";
        let parts = conjunctions(tla);
        assert_eq!(parts.get("Safe").unwrap(), &["NoLoss", "NoTear"]);
        assert_eq!(parts.get("Both").unwrap(), &["Safe", "Other"]);
        assert!(!parts.contains_key("IndInv"));
        let hits: BTreeSet<String> = ["NoLoss".to_string(), "Other".to_string()].into();
        assert_eq!(
            unrefuted("Both", &hits, &parts, &mut Vec::new()),
            ["NoTear"]
        );
        assert!(unrefuted("TypeOK", &hits, &parts, &mut Vec::new()).is_empty());
        // A name that a part of it names again is one part, so the walk ends.
        let looped: BTreeMap<String, Vec<String>> = [
            ("A".to_string(), vec!["B".to_string()]),
            ("B".to_string(), vec!["A".to_string()]),
        ]
        .into();
        let none = BTreeSet::new();
        assert_eq!(unrefuted("A", &none, &looped, &mut Vec::new()), ["A"]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn java_is_the_one_in_java_home_else_the_one_on_path() {
        let mut ctx = crate::testdir::Held::tree("gate-tla-java", &[]);
        ctx.tools = crate::testdir::faked(&ctx.root, "exit 0\n");
        let program = if cfg!(windows) { "java.exe" } else { "java" };
        let bin = ctx.root.join("jdk").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(program), "").unwrap();
        let home = Some(ctx.root.join("jdk").into_os_string());
        let found = bin.join(program).display().to_string();
        assert_eq!(java_in(&ctx, home).unwrap(), found);
        let empty = Some(ctx.root.join("empty").into_os_string());
        assert_eq!(java_in(&ctx, empty).unwrap(), "java");
    }

    #[test]
    fn an_invariant_apalache_left_unnamed_runs_alone_on_each_variant_that_checks_it() {
        let cfgs = [
            cfg("M", "M", "INVARIANT Safe Fresh\n"),
            cfg("M_a", "M", "INVARIANT Safe Fresh\n"),
            cfg("M_b", "M", "INVARIANT Safe Fresh\n"),
            cfg("M_reach", "M", "INVARIANT Reach\n"),
            cfg("M_c", "M", "INVARIANT Safe Fresh\n"),
        ];
        let safe = || Said::Violated(Some("Safe".into()));
        let reach = Said::Violated(Some("Reach".into()));
        let said = [Said::Passed, safe(), safe(), reach, Said::Passed];
        let inductive = |_: &str| INDUCTIVE.to_string();
        let alone = engines::Split {
            apalache: BTreeSet::from(["M".to_string()]),
            ..engines::Split::default()
        };
        let asks = unnamed(&cfgs, &said, &alone, &inductive);
        let asks: Vec<_> = asks
            .iter()
            .map(|(cfg, name)| (cfg.stem.as_str(), name.as_str()))
            .collect();
        assert_eq!(asks, [("M_a", "Fresh"), ("M_b", "Fresh")]);
        let both = engines::Split::default();
        assert!(unnamed(&cfgs, &said, &both, &inductive).is_empty());
        let named = |also: &[Ask]| {
            let found = uncovered(Path::new(""), &cfgs, &said, also, &inductive);
            found.into_iter().map(|it| it.message).collect::<Vec<_>>()
        };
        let left = "lists Fresh, and no bad variant of M violates it";
        assert_eq!(named(&[]), [left]);
        let [own, _, variant, ..] = &cfgs;
        assert!(named(&[(variant, "Fresh".into())]).is_empty());
        assert_eq!(named(&[(own, "Fresh".into())]), [left]);
        let pair = |name: &str, parts: &[&str]| {
            (
                name.to_string(),
                parts.iter().map(|it| it.to_string()).collect(),
            )
        };
        let parts = BTreeMap::from([pair("Safe", &["A", "B"]), pair("A", &["Safe"])]);
        assert!(reaches("Safe", "B", &parts, &mut Vec::new()));
        assert!(!reaches("Safe", "C", &parts, &mut Vec::new()));
    }

    #[test]
    fn a_model_without_a_reach_cfg_or_with_an_unrefuted_invariant_is_named() {
        let cfgs = vec![
            cfg("M", "M", "INVARIANT TypeOK Safe Other IndInv\n"),
            cfg("M_bad", "M", "INVARIANT Safe\n"),
            cfg("N_bad", "N", "INVARIANT Other\n"),
        ];
        let other = Said::Violated(Some("Other".into()));
        let said = [Said::Passed, Said::Violated(Some("Safe".into())), other];
        let found = uncovered(Path::new(""), &cfgs, &said, &[], &|_| INDUCTIVE.to_string());
        let items: Vec<_> = found
            .iter()
            .map(|finding| finding.item.clone().unwrap())
            .collect();
        assert_eq!(items, ["M", "Other"]);
        assert!(found[0].message.contains("M_reach.cfg"));
    }

    #[test]
    fn a_bad_variant_shares_the_directory_and_the_model_of_the_own_cfg() {
        let own = cfg("M", "M", "");
        let moved = Cfg {
            dir: PathBuf::from("n"),
            ..cfg("M_bad", "M", "")
        };
        assert!(bad_of(&own, &cfg("M_bad", "M", "")));
        assert!(!bad_of(&own, &moved), "a variant in another directory");
        assert!(
            !bad_of(&own, &cfg("N_bad", "N", "")),
            "a variant of another model"
        );
        assert!(!bad_of(&own, &own), "the own cfg");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_cfg_four_directories_down_is_read_and_one_that_names_no_model_is_a_finding() {
        let files = [
            ("m/M.tla", ""),
            ("m/M_x.cfg", ""),
            ("m/a/b/c/d/N.cfg", ""),
            ("m/a/b/c/d/e/O.cfg", ""),
        ];
        let dir = crate::testdir::tree("tla-discovered", &files);
        let mut holders = Vec::new();
        holders_under(&dir.join("m"), ".cfg", 4, &mut holders);
        assert_eq!(holders, [dir.join("m"), dir.join("m/a/b/c/d")]);
        let (cfgs, findings) = discovered(&dir, &["m".into()]);
        let read: Vec<(&str, Expect)> = cfgs
            .iter()
            .map(|cfg| (cfg.stem.as_str(), cfg.expect))
            .collect();
        assert_eq!(read, [("M_x", Expect::Violation)]);
        assert_eq!(findings, [Finding::at("m/a/b/c/d/N.cfg", NO_MODEL)]);
        let own = cfg_at(&dir, &dir.join("m"), "M".into(), &["M".into()]).unwrap();
        assert_eq!((own.model.as_str(), own.expect), ("M", Expect::Pass));
    }

    #[test]
    fn a_tlc_run_keeps_its_states_and_its_trace_spec_out_of_the_model_directory() {
        let (jar, meta) = (Path::new("t.jar"), Path::new("s/0-M_x"));
        let run = tlc(&Tlc { jar, traces: None }, &cfg("M_x", "M", ""), meta, 3, 9);
        let argv = "-XX:+UseParallelGC -Xmx2g -cp t.jar tlc2.TLC -workers 3 -cleanup \
                    -metadir s/0-M_x -config M_x.cfg M.tla";
        assert_eq!(
            (run.dir, run.argv.join(" "), run.limit),
            (PathBuf::from("m"), argv.into(), 9)
        );
        let quiet = Tlc {
            jar,
            traces: Some("-q"),
        };
        let quiet = tlc(&quiet, &cfg("M_x", "M", ""), meta, 3, 9);
        assert_eq!(
            quiet.argv.join(" "),
            argv.replace(" -config", " -q -config")
        );
    }

    #[test]
    fn tlc_from_1_8_is_told_to_write_no_trace_spec_and_an_older_one_is_not() {
        let flag = Some("-noGenerateSpecTE");
        assert_eq!(no_trace_spec("1.7.4"), None);
        assert_eq!(no_trace_spec("1.8.0"), flag);
        assert_eq!(no_trace_spec("1.10.0-pre"), flag);
        assert_eq!((no_trace_spec("2"), no_trace_spec("nightly")), (flag, None));
    }

    /// A TLC and an Apalache in one: `-version` passes, each Apalache step exits as its flags
    /// say, and TLC exits as the cfg after `-config` says.
    const FAKE: &str = r#"shift
case "$*" in
  -version) exit 0 ;;
  *"--init=IndInv --inv=IndInv"*|*--length=5*) exit 12 ;;
  *"--init=IndInv --length=0"*) echo 'Parse failed   E@12:00:01.1'; exit 1 ;;
  *--inv=IndInv*) exit 0 ;;
esac
prev=
for arg; do [ "$prev" = -config ] && cfg=$arg; prev=$arg; done
case "$cfg" in
  *_reach.cfg) name=Reach ;;
  Spec_loss.cfg) name=NoLoss ;;
  Spec_tear.cfg) name=NoTear ;;
  Lone_hold.cfg) name=Inv ;;
  Spec_fair.cfg) exit 13 ;;
  Spec_stuck.cfg|Lone.cfg) exit 11 ;;
  Spec_broken.cfg) printf 'Error: parse\nline 3\n'; exit 255 ;;
  *) exit 0 ;;
esac
echo "Error: Invariant $name is violated."; exit 12
"#;

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn each_cfg_is_judged_each_model_covered_and_each_inductive_invariant_proved() {
        let tree = [
            ("proofs/tla/Lone.tla", "---- MODULE Lone ----\n====\n"),
            ("proofs/tla/Lone.cfg", "INVARIANT Inv\n"),
            (
                "proofs/tla/Lone_hold.cfg",
                "\\* expect: deadlock\nINVARIANT Inv\n",
            ),
            ("proofs/tla/Nomodel.cfg", ""),
            (
                "proofs/tla/Spec.tla",
                "Safe == NoLoss /\\ NoTear\nIndInv == TypeOK /\\ Safe\n",
            ),
            ("proofs/tla/Spec.cfg", "INVARIANT TypeOK Safe IndInv\n"),
            ("proofs/tla/Spec_broken.cfg", "\\* expect: pass\n"),
            ("proofs/tla/Spec_fair.cfg", "PROPERTIES Live\n"),
            ("proofs/tla/Spec_live.cfg", "PROPERTY Live\n"),
            ("proofs/tla/Spec_loss.cfg", "INVARIANT NoLoss\n"),
            ("proofs/tla/Spec_odd.cfg", "\\* expect: maybe\n"),
            ("proofs/tla/Spec_reach.cfg", "INVARIANT Reach\n"),
            ("proofs/tla/Spec_stuck.cfg", "\\* expect: deadlock\n"),
            ("proofs/tla/Spec_tear.cfg", "INVARIANTS NoTear\n"),
            ("proofs/tla/sub/deeper/Deep.tla", ""),
            ("proofs/tla/sub/deeper/Deep.cfg", "INIT Init\n"),
            ("proofs/tla/states/X.cfg", ""),
            ("proofs/tla/.hidden/Y.cfg", ""),
            ("proofs/tla/a/b/c/d/e/Far.cfg", ""),
        ];
        let mut ctx = pinned("gate-tla-run", &tree, FAKE);
        ctx.tla.length = Some(5);
        let outcome = check(&ctx).unwrap();
        assert!(!outcome.passed);
        assert!(!ctx.root.join(SCRATCH).exists(), "the scratch stayed");
        let found = told(&outcome);
        let (lone, spec) = ("proofs/tla/Lone.cfg", "proofs/tla/Spec.cfg");
        let reach =
            "has no Lone_reach.cfg, so nothing shows that the states it guards are ever reached";
        let deep = reach.replace("Lone", "Deep");
        let held = "IndInv is not inductive: one step from a state that holds it reaches one that does not";
        let expected: [(&str, Option<&str>, &str); 11] = [
            ("proofs/tla/Nomodel.cfg", None, NO_MODEL),
            (
                "proofs/tla/Spec_odd.cfg",
                None,
                "`expect: maybe` is not pass, deadlock or violation",
            ),
            (lone, Some("Lone"), "must pass, and TLC reaches a deadlock"),
            (
                "proofs/tla/Lone_hold.cfg",
                Some("Lone"),
                "must deadlock, as its `expect: deadlock` says, and TLC finds Inv violated",
            ),
            (
                "proofs/tla/Spec_broken.cfg",
                Some("Spec"),
                "TLC could not check it: exit 255: Error: parse line 3",
            ),
            (lone, Some("Lone"), reach),
            (
                lone,
                Some("Inv"),
                "lists Inv, and no bad variant of Lone violates it",
            ),
            ("proofs/tla/sub/deeper/Deep.cfg", Some("Deep"), &deep),
            (spec, Some("Spec"), held),
            (
                spec,
                Some("Spec"),
                "Apalache could not check it: exit 1: Parse failed",
            ),
            (
                spec,
                Some("Spec"),
                "an invariant Spec.cfg lists fails within 5 steps of Init",
            ),
        ];
        assert_eq!(found, expected);
        ctx.scope.models = vec!["Lone".to_string()];
        let narrowed = check(&ctx).unwrap();
        let models: BTreeSet<Option<&str>> = narrowed
            .findings
            .iter()
            .filter(|it| it.file.starts_with("proofs/tla/Lone"))
            .map(|it| it.item.as_deref())
            .collect();
        assert_eq!(models, BTreeSet::from([Some("Lone"), Some("Inv")]));
        let others = narrowed
            .findings
            .iter()
            .filter(|it| !it.file.starts_with("proofs/tla/Lone"));
        let unread: Vec<&str> = others.map(|it| it.file.as_str()).collect();
        assert_eq!(
            unread,
            ["proofs/tla/Nomodel.cfg", "proofs/tla/Spec_odd.cfg"]
        );
        ctx.scope.models = vec!["Lone".to_string(), "Gone".to_string()];
        let gone =
            "`--model` names Gone, and no cfg and no theorem under proofs/tla is of that model";
        assert_eq!(check(&ctx).unwrap_err(), gone);
    }

    /// A project of `tree` whose Java runs `fake`, with TLC and Apalache pinned.
    fn pinned(name: &str, tree: &[(&str, &str)], fake: &str) -> crate::testdir::Held {
        let mut ctx = crate::testdir::Held::tree(name, tree);
        ctx.tools = crate::testdir::faked(&ctx.root, fake);
        let pins = [("TLA_TOOLS", "tla2tools.jar"), ("APALACHE", "apalache.jar")];
        let pins = pins.map(|(stem, file)| crate::testdir::fetched(&ctx.root, stem, file));
        std::fs::write(ctx.root.join(crate::project::PIN_FILE), pins.concat()).unwrap();
        ctx
    }

    /// Each finding of `outcome` as its file, its item and its message.
    fn told(outcome: &Outcome) -> Vec<(&str, Option<&str>, &str)> {
        let each = outcome.findings.iter();
        each.map(|it| (it.file.as_str(), it.item.as_deref(), it.message.as_str()))
            .collect()
    }

    const FAKE_APALACHE: &str = r#"shift
case "$*" in
  -version) exit 0 ;;
  *--init=*|*--inv=IndInv*|*Duo_loss.cfg*--inv=NoTear*|*Duo_lax.cfg*) exit 0 ;;
  *Duo_stale.cfg*--inv=Fresh*) echo 'Bad Fresh   E@12:00:01.1'; exit 1 ;;
  *--inv=*|*Duo_mute.cfg*) exit 12 ;;
  *Duo_reach.cfg*) name=Reach ;;
  *) name=NoLoss ;;
esac
echo "verification conditions from the invariant $name"
printf 'VCGen produced 1\nState 0: state invariant 0 violated.\n'
exit 12
"#;

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn apalache_alone_refutes_each_variant_and_checks_alone_each_invariant_it_left_unnamed() {
        let cfgs = [
            ("Duo", "TypeOK Safe Fresh IndInv"),
            ("Duo_lax", "NoTear"),
            ("Duo_loss", "NoLoss NoTear"),
            ("Duo_mute", "NoLoss"),
            ("Duo_reach", "Reach"),
            ("Duo_stale", "NoLoss Fresh"),
            ("Duo_tear", "NoLoss Safe"),
        ];
        let cfgs = cfgs.map(|(stem, names)| {
            let text = format!("INVARIANT {names}\nCHECK_DEADLOCK FALSE\n");
            (format!("proofs/tla/{stem}.cfg"), text)
        });
        let module = "Safe == NoLoss /\\ NoTear\nIndInv == TypeOK /\\ Safe\n";
        let mut tree = vec![("proofs/tla/Duo.tla", module)];
        tree.extend(
            cfgs.iter()
                .map(|(path, text)| (path.as_str(), text.as_str())),
        );
        let mut ctx = pinned("gate-tla-apalache", &tree, FAKE_APALACHE);
        ctx.tla = serde_json::from_str(r#"{"engines": {"Duo": ["apalache"]}}"#).unwrap();
        let outcome = check(&ctx).unwrap();
        assert!(!ctx.root.join(SCRATCH).exists(), "the scratch stayed");
        let lax = "is a bad variant of Duo, so Apalache must refute it, and Apalache finds no \
                   violation within `tla.length` steps of Init";
        let expected = [
            ("proofs/tla/Duo_lax.cfg", Some("Duo"), lax),
            (
                "proofs/tla/Duo_mute.cfg",
                Some("Duo"),
                "must violate an invariant or property it lists, and Apalache finds an invariant \
                 violated, and its log does not name it",
            ),
            (
                "proofs/tla/Duo_stale.cfg",
                Some("Duo"),
                "checks Fresh alone, and Apalache could not check it: exit 1: Bad Fresh",
            ),
            (
                "proofs/tla/Duo.cfg",
                Some("Fresh"),
                "lists Fresh, and no bad variant of Duo violates it",
            ),
        ];
        assert_eq!(told(&outcome), expected);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_gate_names_the_tool_or_the_setting_it_lacks() {
        let model = [
            ("proofs/tla/M.tla", ""),
            ("proofs/tla/M.cfg", "INVARIANT IndInv\n"),
            ("proofs/tla/M_reach.cfg", "INVARIANT Reach\n"),
        ];
        let mut ctx = crate::testdir::Held::tree("gate-tla-tools", &model);
        ctx.tools = crate::testdir::faked(&ctx.root, "exit 1\n");
        assert_eq!(
            check(&ctx).unwrap_err(),
            crate::gates::fixes::not_installed("java")
        );
        ctx.tools = crate::testdir::faked(&ctx.root, FAKE);
        let unpinned = |name: &str| format!("`{name}`{}", crate::setup::downloads::NOT_PINNED);
        assert_eq!(check(&ctx).unwrap_err(), unpinned("tla-tools"));
        let pin = crate::testdir::fetched(&ctx.root, "TLA_TOOLS", "tla2tools.jar");
        std::fs::write(ctx.root.join(crate::project::PIN_FILE), pin).unwrap();
        let noted = check(&ctx).unwrap();
        assert!(noted.passed);
        let notes: Vec<(&str, &str)> = noted
            .findings
            .iter()
            .map(|it| (it.file.as_str(), it.message.as_str()))
            .collect();
        assert_eq!(notes, [(crate::project::PIN_FILE, apalache::UNPINNED)]);
        let named = |model: &str| Some(BTreeMap::from([(model.to_string(), INDUCTIVE.into())]));
        ctx.tla.inductive = named("M");
        assert_eq!(check(&ctx).unwrap_err(), unpinned("apalache"));
        ctx.tla.inductive = named("Ghost");
        let ghost = "`tla.inductive` names Ghost, and no Ghost.cfg is under the model directories";
        assert_eq!(check(&ctx).unwrap_err(), ghost);
        ctx.tla.inductive = Some(BTreeMap::new());
        let clean = check(&ctx).unwrap();
        assert!(
            clean.passed && clean.findings.is_empty(),
            "{:?}",
            clean.findings
        );
        std::fs::write(
            ctx.root.join("proofs/tla/Proved.tla"),
            "THEOREM T == TRUE\n",
        )
        .unwrap();
        let pin_file = ctx.root.join(crate::project::PIN_FILE);
        let pins = std::fs::read_to_string(&pin_file).unwrap();
        let tlapm = crate::testdir::fetched(&ctx.root, "TLAPM", "tlapm");
        std::fs::write(&pin_file, pins + &tlapm).unwrap();
        let proved = check(&ctx).unwrap();
        let said: Vec<String> = proved.findings.iter().map(Finding::render).collect();
        assert_eq!(
            said,
            ["proofs/tla/Proved.tla: Proved: tlapm gave no verdict on its proofs"]
        );
        ctx.tla.dirs = Some(vec!["nowhere".into()]);
        let none = "no .cfg file under nowhere, so TLC would check nothing";
        assert_eq!(check(&ctx).unwrap_err(), none);
        assert_eq!(asked(&ctx, &["nowhere".into()]).unwrap_err(), none);
    }

    #[test]
    fn a_run_past_its_limit_is_timed_out_and_one_that_did_not_start_is_an_error() {
        let error = |stage| exec::ExecError::of("java", stage, "gone");
        assert_eq!(
            read_tlc(Err(error(exec::Stage::Hung)), 7),
            Ok(Said::TimedOut(7))
        );
        let timed = Said::TimedOut(7).describe(Engine::Tlc);
        assert!(timed.starts_with("TLC ran past its 7 s limit; "), "{timed}");
        let unnamed = Said::Violated(None).describe(Engine::Tlc);
        assert_eq!(unnamed, "TLC finds a safety property violated");
        let lost = error(exec::Stage::Spawn).to_string();
        assert_eq!(read_tlc(Err(error(exec::Stage::Spawn)), 7), Err(lost));
        let settings = crate::project::config::Tla {
            timeouts: [("M_slow".to_string(), 1), ("M".to_string(), 2)].into(),
            timeout_s: Some(3),
            ..Default::default()
        };
        let limits = [("M_slow", "M"), ("M_x", "M"), ("N", "N")]
            .map(|(stem, model)| limit_of(&settings, stem, model));
        assert_eq!(limits, [1, 2, 3]);
        assert_eq!(limit_of(&Default::default(), "N", "N"), TIMEOUT_S);
        assert_eq!(lanes(8, 2), 4);
    }
}
