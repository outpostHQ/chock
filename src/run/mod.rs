//! What a gate is, and the one place a measurement meets its baseline. A ratchet written per gate
//! becomes its own reader, comparer and updater; here a gate supplies numbers and nothing else.

pub mod baseline;
pub mod debt;
pub mod evidence;
pub mod lock;
pub mod report;
pub mod verdicts;

mod context;
pub use context::{
    Coverage, Ctx, LCOV, Runner, coverage_for, default_coverage, default_runner, runner_for,
};

use std::hash::{Hash, Hasher};
use std::time::Instant;

use crate::run::baseline::{Baseline, Keys, Series};
use crate::run::report::{Finding, GateReport, Run, Verdict};

/// What a pass/fail gate found. `passed` is the tool's own verdict rather than a count of
/// findings, because a tool can warn about things that are not failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub passed: bool,
    pub findings: Vec<Finding>,
    /// What the tool wrote, for when no finding could be read from it; the reason quotes its tail.
    pub said: Option<String>,
}

impl Outcome {
    #[must_use]
    pub fn passed() -> Self {
        Self {
            passed: true,
            findings: Vec::new(),
            said: None,
        }
    }

    /// A pass that carries what the tool said. A warn-level diagnostic is not a failure, and
    /// reporting it as nothing hid a live advisory behind a green gate.
    #[must_use]
    pub fn noted(findings: Vec<Finding>) -> Self {
        Self {
            passed: true,
            findings,
            said: None,
        }
    }

    #[must_use]
    pub fn failed(findings: Vec<Finding>) -> Self {
        Self {
            passed: false,
            findings,
            said: None,
        }
    }

    /// Everything the tool wrote, kept so a failure no finding could be read from still shows it.
    #[must_use]
    pub fn saying(mut self, out: &crate::exec::Output) -> Self {
        self.said = Some(format!("{}{}", out.stdout, out.stderr));
        self
    }
}

/// `Err` is always "could not run", never "found something". A gate that reports a tool crash as a
/// clean result is the one failure a quality system cannot afford.
pub type Measure = fn(&Ctx) -> Result<Series, String>;
pub type Check = fn(&Ctx) -> Result<Outcome, String>;

/// A measured series with scope notes that must survive comparison and baseline recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measurement {
    pub series: Series,
    pub findings: Vec<Finding>,
}

impl Measurement {
    /// Baseline output keeps scope notes visible without recording them as measured debt.
    #[must_use]
    pub fn notes(&self, gate: &str) -> String {
        self.findings
            .iter()
            .map(|finding| format!("  note      {gate:<12} {}\n", finding.render()))
            .collect()
    }
}

pub type AnnotatedMeasure = fn(&Ctx) -> Result<Measurement, String>;

/// Existing debt may be adopted; blockers still fail even when the debt has a baseline.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Inspection {
    pub debt: Vec<Finding>,
    pub blockers: Vec<Finding>,
}

impl Inspection {
    #[must_use]
    pub fn debt(findings: Vec<Finding>) -> Self {
        Self {
            debt: findings,
            blockers: Vec::new(),
        }
    }

    fn series(&self) -> Series {
        let mut series = Series::new();
        for finding in &self.debt {
            let key = match &finding.item {
                Some(item) => format!("{}#{item}", finding.file),
                None => finding.file.clone(),
            };
            series.set(&key, series.get(&key).unwrap_or(0) + 1);
        }
        series
    }

    pub(crate) fn outcome(self) -> Outcome {
        let findings = self
            .blockers
            .into_iter()
            .chain(self.debt)
            .collect::<Vec<_>>();
        if findings.is_empty() {
            Outcome::passed()
        } else {
            Outcome::failed(findings)
        }
    }
}

pub type Inspect = fn(&Ctx) -> Result<Inspection, String>;

#[derive(Debug)]
pub enum Kind {
    /// The tool decides. chock normalises its exit code and nothing more.
    Binary(Check),
    /// Without a baseline this preserves strict enforcement; recording debt is an explicit choice.
    Debt {
        inspect: Inspect,
        unit: &'static str,
    },
    /// chock decides, by comparing what the gate measured against the committed baseline.
    Ratchet {
        measure: Measure,
        keys: Keys,
        unit: &'static str,
    },
    /// Scope advisories travel with the numbers; they never become keys or change the verdict.
    AnnotatedRatchet {
        measure: AnnotatedMeasure,
        keys: Keys,
        unit: &'static str,
    },
}

/// Which list a gate belongs to. An instrument only reports, so it is never enforced or turned on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Correctness. Everything that must pass before a change lands.
    Gates,
    /// Shape. Ratchets and supply chain.
    Quality,
    /// Reported, never enforced.
    Instrument,
    /// A real gate whose cost — hours, or a second toolchain — means it is asked for, not assumed.
    OptIn,
    /// A gate about chock's own wiring. `init` creates that wiring, so on a tree `init` has not
    /// finished, it would only report what `init` is there to fix.
    Setup,
}

#[derive(Debug)]
pub struct Gate {
    pub name: &'static str,
    pub about: &'static str,
    pub group: Group,
    /// Whether answering costs a compile. It decides which gates a commit can wait for, and it is
    /// a fact about the gate, not a guess by the hook script.
    pub builds: bool,
    /// What the gate reads, so a run may answer from an earlier verdict. `None` is never recalled,
    /// and is right until somebody shows the list is complete.
    pub reads: Option<crate::run::verdicts::Reads>,
    pub kind: Kind,
}

/// One run of the project's suite, shared by the gates that begin with one.
pub type FirstRun = std::sync::Arc<std::sync::OnceLock<Result<crate::exec::Output, String>>>;

impl Gate {
    /// What this gate counts in, or `None` where it answers pass or fail. It takes any gate, so
    /// both answers are reachable; a caller that filters to ratchets first has a dead arm.
    #[must_use]
    pub fn counts_in(&self) -> Option<&'static str> {
        match self.kind {
            Kind::Ratchet { unit, .. }
            | Kind::AnnotatedRatchet { unit, .. }
            | Kind::Debt { unit, .. } => Some(unit),
            Kind::Binary(_) => None,
        }
    }
}

/// Measure one ratchet without judging it, which is what `chock baseline` records.
pub fn measure(gate: &Gate, ctx: &Ctx) -> Result<Series, String> {
    measured(gate, ctx).map(|read| read.series)
}

/// Baseline recording and verdicts consume the same measurement, including scope exclusions.
pub fn measured(gate: &Gate, ctx: &Ctx) -> Result<Measurement, String> {
    match gate.kind {
        Kind::Ratchet { measure, .. } => measure(ctx).map(|series| Measurement {
            series,
            findings: Vec::new(),
        }),
        Kind::AnnotatedRatchet { measure, .. } => measure(ctx),
        Kind::Debt { inspect, .. } => adoptable(inspect(ctx)?),
        Kind::Binary(_) => Err(format!("{} is not a ratchet; it has no number", gate.name)),
    }
}

/// One chock inside another is a test harness exercising the real binary, which is legitimate.
/// Two is the `test` gate running the suite that runs the gate, which never terminates.
pub const MAX_DEPTH: u32 = 2;

/// How to run this one check again. It must mean the same everywhere: in chock's first adopter,
/// `just deps` ran a different tool than the `deps` gate.
#[must_use]
pub fn rerun(name: &str) -> String {
    format!("chock run {name}")
}

/// The first tool a gate declared that answers no version. The gate already declares these for the
/// recall key, so there is no second list.
fn absent_tool(gate: &Gate, ctx: &Ctx) -> Option<&'static str> {
    gate.reads?
        .tools
        .iter()
        .copied()
        .find(|tool| crate::run::verdicts::installed(&ctx.root, tool).is_none())
}

/// What this gate's verdict would be about, or `None` where it declares no inputs or one of them
/// could not be read. Either way there is then nothing to answer from, so the gate runs.
fn keyed(gate: &Gate, ctx: &Ctx) -> Option<String> {
    let reads = gate.reads?;
    let key = crate::run::verdicts::key(
        reads,
        gate.name,
        &ctx.root,
        &ctx.listing,
        &|tool| crate::run::verdicts::installed(&ctx.root, tool),
        &ctx.runner.tools,
    );
    key.map(|key| key_format(gate, reads.reader, &key))
        .and_then(|key| with_change_set(gate, ctx, key))
}

/// The same tree can answer differently once the change set moves, so a gate held clean where
/// touched or narrowed to the change keys on it too, and keeps nothing when it cannot be read.
fn with_change_set(gate: &Gate, ctx: &Ctx, key: String) -> Option<String> {
    let narrowed = gate.reads.is_some_and(|reads| reads.change_set);
    if !clean_when_touched(gate, ctx) && !narrowed {
        return Some(key);
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ctx.changed().ok()?.hash(&mut hasher);
    Some(format!("{key}:touched-{:016x}", hasher.finish()))
}

/// Reader and invocation changes must not reuse earlier answers while the package version is fixed.
/// The gate names its own reader; carrying scope notes changes what any ratchet's answer holds.
fn key_format(gate: &Gate, reader: &str, key: &str) -> String {
    let scoped = matches!(gate.kind, Kind::AnnotatedRatchet { .. }).then_some("scope-v2");
    // A recalled pass lowers no record, so one kept before records were lowered must not answer.
    let lowers = gate.counts_in().map(|_| "lowers-v1");
    let parts: Vec<&str> = [reader]
        .into_iter()
        .filter(|reader| !reader.is_empty())
        .chain(scoped)
        .chain(lowers)
        .chain([key])
        .collect();
    parts.join(":")
}

pub fn run_one(gate: &Gate, ctx: &Ctx) -> GateReport {
    if ctx.depth >= MAX_DEPTH {
        return GateReport::cannot_run(
            gate.name,
            rerun(gate.name).as_str(),
            "refusing to recurse: chock is already two gates deep",
        );
    }
    let started = Instant::now();
    let key = keyed(gate, ctx);
    if let Some(mut report) = key
        .as_deref()
        .and_then(|key| crate::run::verdicts::recall(&ctx.root, gate.name, key))
    {
        // Marked, so a recalled verdict never reads as one taken now.
        report.recalled = true;
        report.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        return report;
    }
    let mut report = judged(gate, ctx);
    report.fix = advice(&report);
    report.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if let Some(key) = key.as_deref() {
        crate::run::verdicts::keep(&ctx.root, gate.name, key, &report);
    }
    report
}

/// What to do about a gate that tripped; a pass or a refusal needs no fix of the code.
fn advice(report: &GateReport) -> Option<String> {
    let hint = crate::gates::fixes::fix(&report.gate)?;
    (report.verdict == Verdict::Tripped).then(|| hint.to_string())
}

fn judged(gate: &Gate, ctx: &Ctx) -> GateReport {
    match &gate.kind {
        Kind::Binary(check) => binary(gate, ctx, *check),
        Kind::Debt { inspect, unit } => debt_report(gate, ctx, *inspect, unit),
        Kind::Ratchet { keys, unit, .. } | Kind::AnnotatedRatchet { keys, unit, .. } => {
            ratchet(gate, ctx, *keys, unit)
        }
    }
}

/// Hears each gate that compiles as it ends, and each other gate that did not pass.
pub type Told<'a> = &'a (dyn Fn(&GateReport) + Sync);

/// `told` hears each gate that compiles as it ends. The rest take seconds, so it hears those that
/// did not pass once they have all run, before the first build starts.
pub fn run_all(gates: &[&Gate], ctx: &Ctx, chock_version: &str, told: Told) -> Run {
    let quick = side_by_side(gates, ctx, ctx.jobs.clamp(1, 8));
    quick
        .iter()
        .flatten()
        .filter(|report| report.verdict != Verdict::Pass)
        .for_each(told);
    let compiled = in_lanes(gates, ctx, told);
    let reports = gates
        .iter()
        .zip(quick.into_iter().zip(compiled))
        .map(|(gate, (quick, compiled))| {
            quick
                .or(compiled)
                .unwrap_or_else(|| in_turn(gate, ctx, told))
        })
        .collect();
    Run::new(chock_version, reports)
}

/// With jobs for two lanes, the gates that compile: the gates that build apart in turn on one lane,
/// the rest in turn on the other. Otherwise every slot stays empty.
fn in_lanes(gates: &[&Gate], ctx: &Ctx, told: Told) -> Vec<Option<GateReport>> {
    let width = crate::exec::budget::lanes(ctx.jobs);
    let lanes = if width > 1 {
        lanes_of(gates, ctx)
    } else {
        Vec::new()
    };
    on_workers(gates, &lanes, width, &|gate| {
        let _share = crate::exec::budget::Lane::enter();
        in_turn(gate, ctx, told)
    })
}

/// The two lanes as indexes into `gates`, in order: the compiling gates that share the suite's
/// target directory, then those that build apart, so no two release builds run at once.
fn lanes_of(gates: &[&Gate], ctx: &Ctx) -> Vec<Vec<usize>> {
    let (apart, beside): (Vec<usize>, Vec<usize>) = gates
        .iter()
        .enumerate()
        .filter(|(_, gate)| gate.builds)
        .map(|(at, _)| at)
        .partition(|&at| crate::gates::builds_apart(gates[at].name, &ctx.build));
    vec![beside, apart]
}

fn in_turn(gate: &Gate, ctx: &Ctx, told: Told) -> GateReport {
    let report = run_one(gate, ctx);
    told(&report);
    report
}

/// The gates that need no compiler, run by `workers` threads at once; the slots of the rest stay
/// empty, for them to run one at a time.
fn side_by_side(gates: &[&Gate], ctx: &Ctx, workers: usize) -> Vec<Option<GateReport>> {
    let quick: Vec<Vec<usize>> = gates
        .iter()
        .enumerate()
        .filter(|(_, gate)| !gate.builds)
        .map(|(at, _)| vec![at])
        .collect();
    on_workers(gates, &quick, workers, &|gate| run_one(gate, ctx))
}

/// Runs `lanes`, indexes into `items`, on `workers` threads: each lane on one thread in order. An
/// item in no lane keeps an empty slot.
fn on_workers<T: Sync, R: Send>(
    items: &[T],
    lanes: &[Vec<usize>],
    workers: usize,
    run: &(dyn Fn(&T) -> R + Sync),
) -> Vec<Option<R>> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<R>>> =
        items.iter().map(|_| std::sync::Mutex::default()).collect();
    std::thread::scope(|scope| {
        for _ in 0..workers.min(lanes.len()) {
            scope.spawn(|| {
                while let Some(lane) =
                    lanes.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                {
                    for (item, slot) in lane
                        .iter()
                        .filter_map(|at| Some((items.get(*at)?, slots.get(*at)?)))
                    {
                        let done = run(item);
                        *slot
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(done);
                    }
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        })
        .collect()
}

/// What every gate can measure about a tree, not how it compares to a record. It reads no baseline,
/// recalls or keeps no verdict, and writes nothing, so a tree chock does not own stays as it is.
pub fn survey_all(gates: &[&Gate], ctx: &Ctx, chock_version: &str) -> Run {
    let reports = gates.iter().map(|gate| survey_one(gate, ctx)).collect();
    Run::new(chock_version, reports)
}

/// `passed` here means the gate measured something; there is no record to meet. A pass/fail check
/// answers about the tree alone, so it runs as it always does.
fn survey_one(gate: &Gate, ctx: &Ctx) -> GateReport {
    let started = Instant::now();
    let mut report = observed(gate, ctx);
    report.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    report
}

fn observed(gate: &Gate, ctx: &Ctx) -> GateReport {
    match &gate.kind {
        Kind::Binary(check) => binary(gate, ctx, *check),
        Kind::Debt { inspect, .. } => outcome_report(gate, inspect(ctx).map(Inspection::outcome)),
        Kind::Ratchet { unit, .. } | Kind::AnnotatedRatchet { unit, .. } => {
            surveyed(gate, ctx, unit)
        }
    }
}

/// A ratchet's numbers with the comparison left out. The tool check stays: a gate whose tool is
/// absent measured nothing, and calling that zero would be a false pass.
fn surveyed(gate: &Gate, ctx: &Ctx, unit: &str) -> GateReport {
    if let Some(missing) = absent_tool(gate, ctx) {
        return GateReport::cannot_run(
            gate.name,
            rerun(gate.name).as_str(),
            &format!("`{missing}` is not installed, so this gate has nothing to run"),
        );
    }
    match measured(gate, ctx) {
        Err(reason) => GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &reason),
        Ok(read) => {
            let mut report = GateReport::new(gate.name, Verdict::Pass, rerun(gate.name).as_str());
            report.measured = Some(read.series.0.values().sum());
            report.unit = Some(unit.to_string());
            report.findings = read.findings;
            report
        }
    }
}

fn adoptable(inspection: Inspection) -> Result<Measurement, String> {
    if !inspection.blockers.is_empty() {
        return Err(format!(
            "cannot baseline unresolved correctness failures: {}",
            inspection
                .blockers
                .iter()
                .map(Finding::render)
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    Ok(Measurement {
        series: inspection.series(),
        findings: inspection.debt,
    })
}

fn debt_report(gate: &Gate, ctx: &Ctx, inspect: Inspect, unit: &str) -> GateReport {
    let inspection = match inspect(ctx) {
        Ok(inspection) => inspection,
        Err(why) => return outcome_report(gate, Err(why)),
    };
    if !inspection.blockers.is_empty() || !ctx.baseline.has(gate.name) {
        return outcome_report(gate, Ok(inspection.outcome()));
    }
    let read = Measurement {
        series: inspection.series(),
        findings: inspection.debt,
    };
    measured_ratchet(gate, ctx, read, Keys::Items, unit)
}

fn binary(gate: &Gate, ctx: &Ctx, check: Check) -> GateReport {
    outcome_report(gate, check(ctx))
}

pub(crate) fn outcome_report(gate: &Gate, outcome: Result<Outcome, String>) -> GateReport {
    match outcome {
        Err(reason) => GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &reason),
        // A passing gate keeps its findings: they are advisories.
        Ok(outcome) if outcome.passed => {
            let mut report = GateReport::new(gate.name, Verdict::Pass, rerun(gate.name).as_str());
            report.findings = outcome.findings;
            report
        }
        Ok(outcome) if outcome.findings.is_empty() => GateReport::cannot_run(
            gate.name,
            rerun(gate.name).as_str(),
            &said_nothing(outcome.said.as_deref()),
        ),
        Ok(outcome) => {
            let mut report =
                GateReport::new(gate.name, Verdict::Tripped, rerun(gate.name).as_str());
            report.findings = outcome.findings;
            report
        }
    }
}

/// A parser fed a format that has moved returns nothing while the exit code still says the tool
/// objected. Held centrally so no gate can forget it.
const SAID_NOTHING: &str = "the tool objected but chock could read no finding out of its output";

/// How much of a tool's own output travels with that reason. Enough to hold a rustc diagnostic and
/// the note under it; not so much that a reader scrolls past a build log to find it.
const QUOTED_LINES: usize = 20;

/// The reason for a failure nothing could be read out of, with the tail of what the tool wrote,
/// because that output is the only thing a reader can act on.
fn said_nothing(said: Option<&str>) -> String {
    let Some(tail) = said.map(tail_of).filter(|tail| !tail.is_empty()) else {
        return format!("{SAID_NOTHING}, so there is nothing here to act on");
    };
    // The tail is often a build script's `cargo:` lines; muxel's missing glib was 20 lines above.
    let marked = said
        .and_then(crate::exec::marked)
        .map(|line| format!(" What it marked as the error: {line}."))
        .unwrap_or_default();
    format!("{SAID_NOTHING}.{marked} Its last {QUOTED_LINES} line(s):\n{tail}")
}

/// The last lines a tool wrote, blank ones dropped, in the order it wrote them.
fn tail_of(said: &str) -> String {
    let kept: Vec<&str> = said
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    kept[kept.len().saturating_sub(QUOTED_LINES)..].join("\n")
}

/// Why this run cannot be compared with the record, if it cannot. Sharing no key at all is a reader
/// that changed, not a tree that got worse.
#[must_use]
fn rekeyed(now: &Series, was: &Series, keys: Keys) -> Option<String> {
    if was.is_empty() {
        return None;
    }
    if now.is_empty() {
        // A gone item or measure is paid debt. Only a census owes a row for every key, so only
        // there does measuring nothing mean the gate stopped answering.
        return match keys {
            Keys::Items | Keys::Sizes { .. } | Keys::Measures => None,
            Keys::Census => Some(format!(
                "nothing was measured, and the baseline records {} key(s) — that is a gate that \
                 stopped answering, not a tree that got better",
                was.len()
            )),
        };
    }
    if now.0.keys().any(|key| was.get(key).is_some()) {
        return None;
    }
    let mine = now.0.keys().next().map_or("", String::as_str);
    let theirs = was.0.keys().next().map_or("", String::as_str);
    Some(format!(
        "this run shares no key with the baseline, so nothing can be compared: it measured \
         `{mine}` where the record holds `{theirs}`. The keys changed rather than the tree — \
         re-record with `chock baseline <gate>`"
    ))
}

/// The ratchet, in the only place it is written. A gate with no recorded baseline cannot rule on
/// the tree, so it reports what it measured and says it could not run.
fn ratchet(gate: &Gate, ctx: &Ctx, keys: Keys, unit: &str) -> GateReport {
    // A missing tool is said before a missing baseline: recording one would not help.
    if let Some(missing) = absent_tool(gate, ctx) {
        let mut report = GateReport::cannot_run(
            gate.name,
            rerun(gate.name).as_str(),
            &format!("`{missing}` is not installed, so this gate has nothing to run"),
        );
        report.unit = Some(unit.to_string());
        return report;
    }
    // Asked before measuring, which can take minutes of release builds.
    if !ctx.baseline.has(gate.name) && !strict(gate, ctx) {
        let mut report = GateReport::cannot_run(
            gate.name,
            rerun(gate.name).as_str(),
            &format!(
                "no baseline recorded — `chock baseline {}` records one, or says why it cannot",
                gate.name
            ),
        );
        report.unit = Some(unit.to_string());
        return report;
    }
    let read = match measured(gate, ctx) {
        Ok(read) => read,
        Err(reason) => {
            return GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &reason);
        }
    };
    measured_ratchet(gate, ctx, read, keys, unit)
}

fn measured_ratchet(
    gate: &Gate,
    ctx: &Ctx,
    read: Measurement,
    keys: Keys,
    unit: &str,
) -> GateReport {
    let now = read.series;
    if let Err(why) = portable(&now) {
        return GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &why);
    }
    let total = now.0.values().sum();
    let was = held_against(gate, ctx, &now);
    // A reader that re-keys makes every old key absent and every new one look like new debt.
    if !matches!(gate.kind, Kind::Debt { .. })
        && let Some(reason) = rekeyed(&now, &was, keys)
    {
        return GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &reason);
    }
    // A gate that changes what it counts keeps its keys, so only the unit tells the numbers apart.
    if let Some(before) = ctx
        .baseline
        .unit(gate.name)
        .filter(|before| *before != unit)
    {
        return GateReport::cannot_run(
            gate.name,
            rerun(gate.name).as_str(),
            &format!(
                "this run counts {unit} where the record holds {before}, so the numbers are not \
                 comparable — re-record with `chock baseline <gate>`"
            ),
        );
    }
    let (verdict, findings) = compared(&now, &was, keys, gate.name, unit);
    let mut report = GateReport::new(gate.name, verdict, rerun(gate.name).as_str());
    report.measured = Some(total);
    report.baseline = Some(was.0.values().sum());
    report.unit = Some(unit.to_string());
    report.findings = findings;
    report.findings.extend(read.findings);
    if let Err(why) = touched(&mut report, gate, ctx, &now, &was, keys) {
        return GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &why);
    }
    lock_in(&mut report, gate, ctx, now.tightened(&was, keys), &was);
    report
}

/// Debt the record allows in a file the change touched, where the project holds such files to zero.
/// Debt over the record is a finding already.
fn touched(
    report: &mut GateReport,
    gate: &Gate,
    ctx: &Ctx,
    now: &Series,
    was: &Series,
    keys: Keys,
) -> Result<(), String> {
    if keys != Keys::Items || !clean_when_touched(gate, ctx) {
        return Ok(());
    }
    let changed = ctx
        .changed()
        .map_err(|why| format!("`clean_when_touched` needs the changed files: {why}"))?;
    let unit = report.unit.clone().unwrap_or_default();
    let held: Vec<Finding> = now
        .0
        .iter()
        .filter(|(key, count)| **count > 0 && **count <= was.get(key).unwrap_or_default())
        .filter(|(key, _)| {
            let file = key.split_once('#').map_or(key.as_str(), |(path, _)| path);
            changed.iter().any(|path| path == file)
        })
        .map(|(key, count)| {
            let said = format!("{count} {unit} in a file this change touched: `clean_when_touched` holds it to zero");
            placed(key, &said).numbers(*count, was.get(key))
        })
        .collect();
    if !held.is_empty() {
        report.verdict = Verdict::Tripped;
        report.exit_code = Verdict::Tripped.code();
        report.findings.extend(held);
    }
    Ok(())
}

/// Whether the project holds this gate to zero in each file the change touched.
fn clean_when_touched(gate: &Gate, ctx: &Ctx) -> bool {
    ctx.clean_when_touched.iter().any(|name| name == gate.name)
}

/// Whether the project holds this gate to zero rather than to its record.
fn strict(gate: &Gate, ctx: &Ctx) -> bool {
    ctx.strict.iter().any(|name| name == gate.name)
}

/// What a run is compared against: the record, or zero for every key a strict gate measured.
fn held_against(gate: &Gate, ctx: &Ctx, now: &Series) -> Series {
    match strict(gate, ctx) {
        true => now.zeroed(),
        false => ctx.baseline.gate(gate.name),
    }
}

/// A passing gate whose record went down keeps the lower number for the run to write. CI writes
/// nothing, so there each gain the committed record is missing fails the gate instead.
fn lock_in(report: &mut GateReport, gate: &Gate, ctx: &Ctx, tighter: Option<Series>, was: &Series) {
    let passed = report.verdict == Verdict::Pass;
    let Some(tighter) = tighter.filter(|_| passed && crate::gates::settles_anywhere(gate.name))
    else {
        return;
    };
    if !ctx.ci {
        report.tightened = Some(tighter);
        return;
    }
    report.verdict = Verdict::Tripped;
    report.exit_code = Verdict::Tripped.code();
    let unit = report.unit.clone().unwrap_or_default();
    report.findings.extend(unrecorded(&tighter, was, &unit));
}

/// Each key whose committed record is above what this run measured, where an editor can open it.
fn unrecorded(tighter: &Series, was: &Series, unit: &str) -> Vec<Finding> {
    was.0
        .iter()
        .filter(|(key, held)| tighter.get(key) != Some(**held))
        .map(|(key, held)| {
            let now = tighter.get(key).unwrap_or_default();
            let said = format!(
                "{now} {unit} where the record holds {held}: `chock run` lowers the record, and \
                 the change has to commit it"
            );
            placed(key, &said).numbers(now, Some(*held))
        })
        .collect()
}

/// The record with every lower number this run measured written in, and the gates it lowered;
/// `None` when no gate went down.
#[must_use]
pub fn locked_in(held: &Baseline, reports: &[GateReport]) -> Option<(Baseline, Vec<String>)> {
    let mut record = held.clone();
    let mut lowered = Vec::new();
    for report in reports {
        if let Some(series) = &report.tightened {
            record.set(&report.gate, series.clone());
            lowered.push(report.gate.clone());
        }
    }
    (!lowered.is_empty()).then_some((record, lowered))
}

/// What the comparison says, once it is known to be possible. Each of its three questions goes
/// only to the key kinds it fits, and only the first two can fail the gate.
fn compared(
    now: &Series,
    was: &Series,
    keys: Keys,
    gate: &str,
    unit: &str,
) -> (Verdict, Vec<Finding>) {
    let regressions = now.regressions(was, keys);
    // A census row on record that this run did not produce is a check that stopped happening, and
    // no number reports it. An absent item is a deleted file, so the question is not asked.
    let dropped = match keys {
        Keys::Census => now.stopped_measuring(was),
        Keys::Items | Keys::Sizes { .. } | Keys::Measures => Vec::new(),
    };
    // A new key that is not an item means the checking grew, which is reported rather than failed.
    let added = match keys {
        Keys::Measures | Keys::Census => now.unmeasured_in(was),
        Keys::Items | Keys::Sizes { .. } => Vec::new(),
    };
    let verdict = match regressions.is_empty() && dropped.is_empty() {
        true => Verdict::Pass,
        false => Verdict::Tripped,
    };
    let findings = regressions
        .iter()
        .map(|change| finding_for(change, was, unit))
        .chain(dropped.iter().map(|key| dropped_finding(key, was, unit)))
        .chain(added.iter().map(|key| added_finding(key, now, gate, unit)))
        .collect();
    (verdict, findings)
}

/// Whether a baseline may carry these keys. Asked where a record is written as well as where one is
/// read, because a first run refuses before it measures and never reaches the comparison.
pub fn portable(series: &Series) -> Result<(), String> {
    match crate::project::unportable(series.0.keys().map(String::as_str)) {
        None => Ok(()),
        Some(key) => Err(format!(
            "measured `{key}`, which is not inside this project — a baseline is committed and read \
             elsewhere, so it cannot be keyed by one machine's paths"
        )),
    }
}

/// A key is a path when it looks like one, so the finding lands where an editor can open it.
fn placed(key: &str, message: &str) -> Finding {
    match key.split_once('#') {
        Some((path, name)) => Finding::at(path, message).item(name),
        None if key.contains('/') || key.contains(".rs") => Finding::at(key, message),
        None => Finding::at("", message).item(key),
    }
}

fn finding_for(change: &crate::run::baseline::Change, was: &Series, unit: &str) -> Finding {
    let key = change.key();
    let message = match change {
        crate::run::baseline::Change::Grew { was, now, .. } => {
            format!("{now} {unit}, over the recorded {was}")
        }
        crate::run::baseline::Change::New { now, .. } => {
            format!("{now} {unit}, not in the baseline")
        }
    };
    placed(key, &message).numbers(change.now(), was.get(key))
}

/// A key this run measured and the baseline does not carry, where a new key is not debt. It passes:
/// nothing got worse, and a number nobody recorded cannot be compared to one.
fn added_finding(key: &str, now: &Series, gate: &str, unit: &str) -> Finding {
    let measured = now.get(key).unwrap_or_default();
    let message =
        format!("{measured} {unit}, newly measured — `chock baseline {gate}` puts it on record");
    placed(key, &message).numbers(measured, None)
}

/// A key the baseline records and this run never produced. `measured` stays absent, because a zero
/// there reads as a clean result rather than as nothing measured.
fn dropped_finding(key: &str, was: &Series, unit: &str) -> Finding {
    let recorded = was.get(key).unwrap_or_default();
    let message = format!("not measured this run; the baseline records {recorded} {unit}");
    let mut finding = placed(key, &message);
    finding.baseline = Some(recorded);
    finding
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing, and one measure panics on purpose"
)]
mod tests {
    use super::*;
    use crate::gates;
    use std::path::PathBuf;

    fn ctx_with(gate: &str, pairs: &[(&str, u64)]) -> Ctx {
        let mut baseline = Baseline::empty("0.1.0");
        let mut series = Series::new();
        for (key, value) in pairs {
            series.set(key, *value);
        }
        baseline.set(gate, series);
        Ctx::for_root(PathBuf::from("/w"), baseline)
    }

    #[test]
    fn a_configured_build_reaches_cargo_and_a_tool_that_cannot_take_it_refuses() {
        let mut ctx = ctx_with("slop", &[]);
        assert_eq!(ctx.default_build("cargo kani"), Ok(()));
        ctx.features = vec!["--all-features".to_string()];
        ctx.build = ["--target", "wasm32-wasip1", "--profile", "dist"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            ctx.cargo_args(),
            [
                "--all-features",
                "--target",
                "wasm32-wasip1",
                "--profile",
                "dist"
            ]
        );
        assert_eq!(ctx.build_flag("--target"), Some("wasm32-wasip1"));
        assert_eq!(ctx.build_flag("--profile"), Some("dist"));
        assert_eq!(ctx.build_flag("--features"), None);
        assert_eq!(
            ctx.default_build("cargo kani"),
            Err("cargo kani cannot be told the configured `--target wasm32-wasip1 --profile dist`, \
                 so it would measure another build"
                .to_string())
        );
    }

    fn existing_debt(_ctx: &Ctx) -> Result<Inspection, String> {
        Ok(Inspection::debt(vec![Finding::at(
            "src/unused.rs",
            "orphan module",
        )]))
    }

    fn blocked_debt(ctx: &Ctx) -> Result<Inspection, String> {
        let mut found = existing_debt(ctx)?;
        found
            .blockers
            .push(Finding::at("src/lib.rs", "missing module").item("missing"));
        Ok(found)
    }

    fn debt_gate(inspect: Inspect) -> Gate {
        gate_of(Kind::Debt {
            inspect,
            unit: "issue(s)",
        })
    }

    #[test]
    fn absent_debt_baselines_preserve_strict_enforcement_without_writing_anything() {
        let ctx = ctx_with("another", &[]);
        let report = run_one(&debt_gate(existing_debt), &ctx);
        assert_eq!(report.verdict, Verdict::Tripped);
        assert_eq!(report.findings, existing_debt(&ctx).unwrap().debt);
        assert!(!ctx.baseline.has("probe"));
        let clean = debt_gate(|_| Ok(Inspection::default()));
        assert_eq!(run_one(&clean, &ctx).verdict, Verdict::Pass);
    }

    #[test]
    fn adopted_debt_cannot_grow_and_new_debt_still_trips() {
        let gate = debt_gate(existing_debt);
        let ctx = ctx_with("probe", &[("src/unused.rs", 1)]);
        let report = run_one(&gate, &ctx);
        assert_eq!(report.verdict, Verdict::Pass);
        assert_eq!(report.measured, Some(1));
        assert_eq!(report.baseline, Some(1));
        let new = ctx_with("probe", &[("src/removed.rs", 1)]);
        assert_eq!(run_one(&gate, &new).verdict, Verdict::Tripped);
        assert_eq!(
            run_one(&gate, &ctx_with("probe", &[])).verdict,
            Verdict::Tripped
        );
    }

    #[test]
    fn correctness_blockers_cannot_be_recorded_or_hidden_by_an_orphan_baseline() {
        let gate = debt_gate(blocked_debt);
        let ctx = ctx_with("probe", &[("src/unused.rs", 99)]);
        let report = run_one(&gate, &ctx);
        assert_eq!(report.verdict, Verdict::Tripped);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.item.as_deref() == Some("missing"))
        );
        assert!(
            measured(&gate, &ctx)
                .unwrap_err()
                .contains("cannot baseline unresolved correctness")
        );
    }

    #[test]
    fn adoption_counts_stable_items_and_keeps_the_actual_findings() {
        let inspection = Inspection::debt(vec![
            Finding::at("Cargo.toml", "unpinned").item("a").line(4),
            Finding::at("Cargo.toml", "unpinned").item("a").line(70),
            Finding::at("src/orphan.rs", "orphan"),
        ]);
        assert_eq!(
            inspection.series(),
            Series(std::collections::BTreeMap::from([
                ("Cargo.toml#a".to_string(), 2),
                ("src/orphan.rs".to_string(), 1),
            ]))
        );
        let read = adoptable(inspection.clone()).unwrap();
        assert_eq!(read.findings, inspection.debt);
        let ctx = ctx_with("probe", &[("src/unused.rs", 1)]);
        assert_eq!(
            survey_one(&debt_gate(existing_debt), &ctx).verdict,
            Verdict::Tripped
        );
        let broken = debt_gate(|_| Err("source unreadable".to_string()));
        assert_eq!(run_one(&broken, &ctx).verdict, Verdict::CannotRun);
        assert_eq!(measure(&broken, &ctx), Err("source unreadable".to_string()));
    }

    /// A run already this many gates deep, which is the only thing the refusal looks at.
    fn at_depth(depth: u32) -> Ctx {
        Ctx {
            depth,
            ..ctx_with("probe", &[])
        }
    }

    fn gate_of(kind: Kind) -> Gate {
        Gate {
            name: "probe",
            about: "a gate that exists only in this test",
            group: Group::Quality,
            builds: false,
            reads: None,
            kind,
        }
    }

    fn two_items(_ctx: &Ctx) -> Result<Series, String> {
        let mut series = Series::new();
        series.set("src/a.rs", 10);
        series.set("src/b.rs", 5);
        Ok(series)
    }

    fn one_file_four_ways(_ctx: &Ctx) -> Result<Series, String> {
        let mut series = Series::new();
        series.set("src/a.rs#at_record", 3);
        series.set("src/a.rs#clean", 0);
        series.set("src/a.rs#over_record", 4);
        series.set("src/b.rs#untouched", 5);
        Ok(series)
    }

    fn kept_clean(changed: Result<Vec<String>, String>) -> Ctx {
        let mut ctx = ctx_with(
            "probe",
            &[
                ("src/a.rs#at_record", 3),
                ("src/a.rs#over_record", 2),
                ("src/b.rs#untouched", 5),
            ],
        );
        ctx.clean_when_touched = vec!["probe".to_string()];
        ctx.changed = std::sync::Arc::new(std::sync::OnceLock::from(changed));
        ctx
    }

    #[test]
    fn debt_the_record_allows_fails_in_a_touched_file_and_nowhere_else() {
        let gate = gate_of(Kind::Ratchet {
            measure: one_file_four_ways,
            keys: Keys::Items,
            unit: "lines",
        });
        let report = run_one(&gate, &kept_clean(Ok(vec!["src/a.rs".to_string()])));
        assert_eq!(report.verdict, Verdict::Tripped);
        let named: Vec<_> = report
            .findings
            .iter()
            .map(|f| (f.item.as_deref(), f.message.contains("clean_when_touched")))
            .collect();
        assert_eq!(
            named,
            [(Some("over_record"), false), (Some("at_record"), true)]
        );
        let untouched = run_one(&gate, &kept_clean(Ok(vec!["src/c.rs".to_string()])));
        let items: Vec<_> = untouched
            .findings
            .iter()
            .map(|f| f.item.as_deref())
            .collect();
        assert_eq!(items, [Some("over_record")]);
    }

    #[test]
    fn a_change_set_that_cannot_be_read_stops_the_gate_rather_than_passing_it() {
        let gate = gate_of(Kind::Ratchet {
            measure: one_file_four_ways,
            keys: Keys::Items,
            unit: "lines",
        });
        let report = run_one(&gate, &kept_clean(Err("no repository".to_string())));
        assert_eq!(report.verdict, Verdict::CannotRun);
        let why = report.cannot_run_reason.unwrap_or_default();
        assert!(
            why.contains("`clean_when_touched` needs the changed files: no repository"),
            "{why}"
        );
    }

    #[test]
    fn only_a_listed_gate_that_counts_items_holds_touched_files_to_zero() {
        let touched = || kept_clean(Ok(vec!["src/a.rs".to_string()]));
        let measures = gate_of(Kind::Ratchet {
            measure: two_items,
            keys: Keys::Measures,
            unit: "lines",
        });
        let mut ctx = touched();
        ctx.baseline.set("probe", two_items(&ctx).unwrap());
        assert_eq!(run_one(&measures, &ctx).verdict, Verdict::Pass);
        let mut unlisted = touched();
        unlisted.clean_when_touched.clear();
        unlisted
            .baseline
            .set("probe", two_items(&unlisted).unwrap());
        assert_eq!(run_one(&ratchet_gate(), &unlisted).verdict, Verdict::Pass);
        let mut listed = touched();
        listed.baseline.set("probe", two_items(&listed).unwrap());
        assert_eq!(run_one(&ratchet_gate(), &listed).verdict, Verdict::Tripped);
    }

    fn measured_nothing(_ctx: &Ctx) -> Result<Series, String> {
        Err("the tool printed nothing chock could read".to_string())
    }

    /// A survey still tells a gate that measured from one that could not.
    #[test]
    fn a_survey_reports_what_a_gate_measured_without_a_baseline_to_compare() {
        // Named for another gate, so `probe` has no record to be compared against.
        let ctx = ctx_with("someone-else", &[]);
        assert!(!ctx.baseline.has("probe"), "the point is there is none");

        let report = survey_one(&ratchet_gate(), &ctx);
        assert_eq!(report.verdict, Verdict::Pass);
        assert_eq!(report.measured, Some(15));
        assert_eq!(report.unit.as_deref(), Some("lines"));

        let refused = survey_one(
            &gate_of(Kind::Ratchet {
                measure: measured_nothing,
                keys: Keys::Items,
                unit: "lines",
            }),
            &ctx,
        );
        assert_eq!(refused.verdict, Verdict::CannotRun);
        assert!(
            refused
                .cannot_run_reason
                .unwrap_or_default()
                .contains("nothing chock could read")
        );
    }

    #[test]
    fn a_survey_still_refuses_a_gate_whose_tool_is_not_installed() {
        let ctx = ctx_with("probe", &[]);
        let mut gate = ratchet_gate();
        gate.reads = Some(crate::run::verdicts::Reads::tree_and(&[
            "a-tool-nothing-installs",
        ]));
        let report = survey_one(&gate, &ctx);
        assert_eq!(report.verdict, Verdict::CannotRun);
        assert!(
            report
                .cannot_run_reason
                .unwrap_or_default()
                .contains("a-tool-nothing-installs")
        );
    }

    fn ratchet_gate() -> Gate {
        gate_of(Kind::Ratchet {
            measure: two_items,
            keys: Keys::Items,
            unit: "lines",
        })
    }

    fn a_key_from_another_tree(_ctx: &Ctx) -> Result<Series, String> {
        let mut series = Series::new();
        series.set("src/a.rs", 10);
        series.set("/home/someone/else/src/b.rs", 5);
        Ok(series)
    }

    #[test]
    fn a_run_counting_something_else_than_the_record_is_refused_rather_than_tripped() {
        let mut ctx = ctx_with("probe", &[("src/a.rs", 1), ("src/b.rs", 1)]);
        ctx.baseline
            .units
            .insert("probe".to_string(), "duplicated group(s)".to_string());
        let report = run_one(&ratchet_gate(), &ctx);
        assert_eq!(report.verdict, Verdict::CannotRun);
        let why = report.cannot_run_reason.unwrap_or_default();
        assert!(why.contains("counts lines where the record holds"), "{why}");
        assert!(why.contains("duplicated group(s)"), "{why}");
    }

    #[test]
    fn a_ratchet_whose_tool_is_absent_names_the_tool_and_not_the_baseline() {
        let ctx = ctx_with("probe", &[]);
        let mut gate = ratchet_gate();
        gate.reads = Some(crate::run::verdicts::Reads::tree_and(&[
            "a-tool-nothing-installs",
        ]));
        let report = run_one(&gate, &ctx);
        assert_eq!(report.verdict, Verdict::CannotRun);
        let why = report.cannot_run_reason.unwrap_or_default();
        assert!(why.contains("a-tool-nothing-installs"), "{why}");
        assert!(!why.contains("baseline"), "{why}");
    }

    /// A baseline from before chock kept units must still compare, or every upgrading project
    /// would be refused until it re-recorded everything.
    #[test]
    fn a_baseline_that_names_no_unit_is_compared_rather_than_refused() {
        let ctx = ctx_with("probe", &[("src/a.rs", 10), ("src/b.rs", 5)]);
        assert_eq!(ctx.baseline.unit("probe"), None);
        assert_eq!(run_one(&ratchet_gate(), &ctx).verdict, Verdict::Pass);
    }

    #[test]
    fn a_run_sharing_no_key_with_its_baseline_is_refused_rather_than_called_debt() {
        let mut was = Series::new();
        was.set("a.py#long-comment-block", 3);
        let mut now = Series::new();
        now.set("a.py#hazard_long_comment_block", 3);
        let why = rekeyed(&now, &was, Keys::Items).unwrap_or_default();
        assert!(why.contains("shares no key with the baseline"), "{why}");
        assert!(why.contains("chock baseline"), "{why}");

        // One key in common is a tree that moved, which the ratchet is for.
        let mut overlapping = Series::new();
        overlapping.set("a.py#long-comment-block", 9);
        overlapping.set("b.py#hazard_new", 1);
        assert_eq!(rekeyed(&overlapping, &was, Keys::Items), None);
        // Nothing on record yet is the first recording, not a re-key.
        assert_eq!(rekeyed(&now, &Series::new(), Keys::Items), None);
    }

    /// A census owes a row per key, so measuring none is a gate that stopped answering — `lenses`
    /// reported that as tripped. An item gone is debt paid, which `citations` reaching zero showed.
    #[test]
    fn measuring_nothing_is_refused_for_a_census_and_welcomed_for_an_item() {
        let mut was = Series::new();
        was.set("hazard_long_comment_block", 3);
        let why = rekeyed(&Series::new(), &was, Keys::Census).unwrap_or_default();
        assert!(why.contains("stopped answering"), "{why}");
        assert!(why.contains("1 key(s)"), "{why}");
        // The goal state, not a failure: every recorded item was fixed.
        assert_eq!(rekeyed(&Series::new(), &was, Keys::Items), None);
        assert_eq!(rekeyed(&Series::new(), &was, Keys::Measures), None);
    }

    /// A baseline is committed and read on CI and in every other checkout, so a key naming one
    /// machine's filesystem can never match there. coverage recorded exactly that from a stale lcov.
    #[test]
    fn a_measurement_keyed_outside_this_project_is_refused_rather_than_recorded() {
        let gate = gate_of(Kind::Ratchet {
            measure: a_key_from_another_tree,
            keys: Keys::Items,
            unit: "lines",
        });
        let report = run_one(&gate, &ctx_with("probe", &[("src/a.rs", 10)]));
        assert_eq!(report.verdict, Verdict::CannotRun);
        let why = report.cannot_run_reason.unwrap_or_default();
        assert!(why.contains("/home/someone/else/src/b.rs"), "{why}");
        assert!(why.contains("one machine's paths"), "{why}");
    }

    #[test]
    fn a_ratchet_holding_its_baseline_passes_and_shows_both_numbers() {
        let ctx = ctx_with("probe", &[("src/a.rs", 10), ("src/b.rs", 5)]);
        let report = run_one(&ratchet_gate(), &ctx);
        assert_eq!(report.verdict, Verdict::Pass);
        assert_eq!(report.measured, Some(15));
        assert_eq!(report.baseline, Some(15));
        assert!(report.findings.is_empty());
    }

    #[test]
    fn a_ratchet_that_grew_trips_and_points_at_the_key_that_moved() {
        let ctx = ctx_with("probe", &[("src/a.rs", 4), ("src/b.rs", 5)]);
        let report = run_one(&ratchet_gate(), &ctx);
        assert_eq!(report.verdict, Verdict::Tripped);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(
            report.findings[0].render(),
            "src/a.rs: 10 lines, over the recorded 4"
        );
    }

    #[test]
    fn a_ratchet_below_its_baseline_passes_rather_than_demanding_an_update() {
        let ctx = ctx_with("probe", &[("src/a.rs", 99), ("src/b.rs", 99)]);
        assert_eq!(run_one(&ratchet_gate(), &ctx).verdict, Verdict::Pass);
    }

    /// `slop` counts the same on every machine, so a run lowers its record; `probe` is not one.
    fn settling_gate() -> Gate {
        Gate {
            name: "slop",
            ..ratchet_gate()
        }
    }

    #[test]
    fn a_gain_on_a_gate_that_counts_alike_everywhere_is_kept_for_the_run_to_write() {
        let ctx = ctx_with(
            "slop",
            &[("src/a.rs", 12), ("src/b.rs", 5), ("src/gone.rs", 3)],
        );
        let report = run_one(&settling_gate(), &ctx);
        assert_eq!(report.verdict, Verdict::Pass);
        let mut lower = Series::new();
        lower.set("src/a.rs", 10);
        lower.set("src/b.rs", 5);
        assert_eq!(report.tightened, Some(lower.clone()));
        let (record, lowered) = locked_in(&ctx.baseline, &[report]).unwrap();
        assert_eq!(
            (record.gate("slop"), lowered),
            (lower, vec!["slop".to_string()])
        );
        let unmoved = ctx_with("probe", &[("src/a.rs", 12), ("src/b.rs", 5)]);
        assert_eq!(
            run_one(&ratchet_gate(), &unmoved).tightened,
            None,
            "probe varies by machine"
        );
    }

    #[test]
    fn ci_fails_a_gain_the_committed_record_does_not_hold_and_names_where() {
        let mut ctx = ctx_with("slop", &[("src/a.rs", 12), ("src/b.rs", 5)]);
        ctx.ci = true;
        let report = run_one(&settling_gate(), &ctx);
        assert_eq!(
            (report.verdict, report.tightened.clone()),
            (Verdict::Tripped, None)
        );
        let said: Vec<String> = report.findings.iter().map(Finding::render).collect();
        assert_eq!(
            said,
            [
                "src/a.rs: 10 lines where the record holds 12: `chock run` lowers the record, and the change has to commit it"
            ]
        );
        let held = ctx_with("slop", &[("src/a.rs", 10), ("src/b.rs", 5)]);
        let ci = Ctx { ci: true, ..held };
        assert_eq!(
            run_one(&settling_gate(), &ci).verdict,
            Verdict::Pass,
            "nothing to record"
        );
    }

    #[test]
    fn a_strict_gate_needs_no_record_and_fails_on_anything_it_finds() {
        let mut ctx = ctx_with("other", &[]);
        ctx.strict = vec!["slop".to_string()];
        let report = run_one(&settling_gate(), &ctx);
        assert_eq!(report.verdict, Verdict::Tripped);
        let said: Vec<String> = report.findings.iter().map(Finding::render).collect();
        assert_eq!(
            said,
            [
                "src/a.rs: 10 lines, over the recorded 0",
                "src/b.rs: 5 lines, over the recorded 0"
            ]
        );
        assert_eq!(report.tightened, None);
    }

    #[test]
    fn a_tripped_gate_carries_its_fix_and_a_passing_one_carries_none() {
        let tripped = run_one(&settling_gate(), &ctx_with("slop", &[("src/a.rs", 1)]));
        assert_eq!(tripped.verdict, Verdict::Tripped);
        assert_eq!(tripped.fix.as_deref(), crate::gates::fixes::fix("slop"));
        assert!(tripped.fix.is_some());
        let held = ctx_with("slop", &[("src/a.rs", 10), ("src/b.rs", 5)]);
        assert_eq!(run_one(&settling_gate(), &held).fix, None);
        let unknown = run_one(&ratchet_gate(), &ctx_with("probe", &[("src/a.rs", 1)]));
        assert_eq!((unknown.verdict, unknown.fix), (Verdict::Tripped, None));
    }

    #[test]
    fn nothing_lowered_writes_no_record() {
        let report = GateReport::new("slop", Verdict::Pass, "chock run slop");
        assert_eq!(locked_in(&Baseline::empty("0.1.0"), &[report]), None);
    }

    /// Passes without reading anything, so the first run has a verdict worth keeping.
    fn agreeable(_ctx: &Ctx) -> Result<Outcome, String> {
        Ok(Outcome::passed())
    }

    /// Panics if it is ever reached, which is what proves the second run answered from the record.
    fn never_checked(_ctx: &Ctx) -> Result<Outcome, String> {
        panic!("a recalled verdict must not run the gate again")
    }

    fn declaring(check: Check) -> Gate {
        Gate {
            reads: Some(crate::run::verdicts::Reads::tree_and(&[])),
            ..gate_of(Kind::Binary(check))
        }
    }

    /// The case the verdict record exists for; the report says it was recalled.
    #[test]
    fn a_gate_that_names_its_inputs_answers_the_second_time_without_running() {
        let dir = crate::testdir::make("run-recalled");
        std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        let ctx = Ctx::for_root(dir.to_path_buf(), Baseline::empty("0.1.0"));
        let first = run_one(&declaring(agreeable), &ctx);
        assert_eq!(first.verdict, Verdict::Pass);
        assert!(!first.recalled, "the first run took the verdict itself");

        let second = run_one(&declaring(never_checked), &ctx);
        assert_eq!(second.verdict, Verdict::Pass);
        assert!(
            second.recalled,
            "the second run had a record to answer from"
        );
        assert!(
            second.summary().contains("recalled"),
            "{}",
            second.summary()
        );
    }

    /// A strict gate is held to zero, so its sites are named against no record.
    #[test]
    fn a_strict_gate_names_its_sites_against_no_record() {
        let mut held = Series::new();
        held.set("src/a.rs", 2);
        let mut baseline = Baseline::default();
        baseline.record("slop", "comment block(s)", held.clone());
        let mut ctx = Ctx::for_root(PathBuf::new(), baseline);
        assert_eq!(ctx.record("slop"), held);
        ctx.strict = vec!["slop".to_string()];
        assert_eq!(ctx.record("slop"), Series::new());
    }

    /// A verdict kept for one change set never answers for another, and none is kept without one.
    #[test]
    fn a_gate_held_clean_where_touched_runs_again_when_the_change_set_moves() {
        let dir = crate::testdir::make("run-touched-key");
        std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        let touching = |changed: Result<Vec<String>, String>| Ctx {
            root: dir.to_path_buf(),
            ..kept_clean(changed)
        };
        let one = || touching(Ok(vec!["a.rs".to_string()]));
        assert!(!run_one(&declaring(agreeable), &one()).recalled);
        assert!(run_one(&declaring(never_checked), &one()).recalled);
        assert!(!run_one(&declaring(agreeable), &touching(Ok(Vec::new()))).recalled);
        let unread = touching(Err("no repository".to_string()));
        assert_eq!(keyed(&declaring(agreeable), &unread), None);
    }

    /// A tree that moved is a verdict that no longer describes it, however recently it was taken.
    #[test]
    fn a_file_changing_puts_the_gate_back_to_running() {
        let dir = crate::testdir::make("run-recalled-moved");
        std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        let ctx = Ctx::for_root(dir.to_path_buf(), Baseline::empty("0.1.0"));
        assert!(!run_one(&declaring(agreeable), &ctx).recalled);
        std::fs::write(dir.join("a.rs"), "fn a() { }\n").unwrap();
        // The listing is taken once per run, so a second context is what a second run would have.
        let moved = Ctx {
            listing: std::sync::Arc::default(),
            suite: std::sync::Arc::default(),
            ..ctx
        };
        assert!(
            !run_one(&declaring(agreeable), &moved).recalled,
            "a changed file must not be answered from the old verdict"
        );
    }

    fn never_reached(_ctx: &Ctx) -> Result<Series, String> {
        panic!("a ratchet with no baseline must refuse before it measures anything")
    }

    #[test]
    fn a_ratchet_with_no_baseline_refuses_before_it_measures() {
        let ctx = Ctx::for_root(PathBuf::from("/w"), Baseline::empty("0.1.0"));
        let gate = gate_of(Kind::Ratchet {
            measure: never_reached,
            keys: Keys::Items,
            unit: "lines",
        });
        let report = run_one(&gate, &ctx);
        assert_eq!(report.verdict, Verdict::CannotRun);
        assert_eq!(
            report.cannot_run_reason.as_deref(),
            Some(
                "no baseline recorded — `chock baseline probe` records one, or says why it cannot"
            )
        );
        // No number, because none was taken. A zero here would read as a clean measurement.
        assert_eq!(report.measured, None);
        assert_eq!(report.unit.as_deref(), Some("lines"));
    }

    #[test]
    fn an_item_the_baseline_never_saw_is_new_debt_and_trips() {
        let ctx = ctx_with("probe", &[("src/a.rs", 10)]);
        let report = run_one(&ratchet_gate(), &ctx);
        assert_eq!(report.verdict, Verdict::Tripped);
        assert_eq!(
            report.findings[0].render(),
            "src/b.rs: 5 lines, not in the baseline"
        );
    }

    #[test]
    fn a_measure_the_baseline_never_saw_is_named_in_a_finding_and_still_passes() {
        let gate = gate_of(Kind::Ratchet {
            measure: two_items,
            keys: Keys::Measures,
            unit: "lines",
        });
        let ctx = ctx_with("probe", &[("src/a.rs", 10)]);
        let report = run_one(&gate, &ctx);
        assert_eq!(report.verdict, Verdict::Pass);
        assert_eq!(report.exit_code, 0);
        assert_eq!(
            report.findings[0].render(),
            "src/b.rs: 5 lines, newly measured — `chock baseline probe` puts it on record"
        );
        assert_eq!(report.findings[0].baseline, None);
    }

    /// A lens outpost added is checking that grew, the mirror of a lens it retired. Both are the
    /// roster moving under a gate that cannot see it in any number.
    #[test]
    fn a_census_row_this_run_produced_for_the_first_time_is_named_in_a_finding_and_still_passes() {
        let ctx = ctx_with("probe", &[("src/a.rs", 10)]);
        let report = run_one(&census_gate(two_items), &ctx);
        assert_eq!(report.verdict, Verdict::Pass);
        assert_eq!(
            report.findings[0].render(),
            "src/b.rs: 5 hazard(s), newly measured — `chock baseline probe` puts it on record"
        );
    }

    /// An item is somebody's to add, so a new one is debt and fails. It must not also be reported as
    /// growth, or the same key would arrive twice in one report.
    #[test]
    fn a_new_item_is_charged_as_debt_once_and_never_also_reported_as_growth() {
        let ctx = ctx_with("probe", &[("src/a.rs", 10)]);
        let report = run_one(&ratchet_gate(), &ctx);
        assert_eq!(report.verdict, Verdict::Tripped);
        let named: Vec<String> = report
            .findings
            .iter()
            .map(Finding::render)
            .filter(|said| said.contains("src/b.rs"))
            .collect();
        assert_eq!(named, vec!["src/b.rs: 5 lines, not in the baseline"]);
    }

    fn census_gate(measure: Measure) -> Gate {
        gate_of(Kind::Ratchet {
            measure,
            keys: Keys::Census,
            unit: "hazard(s)",
        })
    }

    /// The false green this answers: a producer stops reporting one of its checks, that row leaves
    /// the series, and the gate passes over a smaller domain than the baseline recorded.
    #[test]
    fn a_census_row_this_run_no_longer_produced_trips_and_names_the_measure_that_went_missing() {
        let ctx = ctx_with(
            "probe",
            &[("src/a.rs", 10), ("src/b.rs", 5), ("half-an-inverse", 2)],
        );
        let report = run_one(&census_gate(two_items), &ctx);
        assert_eq!(report.verdict, Verdict::Tripped);
        assert_eq!(report.exit_code, 1);
        let finding = report
            .findings
            .iter()
            .find(|found| found.item.as_deref() == Some("half-an-inverse"))
            .unwrap();
        assert_eq!(
            finding.render(),
            "half-an-inverse: not measured this run; the baseline records 2 hazard(s)"
        );
        assert_eq!(finding.baseline, Some(2));
        assert_eq!(finding.measured, None);
    }

    /// Deleting a file is how a ratchet is meant to go down, so the question is asked of a measure
    /// and never of an item.
    #[test]
    fn an_item_on_record_that_this_run_no_longer_finds_is_a_fix_rather_than_a_failure() {
        let ctx = ctx_with(
            "probe",
            &[("src/a.rs", 10), ("src/b.rs", 5), ("src/deleted.rs", 40)],
        );
        let report = run_one(&ratchet_gate(), &ctx);
        assert_eq!(report.verdict, Verdict::Pass);
        assert!(report.findings.is_empty());
    }

    /// `codeslop` keys by whichever clippy lint fired, so a lint fixed to zero leaves the series.
    /// Charging that as a lost check would fail a project for fixing its code.
    #[test]
    fn a_measure_whose_last_finding_was_fixed_passes_rather_than_reading_as_a_lost_check() {
        let gate = gate_of(Kind::Ratchet {
            measure: two_items,
            keys: Keys::Measures,
            unit: "findings",
        });
        let ctx = ctx_with(
            "probe",
            &[
                ("clippy::redundant_clone", 3),
                ("src/a.rs", 10),
                ("src/b.rs", 5),
            ],
        );
        let report = run_one(&gate, &ctx);
        assert_eq!(report.verdict, Verdict::Pass);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn a_census_row_that_fell_to_zero_and_still_reports_its_row_passes() {
        fn one_quiet_lens(_ctx: &Ctx) -> Result<Series, String> {
            let mut series = Series::new();
            series.set("half-an-inverse", 0);
            Ok(series)
        }
        let ctx = ctx_with("probe", &[("half-an-inverse", 4)]);
        let report = run_one(&census_gate(one_quiet_lens), &ctx);
        assert_eq!(report.verdict, Verdict::Pass);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn a_run_that_both_grew_and_lost_a_row_reports_each_of_them() {
        let ctx = ctx_with(
            "probe",
            &[("half-an-inverse", 2), ("src/a.rs", 1), ("src/b.rs", 5)],
        );
        let report = run_one(&census_gate(two_items), &ctx);
        let said: Vec<String> = report.findings.iter().map(Finding::render).collect();
        assert!(
            said.contains(&"src/a.rs: 10 hazard(s), over the recorded 1".to_string()),
            "{said:?}"
        );
        assert!(
            said.contains(
                &"half-an-inverse: not measured this run; the baseline records 2 hazard(s)"
                    .to_string()
            ),
            "{said:?}"
        );
    }

    #[test]
    fn a_gate_that_could_not_measure_never_reports_a_pass() {
        fn broken(_ctx: &Ctx) -> Result<Series, String> {
            Err("cargo is not on PATH".to_string())
        }
        let gate = gate_of(Kind::Ratchet {
            measure: broken,
            keys: Keys::Items,
            unit: "lines",
        });
        let report = run_one(&gate, &ctx_with("probe", &[]));
        assert_eq!(report.verdict, Verdict::CannotRun);
        assert_eq!(report.exit_code, 2);
        assert_eq!(report.measured, None);
    }

    /// `profile` reports an advisory this way. Dropped here, it reached a reader only on a run the
    /// gate had already failed — which is the one moment an advisory reads as the failure.
    #[test]
    fn a_gate_that_passed_still_carries_what_it_had_to_say() {
        fn advised(_ctx: &Ctx) -> Result<Outcome, String> {
            Ok(Outcome::noted(vec![Finding::at(
                "Cargo.toml",
                "advisory, profile.release.panic",
            )]))
        }
        let report = run_one(&gate_of(Kind::Binary(advised)), &ctx_with("probe", &[]));
        assert_eq!(report.verdict, Verdict::Pass);
        assert_eq!(report.exit_code, 0);
        assert_eq!(
            report.findings[0].render(),
            "Cargo.toml: advisory, profile.release.panic"
        );
    }

    fn scoped_measurement(_ctx: &Ctx) -> Result<Measurement, String> {
        let mut series = Series::new();
        series.set("src/a.rs#eq_op_invert", 1);
        Ok(Measurement {
            series,
            findings: vec![
                Finding::at(
                    "",
                    "not applicable to linked-crate mutation; not mutation-covered",
                )
                .item("integration test commands"),
            ],
        })
    }

    fn scoped_ratchet() -> Gate {
        gate_of(Kind::AnnotatedRatchet {
            measure: scoped_measurement,
            keys: Keys::Items,
            unit: "survivors",
        })
    }

    #[test]
    fn an_unannotated_cached_verdict_cannot_answer_for_a_scope_aware_measurement() {
        let dir = crate::testdir::make("scope-key");
        let gate = scoped_ratchet();
        let old = GateReport::new("probe", Verdict::Pass, "chock run probe");
        crate::run::verdicts::keep(&dir, "probe", "same-inputs", &old);
        let key = key_format(&gate, "", "same-inputs");
        assert_eq!(key, "scope-v2:lowers-v1:same-inputs");
        assert_eq!(crate::run::verdicts::recall(&dir, "probe", &key), None);
        let ctx = ctx_with("probe", &[("src/a.rs#eq_op_invert", 1)]);
        let annotated = run_one(&gate, &ctx);
        crate::run::verdicts::keep(&dir, "probe", &key, &annotated);
        assert_eq!(
            crate::run::verdicts::recall(&dir, "probe", &key),
            Some(annotated)
        );
        assert_eq!(
            key_format(&ratchet_gate(), "", "same-inputs"),
            "lowers-v1:same-inputs"
        );
        assert_eq!(
            key_format(&gates::tools::TEST, "", "same-inputs"),
            "same-inputs"
        );
    }

    #[test]
    fn direct_outcome_classification_preserves_pass_failure_and_unmeasured_results() {
        let gate = ratchet_gate();
        let finding = Finding::at("src/lib.rs", "broken");
        assert!(Inspection::default().outcome().passed);
        assert!(
            !Inspection {
                debt: vec![finding.clone()],
                blockers: Vec::new()
            }
            .outcome()
            .passed
        );
        let passed = outcome_report(&gate, Ok(Outcome::passed()));
        assert_eq!(passed.verdict, Verdict::Pass);
        let silent = outcome_report(&gate, Ok(Outcome::failed(Vec::new())));
        assert_eq!(silent.verdict, Verdict::CannotRun);
        let failed = outcome_report(&gate, Ok(Outcome::failed(vec![finding.clone()])));
        assert_eq!(failed.verdict, Verdict::Tripped);
        assert_eq!(failed.findings, vec![finding]);
    }

    #[test]
    fn changed_reader_and_feature_contracts_invalidate_previous_cached_verdicts() {
        let cases = [
            (&gates::tools::TEST, "features-v1:input"),
            (&gates::tools::IDEMPOTENT, "features-v1:input"),
            (&gates::tools::proof::GATE, "proof-v2:input"),
            (&gates::tools::binsize::GATE, "targets-v1:lowers-v1:input"),
            (&gates::tools::DOC, "targets-v1:input"),
            (
                &gates::tools::MUTEST,
                "watchdog-v1:isolated-v1:results-v1:scoped-v1:scope-v2:lowers-v1:input",
            ),
        ];
        for (gate, expected) in cases {
            let reader = gate.reads.map_or("", |reads| reads.reader);
            assert_eq!(key_format(gate, reader, "input"), expected);
        }
    }

    #[test]
    fn scope_advisories_survive_a_passing_ratchet_and_its_json() {
        let ctx = ctx_with("probe", &[("src/a.rs#eq_op_invert", 1)]);
        let report = run_one(&scoped_ratchet(), &ctx);
        assert_eq!(report.verdict, Verdict::Pass);
        assert_eq!(report.measured, Some(1));
        assert_eq!(report.findings, scoped_measurement(&ctx).unwrap().findings);
        let run = Run::new("0.1.0", vec![report]);
        let read: Run = serde_json::from_str(&run.render_json()).unwrap();
        assert_eq!(
            read.gates[0].findings[0].item.as_deref(),
            Some("integration test commands")
        );
        let text = run.render();
        assert!(text.contains("advisory, and it passed"), "{text}");
        assert!(text.contains("not mutation-covered"), "{text}");
    }

    #[test]
    fn scope_advisories_do_not_hide_a_real_survivor_regression() {
        let report = run_one(
            &scoped_ratchet(),
            &ctx_with("probe", &[("src/a.rs#eq_op_invert", 0)]),
        );
        assert_eq!(report.verdict, Verdict::Tripped);
        assert_eq!(report.findings[0].measured, Some(1));
        assert_eq!(
            report.findings[1].item.as_deref(),
            Some("integration test commands")
        );
        assert_eq!(report.baseline, Some(0));
    }

    #[test]
    fn baseline_recording_and_surveying_receive_the_same_scope_notes() {
        let gate = scoped_ratchet();
        let ctx = ctx_with("probe", &[]);
        let result = measured(&gate, &ctx).unwrap();
        assert_eq!(survey_one(&gate, &ctx).findings, result.findings);
        assert_eq!(measure(&gate, &ctx).unwrap(), result.series);
        assert_eq!(gate.counts_in(), Some("survivors"));
    }

    #[test]
    fn a_failed_annotated_measurement_has_no_partial_series_to_record() {
        fn failed(_ctx: &Ctx) -> Result<Measurement, String> {
            Err("eligible target launch did not build".to_string())
        }
        let gate = gate_of(Kind::AnnotatedRatchet {
            measure: failed,
            keys: Keys::Items,
            unit: "survivors",
        });
        let ctx = ctx_with("probe", &[("src/a.rs#eq_op_invert", 1)]);
        let report = run_one(&gate, &ctx);
        assert_eq!(report.verdict, Verdict::CannotRun);
        assert_eq!(report.measured, None);
        assert_eq!(
            report.cannot_run_reason.as_deref(),
            Some("eligible target launch did not build")
        );
        assert!(measured(&gate, &ctx).is_err());
        assert_eq!(survey_one(&gate, &ctx).verdict, Verdict::CannotRun);
    }

    static CALLER: std::sync::OnceLock<std::thread::ThreadId> = std::sync::OnceLock::new();

    fn on_the_caller() -> bool {
        CALLER.get() == Some(&std::thread::current().id())
    }

    #[test]
    fn gates_that_need_no_compiler_run_on_workers_and_the_rest_take_turns_in_order() {
        // A failed outcome with nothing to point at reads as `cannot_run`, never as the pass asserted.
        fn off_caller(_ctx: &Ctx) -> Result<Outcome, String> {
            Ok(Outcome {
                passed: !on_the_caller(),
                findings: Vec::new(),
                said: None,
            })
        }
        fn on_caller(_ctx: &Ctx) -> Result<Outcome, String> {
            Ok(Outcome {
                passed: on_the_caller(),
                findings: Vec::new(),
                said: None,
            })
        }
        let quick = Gate {
            name: "quick",
            ..gate_of(Kind::Binary(off_caller))
        };
        let slow = Gate {
            name: "slow",
            builds: true,
            ..gate_of(Kind::Binary(on_caller))
        };
        let mut ctx = ctx_with("probe", &[]);
        ctx.jobs = 4;
        CALLER.set(std::thread::current().id()).unwrap();
        let heard = std::sync::Mutex::new(Vec::new());
        let told = |report: &GateReport| heard.lock().unwrap().push(report.gate.clone());
        let run = run_all(&[&quick, &slow, &quick], &ctx, "0.1.0", &told);
        let names: Vec<String> = run
            .gates
            .iter()
            .map(|gate| format!("{} {:?}", gate.gate, gate.verdict))
            .collect();
        assert_eq!(names, ["quick Pass", "slow Pass", "quick Pass"]);
        assert_eq!(
            *heard.lock().unwrap(),
            ["slow"],
            "only the gate that compiles, as it ends"
        );
    }

    /// A quick gate that fails is told before the builds, which can take an hour, start.
    #[test]
    fn a_quick_gate_that_did_not_pass_is_told_before_the_gates_that_compile() {
        fn refused(_ctx: &Ctx) -> Result<Outcome, String> {
            Err("no tool".to_string())
        }
        let passes = || gate_of(Kind::Binary(|_| Ok(Outcome::passed())));
        let fails = Gate {
            name: "fails",
            ..gate_of(Kind::Binary(refused))
        };
        let slow = Gate {
            name: "slow",
            builds: true,
            ..passes()
        };
        let heard = std::sync::Mutex::new(Vec::new());
        let told = |report: &GateReport| heard.lock().unwrap().push(report.gate.clone());
        run_all(
            &[&slow, &passes(), &fails],
            &ctx_with("probe", &[]),
            "0.1.0",
            &told,
        );
        assert_eq!(*heard.lock().unwrap(), ["fails", "slow"]);
    }

    static LANES_CALLER: std::sync::OnceLock<std::thread::ThreadId> = std::sync::OnceLock::new();

    #[test]
    fn with_jobs_to_spare_a_gate_that_builds_apart_runs_beside_the_suites_lane() {
        fn in_a_lane(_ctx: &Ctx) -> Result<Outcome, String> {
            Ok(Outcome {
                passed: LANES_CALLER.get() != Some(&std::thread::current().id()),
                findings: Vec::new(),
                said: None,
            })
        }
        let apart = Gate {
            name: "binsize",
            builds: true,
            ..gate_of(Kind::Binary(in_a_lane))
        };
        let suite = Gate {
            name: "slow",
            builds: true,
            ..gate_of(Kind::Binary(in_a_lane))
        };
        let also_apart = Gate {
            name: "bsize",
            builds: true,
            ..gate_of(Kind::Binary(in_a_lane))
        };
        let mut ctx = ctx_with("probe", &[]);
        ctx.jobs = 32;
        LANES_CALLER.set(std::thread::current().id()).unwrap();
        let heard = std::sync::Mutex::new(Vec::new());
        let told = |report: &GateReport| heard.lock().unwrap().push(report.gate.clone());
        let gates = [&suite, &apart, &suite, &also_apart];
        let run = run_all(&gates, &ctx, "0.1.0", &told);
        let names: Vec<String> = run
            .gates
            .iter()
            .map(|gate| format!("{} {:?}", gate.gate, gate.verdict))
            .collect();
        assert_eq!(
            names,
            ["slow Pass", "binsize Pass", "slow Pass", "bsize Pass"]
        );
        let mut heard = heard.into_inner().unwrap();
        heard.sort();
        assert_eq!(heard, ["binsize", "bsize", "slow", "slow"]);
        assert_eq!(lanes_of(&gates, &ctx), [vec![0, 2], vec![1, 3]]);
    }

    #[test]
    fn a_binary_gate_passes_and_fails_on_its_tools_own_verdict() {
        fn ok(_ctx: &Ctx) -> Result<Outcome, String> {
            Ok(Outcome::passed())
        }
        fn bad(_ctx: &Ctx) -> Result<Outcome, String> {
            Ok(Outcome::failed(vec![Finding::at("src/a.rs", "boom")]))
        }
        let ctx = ctx_with("probe", &[]);
        assert_eq!(
            run_one(&gate_of(Kind::Binary(ok)), &ctx).verdict,
            Verdict::Pass
        );
        let failed = run_one(&gate_of(Kind::Binary(bad)), &ctx);
        assert_eq!(failed.verdict, Verdict::Tripped);
        assert_eq!(failed.findings[0].render(), "src/a.rs: boom");
    }

    /// The shape every tool-output parser fails into when its tool changes format: a non-zero
    /// exit with nothing readable behind it.
    #[test]
    fn a_failure_with_nothing_to_point_at_is_a_gate_that_could_not_run() {
        fn bad(_ctx: &Ctx) -> Result<Outcome, String> {
            Ok(Outcome::failed(Vec::new()))
        }
        let report = run_one(&gate_of(Kind::Binary(bad)), &ctx_with("probe", &[]));
        assert_eq!(report.verdict, Verdict::CannotRun);
        assert_eq!(report.exit_code, 2);
        assert_eq!(
            report.cannot_run_reason.as_deref(),
            Some(format!("{SAID_NOTHING}, so there is nothing here to act on").as_str())
        );
    }

    #[test]
    fn a_failure_nothing_could_be_read_out_of_quotes_what_the_tool_said() {
        let wrote = |n: usize| (1..=n).map(|i| format!("line {i}\n")).collect::<String>();
        let reason = said_nothing(Some(&wrote(3)));
        assert_eq!(
            reason,
            format!("{SAID_NOTHING}. Its last {QUOTED_LINES} line(s):\nline 1\nline 2\nline 3")
        );
        // More than it quotes: the tail is what a diagnostic and its notes sit in.
        let long = said_nothing(Some(&wrote(50)));
        assert!(long.ends_with("line 50"), "{long}");
        assert!(long.contains("line 31"), "{long}");
        assert!(!long.contains("line 30\n"), "{long}");
        // The error sits above a build script's chatter, where no tail reaches it.
        let buried = format!(
            "error: failed to run custom build command for `glib-sys`\n{}",
            wrote(30)
        );
        let told = said_nothing(Some(&buried));
        let lead = format!(
            "{SAID_NOTHING}. What it marked as the error: error: failed to run custom build \
             command for `glib-sys`. Its last"
        );
        assert!(told.starts_with(&lead), "{told}");
        // Nothing written, and nothing but blank lines, both fall back to saying so.
        for said in [None, Some(""), Some("\n  \n")] {
            assert_eq!(
                said_nothing(said),
                format!("{SAID_NOTHING}, so there is nothing here to act on"),
                "{said:?}"
            );
        }
    }

    #[test]
    fn measuring_a_pass_fail_gate_is_refused_rather_than_answered_with_zero() {
        fn ok(_ctx: &Ctx) -> Result<Outcome, String> {
            Ok(Outcome::passed())
        }
        let gate = gate_of(Kind::Binary(ok));
        assert!(measure(&gate, &ctx_with("probe", &[])).is_err());
    }

    #[test]
    fn a_key_naming_a_function_inside_a_file_splits_into_both() {
        let mut was = Series::new();
        was.set("src/a.rs#parse", 3);
        let change = crate::run::baseline::Change::Grew {
            key: "src/a.rs#parse".to_string(),
            was: 3,
            now: 9,
        };
        let finding = finding_for(&change, &was, "cognitive");
        assert_eq!(finding.file, "src/a.rs");
        assert_eq!(finding.item.as_deref(), Some("parse"));
        assert_eq!(finding.baseline, Some(3));
        assert_eq!(finding.measured, Some(9));
    }

    #[test]
    fn a_key_that_names_no_file_is_carried_as_the_item() {
        let change = crate::run::baseline::Change::Grew {
            key: "clippy::redundant_clone".to_string(),
            was: 1,
            now: 4,
        };
        let finding = finding_for(&change, &Series::new(), "findings");
        assert_eq!(finding.file, "");
        assert_eq!(finding.item.as_deref(), Some("clippy::redundant_clone"));
    }

    #[test]
    fn a_key_that_is_a_bare_path_is_carried_as_the_file() {
        for key in ["src/a.rs", "docs/design.md", "a.rs"] {
            let change = crate::run::baseline::Change::Grew {
                key: key.to_string(),
                was: 1,
                now: 2,
            };
            let finding = finding_for(&change, &Series::new(), "line(s)");
            assert_eq!(finding.file, key, "{key}");
            assert_eq!(finding.item, None, "{key}");
        }
    }

    #[test]
    fn the_depth_one_below_the_limit_still_runs_and_the_limit_itself_does_not() {
        let recursion = Some("refusing to recurse: chock is already two gates deep");
        let below = run_one(&ratchet_gate(), &at_depth(MAX_DEPTH - 1));
        assert_ne!(below.cannot_run_reason.as_deref(), recursion);
        let at_limit = run_one(&ratchet_gate(), &at_depth(MAX_DEPTH));
        assert_eq!(at_limit.cannot_run_reason.as_deref(), recursion);
        assert_eq!(at_limit.verdict, Verdict::CannotRun);
    }

    #[test]
    fn a_gate_asked_to_run_inside_another_gate_refuses_rather_than_forking() {
        let report = run_one(&ratchet_gate(), &at_depth(MAX_DEPTH));
        assert_eq!(report.verdict, Verdict::CannotRun);
        assert_eq!(report.exit_code, 2);
        assert_eq!(
            report.cannot_run_reason.as_deref(),
            Some("refusing to recurse: chock is already two gates deep")
        );
    }

    #[test]
    fn the_refusal_covers_every_gate_in_the_registry_not_only_the_spawning_ones() {
        let deep = at_depth(MAX_DEPTH);
        for gate in gates::registry() {
            let report = run_one(gate, &deep);
            assert_eq!(
                report.verdict,
                Verdict::CannotRun,
                "{} ran while recursing",
                gate.name
            );
        }
    }
}
