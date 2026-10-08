//! What a gate is, and the one place a measurement meets its baseline. A ratchet written per gate
//! becomes its own reader, comparer and updater; here a gate supplies numbers and nothing else.

pub mod baseline;
pub mod debt;
pub mod evidence;
pub mod lock;
pub mod report;
pub mod verdicts;

mod context;
mod lanes;
pub(crate) mod workers;
pub use context::{
    Coverage, Ctx, LCOV, Runner, coverage_for, default_coverage, default_runner, runner_for,
};

use std::hash::{Hash, Hasher};
use std::time::Instant;

use crate::run::baseline::{Keys, Series};
use crate::run::report::{Detail, Finding, GateReport, Run, Verdict};
pub(crate) use lanes::recorded_tests;
use workers::on_workers;

/// What a pass/fail gate found. `passed` is the tool's own verdict rather than a count of
/// findings, because a tool can warn about things that are not failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub passed: bool,
    pub findings: Vec<Finding>,
    /// What the tool wrote, for when no finding could be read from it; the reason quotes its tail.
    pub said: Option<String>,
    /// What a pass-or-fail gate counted, what its record holds, and the unit of both.
    pub counted: Option<(u64, u64, &'static str)>,
    /// Each test's time in ms under its binary and name, from a gate that timed its tests.
    pub tests_ms: std::collections::BTreeMap<String, u64>,
}

impl Outcome {
    #[must_use]
    pub fn passed() -> Self {
        Self {
            passed: true,
            findings: Vec::new(),
            said: None,
            counted: None,
            tests_ms: std::collections::BTreeMap::new(),
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
            counted: None,
            tests_ms: std::collections::BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn failed(findings: Vec<Finding>) -> Self {
        Self {
            passed: false,
            findings,
            said: None,
            counted: None,
            tests_ms: std::collections::BTreeMap::new(),
        }
    }

    /// The number this run counted beside the number on record, so the report shows both.
    #[must_use]
    pub fn counting(mut self, measured: u64, baseline: u64, unit: &'static str) -> Self {
        self.counted = Some((measured, baseline, unit));
        self
    }

    /// Everything the tool wrote, kept so a failure no finding could be read from still shows it.
    #[must_use]
    pub fn saying(mut self, out: &crate::exec::Output) -> Self {
        let cut = cut_short(out.truncated);
        self.said = Some(format!("{}{}{cut}", out.stdout, out.stderr));
        self
    }
}

/// Ends what a cut capture said, so the last lines chock kept are not read as the tool's last.
const CUT_SHORT: &str =
    "\n(the tool printed more than chock keeps, so these are not the last lines it wrote)";

/// `CUT_SHORT` where the capture was cut, and nothing where it holds all the tool wrote.
fn cut_short(truncated: bool) -> &'static str {
    match truncated {
        true => CUT_SHORT,
        false => "",
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
    /// What the gate knows about a key beyond its number, by key.
    pub details: Details,
}

/// A gate's details, by the key they are about.
pub type Details = std::collections::BTreeMap<String, Detail>;

impl Measurement {
    /// A series and its notes, with no details for any key.
    #[must_use]
    pub fn of(series: Series, findings: Vec<Finding>) -> Self {
        Self {
            series,
            findings,
            details: Details::new(),
        }
    }

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
    /// Debt on record passes. A local run takes the first record; CI, which writes none, holds a
    /// gate without one to zero.
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

#[cfg(test)]
impl Kind {
    /// What a ratchet keys its record by and counts in; `None` for a gate that reads a verdict.
    pub(crate) fn ratcheted(&self) -> Option<(Keys, &'static str)> {
        match self {
            Self::Ratchet { keys, unit, .. } | Self::AnnotatedRatchet { keys, unit, .. } => {
                Some((*keys, unit))
            }
            Self::Binary(_) | Self::Debt { .. } => None,
        }
    }
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
        Kind::Ratchet { measure, .. } => {
            measure(ctx).map(|series| Measurement::of(series, Vec::new()))
        }
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
    let held = crate::run::verdicts::Held {
        root: &ctx.root,
        listed: &ctx.listing,
        version_of: &|tool| crate::run::verdicts::installed(&ctx.root, tool),
        runner_tools: &ctx.runner.tools,
        coverage_tools: &ctx.coverage.tools,
        record: own_record(gate, ctx),
    };
    let key = crate::run::verdicts::key(reads, gate.name, &held);
    key.map(|key| key_format(gate, reads.reader, ctx.ci, &key))
        .map(|key| with_part(gate, ctx, key))
        .and_then(|key| with_change_set(gate, ctx, key))
}

/// A verdict over one part of the Miri suite answers for that part alone.
fn with_part(gate: &Gate, ctx: &Ctx, key: String) -> String {
    match ctx
        .miri_part
        .filter(|_| gate.name == crate::gates::tools::miri::GATE.name)
    {
        Some(part) => format!("{key}:part-{}-of-{}", part.index, part.of),
        None => key,
    }
}

/// A digest of what the gate's verdict is held to: its record and unit, and the file a gate keeps
/// for itself. Another gate's record is not in it, so a gain there keeps this verdict.
fn own_record(gate: &Gate, ctx: &Ctx) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ctx.baseline.recorded(gate.name).hash(&mut hasher);
    ctx.baseline.unit(gate.name).hash(&mut hasher);
    let own = crate::gates::own_baseline(gate.name);
    own.map(|file| std::fs::read(ctx.root.join(file)).ok())
        .hash(&mut hasher);
    hasher.finish()
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
fn key_format(gate: &Gate, reader: &str, ci: bool, key: &str) -> String {
    let scoped = matches!(gate.kind, Kind::AnnotatedRatchet { .. }).then_some("scope-v2");
    // A recalled pass lowers no record, so one kept before records were lowered must not answer.
    let lowers = gate.counts_in().map(|_| "lowers-v1");
    // CI fails a gain the record lacks where a local run writes it, so the two share no verdict.
    let mode = ci.then_some("ci");
    let parts: Vec<&str> = [reader]
        .into_iter()
        .filter(|reader| !reader.is_empty())
        .chain(scoped)
        .chain(lowers)
        .chain(mode)
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
    // `--no-cache` recalls nothing; the verdict this run takes is still kept under the key.
    let kept = key
        .as_deref()
        .filter(|_| !ctx.no_cache)
        .and_then(|key| crate::run::verdicts::recall(&ctx.root, gate.name, key));
    if let Some(mut report) = kept {
        // Marked, so a recalled verdict never reads as one taken now.
        report.recalled = true;
        report.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        return report;
    }
    let peak = std::sync::Arc::new(crate::exec::peak::Peak::default());
    let counted = crate::exec::peak::enter(Some(std::sync::Arc::clone(&peak)));
    let mut report = judged(gate, ctx);
    drop(counted);
    report.peak_mb = peak.most();
    report.fix = advice(&report);
    report.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if let Some(key) = kept_under(gate, ctx, key) {
        crate::run::verdicts::keep(&ctx.root, gate.name, &key, &report);
    }
    report
}

/// The key a judged verdict is kept under. A gate that keeps its own record may have written its
/// first one while judging, so its key is read again and names the record it was held to.
fn kept_under(gate: &Gate, ctx: &Ctx, asked: Option<String>) -> Option<String> {
    match crate::gates::own_baseline(gate.name) {
        Some(_) => keyed(gate, ctx),
        None => asked,
    }
}

/// What to do next: the fix for a gate that tripped, the install for one a tool stopped.
fn advice(report: &GateReport) -> Option<String> {
    let fix = crate::gates::fixes::fix(&report.gate).filter(|_| report.verdict == Verdict::Tripped);
    let install = report
        .cannot_run_reason
        .as_deref()
        .and_then(crate::gates::fixes::repair);
    fix.or(install).map(str::to_string)
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
    let compiled = lanes::in_lanes(gates, ctx, told);
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
            &crate::gates::fixes::not_installed(missing),
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
    Ok(Measurement::of(inspection.series(), inspection.debt))
}

fn debt_report(gate: &Gate, ctx: &Ctx, inspect: Inspect, unit: &str) -> GateReport {
    let inspection = match inspect(ctx) {
        Ok(inspection) => inspection,
        Err(why) => return outcome_report(gate, Err(why)),
    };
    if !inspection.blockers.is_empty() || held_to_zero(gate, ctx) {
        return outcome_report(gate, Ok(inspection.outcome()));
    }
    let read = Measurement::of(inspection.series(), inspection.debt);
    measured_ratchet(gate, ctx, read, Keys::Items, unit)
}

fn binary(gate: &Gate, ctx: &Ctx, check: Check) -> GateReport {
    outcome_report(gate, check(ctx))
}

pub(crate) fn outcome_report(gate: &Gate, outcome: Result<Outcome, String>) -> GateReport {
    match outcome {
        Err(reason) => GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &reason),
        // A passing gate keeps its findings: they are advisories.
        Ok(outcome) if outcome.passed => reported(gate, Verdict::Pass, outcome),
        Ok(outcome) if outcome.findings.is_empty() => GateReport::cannot_run(
            gate.name,
            rerun(gate.name).as_str(),
            &said_nothing(outcome.said.as_deref()),
        ),
        Ok(outcome) => reported(gate, Verdict::Tripped, outcome),
    }
}

/// A report with the outcome's findings and, where the gate counted, its two numbers.
fn reported(gate: &Gate, verdict: Verdict, outcome: Outcome) -> GateReport {
    let mut report = GateReport::new(gate.name, verdict, rerun(gate.name).as_str());
    if let Some((measured, baseline, unit)) = outcome.counted {
        report.measured = Some(measured);
        report.baseline = Some(baseline);
        report.unit = Some(unit.to_string());
    }
    report.findings = outcome.findings;
    report.tests_ms = outcome.tests_ms;
    report
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
    // The tail is often a build script's `cargo:` lines; a missing system library is far above.
    let marked = said
        .and_then(crate::exec::marked)
        .map(|line| format!(" What it marked as the error: {line}."))
        .unwrap_or_default();
    format!("{SAID_NOTHING}.{marked} Its last {QUOTED_LINES} lines:\n{tail}")
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

/// Whether the gate is held to a record it does not have yet.
fn lacks_record(gate: &Gate, ctx: &Ctx) -> bool {
    !ctx.baseline.has(gate.name) && !strict(gate, ctx)
}

/// Whether this run takes the gate's first record. CI writes no record, so it takes none.
fn takes_first(gate: &Gate, ctx: &Ctx) -> bool {
    !ctx.ci && lacks_record(gate, ctx)
}

/// No record and none to take: a strict gate, or any gate in CI, holds the tree to zero.
fn held_to_zero(gate: &Gate, ctx: &Ctx) -> bool {
    !ctx.baseline.has(gate.name) && !takes_first(gate, ctx)
}

/// What CI says of a gate whose record no change committed.
fn uncommitted(gate: &str) -> String {
    format!(
        "no record `{}` is committed — `chock run {gate}` outside CI takes the first one and \
         writes {}",
        crate::run::baseline::kept_here(gate),
        crate::run::baseline::FILE
    )
}

/// A report with its two numbers and what they count.
fn numbered(gate: &Gate, verdict: Verdict, now: u64, was: u64, unit: &str) -> GateReport {
    let mut report = GateReport::new(gate.name, verdict, rerun(gate.name).as_str());
    report.measured = Some(now);
    report.baseline = Some(was);
    report.unit = Some(unit.to_string());
    report
}

/// What a first record says beside its number. An annotated ratchet names each site past its
/// record, and with none every site is past it, so only its notes, which name no file, stay.
fn first_notes(gate: &Gate, findings: Vec<Finding>) -> Vec<Finding> {
    let sited = matches!(gate.kind, Kind::AnnotatedRatchet { .. });
    findings
        .into_iter()
        .filter(|found| !sited || found.file.is_empty())
        .collect()
}

/// A gate's first local run: what it measured is its record from here on, written whatever the
/// verdict. A file the change touched is still held to zero where the project asks that.
fn first_record(gate: &Gate, ctx: &Ctx, read: Measurement, keys: Keys, unit: &str) -> GateReport {
    let now = read.series;
    let total = now.0.values().sum();
    let mut report = numbered(gate, Verdict::Pass, total, total, unit);
    report.findings = first_notes(gate, read.findings);
    if let Err(why) = touched(&mut report, gate, ctx, &now, &now, keys) {
        return GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &why);
    }
    report.tightened = Some(now);
    report
}

/// The ratchet, in the only place it is written. A local run takes a gate's first record from what
/// it measured. CI writes no record, so there a gate without one cannot rule on the tree.
fn ratchet(gate: &Gate, ctx: &Ctx, keys: Keys, unit: &str) -> GateReport {
    // A missing tool is said before a missing baseline: recording one would not help.
    if let Some(missing) = absent_tool(gate, ctx) {
        let mut report = GateReport::cannot_run(
            gate.name,
            rerun(gate.name).as_str(),
            &crate::gates::fixes::not_installed(missing),
        );
        report.unit = Some(unit.to_string());
        return report;
    }
    // Asked before measuring, which can take minutes of release builds.
    if ctx.ci && lacks_record(gate, ctx) {
        let mut report = GateReport::cannot_run(
            gate.name,
            rerun(gate.name).as_str(),
            &uncommitted(gate.name),
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
    if let Err(why) = portable(&read.series) {
        return GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &why);
    }
    if takes_first(gate, ctx) {
        return first_record(gate, ctx, read, keys, unit);
    }
    let now = read.series;
    let total = now.0.values().sum();
    let was = held_against(gate, ctx, &now);
    let regroup = regrouped(gate, &now, &was, keys);
    let recount = recounted(ctx.baseline.unit(gate.name), unit);
    if let Some(reason) = regroup.or(recount) {
        return GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &reason);
    }
    let (verdict, findings) = compared(&now, &was, keys, gate.name, unit, &read.details);
    let mut report = numbered(gate, verdict, total, was.0.values().sum(), unit);
    report.findings = findings;
    report.findings.extend(read.findings);
    if let Err(why) = touched(&mut report, gate, ctx, &now, &was, keys) {
        return GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &why);
    }
    lock_in(&mut report, gate, ctx, now.tightened(&was, keys), &was);
    report
}

/// A reader that re-keys makes every old key absent and every new one look like new debt.
fn regrouped(gate: &Gate, now: &Series, was: &Series, keys: Keys) -> Option<String> {
    if matches!(gate.kind, Kind::Debt { .. }) {
        return None;
    }
    rekeyed(now, was, keys)
}

/// A gate that changes what it counts keeps its keys, so only the unit tells the numbers apart.
fn recounted(held: Option<&str>, unit: &str) -> Option<String> {
    let before = held.filter(|before| *before != unit)?;
    Some(format!(
        "this run counts {unit} where the record holds {before}, so the numbers are not \
         comparable — re-record with `chock baseline <gate>`"
    ))
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

/// What the comparison says, once it is known to be possible. Each of its three questions goes
/// only to the key kinds it fits, and only the first two can fail the gate.
fn compared(
    now: &Series,
    was: &Series,
    keys: Keys,
    gate: &str,
    unit: &str,
    details: &Details,
) -> (Verdict, Vec<Finding>) {
    let regressions = now.regressions(was, keys);
    // A census row on record that this run did not produce is a check that stopped happening, and
    // no number reports it. An absent item is a deleted file, so the question is not asked.
    let dropped = match keys {
        Keys::Census => now.stopped_measuring(was),
        Keys::Items | Keys::Sizes { .. } | Keys::Measures => Vec::new(),
    };
    // A new key at zero that is not an item means the checking grew and found nothing: a note.
    let added = match keys {
        Keys::Measures | Keys::Census => now.new_at_zero(was),
        Keys::Items | Keys::Sizes { .. } => Vec::new(),
    };
    let verdict = match regressions.is_empty() && dropped.is_empty() {
        true => Verdict::Pass,
        false => Verdict::Tripped,
    };
    let findings = regressions
        .iter()
        .map(|change| finding_for(change, was, unit).detailed(details.get(change.key())))
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

/// A key this run measured at zero and the baseline does not carry. It passes: zero is no worse
/// than absent, and the same key above zero is a regression.
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
    use crate::gates::tools::miri::Part;
    use crate::run::baseline::Baseline;
    use crate::run::report::locked_in;
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
    fn a_debt_gate_takes_its_first_record_in_a_local_run_and_ci_holds_it_to_zero() {
        let ctx = ctx_with("another", &[]);
        let report = run_one(&debt_gate(existing_debt), &ctx);
        assert_eq!(report.verdict, Verdict::Pass);
        assert_eq!((report.measured, report.baseline), (Some(1), Some(1)));
        assert_eq!(report.findings, existing_debt(&ctx).unwrap().debt);
        let mut first = Series::new();
        first.set("src/unused.rs", 1);
        assert_eq!(report.tightened, Some(first));
        // CI writes no record, so there the same debt fails and only a clean tree passes.
        let ci = Ctx {
            ci: true,
            ..ctx_with("another", &[])
        };
        let held = run_one(&debt_gate(existing_debt), &ci);
        assert_eq!(
            (held.verdict, held.tightened.clone()),
            (Verdict::Tripped, None)
        );
        assert_eq!(held.findings, existing_debt(&ci).unwrap().debt);
        let clean = debt_gate(|_| Ok(Inspection::default()));
        assert_eq!(run_one(&clean, &ci).verdict, Verdict::Pass);
        // A clean tree's first record is empty, and it is still a record.
        assert_eq!(run_one(&clean, &ctx).tightened, Some(Series::new()));
    }

    #[test]
    fn a_blocker_or_a_strict_debt_gate_takes_no_first_record() {
        let ctx = ctx_with("another", &[]);
        let blocked = run_one(&debt_gate(blocked_debt), &ctx);
        assert_eq!(
            (blocked.verdict, blocked.tightened),
            (Verdict::Tripped, None)
        );
        let mut strict = ctx_with("another", &[]);
        strict.strict = vec!["probe".to_string()];
        let held = run_one(&debt_gate(existing_debt), &strict);
        assert_eq!((held.verdict, held.tightened), (Verdict::Tripped, None));
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

    fn ratchet_of(measure: Measure, keys: Keys, unit: &'static str) -> Gate {
        gate_of(Kind::Ratchet {
            measure,
            keys,
            unit,
        })
    }

    fn annotated_of(measure: AnnotatedMeasure, keys: Keys, unit: &'static str) -> Gate {
        gate_of(Kind::AnnotatedRatchet {
            measure,
            keys,
            unit,
        })
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
        let gate = ratchet_of(one_file_four_ways, Keys::Items, "lines");
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
        let gate = ratchet_of(one_file_four_ways, Keys::Items, "lines");
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
        let measures = ratchet_of(two_items, Keys::Measures, "lines");
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

        let refused = survey_one(&ratchet_of(measured_nothing, Keys::Items, "lines"), &ctx);
        assert_eq!(refused.verdict, Verdict::CannotRun);
        assert!(
            refused
                .cannot_run_reason
                .unwrap_or_default()
                .contains("nothing chock could read")
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_survey_still_refuses_a_gate_whose_tool_is_not_installed() {
        let ctx = ctx_with("probe", &[]);
        let report = survey_one(&gate_without_its_tool(), &ctx);
        assert_eq!(report.verdict, Verdict::CannotRun);
        assert!(
            report
                .cannot_run_reason
                .unwrap_or_default()
                .contains("a-tool-nothing-installs")
        );
    }

    fn ratchet_gate() -> Gate {
        ratchet_of(two_items, Keys::Items, "lines")
    }

    /// A ratchet that reads a tool no machine holds.
    fn gate_without_its_tool() -> Gate {
        let mut gate = ratchet_gate();
        gate.reads = Some(crate::run::verdicts::Reads::tree_and(&[
            "a-tool-nothing-installs",
        ]));
        gate
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
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_ratchet_whose_tool_is_absent_names_the_tool_and_not_the_baseline() {
        let ctx = ctx_with("probe", &[]);
        let report = run_one(&gate_without_its_tool(), &ctx);
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
        let gate = ratchet_of(a_key_from_another_tree, Keys::Items, "lines");
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
        let settled = locked_in(&ctx.baseline, &[report]).unwrap();
        assert_eq!(settled.record.gate("slop"), lower);
        assert_eq!(
            (settled.first.clone(), settled.lowered.clone()),
            (Vec::new(), vec!["slop".to_string()])
        );
        assert_eq!(settled.said(), "lowered the record for slop");
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
        let said: Vec<String> = Finding::rendered(&report.findings);
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
        let said: Vec<String> = Finding::rendered(&report.findings);
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
    fn a_gate_a_missing_tool_stopped_carries_the_install_and_any_other_refusal_carries_none() {
        let stopped = |reason: &str| {
            advice(&GateReport::cannot_run(
                "mutest",
                "chock run mutest",
                reason,
            ))
        };
        assert_eq!(
            stopped(&crate::gates::fixes::not_installed("cargo-mutest")).as_deref(),
            Some(crate::gates::mutation::tool::REPAIR)
        );
        assert_eq!(stopped("the suite does not compile"), None);
        // `unused-deep` reads no finding where rustup has no `nightly`; rustup's words name the repair.
        let no_nightly = crate::exec::Output::of(
            Some(1),
            "",
            "error: toolchain 'nightly-x86_64-unknown-linux-gnu' is not installed\n",
        );
        let unread = Outcome::failed(Vec::new()).saying(&no_nightly);
        let unread = outcome_report(&settling_gate(), Ok(unread));
        assert_eq!(unread.verdict, Verdict::CannotRun);
        assert_eq!(
            advice(&unread).as_deref(),
            crate::gates::fixes::repair(crate::gates::tools::miri::ABSENT)
        );
        assert!(advice(&unread).is_some_and(|fix| fix.contains("chock init --global")));
        // A refusal with no reason on it has nothing to read.
        let silent = GateReport::new("mutest", Verdict::CannotRun, "chock run mutest");
        assert_eq!(advice(&silent), None);
    }

    #[test]
    fn a_part_of_the_miri_suite_keeps_its_verdict_under_a_key_of_its_own() {
        let miri = gates::tools::miri::GATE;
        let mut ctx = Ctx::for_root(PathBuf::from("/w"), Baseline::empty("0.1.0"));
        assert_eq!(with_part(&miri, &ctx, "k".to_string()), "k");
        ctx.miri_part = Some(Part { index: 2, of: 12 });
        assert_eq!(with_part(&miri, &ctx, "k".to_string()), "k:part-2-of-12");
        let other = gates::tools::binsize::GATE;
        assert_eq!(with_part(&other, &ctx, "k".to_string()), "k");
    }

    #[test]
    fn a_verdict_is_keyed_on_the_record_of_its_own_gate_and_of_no_other() {
        let gate = ratchet_gate();
        let same = own_record(&gate, &ctx_with("probe", &[("src/a.rs", 10)]));
        let mut other = ctx_with("probe", &[("src/a.rs", 10)]);
        other.baseline.set("slop", Series::new());
        assert_eq!(own_record(&gate, &other), same);
        let lower = ctx_with("probe", &[("src/a.rs", 9)]);
        assert_ne!(own_record(&gate, &lower), same);
        let mut united = ctx_with("probe", &[("src/a.rs", 10)]);
        united
            .baseline
            .units
            .insert("probe".to_string(), "lines".to_string());
        assert_ne!(own_record(&gate, &united), same);
        // No record takes a first one and an empty record holds zero, so the two key apart.
        assert_ne!(
            own_record(&gate, &ctx_with("another", &[])),
            own_record(&gate, &ctx_with("probe", &[]))
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_that_keeps_its_own_record_is_keyed_again_after_it_was_judged() {
        let dir = crate::testdir::tree("run-kept-under", &[("a.rs", "fn a() {}\n")]);
        let ctx = Ctx::at(&dir);
        let plain = declaring(agreeable);
        let asked = Some("asked".to_string());
        assert_eq!(kept_under(&plain, &ctx, asked.clone()), asked);
        assert_eq!(kept_under(&plain, &ctx, None), None);
        let own = Gate {
            name: "crap",
            ..declaring(agreeable)
        };
        let before = keyed(&own, &ctx);
        assert!(before.is_some());
        assert_eq!(kept_under(&own, &ctx, asked), before);
        // Once the gate wrote its first record, the key names that record.
        let file = dir.join(crate::gates::own_baseline("crap").unwrap());
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "{}").unwrap();
        let after = kept_under(&own, &ctx, before.clone());
        assert!(after.is_some());
        assert_ne!(after, before);
    }

    /// Recalled, a verdict writes nothing, so one that leaves a record to write is judged again.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_verdict_that_took_a_first_record_is_not_recalled() {
        let dir = crate::testdir::tree("run-first-record", &[("a.rs", "fn a() {}\n")]);
        let ctx = Ctx::at(&dir);
        let gate = Gate {
            reads: Some(crate::run::verdicts::Reads::tree_and(&[])),
            ..ratchet_gate()
        };
        assert!(
            keyed(&gate, &ctx).is_some(),
            "the gate has a key to keep under"
        );
        let first = run_one(&gate, &ctx);
        assert_eq!(first.tightened, Some(two_items(&ctx).unwrap()));
        let again = run_one(&gate, &ctx);
        assert!(!again.recalled);
        assert_eq!(again.tightened, first.tightened);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_run_told_not_to_recall_judges_again_and_still_keeps_its_verdict() {
        fn broken(_ctx: &Ctx) -> Result<Outcome, String> {
            Ok(Outcome::failed(vec![Finding::at("a.rs", "broken")]))
        }
        let dir = crate::testdir::tree("run-no-cache", &[("a.rs", "fn a() {}\n")]);
        let ctx = Ctx::at(&dir);
        assert!(!run_one(&declaring(agreeable), &ctx).recalled);
        let fresh = Ctx {
            no_cache: true,
            ..Ctx::at(&dir)
        };
        let judged = run_one(&declaring(broken), &fresh);
        assert_eq!((judged.verdict, judged.recalled), (Verdict::Tripped, false));
        // What it judged took the place of the verdict kept before.
        let later = run_one(&declaring(never_checked), &ctx);
        assert_eq!((later.verdict, later.recalled), (Verdict::Tripped, true));
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

    /// CI fails a gain that a local run writes, so the two keep their verdicts apart.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_ci_run_keeps_its_verdict_under_its_own_key() {
        let dir = crate::testdir::tree("run-ci-key", &[("a.rs", "fn a() {}\n")]);
        let local = Ctx::at(&dir);
        let here = keyed(&declaring(agreeable), &local).unwrap();
        let ci = Ctx { ci: true, ..local };
        let there = keyed(&declaring(agreeable), &ci).unwrap();
        assert_eq!(there, format!("ci:{here}"));
    }

    /// The case the verdict record exists for; the report says it was recalled.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_that_names_its_inputs_answers_the_second_time_without_running() {
        let dir = crate::testdir::tree("run-recalled", &[("a.rs", "fn a() {}\n")]);
        let ctx = Ctx::at(&dir);
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

    /// A touched file under `clean_when_touched` is held to zero, so its sites are all named.
    #[test]
    fn a_file_held_clean_where_touched_names_its_sites_against_no_record() {
        let touching = kept_clean(Ok(vec!["src/a.rs".to_string()]));
        let mut rest = Series::new();
        rest.set("src/b.rs#untouched", 5);
        assert_eq!(touching.named_against("probe"), rest);
        let whole = touching.record("probe");
        assert_eq!(whole.get("src/a.rs#at_record"), Some(3));
        assert_eq!(whole.get("src/a.rs#over_record"), Some(2));
        let mut unlisted = kept_clean(Ok(vec!["src/a.rs".to_string()]));
        unlisted.clean_when_touched = vec!["other".to_string()];
        assert_eq!(unlisted.named_against("probe"), whole);
        let unread = kept_clean(Err("no repository".to_string()));
        assert_eq!(unread.named_against("probe"), whole);
    }

    /// A verdict kept for one change set never answers for another, and none is kept without one.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_held_clean_where_touched_runs_again_when_the_change_set_moves() {
        let dir = crate::testdir::tree("run-touched-key", &[("a.rs", "fn a() {}\n")]);
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
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_changing_puts_the_gate_back_to_running() {
        let dir = crate::testdir::tree("run-recalled-moved", &[("a.rs", "fn a() {}\n")]);
        let ctx = Ctx::at(&dir);
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
        panic!("in CI a ratchet with no record must refuse before it measures anything")
    }

    #[test]
    fn in_ci_a_ratchet_with_no_record_refuses_before_it_measures() {
        let ctx = Ctx {
            ci: true,
            ..Ctx::for_root(PathBuf::from("/w"), Baseline::empty("0.1.0"))
        };
        let gate = ratchet_of(never_reached, Keys::Items, "lines");
        let report = run_one(&gate, &ctx);
        assert_eq!(report.verdict, Verdict::CannotRun);
        assert_eq!(
            report.cannot_run_reason.as_deref(),
            Some(
                "no record `probe` is committed — `chock run probe` outside CI takes the first \
                 one and writes .chock/baseline.json"
            )
        );
        // No number, because none was taken. A zero here would read as a clean measurement.
        assert_eq!(report.measured, None);
        assert_eq!(report.unit.as_deref(), Some("lines"));
    }

    #[test]
    fn only_a_local_run_of_a_gate_held_to_a_record_it_lacks_takes_the_first_one() {
        let gate = ratchet_gate();
        let asked = |ctx: &Ctx| (lacks_record(&gate, ctx), takes_first(&gate, ctx));
        assert_eq!(asked(&ctx_with("another", &[])), (true, true));
        let ci = Ctx {
            ci: true,
            ..ctx_with("another", &[])
        };
        assert_eq!(asked(&ci), (true, false), "CI writes no record");
        assert_eq!(asked(&ctx_with("probe", &[])), (false, false));
        let mut strict = ctx_with("another", &[]);
        strict.strict = vec!["probe".to_string()];
        assert_eq!(
            asked(&strict),
            (false, false),
            "held to zero, not to a record"
        );
    }

    #[test]
    fn a_local_run_takes_a_ratchets_first_record_from_what_it_measured() {
        let ctx = ctx_with("another", &[]);
        let report = run_one(&ratchet_gate(), &ctx);
        assert_eq!(report.verdict, Verdict::Pass);
        assert_eq!((report.measured, report.baseline), (Some(15), Some(15)));
        assert_eq!(report.unit.as_deref(), Some("lines"));
        assert_eq!(report.findings, Vec::new());
        let counted = two_items(&ctx).unwrap();
        assert_eq!(report.tightened, Some(counted.clone()));
        let settled = locked_in(&ctx.baseline, &[report]).unwrap();
        assert_eq!(settled.record.gate("probe"), counted);
        assert_eq!(settled.record.unit("probe"), Some("lines"));
        assert!(settled.record.has("another"), "the other records stay");
        assert_eq!(
            (settled.first.clone(), settled.lowered.clone()),
            (vec!["probe".to_string()], Vec::new())
        );
        assert_eq!(settled.said(), "wrote the first record for probe");
    }

    #[test]
    fn a_first_record_of_an_annotated_ratchet_names_no_site_and_keeps_its_notes() {
        fn sited(ctx: &Ctx) -> Result<Measurement, String> {
            let mut read = scoped_measurement(ctx)?;
            read.findings
                .push(Finding::at("src/a.rs", "survived").item("eq_op_invert"));
            Ok(read)
        }
        let gate = annotated_of(sited, Keys::Items, "survivors");
        let ctx = ctx_with("another", &[]);
        let report = run_one(&gate, &ctx);
        let scoped = scoped_measurement(&ctx).unwrap();
        assert_eq!(report.verdict, Verdict::Pass);
        assert_eq!(report.findings, scoped.findings);
        assert_eq!(report.tightened, Some(scoped.series));
    }

    #[test]
    fn a_first_record_is_written_also_where_a_touched_file_trips_the_gate() {
        let with_no_record = |changed: Result<Vec<String>, String>| Ctx {
            baseline: Baseline::empty("0.1.0"),
            ..kept_clean(changed)
        };
        let ctx = with_no_record(Ok(vec!["src/a.rs".to_string()]));
        let report = run_one(&ratchet_gate(), &ctx);
        assert_eq!((report.verdict, report.exit_code), (Verdict::Tripped, 1));
        let said: Vec<String> = Finding::rendered(&report.findings);
        assert_eq!(
            said,
            [
                "src/a.rs: 10 lines in a file this change touched: `clean_when_touched` holds it to zero"
            ]
        );
        assert_eq!(report.tightened, Some(two_items(&ctx).unwrap()));
        // The changed files could not be read, so nothing was judged and nothing is recorded.
        let unread = with_no_record(Err("no repository".to_string()));
        let refused = run_one(&ratchet_gate(), &unread);
        assert_eq!(
            (refused.verdict, refused.tightened),
            (Verdict::CannotRun, None)
        );
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

    /// The false green this answers: a count the record lacks passed with a note at every size.
    #[test]
    fn a_measure_the_baseline_never_saw_trips_once_it_counts_anything() {
        let gate = ratchet_of(two_items, Keys::Measures, "lines");
        let ctx = ctx_with("probe", &[("src/a.rs", 10)]);
        let report = run_one(&gate, &ctx);
        assert_eq!(report.verdict, Verdict::Tripped);
        assert_eq!(report.exit_code, 1);
        let said: Vec<String> = Finding::rendered(&report.findings);
        assert_eq!(said, vec!["src/b.rs: 5 lines, not in the baseline"]);
    }

    #[test]
    fn a_census_row_this_run_produced_for_the_first_time_trips_once_it_counts_anything() {
        let ctx = ctx_with("probe", &[("src/a.rs", 10)]);
        let report = run_one(&census_gate(two_items), &ctx);
        assert_eq!(report.verdict, Verdict::Tripped);
        let said: Vec<String> = Finding::rendered(&report.findings);
        assert_eq!(said, vec!["src/b.rs: 5 hazard(s), not in the baseline"]);
    }

    /// A lens outpost added is checking that grew, the mirror of a lens it retired. At zero it is
    /// named and passes.
    #[test]
    fn a_census_row_first_produced_at_zero_is_named_in_a_finding_and_still_passes() {
        fn a_quiet_new_lens(_ctx: &Ctx) -> Result<Series, String> {
            let mut series = Series::new();
            series.set("held", 3);
            series.set("quiet", 0);
            Ok(series)
        }
        let ctx = ctx_with("probe", &[("held", 3)]);
        let report = run_one(&census_gate(a_quiet_new_lens), &ctx);
        assert_eq!(report.verdict, Verdict::Pass);
        assert_eq!(report.exit_code, 0);
        assert_eq!(
            report.findings[0].render(),
            "quiet: 0 hazard(s), newly measured — `chock baseline probe` puts it on record"
        );
        assert_eq!(report.findings[0].baseline, None);
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
        let gate = ratchet_of(two_items, Keys::Measures, "findings");
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
        let said: Vec<String> = Finding::rendered(&report.findings);
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
        let gate = ratchet_of(broken, Keys::Items, "lines");
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
        let note = Finding::at(
            "",
            "not applicable to linked-crate mutation; not mutation-covered",
        );
        Ok(Measurement::of(
            series,
            vec![note.item("integration test commands")],
        ))
    }

    fn scoped_ratchet() -> Gate {
        annotated_of(scoped_measurement, Keys::Items, "survivors")
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn an_unannotated_cached_verdict_cannot_answer_for_a_scope_aware_measurement() {
        let dir = crate::testdir::make("scope-key");
        let gate = scoped_ratchet();
        let old = GateReport::new("probe", Verdict::Pass, "chock run probe");
        crate::run::verdicts::keep(&dir, "probe", "same-inputs", &old);
        let key = key_format(&gate, "", false, "same-inputs");
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
            key_format(&ratchet_gate(), "", false, "same-inputs"),
            "lowers-v1:same-inputs"
        );
        assert_eq!(
            key_format(&gates::tools::TEST, "", false, "same-inputs"),
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
    fn a_gate_that_counted_shows_both_numbers_on_a_pass_and_on_a_trip() {
        let gate = ratchet_gate();
        let numbers = |outcome: Outcome| {
            let report = outcome_report(&gate, Ok(outcome.counting(3, 2, "function(s)")));
            (report.measured, report.baseline, report.unit)
        };
        let both = (Some(3), Some(2), Some("function(s)".to_string()));
        assert_eq!(numbers(Outcome::passed()), both);
        let tripped = Outcome::failed(vec![Finding::at("src/lib.rs", "broken")]);
        assert_eq!(numbers(tripped), both);
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
            assert_eq!(key_format(gate, reader, false, "input"), expected);
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
        let gate = annotated_of(failed, Keys::Items, "survivors");
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
                ..Outcome::passed()
            })
        }
        fn on_caller(_ctx: &Ctx) -> Result<Outcome, String> {
            Ok(Outcome {
                passed: on_the_caller(),
                ..Outcome::passed()
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
                ..Outcome::passed()
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
        assert_eq!(
            lanes::lanes_of(&gates, &ctx, &[], 0),
            [vec![0, 2], vec![1, 3]]
        );
    }

    #[test]
    fn a_slow_gate_with_a_record_that_fits_runs_on_a_lane_of_its_own() {
        let named = |name| Gate {
            name,
            builds: true,
            ..gate_of(Kind::Binary(agreeable))
        };
        let (test, mutest, miri) = (named("test"), named("mutest"), named("miri"));
        let bsize = named("bsize");
        let gates = [&test, &mutest, &miri, &bsize];
        let ctx = ctx_with("probe", &[]);
        let peaks =
            [("mutest", 9), ("miri", 7), ("test", 2)].map(|(gate, mb)| (gate.to_string(), mb));
        let each = [vec![0], vec![3], vec![1], vec![2]];
        assert_eq!(lanes::lanes_of(&gates, &ctx, &peaks, 18), each, "9 + 7 + 2");
        assert_eq!(
            lanes::lanes_of(&gates, &ctx, &peaks, 17),
            [vec![0, 2], vec![3], vec![1]]
        );
        let none = [vec![0, 1, 2], vec![3]];
        assert_eq!(
            lanes::lanes_of(&gates, &ctx, &[], 1000),
            none,
            "no record, no lane"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_gate_that_starts_processes_records_the_most_they_held_at_once() {
        fn held(_ctx: &Ctx) -> Result<Outcome, String> {
            let both = "sleep 1.2 & sleep 1.2; wait";
            let here = std::path::Path::new(".");
            crate::exec::run("sh", &["-c", both], here).map_err(|error| error.to_string())?;
            Ok(Outcome::passed())
        }
        let report = run_one(&gate_of(Kind::Binary(held)), &ctx_with("probe", &[]));
        assert_eq!(report.verdict, Verdict::Pass);
        assert!(
            report.peak_mb.is_some_and(|mb| mb >= 2),
            "{:?}",
            report.peak_mb
        );
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
            format!("{SAID_NOTHING}. Its last {QUOTED_LINES} lines:\nline 1\nline 2\nline 3")
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
    fn a_failure_whose_output_was_cut_says_the_quoted_lines_are_not_the_last() {
        let wrote = |truncated| crate::exec::Output {
            code: Some(1),
            stdout: "one\n".to_string(),
            stderr: "two".to_string(),
            truncated,
        };
        let whole = Outcome::failed(Vec::new()).saying(&wrote(false));
        assert_eq!(whole.said.as_deref(), Some("one\ntwo"));
        let cut = Outcome::failed(Vec::new()).saying(&wrote(true));
        assert_eq!(
            said_nothing(cut.said.as_deref()),
            format!("{SAID_NOTHING}. Its last {QUOTED_LINES} lines:\none\ntwo{CUT_SHORT}")
        );
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
