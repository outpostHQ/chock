//! What a gate reports, in one shape for every gate. Exit codes stay the ground truth; an agent
//! or a CI job reads this instead of each tool's prose.

use std::fmt;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::run::baseline::Series;

mod settle;
use settle::stamped;
pub use settle::{Gating, locked_in, remember_or_say, settle, some_of};
#[cfg(test)]
use settle::{forgets, forgotten, lowering, settles};

/// Bumped only by adding a field, so a consumer written against v1 keeps working.
pub const SCHEMA: u32 = 1;

/// The exit-code contract, as a value so it can be asserted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The gate ran and found nothing that fails it.
    #[serde(rename = "passed")]
    Pass,
    /// The gate ran and found something that fails it.
    Tripped,
    /// The gate could not run, so its silence means nothing.
    CannotRun,
}

fn schema_url() -> String {
    crate::project::document::schema_url("run", SCHEMA)
}

impl Verdict {
    /// The exit code of this verdict: 0, 1 or 2.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Tripped => 1,
            Self::CannotRun => 2,
        }
    }

    /// The worst verdict of several. "Could not run" outranks "tripped" because it means no
    /// number was produced.
    #[must_use]
    pub fn worst(verdicts: impl IntoIterator<Item = Self>) -> Self {
        verdicts
            .into_iter()
            .max_by_key(|v| v.code())
            .unwrap_or(Self::Pass)
    }
}

impl fmt::Display for Verdict {
    /// `f.pad`, not `write!`, so the width flag of an aligned column applies.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Self::Pass => "ok",
            Self::Tripped => "TRIPPED",
            Self::CannotRun => "CANNOT RUN",
        })
    }
}

/// One thing to fix, at a place an agent can open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Relative to the project root, so it means the same on the next machine.
    pub file: String,
    /// The line in `file`, when the finding has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The function, lint or measure this is about, when the file alone is not the subject.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The number the gate measured, for a ratchet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measured: Option<u64>,
    /// The number the baseline holds, for a ratchet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<u64>,
    /// What is wrong, in words.
    pub message: String,
    /// The other places it is about, each named by what it is to this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<Place>,
    /// The change that clears it, where the gate can name one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// Whether the finding counts toward the verdict or is only a lead.
    #[serde(default, skip_serializing_if = "Grade::is_finding")]
    pub grade: Grade,
}

/// How sure a gate is: a finding counts toward its verdict, a candidate is a lead the source alone
/// cannot settle, and never trips a gate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Grade {
    /// Counts toward the gate's verdict.
    #[default]
    Finding,
    /// A lead to check, which never trips a gate.
    Candidate,
}

impl Grade {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `skip_serializing_if` passes a reference"
    )]
    fn is_finding(&self) -> bool {
        *self == Self::Finding
    }
}

impl Finding {
    /// A finding in `file` that says `message`, with nothing else set.
    #[must_use]
    pub fn at(file: &str, message: &str) -> Self {
        Self {
            file: file.to_string(),
            line: None,
            item: None,
            measured: None,
            baseline: None,
            message: message.to_string(),
            places: Vec::new(),
            fix: None,
            grade: Grade::Finding,
        }
    }

    /// By file, then line, then item: the one order every gate sorts its findings in, so the
    /// binary holds one copy of the sort.
    pub fn sort_by_place(found: &mut [Self]) {
        found.sort_by(|a, b| (&a.file, a.line, &a.item).cmp(&(&b.file, b.line, &b.item)));
    }

    /// The same finding, graded as a candidate.
    #[must_use]
    pub fn candidate(mut self) -> Self {
        self.grade = Grade::Candidate;
        self
    }

    /// The same finding, at `line`.
    #[must_use]
    pub fn line(mut self, line: u32) -> Self {
        self.line = Some(line);
        self
    }

    /// The same finding, about `item`.
    #[must_use]
    pub fn item(mut self, item: &str) -> Self {
        self.item = Some(item.to_string());
        self
    }

    /// The same finding, with the number measured and the number on record.
    #[must_use]
    pub fn numbers(mut self, measured: u64, baseline: Option<u64>) -> Self {
        self.measured = Some(measured);
        self.baseline = baseline;
        self
    }

    /// Each finding rendered, in order: what a test compares.
    #[cfg(test)]
    pub(crate) fn rendered(findings: &[Self]) -> Vec<String> {
        findings.iter().map(Self::render).collect()
    }

    /// `file:line: message`, the form editors and agents follow. A finding with no file shows its
    /// item instead.
    #[must_use]
    pub fn render(&self) -> String {
        let locus = match (self.file.is_empty(), self.line) {
            (true, _) => self.item.clone().unwrap_or_else(|| "?".to_string()),
            (false, Some(line)) => format!("{}:{}", self.file, line),
            (false, None) => self.file.clone(),
        };
        match &self.item {
            Some(item) if !self.file.is_empty() => format!("{locus}: {item}: {}", self.message),
            _ => format!("{locus}: {}", self.message),
        }
    }

    /// The line, other places and fix a gate knows for this finding's key.
    #[must_use]
    pub fn detailed(mut self, detail: Option<&Detail>) -> Self {
        if let Some(detail) = detail {
            self.line = detail.line.or(self.line);
            self.places.clone_from(&detail.places);
            self.fix.clone_from(&detail.fix);
        }
        self
    }

    /// The finding's line, then one indented line per other place and one for the fix.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let places = self
            .places
            .iter()
            .map(|place| format!("  {}", place.render()));
        let fix = self.fix.iter().map(|fix| format!("  fix: {fix}"));
        [self.render()]
            .into_iter()
            .chain(places)
            .chain(fix)
            .collect()
    }

    /// `lines`, with the code of each place under it, for `chock explain`.
    #[must_use]
    pub fn shown(&self, root: &std::path::Path) -> Vec<String> {
        let mut out = vec![self.render()];
        for place in &self.places {
            out.push(format!("  {}", place.render()));
            out.extend(
                place
                    .code(root)
                    .into_iter()
                    .map(|code| format!("    {code}")),
            );
        }
        out.extend(self.fix.iter().map(|fix| format!("  fix: {fix}")));
        out
    }
}

/// What a gate knows about one key beyond its number. A finding about that key carries it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detail {
    /// The line the key is about, when it has one.
    pub line: Option<u32>,
    /// The other places the key is about.
    pub places: Vec<Place>,
    /// The change that clears the key, where the gate can name one.
    pub fix: Option<String>,
}

/// Most lines of code `chock explain` shows for one place; the rest are counted, not shown.
const SHOWN_LINES: u32 = 12;

/// Another place a finding is about, such as the other copy of a duplicated body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    /// What this place is to the finding, such as `copy` or `caller`.
    pub role: String,
    /// The file of the place, relative to the project root.
    pub file: String,
    /// The first line of the place.
    pub line: u32,
    /// The last line, where the place spans more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<u32>,
    /// The function or item at the place, when the file and line alone do not name it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
}

impl Place {
    /// A place in `file` at `line` that is `role` to its finding.
    #[must_use]
    pub fn at(role: &str, file: &str, line: u32) -> Self {
        Self {
            role: role.to_string(),
            file: file.to_string(),
            line,
            last: None,
            item: None,
        }
    }

    /// The same place, ending at `last` where that is past its first line.
    #[must_use]
    pub fn through(mut self, last: u32) -> Self {
        self.last = (last > self.line).then_some(last);
        self
    }

    /// The same place, naming `item`.
    #[must_use]
    pub fn item(mut self, item: &str) -> Self {
        self.item = Some(item.to_string());
        self
    }

    /// `role: file:line-last item`.
    #[must_use]
    pub fn render(&self) -> String {
        let span = match self.last {
            Some(last) => format!("{}:{}-{last}", self.file, self.line),
            None => format!("{}:{}", self.file, self.line),
        };
        match &self.item {
            Some(item) => format!("{}: {span} {item}", self.role),
            None => format!("{}: {span}", self.role),
        }
    }

    /// The place's lines under `root`, numbered, at most `SHOWN_LINES` of them. A file that cannot
    /// be read shows nothing, since the place line already names it.
    #[must_use]
    pub fn code(&self, root: &std::path::Path) -> Vec<String> {
        let Ok(text) = std::fs::read_to_string(root.join(&self.file)) else {
            return Vec::new();
        };
        let last = self.last.unwrap_or(self.line);
        let shown_last = last.min(self.line.saturating_add(SHOWN_LINES - 1));
        let mut out: Vec<String> = text
            .lines()
            .zip(1_u32..)
            .filter(|(_, number)| (self.line..=shown_last).contains(number))
            .map(|(code, number)| format!("{number:>5} | {code}"))
            .collect();
        if last > shown_last {
            out.push(format!("      | {} more line(s)", last - shown_last));
        }
        out
    }
}

/// What one gate reported in one run, in the shape every gate shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateReport {
    /// The gate's name.
    pub gate: String,
    /// The gate's verdict.
    pub verdict: Verdict,
    /// The verdict's exit code.
    pub exit_code: u8,
    /// What this run measured, summed over the gate's keys. Absent for a gate that is pass/fail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measured: Option<u64>,
    /// What the record holds, summed over the gate's keys. Absent for a gate that is pass/fail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<u64>,
    /// What the gate counts, such as `finding(s)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Why no number was produced. Present exactly when the verdict is `CannotRun`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cannot_run_reason: Option<String>,
    /// What the gate found.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    /// The narrowest command that re-runs this gate.
    pub rerun: String,
    /// How long the gate took, in ms.
    pub duration_ms: u64,
    /// Seconds since the Unix epoch, set only in the cached record, so an old pass does not read
    /// as a fresh one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_at: Option<u64>,
    /// Answered from an earlier verdict because every input the gate names hashed the same. Shown,
    /// so a gate that did not run looks different.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recalled: bool,
    /// The record this run leaves the gate: its first, or a lower one. Not serialised: a cached
    /// verdict has nothing to write.
    #[serde(skip)]
    pub tightened: Option<Series>,
    /// Keys a newer chock measures that the record lacks, which this run records first.
    #[serde(skip)]
    pub new_measures: Series,
    /// What to do about a gate that tripped, in one sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// The most memory in MB the gate's processes held at once, read on Linux, so a later run knows
    /// which slow gates fit side by side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_mb: Option<u64>,
    /// Each test's time in ms under its binary and name, from a gate that times its tests
    /// (`miri`), so a later run starts the slowest first.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub tests_ms: std::collections::BTreeMap<String, u64>,
}

/// The last result for each gate: this run's, then each earlier result this run did not replace.
/// A one-gate run must not erase a slow gate's failure.
#[must_use]
pub fn merged(previous: Vec<GateReport>, now: Vec<GateReport>) -> Vec<GateReport> {
    let fresh: Vec<String> = now.iter().map(|report| report.gate.clone()).collect();
    now.into_iter()
        .chain(
            previous
                .into_iter()
                .filter(|report| !fresh.contains(&report.gate)),
        )
        .collect()
}

/// Where a run leaves its result, so `explain` can answer without paying for the gates again.
pub const LAST_RUN: &str = ".chock/last-run.json";

/// Best effort: a run that gated correctly is done even if the cache cannot be written. `run`
/// and `init` both keep reports here, so each is stamped and none erases the rest.
/// # Errors
/// The error from making the directory or writing the file.
pub fn remember(
    root: &std::path::Path,
    reports: &[GateReport],
    chock_version: &str,
) -> std::io::Result<()> {
    if reports.is_empty() {
        return Ok(());
    }
    let path = root.join(LAST_RUN);
    std::fs::create_dir_all(path.parent().unwrap_or(root))?;
    let held = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Run>(&text).ok())
        .map(|last| last.gates)
        .unwrap_or_default();
    let record = Run::new(chock_version, merged(held, stamped(reports)));
    crate::project::document::write(&path, &record.render_json())
}

impl GateReport {
    /// A report of `verdict` for `gate`, with nothing measured and nothing found.
    #[must_use]
    pub fn new(gate: &str, verdict: Verdict, rerun: &str) -> Self {
        Self {
            gate: gate.to_string(),
            verdict,
            exit_code: verdict.code(),
            measured: None,
            baseline: None,
            unit: None,
            cannot_run_reason: None,
            findings: Vec::new(),
            rerun: rerun.to_string(),
            recalled: false,
            duration_ms: 0,
            ran_at: None,
            tightened: None,
            new_measures: Series::new(),
            fix: None,
            peak_mb: None,
            tests_ms: std::collections::BTreeMap::new(),
        }
    }

    /// A report that `gate` could not run, and why.
    #[must_use]
    pub fn cannot_run(gate: &str, rerun: &str, reason: &str) -> Self {
        let mut report = Self::new(gate, Verdict::CannotRun, rerun);
        report.cannot_run_reason = Some(reason.to_string());
        report
    }

    /// A line per item: each finding, or, for a report with none, what its baseline holds under
    /// `root`, so a pass can be audited as well as a failure.
    #[must_use]
    pub fn explained(&self, root: &std::path::Path) -> Vec<String> {
        if self.findings.is_empty() {
            return crate::run::baseline::held_at(root, &self.gate);
        }
        let fix = self.fix.iter().map(|fix| format!("fix: {fix}"));
        self.findings
            .iter()
            .flat_map(|finding| finding.shown(root))
            .chain(fix)
            .collect()
    }

    /// The two totals. A trip at or under the recorded total adds its finding count, because the
    /// totals alone read as a pass there.
    fn against(&self, now: u64, was: u64) -> String {
        if self.verdict == Verdict::Tripped && now <= was {
            format!(
                "{now} against {was} in total, {}",
                plural(self.findings.len(), "finding")
            )
        } else {
            format!("{now} against {was}")
        }
    }

    /// Whether the report says more than its summary line: findings, or what to do next.
    fn detailed(&self) -> bool {
        !self.findings.is_empty() || self.fix.is_some()
    }

    /// One line, aligned, for a human reading a whole run.
    #[must_use]
    pub fn summary(&self) -> String {
        let detail = match (&self.cannot_run_reason, self.measured, self.baseline) {
            (Some(reason), _, _) => reason.clone(),
            (None, Some(now), Some(was)) => self.against(now, was),
            (None, Some(now), None) => format!("{now}, not yet recorded"),
            (None, None, _) => String::new(),
        };
        let how = if self.recalled { " recalled" } else { "" };
        format!("  {:<10} {:<11} {detail}{how}", self.gate, self.verdict)
            .trim_end()
            .to_string()
    }

    /// A line for stderr as a gate finishes, so a long CI step does not look hung. A gate that did
    /// not pass adds its part of the final report, so its reasons arrive before the run ends.
    #[must_use]
    pub fn progress(&self) -> String {
        let seconds = self.duration_ms / 1000;
        // "after 0 s" read as a fresh judgement, so a run that judged nothing looked like one that did.
        let line = if self.recalled {
            format!(
                "chock: {} {}, recalled: nothing it reads changed",
                self.gate, self.verdict
            )
        } else {
            format!("chock: {} {} after {seconds} s", self.gate, self.verdict)
        };
        if self.verdict == Verdict::Pass {
            return line;
        }
        let reasons = if self.detailed() {
            block(self, Leaves::default())
        } else {
            String::new()
        };
        format!("{line}\n{}{reasons}", self.summary())
            .trim_end()
            .to_string()
    }
}

/// `1 gate` or `3 gates`: a count and its noun, as a person reads them.
#[must_use]
pub(crate) fn plural(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

/// Every gate a run touched, in the order it ran them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// Recomputed on every write and never read back, so an older chock's URL never carries over.
    #[serde(rename = "$schema", skip_deserializing, default = "schema_url")]
    pub schema_url: String,
    /// The schema of the report, which `SCHEMA` names.
    pub version: u32,
    /// The chock that ran the gates.
    pub chock: String,
    /// Each gate's report, in the order the run ran them.
    pub gates: Vec<GateReport>,
}

impl Run {
    /// A run by `chock_version` of `gates`.
    #[must_use]
    pub fn new(chock_version: &str, gates: Vec<GateReport>) -> Self {
        Self {
            schema_url: schema_url(),
            version: SCHEMA,
            chock: chock_version.to_string(),
            gates,
        }
    }

    /// The worst verdict of the run's gates.
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        Verdict::worst(self.gates.iter().map(|g| g.verdict))
    }

    /// The run's result in one line. The exit code cannot say it: `cannot_run` outranks
    /// `tripped`, so seven trips and one refusal exit 2.
    #[must_use]
    pub fn tally(&self) -> String {
        let named = |want: Verdict| {
            let names: Vec<&str> = self
                .gates
                .iter()
                .filter(|g| g.verdict == want)
                .map(|g| g.gate.as_str())
                .collect();
            (names.len(), names.join(", "))
        };
        let ((tripped, trips), (refused, refusals), total) = (
            named(Verdict::Tripped),
            named(Verdict::CannotRun),
            self.gates.len(),
        );
        let gates = plural(total, "gate");
        match (tripped, refused) {
            (0, 0) => format!("{gates} ok."),
            (0, refused) if refused == total => {
                "no gate could run, so nothing was measured.".to_string()
            }
            (0, refused) => format!("{refused} of {gates} could not run ({refusals})."),
            (tripped, 0) => format!("{tripped} of {gates} tripped ({trips})."),
            (tripped, refused) => format!(
                "{tripped} of {gates} tripped ({trips}) and {refused} could not run ({refusals})."
            ),
        }
    }

    /// The one cause that refused several gates, named once.
    #[must_use]
    pub fn not_compiling(&self) -> Option<String> {
        SHARED_CAUSES
            .iter()
            .find_map(|cause| self.refused_by(cause))
    }

    /// A line naming the cause when it refused two or more gates; one refusal names itself.
    fn refused_by(&self, cause: &Cause) -> Option<String> {
        let refused = self
            .gates
            .iter()
            .filter(|gate| gate.verdict == Verdict::CannotRun)
            .filter_map(|gate| gate.cannot_run_reason.as_deref())
            .filter(|reason| (cause.matches)(reason))
            .count();
        (refused > 1).then(|| {
            format!(
                "{refused} of those could not run because {}, which is one problem and not \
                 {refused}. {}",
                cause.because, cause.names_it
            )
        })
    }

    /// The report as pretty JSON, with a newline at the end.
    #[must_use]
    pub fn render_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).unwrap_or_default();
        text.push('\n');
        text
    }

    /// The text report: the rows `rows` asks for, the findings of each gate that has any, less what
    /// `leaves` names, and the tally, which also counts the gates that got no row.
    #[must_use]
    pub fn render(&self, rows: Rows, leaves: Leaves) -> String {
        let mut out = String::new();
        let shown = |gate: &&GateReport| rows == Rows::Every || gate.verdict != Verdict::Pass;
        for gate in self.gates.iter().filter(shown) {
            out.push_str(&gate.summary());
            out.push('\n');
        }
        for gate in self.gates.iter().filter(|g| g.detailed()) {
            if leaves.said && gate.verdict != Verdict::Pass {
                continue;
            }
            out.push_str(&block(gate, leaves));
        }
        if !self.gates.is_empty() {
            let _ = writeln!(out, "\n{}", self.tally());
        }
        if let Some(once) = self.not_compiling() {
            let _ = writeln!(out, "{once}");
        }
        out.trim_start_matches('\n').to_string()
    }
}

/// The gates that get a row in a text report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Rows {
    /// Every gate: `--verbose`, or a run that names its gates.
    Every,
    /// Each gate that tripped or could not run. A run of a whole stage passes most gates, and the
    /// tally counts them.
    #[default]
    NotPassed,
}

impl Rows {
    /// The rows of a `chock run`: these, or every gate for a run that names its gates, since those
    /// are the gates its reader asked about, passed or not.
    #[must_use]
    pub fn asked(self, names: &[&str]) -> Self {
        if names.is_empty() { self } else { Self::Every }
    }
}

/// What a text report leaves out: the blocks its reader saw already, and a hook's advisories.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Leaves {
    /// Progress put the block of each gate that did not pass on the report's own stream.
    pub said: bool,
    /// A hook counts a passing gate's findings, and `chock explain` lists them.
    pub advisories: bool,
}

/// The form of a command's report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Format {
    /// Text for a person.
    #[default]
    Text,
    /// JSON for an agent or a script, from `--json`.
    Json,
}

/// The line `chock baseline` prints for a gate it could not measure, and the install that repairs
/// it where one does.
#[must_use]
pub fn skipped(gate: &str, reason: &str) -> String {
    let line = format!("  SKIPPED   {gate:<12} {reason}");
    match crate::gates::fixes::repair(reason) {
        Some(fix) => format!("{line}\n  fix: {fix}"),
        None => line,
    }
}

/// A gate's findings under a heading; a passing gate's findings are advisories, which `leaves`
/// can count in one line, as a hook shows them.
fn block(gate: &GateReport, leaves: Leaves) -> String {
    let mut out = format!("\n{} — {}\n", gate.gate, headed(gate));
    let (findings, candidates): (Vec<&Finding>, Vec<&Finding>) = gate
        .findings
        .iter()
        .partition(|finding| finding.grade.is_finding());
    let name = &gate.gate;
    if leaves.advisories && gate.verdict == Verdict::Pass && !findings.is_empty() {
        let count = plural(findings.len(), "finding");
        let _ = writeln!(
            out,
            "  {count} not shown in a hook, since this gate passed: `chock explain {name}` lists \
             each one"
        );
    } else {
        for line in findings.into_iter().flat_map(Finding::lines) {
            let _ = writeln!(out, "  {line}");
        }
    }
    if !candidates.is_empty() {
        let count = plural(candidates.len(), "candidate");
        let _ = writeln!(
            out,
            "  {count} not shown, since a candidate never trips a gate: `chock explain {name}` \
             lists each one"
        );
    }
    if let Some(fix) = &gate.fix {
        let _ = writeln!(out, "  fix: {fix}");
    }
    out
}

fn headed(gate: &GateReport) -> String {
    match gate.verdict {
        Verdict::Pass => format!("advisory, and it passed — {}", gate.rerun),
        _ => gate.rerun.clone(),
    }
}

/// One reason several gates could not run, and the command that reproduces it alone.
struct Cause {
    matches: fn(&str) -> bool,
    because: &'static str,
    names_it: &'static str,
}

/// Most upstream first: a tree that does not compile has no suite to run, so the compile error
/// is the first thing to fix.
const SHARED_CAUSES: [Cause; 2] = [
    Cause {
        matches: rustc_refused,
        because: "this tree does not compile",
        names_it: "`cargo check --all-targets` names it.",
    },
    Cause {
        matches: suite_refused,
        because: "this project's own suite does not pass here",
        names_it: "`chock run test` names it, and nothing measured over a failing suite would mean \
                   anything.",
    },
];

/// Whether a failing suite refused the gate. `coverage` and `crap` both run the suite, and each
/// words the failure its own way.
fn suite_refused(reason: &str) -> bool {
    reason.contains("test run failed") || reason.contains("creating test list failed")
}

/// Whether the compiler refused the gate. Only rustc's and cargo's own text counts: a gate's
/// wording changes, and the bare word "error" matches any failure.
fn rustc_refused(reason: &str) -> bool {
    reason.contains("could not compile")
        || reason.contains("error[E")
        || reason.contains("aborting due to")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::baseline::Baseline;

    #[test]
    fn a_verdict_taken_now_reads_without_the_recalled_mark() {
        let mut report = GateReport::new("probe", Verdict::Pass, "chock run probe");
        assert_eq!(report.summary(), "  probe      ok");
        report.recalled = true;
        let recalled = format!("  probe      ok{}recalled", " ".repeat(11));
        assert_eq!(report.summary(), recalled);
    }

    #[test]
    fn each_verdict_keeps_its_documented_exit_code() {
        assert_eq!(Verdict::Pass.code(), 0);
        assert_eq!(Verdict::Tripped.code(), 1);
        assert_eq!(Verdict::CannotRun.code(), 2);
    }

    #[test]
    fn a_run_carries_the_worst_verdict_in_it() {
        assert_eq!(
            Verdict::worst([Verdict::Pass, Verdict::Pass]),
            Verdict::Pass
        );
        assert_eq!(
            Verdict::worst([Verdict::Pass, Verdict::Tripped]),
            Verdict::Tripped
        );
        assert_eq!(
            Verdict::worst([Verdict::Tripped, Verdict::CannotRun]),
            Verdict::CannotRun
        );
    }

    #[test]
    fn could_not_run_outranks_tripped_because_it_produced_no_number() {
        assert_eq!(
            Verdict::worst([Verdict::CannotRun, Verdict::Tripped, Verdict::Pass]),
            Verdict::CannotRun
        );
    }

    #[test]
    fn a_run_of_no_gates_at_all_passes_rather_than_inventing_a_failure() {
        assert_eq!(Verdict::worst([]), Verdict::Pass);
    }

    #[test]
    fn a_verdict_honours_the_width_it_is_printed_into() {
        assert_eq!(format!("[{:<11}]", Verdict::Tripped), "[TRIPPED    ]");
        assert_eq!(format!("[{:<11}]", Verdict::Pass), "[ok         ]");
    }

    #[test]
    fn a_finding_renders_the_form_an_editor_can_follow() {
        assert_eq!(
            Finding::at("src/a.rs", "comment block of 5 lines")
                .line(12)
                .render(),
            "src/a.rs:12: comment block of 5 lines"
        );
    }

    #[test]
    fn a_finding_about_a_measure_rather_than_a_place_renders_under_its_item() {
        let finding =
            Finding::at("", "4 findings, over the recorded 2").item("clippy::redundant_clone");
        assert_eq!(
            finding.render(),
            "clippy::redundant_clone: 4 findings, over the recorded 2"
        );
    }

    #[test]
    fn a_finding_inside_a_file_names_the_item_as_well_as_the_span() {
        let finding = Finding::at("src/a.rs", "18 cognitive, over the recorded 9")
            .line(12)
            .item("parse");
        assert_eq!(
            finding.render(),
            "src/a.rs:12: parse: 18 cognitive, over the recorded 9"
        );
    }

    #[test]
    fn a_finding_with_neither_a_file_nor_an_item_still_renders_its_message() {
        assert_eq!(
            Finding::at("", "something moved").render(),
            "?: something moved"
        );
    }

    #[test]
    fn a_finding_with_no_line_names_the_file_alone() {
        assert_eq!(
            Finding::at("Cargo.toml", "no [profile.release]").render(),
            "Cargo.toml: no [profile.release]"
        );
    }

    #[test]
    fn a_gate_that_could_not_run_carries_the_reason_and_the_code() {
        let report = GateReport::cannot_run("crap", "chock run crap", "no lcov.info");
        assert_eq!(report.verdict, Verdict::CannotRun);
        assert_eq!(report.exit_code, 2);
        assert_eq!(report.cannot_run_reason.as_deref(), Some("no lcov.info"));
    }

    #[test]
    fn a_summary_shows_the_measurement_against_the_baseline() {
        let mut report = GateReport::new("bigfiles", Verdict::Tripped, "chock run bigfiles");
        report.measured = Some(12);
        report.baseline = Some(9);
        assert_eq!(report.summary(), "  bigfiles   TRIPPED     12 against 9");
    }

    #[test]
    fn a_trip_at_or_under_the_recorded_total_says_how_many_findings_tripped_it() {
        let mut report = GateReport::new("bigfiles", Verdict::Tripped, "chock run bigfiles");
        report.measured = Some(9);
        report.baseline = Some(9);
        report.findings = vec![Finding::at("src/a.rs", "not in the baseline")];
        assert_eq!(
            report.summary(),
            "  bigfiles   TRIPPED     9 against 9 in total, 1 finding"
        );
        report.verdict = Verdict::Pass;
        assert_eq!(report.summary(), "  bigfiles   ok          9 against 9");
    }

    #[test]
    fn a_gate_with_no_baseline_yet_says_so_rather_than_showing_a_zero() {
        let mut report = GateReport::new("slop", Verdict::Pass, "chock run slop");
        report.measured = Some(3);
        assert_eq!(
            report.summary(),
            "  slop       ok          3, not yet recorded"
        );
    }

    #[test]
    fn a_pass_fail_gate_summarises_to_its_name_and_verdict_alone() {
        assert_eq!(
            GateReport::new("lint", Verdict::Pass, "chock run lint").summary(),
            "  lint       ok"
        );
    }

    #[test]
    fn a_finished_gate_says_its_verdict_and_its_whole_seconds() {
        let mut done = GateReport::new("mutest", Verdict::Pass, "chock run mutest");
        done.duration_ms = 249_904;
        assert_eq!(done.progress(), "chock: mutest ok after 249 s");
    }

    #[test]
    fn a_recalled_verdict_says_it_was_recalled_and_never_a_time() {
        let mut kept = GateReport::new("coverage", Verdict::Pass, "chock run coverage");
        kept.recalled = true;
        assert_eq!(
            kept.progress(),
            "chock: coverage ok, recalled: nothing it reads changed"
        );
    }

    /// A run of an hour once kept every reason to its end.
    #[test]
    fn a_gate_that_did_not_pass_says_its_reasons_as_it_ends() {
        let mut tripped = GateReport::new("slop", Verdict::Tripped, "chock run slop");
        tripped.duration_ms = 2_100;
        tripped.findings = vec![Finding::at("src/a.rs", "block of 5 lines").line(4)];
        assert_eq!(
            tripped.progress(),
            format!(
                "chock: slop TRIPPED after 2 s\n  slop       TRIPPED\nslop — chock run slop\n  {}",
                tripped.findings[0].render()
            )
        );
        let refused = GateReport::cannot_run("binsize", "chock run binsize", "no record");
        assert_eq!(
            refused.progress(),
            "chock: binsize CANNOT RUN after 0 s\n  binsize    CANNOT RUN  no record"
        );
    }

    #[test]
    fn a_gate_a_tool_stopped_says_the_install_in_the_run_as_it_ends_and_in_baseline() {
        let reason = "`kani` is not installed, so this gate has nothing to run";
        let mut stopped = GateReport::cannot_run("proof", "chock run proof", reason);
        stopped.fix = Some("run `chock init --global`".to_string());
        let block = "\nproof — chock run proof\n  fix: run `chock init --global`";
        assert_eq!(
            stopped.progress(),
            format!("chock: proof CANNOT RUN after 0 s\n  proof      CANNOT RUN  {reason}{block}")
        );
        let run = Run::new("0.1.0", vec![stopped]);
        assert!(
            run.render(Rows::Every, Leaves::default())
                .contains(&format!("{block}\n"))
        );
        assert_eq!(
            skipped("mutest", crate::gates::mutation::tool::FOREIGN),
            format!(
                "  SKIPPED   mutest       {}\n  fix: {}",
                crate::gates::mutation::tool::FOREIGN,
                crate::gates::mutation::tool::REPAIR
            )
        );
        assert_eq!(
            skipped("crap", "no coverage was measured"),
            "  SKIPPED   crap         no coverage was measured"
        );
    }

    #[test]
    fn a_tripped_gate_says_what_to_do_beneath_its_findings_and_in_explain() {
        let mut tripped = GateReport::new("slop", Verdict::Tripped, "chock run slop");
        tripped.findings = vec![Finding::at("src/a.rs", "block of 5 lines").line(4)];
        tripped.fix = Some("cut the comment to two lines".to_string());
        let rendered =
            Run::new("0.1.0", vec![tripped.clone()]).render(Rows::Every, Leaves::default());
        let told = "slop — chock run slop\n  src/a.rs:4: block of 5 lines\n  fix: cut the comment to two lines\n";
        assert!(rendered.contains(told), "{rendered}");
        assert_eq!(
            tripped.explained(std::path::Path::new("/nowhere")),
            [
                "src/a.rs:4: block of 5 lines",
                "fix: cut the comment to two lines"
            ]
        );
    }

    /// A candidate never trips a gate, so the run counts the candidates and `explain` lists them.
    #[test]
    fn a_run_counts_the_candidates_under_the_findings() {
        let mut tripped = GateReport::new("mutest", Verdict::Tripped, "chock run mutest");
        tripped.findings = vec![
            Finding::at("src/a.rs", "survived").line(4),
            Finding::at("src/b.rs", "timed out").candidate(),
            Finding::at("src/c.rs", "timed out").candidate(),
        ];
        let mut passed = GateReport::new("lean", Verdict::Pass, "chock run lean");
        passed.findings = vec![Finding::at("src/d.rs", "only one type").candidate()];
        assert_eq!(
            block(&tripped, Leaves::default()),
            "\nmutest — chock run mutest\n  src/a.rs:4: survived\n  2 candidates not shown, since \
             a candidate never trips a gate: `chock explain mutest` lists each one\n"
        );
        assert_eq!(
            block(&passed, Leaves::default()),
            "\nlean — advisory, and it passed — chock run lean\n  1 candidate not shown, since a \
             candidate never trips a gate: `chock explain lean` lists each one\n"
        );
    }

    fn tripped_and_advised() -> Run {
        let mut tripped = GateReport::new("slop", Verdict::Tripped, "chock run slop");
        tripped.findings = vec![Finding::at("src/a.rs", "block of 5 lines").line(4)];
        let mut advised = GateReport::new("profile", Verdict::Pass, "chock run profile");
        advised.findings = vec![
            Finding::at("Cargo.toml", "opt-level is z").line(9),
            Finding::at("Cargo.toml", "no lto").line(12),
        ];
        Run::new("0.1.0", vec![tripped, advised])
    }

    /// Progress put the tripped block on the reader's stream, so the report keeps its row and the
    /// advisories. Mutants: `said` ignored; every block left out.
    #[test]
    fn a_report_on_the_progress_stream_leaves_out_the_blocks_progress_printed() {
        let said = Leaves {
            said: true,
            advisories: false,
        };
        let rendered = tripped_and_advised().render(Rows::NotPassed, said);
        assert!(rendered.contains("  slop       TRIPPED\n"), "{rendered}");
        assert!(!rendered.contains("slop — chock run slop"), "{rendered}");
        assert!(rendered.contains("  Cargo.toml:12: no lto\n"), "{rendered}");
    }

    /// A hook counts a passing gate's findings in one line, and still lists a tripped gate's.
    /// Mutant: the count takes a tripped gate's findings too.
    #[test]
    fn a_hook_counts_the_advisories_and_lists_what_tripped() {
        let hook = Leaves {
            said: false,
            advisories: true,
        };
        let rendered = tripped_and_advised().render(Rows::NotPassed, hook);
        assert!(
            rendered.contains("slop — chock run slop\n  src/a.rs:4: block of 5 lines\n"),
            "{rendered}"
        );
        let counted = "profile — advisory, and it passed — chock run profile\n  2 findings not \
                       shown in a hook, since this gate passed: `chock explain profile` lists \
                       each one\n";
        assert!(rendered.contains(counted), "{rendered}");
        assert!(!rendered.contains("no lto"), "{rendered}");
    }

    #[test]
    fn a_run_renders_a_line_per_gate_then_the_findings_beneath() {
        let mut tripped = GateReport::new("slop", Verdict::Tripped, "chock run slop");
        tripped.findings = vec![Finding::at("src/a.rs", "block of 5 lines").line(4)];
        let run = Run::new(
            "0.1.0",
            vec![
                GateReport::new("lint", Verdict::Pass, "chock run lint"),
                tripped,
            ],
        );
        assert_eq!(
            run.render(Rows::Every, Leaves::default()),
            "  lint       ok\n  \
             slop       TRIPPED\n\n\
             slop — chock run slop\n  \
             src/a.rs:4: block of 5 lines\n\n\
             1 of 2 gates tripped (slop).\n"
        );
    }

    /// A run of a whole stage passes most of its gates, so the tally counts them and a row goes
    /// only to a gate that did not pass. An advisory keeps its block.
    #[test]
    fn a_run_of_a_stage_gives_a_row_only_to_a_gate_that_did_not_pass() {
        let passed =
            |gate: &str| GateReport::new(gate, Verdict::Pass, &format!("chock run {gate}"));
        let mut tripped = GateReport::new("slop", Verdict::Tripped, "chock run slop");
        tripped.findings = vec![Finding::at("src/a.rs", "block of 5 lines").line(4)];
        let mut advised = passed("profile");
        advised.findings = vec![Finding::at("Cargo.toml", "opt-level is z").line(9)];
        let run = Run::new("0.1.0", vec![passed("lint"), tripped, advised]);
        assert_eq!(
            run.render(Rows::NotPassed, Leaves::default()),
            "  slop       TRIPPED\n\n\
             slop — chock run slop\n  \
             src/a.rs:4: block of 5 lines\n\n\
             profile — advisory, and it passed — chock run profile\n  \
             Cargo.toml:9: opt-level is z\n\n\
             1 of 3 gates tripped (slop).\n"
        );
        let clean = Run::new("0.1.0", vec![passed("lint"), passed("fmt")]);
        assert_eq!(
            clean.render(Rows::NotPassed, Leaves::default()),
            "2 gates ok.\n"
        );
    }

    #[test]
    fn a_run_that_names_its_gates_or_says_verbose_gives_every_gate_a_row() {
        assert_eq!(Rows::NotPassed.asked(&[]), Rows::NotPassed);
        assert_eq!(Rows::Every.asked(&[]), Rows::Every);
        assert_eq!(Rows::NotPassed.asked(&["lint"]), Rows::Every);
    }

    #[test]
    fn a_passing_gate_with_findings_heads_them_as_advisory() {
        let mut passed = GateReport::new("miri", Verdict::Pass, "chock run miri");
        passed.findings = vec![Finding::at("src/a.rs", "deprecated: fetch_update").line(9)];
        let text = Run::new("0.1.0", vec![passed]).render(Rows::Every, Leaves::default());
        assert!(
            text.contains("miri — advisory, and it passed — chock run miri"),
            "{text}"
        );
        assert!(text.contains("1 gate ok."), "{text}");
    }

    /// Each gate words the refusal its own way, so the shared cause is read from the compiler's output.
    #[test]
    fn several_gates_refused_by_one_compile_error_say_so_once_beneath_the_tally() {
        let refused =
            |gate: &str, why: &str| GateReport::cannot_run(gate, &format!("chock run {gate}"), why);
        let run = Run::new(
            "0.1.0",
            vec![
                refused(
                    "proof",
                    "cargo kani could not list harnesses: error: could not compile `chock` (lib)",
                ),
                refused(
                    "coverage",
                    "the coverage run failed: error[E0425]: cannot find function `f` in this scope",
                ),
                refused(
                    "idempotent",
                    "the suite fails on its first run, so a second says nothing",
                ),
            ],
        );
        let text = run.render(Rows::Every, Leaves::default());
        assert!(
            text.contains("2 of those could not run because this tree does not compile"),
            "{text}"
        );
        assert!(text.contains("cargo check --all-targets"), "{text}");
    }

    #[test]
    fn several_gates_refused_by_one_failing_suite_say_so_once_beneath_the_tally() {
        let refused =
            |gate: &str, why: &str| GateReport::cannot_run(gate, &format!("chock run {gate}"), why);
        let mut failed = GateReport::new("test", Verdict::Tripped, "chock run test");
        failed.findings = vec![Finding::at("", "this test failed").item("a_case")];
        let run = Run::new(
            "0.1.0",
            vec![
                failed,
                refused(
                    "coverage",
                    "the coverage run failed, so there is nothing to measure: error: test run failed",
                ),
                refused(
                    "crap",
                    "the coverage run failed, so there is nothing to measure: error: test run failed",
                ),
            ],
        );
        let text = run.render(Rows::Every, Leaves::default());
        assert!(
            text.contains(
                "2 of those could not run because this project's own suite does not pass"
            ),
            "{text}"
        );
        assert!(text.contains("chock run test"), "{text}");
    }

    #[test]
    fn a_tree_that_does_not_compile_is_named_ahead_of_a_suite_that_could_not_run() {
        let refused =
            |gate: &str, why: &str| GateReport::cannot_run(gate, &format!("chock run {gate}"), why);
        let run = Run::new(
            "0.1.0",
            vec![
                refused("proof", "error[E0425]: cannot find function `f`"),
                refused(
                    "binsize",
                    "the release build failed: error[E0432]: unresolved import",
                ),
                refused(
                    "coverage",
                    "the coverage run failed: error: test run failed",
                ),
                refused("crap", "the coverage run failed: error: test run failed"),
            ],
        );
        let text = run.render(Rows::Every, Leaves::default());
        assert!(text.contains("does not compile"), "{text}");
        assert!(!text.contains("own suite does not pass"), "{text}");
    }

    #[test]
    fn a_single_refusal_is_left_to_name_itself() {
        let run = Run::new(
            "0.1.0",
            vec![GateReport::cannot_run(
                "coverage",
                "chock run coverage",
                "the coverage run failed: error: test run failed",
            )],
        );
        let text = run.render(Rows::Every, Leaves::default());
        assert!(!text.contains("which is one problem"), "{text}");
    }

    /// A missing tool is not a compile failure, however many gates it refuses.
    #[test]
    fn a_single_compile_refusal_and_a_refusal_of_another_kind_add_no_line() {
        let one = Run::new(
            "0.1.0",
            vec![GateReport::cannot_run(
                "coverage",
                "chock run coverage",
                "error[E0425]: cannot find function `f`",
            )],
        );
        assert_eq!(one.not_compiling(), None);
        let other = Run::new(
            "0.1.0",
            vec![
                GateReport::cannot_run("acl", "chock run acl", "cargo-acl is not installed"),
                GateReport::cannot_run("proof", "chock run proof", "kani is not installed"),
            ],
        );
        assert_eq!(other.not_compiling(), None);
    }

    #[test]
    fn a_run_reports_the_worst_of_its_gates() {
        let run = Run::new(
            "0.1.0",
            vec![
                GateReport::new("lint", Verdict::Pass, "chock run lint"),
                GateReport::cannot_run("crap", "chock run crap", "no lcov"),
            ],
        );
        assert_eq!(run.verdict(), Verdict::CannotRun);
    }

    #[test]
    fn a_run_round_trips_through_its_json() {
        let run = Run::new(
            "0.1.0",
            vec![GateReport::new("lint", Verdict::Pass, "chock run lint")],
        );
        let text = run.render_json();
        assert_eq!(serde_json::from_str::<Run>(&text).unwrap(), run);
    }

    #[test]
    fn the_json_spells_the_verdict_the_way_the_contract_names_it() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../schema/run-v1.json")).unwrap();
        let named = &schema["$defs"]["gate"]["properties"]["verdict"]["enum"];
        let written = [Verdict::Pass, Verdict::Tripped, Verdict::CannotRun]
            .map(|verdict| serde_json::to_value(verdict).unwrap());
        assert_eq!(named.as_array().unwrap().as_slice(), written.as_slice());
        let run = Run::new(
            "0.1.0",
            vec![GateReport::cannot_run("crap", "chock run crap", "no lcov")],
        );
        assert!(run.render_json().contains("\"verdict\": \"cannot_run\""));
    }

    #[test]
    fn the_contract_names_every_field_a_finding_and_its_places_can_carry() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../schema/run-v1.json")).unwrap();
        let place = Place::at("copy", "src/b.rs", 3).through(9).item("g");
        let mut finding = Finding::at("src/a.rs", "m")
            .line(1)
            .item("f")
            .numbers(2, Some(1));
        finding.places = vec![place];
        finding.fix = Some("merge them".to_string());
        let written = serde_json::to_value(finding.candidate()).unwrap();
        for (value, def) in [(&written, "finding"), (&written["places"][0], "place")] {
            let names = |it: &serde_json::Value| -> std::collections::BTreeSet<String> {
                it.as_object().unwrap().keys().cloned().collect()
            };
            let declared = &schema["$defs"][def]["properties"];
            assert_eq!(names(value), names(declared), "{def}");
        }
        let grades = &schema["$defs"]["finding"]["properties"]["grade"]["enum"];
        let spelled =
            [Grade::Finding, Grade::Candidate].map(|grade| serde_json::to_value(grade).unwrap());
        assert_eq!(grades.as_array().unwrap().as_slice(), spelled.as_slice());
        assert!(
            !serde_json::to_string(&Finding::at("a", "m"))
                .unwrap()
                .contains("grade")
        );
    }

    #[test]
    fn an_absent_optional_field_is_left_out_rather_than_written_null() {
        let run = Run::new(
            "0.1.0",
            vec![GateReport::new("lint", Verdict::Pass, "chock run lint")],
        );
        let text = run.render_json();
        assert!(!text.contains("null"));
        assert!(!text.contains("findings"));
    }

    /// The exit code ranks a refusal above a trip, so the tally is where both are counted.
    #[test]
    fn a_run_that_both_tripped_and_refused_says_both() {
        let of = |name: &str, verdict: Verdict| GateReport::new(name, verdict, "chock run x");
        let mut gates: Vec<GateReport> = (0..21)
            .map(|i| of(&format!("ok{i}"), Verdict::Pass))
            .collect();
        gates.extend((0..7).map(|i| of(&format!("bad{i}"), Verdict::Tripped)));
        gates.push(of("history", Verdict::CannotRun));
        let run = Run::new("0.1.0", gates);
        assert_eq!(
            run.tally(),
            "7 of 29 gates tripped (bad0, bad1, bad2, bad3, bad4, bad5, bad6) and 1 could not run \
             (history)."
        );
        assert_eq!(run.verdict(), Verdict::CannotRun);
    }

    #[test]
    fn nothing_was_measured_is_claimed_only_when_no_check_ran() {
        let of = |name: &str, verdict: Verdict| GateReport::new(name, verdict, "chock run x");
        let all_refused = Run::new(
            "0.1.0",
            vec![of("a", Verdict::CannotRun), of("b", Verdict::CannotRun)],
        );
        assert_eq!(
            all_refused.tally(),
            "no gate could run, so nothing was measured."
        );
        let some_refused = Run::new(
            "0.1.0",
            vec![of("a", Verdict::Pass), of("b", Verdict::CannotRun)],
        );
        assert_eq!(some_refused.tally(), "1 of 2 gates could not run (b).");
        assert!(!some_refused.tally().contains("nothing was measured"));
    }

    #[test]
    fn a_run_with_nothing_wrong_counts_what_it_checked() {
        let of = |name: &str| GateReport::new(name, Verdict::Pass, "chock run x");
        let run = Run::new("0.1.0", vec![of("a"), of("b"), of("c")]);
        assert_eq!(run.tally(), "3 gates ok.");
        let text = run.render(Rows::Every, Leaves::default());
        assert!(text.ends_with("3 gates ok.\n"), "{text}");
    }

    #[test]
    fn a_run_that_only_tripped_does_not_mention_running() {
        let of = |name: &str, verdict: Verdict| GateReport::new(name, verdict, "chock run x");
        let run = Run::new(
            "0.1.0",
            vec![of("a", Verdict::Pass), of("b", Verdict::Tripped)],
        );
        assert_eq!(run.tally(), "1 of 2 gates tripped (b).");
    }

    #[test]
    fn running_one_gate_keeps_what_every_other_gate_last_found() {
        let of = |name: &str, verdict: Verdict| GateReport::new(name, verdict, "chock run x");
        let held = vec![
            of("binsize", Verdict::CannotRun),
            of("slop", Verdict::Tripped),
        ];
        let now = vec![of("slop", Verdict::Pass)];
        let kept = merged(held, now);
        let named: Vec<(&str, Verdict)> = kept
            .iter()
            .map(|report| (report.gate.as_str(), report.verdict))
            .collect();
        assert_eq!(
            named,
            vec![("slop", Verdict::Pass), ("binsize", Verdict::CannotRun)]
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_failing_report_explains_its_findings_and_a_pass_with_no_record_explains_nothing() {
        let mut tripped = GateReport::new("dupdeps", Verdict::Tripped, "chock run dupdeps");
        tripped.findings = vec![Finding::at("Cargo.toml", "built twice").item("syn")];
        let root = crate::testdir::make("report-explained");
        assert_eq!(tripped.explained(&root), ["Cargo.toml: syn: built twice"]);
        let passed = GateReport::new("dupdeps", Verdict::Pass, "chock run dupdeps");
        assert_eq!(passed.explained(&root), Vec::<String>::new());
        assert!(!passed.recalled);
    }

    #[test]
    fn a_place_shows_its_span_where_it_has_more_than_one_line_and_its_item_where_named() {
        assert_eq!(
            Place::at("copy", "src/b.rs", 3).through(3).render(),
            "copy: src/b.rs:3"
        );
        let span = Place::at("copy", "src/b.rs", 3).through(5);
        assert_eq!(span.render(), "copy: src/b.rs:3-5");
        assert_eq!(span.item("g").render(), "copy: src/b.rs:3-5 g");
        assert_eq!(
            Place::at("copy", "src/b.rs", 3).item("g").render(),
            "copy: src/b.rs:3 g"
        );
    }

    #[test]
    fn a_finding_takes_the_line_places_and_fix_a_gate_knows_for_it() {
        let detail = Detail {
            line: Some(9),
            places: vec![Place::at("copy", "src/b.rs", 2)],
            fix: Some("merge them".to_string()),
        };
        let found = Finding::at("src/a.rs", "a copy")
            .line(4)
            .detailed(Some(&detail));
        let lines = [
            "src/a.rs:9: a copy",
            "  copy: src/b.rs:2",
            "  fix: merge them",
        ];
        assert_eq!(found.lines(), lines);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn explain_shows_each_place_with_its_code_cut_at_twelve_lines() {
        let text = (1..=20).fold(String::new(), |mut text, n| {
            let _ = writeln!(text, "line {n}");
            text
        });
        let root = crate::testdir::tree("report-shown", &[("src/b.rs", text.as_str())]);
        let mut found = Finding::at("src/a.rs", "a copy").line(1);
        found.places = vec![
            Place::at("copy", "src/b.rs", 2).through(15),
            Place::at("end", "src/b.rs", 20),
            Place::at("gone", "src/none.rs", 1),
        ];
        found.fix = Some("merge them".to_string());
        let mut shown = vec![
            "src/a.rs:1: a copy".to_string(),
            "  copy: src/b.rs:2-15".to_string(),
        ];
        shown.extend((2..=13).map(|n| format!("    {n:>5} | line {n}")));
        shown.extend(
            [
                "          | 2 more line(s)",
                "  end: src/b.rs:20",
                "       20 | line 20",
                "  gone: src/none.rs:1",
                "  fix: merge them",
            ]
            .map(String::from),
        );
        assert_eq!(found.shown(&root), shown);
    }

    fn remembered(root: &std::path::Path) -> Run {
        serde_json::from_str(&std::fs::read_to_string(root.join(LAST_RUN)).unwrap()).unwrap()
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_remembered_report_says_when_it_ran_and_keeps_the_gates_this_one_did_not_touch() {
        let dir = crate::testdir::make("report-remember-merged");
        let earlier = GateReport::cannot_run("binsize", "chock run binsize", "no baseline");
        remember(&dir, std::slice::from_ref(&earlier), "0.1.0").unwrap();
        let now = GateReport::new("slop", Verdict::Pass, "chock run slop");
        remember(&dir, std::slice::from_ref(&now), "0.1.0").unwrap();
        let held = remembered(&dir);
        let named: Vec<(&str, bool)> = held
            .gates
            .iter()
            .map(|report| (report.gate.as_str(), report.ran_at.is_some()))
            .collect();
        assert_eq!(named, [("slop", true), ("binsize", true)]);
    }

    /// A call with nothing to keep is not a run that measured nothing, and must not stand for one.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn remembering_no_report_leaves_the_record_exactly_as_it_was() {
        let dir = crate::testdir::make("report-remember-nothing");
        let report = GateReport::cannot_run("probe", "chock run probe", "specific failure");
        remember(&dir, std::slice::from_ref(&report), "0.1.0").unwrap();
        let content = std::fs::read_to_string(dir.join(LAST_RUN)).unwrap();
        assert_eq!(
            remembered(&dir).gates[0].cannot_run_reason.as_deref(),
            Some("specific failure")
        );
        remember(&dir, &[], "0.1.0").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(LAST_RUN)).unwrap(),
            content
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_record_with_nowhere_to_go_writes_nothing_and_touches_nothing() {
        let dir = crate::testdir::make("report-remember-no-room");
        std::fs::write(dir.join(".chock"), "not a directory").unwrap();
        let report = GateReport::new("slop", Verdict::Pass, "chock run slop");
        assert!(remember(&dir, std::slice::from_ref(&report), "0.1.0").is_err());
        remember_or_say(&dir, std::slice::from_ref(&report), "0.1.0");
        assert_eq!(
            std::fs::read_to_string(dir.join(".chock")).unwrap(),
            "not a directory"
        );
    }

    #[test]
    fn nothing_lowered_writes_no_record() {
        let report = GateReport::new("slop", Verdict::Pass, "chock run slop");
        assert_eq!(
            locked_in(&Baseline::empty("0.1.0"), &[report], Gating::Commit),
            None
        );
    }

    #[test]
    fn a_run_says_the_first_records_it_wrote_before_the_ones_it_lowered() {
        let mut held = Baseline::empty("0.1.0");
        let mut was = Series::new();
        was.set("src/a.rs", 9);
        held.set("slop", was);
        let wrote = |gate: &str, unit: Option<&str>| {
            let mut report = GateReport::new(gate, Verdict::Pass, "chock run");
            let mut now = Series::new();
            now.set("src/a.rs", 4);
            report.tightened = Some(now);
            report.unit = unit.map(str::to_string);
            report
        };
        let reports = [
            wrote("slop", Some("lines")),
            wrote("typos", Some("typo(s)")),
            wrote("nesting", None),
        ];
        let working = locked_in(&held, &reports, Gating::Other).unwrap();
        assert_eq!(
            working.said(),
            "wrote the first record for typos, nesting; measured less for slop (9 to 4), and the \
             pre-commit hook lowers the record at the next commit: slop src/a.rs 9 to 4"
        );
        assert_eq!(working.record.gate("slop").get("src/a.rs"), Some(9));
        let settled = locked_in(&held, &reports, Gating::Commit).unwrap();
        assert_eq!(
            settled.said(),
            "wrote the first record for typos, nesting; lowered the record for slop (9 to 4)"
        );
        assert_eq!(settled.record.unit("typos"), Some("typo(s)"));
        assert_eq!(settled.record.unit("nesting"), None);
        assert_eq!(
            settled.record.unit("slop"),
            None,
            "a lowered record keeps the unit it has"
        );
        assert_eq!(settled.record.gate("slop").get("src/a.rs"), Some(4));
        assert_eq!(settled.record.gate("nesting").get("src/a.rs"), Some(4));
    }

    /// Outside a commit the run names each key the next commit lowers or drops, ten in full.
    #[test]
    fn a_run_names_the_keys_the_next_commit_lowers_or_drops() {
        let mut was = Series::new();
        let mut now = Series::new();
        for at in 0..12 {
            was.set(&format!("src/{at:02}.rs"), 9);
            now.set(&format!("src/{at:02}.rs"), if at == 1 { 9 } else { 4 });
        }
        was.set("src/gone.rs", 2);
        let lowered = lowering("slop", &was, &now);
        let fell = (0..12).filter(|at| *at != 1);
        let mut expected: Vec<String> = fell
            .map(|at| format!("slop src/{at:02}.rs 9 to 4"))
            .collect();
        expected.push("slop src/gone.rs 2 to none".to_string());
        assert_eq!(lowered, expected);
        let mut held = Baseline::empty("0.1.0");
        held.set("slop", was);
        let mut report = GateReport::new("slop", Verdict::Pass, "chock run");
        report.tightened = Some(now);
        let working = locked_in(&held, &[report], Gating::Other).unwrap();
        let said = working.said();
        assert!(said.ends_with("src/10.rs 9 to 4 and 2 more"), "{said}");
        assert!(
            said.contains(": slop src/00.rs 9 to 4, slop src/02.rs 9 to 4"),
            "{said}"
        );
    }

    /// At a commit the record goes into the commit, unless the file held a change nobody staged.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_commit_stages_the_record_it_wrote_unless_the_file_held_an_unstaged_change() {
        let file = crate::run::baseline::FILE;
        let root = crate::testdir::repo("report-staged", &[(file, "{}\n")]);
        let git = |args: &[&str]| crate::exec::run("git", args, &root).unwrap().stdout;
        let held = crate::run::baseline::Baseline::empty("0.0.9");
        let mut first = GateReport::new("typos", Verdict::Pass, "chock run typos");
        let mut series = crate::run::baseline::Series::new();
        series.set("src/a.rs", 2);
        first.tightened = Some(series);
        let written = |committing: Gating| {
            let settled = locked_in(&held, std::slice::from_ref(&first), committing).unwrap();
            settled.written(&root, "0.1.0", committing, &[])
        };
        let said = "wrote the first record for typos";
        assert_eq!(
            written(Gating::Commit),
            format!("{said}; {file} is in this commit")
        );
        assert_eq!(
            git(&["diff", "--cached", "--name-only"]),
            format!("{file}\n")
        );
        git(&["reset", "--quiet"]);
        git(&["checkout", "--", file]);
        assert_eq!(
            written(Gating::Other),
            format!("{said}; commit {file} with this change")
        );
        assert_eq!(git(&["diff", "--cached", "--name-only"]), "");
        // The file now holds the run's own write, which nobody staged.
        assert_eq!(
            written(Gating::Commit),
            format!("{said}; commit {file} with this change")
        );
        assert_eq!(git(&["diff", "--cached", "--name-only"]), "");
    }

    /// Under `git commit --only` the hook stages into `next-index-*`, and `index.lock` becomes the
    /// index after the commit, so the record goes into both. Mutant: drop the add to `index.lock`.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_commit_of_named_files_also_stages_the_record_into_the_index_it_leaves_behind() {
        let file = crate::run::baseline::FILE;
        let root = crate::testdir::repo("report-only", &[(file, "{}\n")]);
        let (dir, git_on) = (root.join(".git"), crate::project::vcs::git_on);
        let (next, real) = (dir.join("next-index-1.lock"), dir.join("index.lock"));
        let holds_the_file =
            |index: &std::path::Path| git_on(&root, Some(index), &["diff", "--quiet", "--", file]);
        let held = crate::run::baseline::Baseline::empty("0.0.9");
        let mut first = GateReport::new("typos", Verdict::Pass, "chock run typos");
        let mut series = crate::run::baseline::Series::new();
        series.set("src/a.rs", 2);
        first.tightened = Some(series);
        let written = || {
            std::fs::copy(dir.join("index"), &next).unwrap();
            let settled = locked_in(&held, std::slice::from_ref(&first), Gating::Commit).unwrap();
            settled.written_into(&root, "0.1.0", Gating::Commit, &[], Some(&next))
        };
        let said = "wrote the first record for typos";
        assert_eq!(
            crate::project::vcs::real_index(&next),
            None,
            "no index.lock yet"
        );
        std::fs::copy(dir.join("index"), &real).unwrap();
        assert_eq!(crate::project::vcs::real_index(&real), None);
        assert_eq!(written(), format!("{said}; {file} is in this commit"));
        assert!(holds_the_file(&next) && holds_the_file(&real));

        // `index.lock` holds a change of its own, which the hook leaves alone.
        std::fs::write(root.join(file), "[]\n").unwrap();
        assert!(git_on(&root, Some(&real), &["add", "--", file]));
        std::fs::write(root.join(file), "{}\n").unwrap();
        let left =
            format!("{said}; {file} is in this commit; run `git add {file}` after the commit");
        assert_eq!(written(), left);
        assert!(holds_the_file(&next) && !holds_the_file(&real));
        std::fs::remove_file(&real).unwrap();
    }

    /// A key a newer chock measures joins the record after the same commit lowers the old keys, so
    /// the lower record does not drop it.
    #[test]
    fn a_new_measure_is_recorded_beside_a_record_the_same_commit_lowers() {
        let mut held = crate::run::baseline::Baseline::empty("0.19.0");
        let mut was = crate::run::baseline::Series::new();
        was.set("clippy::redundant_clone", 4);
        held.set("codeslop", was);
        let mut report = GateReport::new("codeslop", Verdict::Pass, "chock run codeslop");
        let mut lower = crate::run::baseline::Series::new();
        lower.set("clippy::redundant_clone", 2);
        report.tightened = Some(lower.clone());
        report.new_measures.set("chock::long_literal", 3);
        let settled = locked_in(&held, &[report], Gating::Commit).unwrap();
        lower.set("chock::long_literal", 3);
        assert_eq!(settled.record.gate("codeslop"), lower);
        assert_eq!(
            settled.said(),
            "wrote the first record of a new measure for codeslop chock::long_literal; lowered the \
             record for codeslop (4 to 2)"
        );
    }

    /// The run measured the work tree, so a commit that leaves out a change or a file there lowers no
    /// record. What chock writes itself, such as the last run, is no such file.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_commit_that_leaves_out_a_change_in_the_work_tree_lowers_no_record() {
        let file = crate::run::baseline::FILE;
        let files = [(file, "{}\n"), ("src/a.rs", "fn a() {}\n")];
        let root = crate::testdir::repo("report-left-out", &files);
        let git = |args: &[&str]| crate::exec::run("git", args, &root).unwrap().stdout;
        let mut held = crate::run::baseline::Baseline::empty("0.0.9");
        let mut was = crate::run::baseline::Series::new();
        was.set("src/a.rs", 4);
        held.set("slop", was);
        let mut gained = GateReport::new("slop", Verdict::Pass, "chock run slop");
        let mut lower = crate::run::baseline::Series::new();
        lower.set("src/a.rs", 2);
        gained.tightened = Some(lower.clone());
        let settled = |held: &Baseline| {
            settles(
                &root,
                held,
                std::slice::from_ref(&gained),
                "0.1.0",
                Gating::Commit,
            )
        };
        // Outside a commit a gain waits, and the tree leaves nothing out.
        let outside = settles(
            &root,
            &held,
            std::slice::from_ref(&gained),
            "0.1.0",
            Gating::Other,
        );
        let names = |said: &[String], path: &str| {
            let left = format!("this commit leaves out {path}");
            said.iter().any(|line| line.starts_with(&left))
        };
        assert!(!outside.is_empty() && !names(&outside, ""), "{outside:?}");
        std::fs::write(root.join("src/a.rs"), "fn b() {}\n").unwrap();
        assert!(names(&settled(&held), "src/a.rs"));
        assert_eq!(std::fs::read_to_string(root.join(file)).unwrap(), "{}\n");
        assert_eq!(git(&["diff", "--cached", "--name-only"]), "");
        git(&["add", "src/a.rs"]);
        std::fs::write(root.join("src/b.rs"), "fn b() {}\n").unwrap();
        assert!(names(&settled(&held), "src/b.rs"));
        assert_eq!(git(&["diff", "--cached", "--name-only"]), "src/a.rs\n");
        std::fs::remove_file(root.join("src/b.rs")).unwrap();
        assert!(!names(&settled(&held), ""));
        assert_eq!(
            git(&["diff", "--cached", "--name-only"]),
            format!("{file}\nsrc/a.rs\n")
        );
        // The commit message is refused, git drops the index, and the retry measures the record.
        git(&["reset", "--quiet", "--", file]);
        held.set("slop", lower);
        let level = GateReport::new("slop", Verdict::Pass, "chock run slop");
        let notice =
            format!("{file} holds a record that this commit leaves out; stage it to commit it");
        let levels = std::slice::from_ref(&level);
        assert_eq!(
            settles(&root, &held, levels, "0.1.0", Gating::Other),
            Vec::<String>::new()
        );
        assert_eq!(
            settles(&root, &held, levels, "0.1.0", Gating::Commit),
            vec![notice]
        );
    }

    /// A key of a gone file goes in each gate that keys by file, on each system; a crate stays.
    #[test]
    fn a_commit_forgets_the_records_of_a_file_that_is_gone() {
        let series = |pairs: &[(&str, u64)]| {
            let mut series = Series::new();
            for (key, count) in pairs {
                series.set(key, *count);
            }
            series
        };
        let mut held = Baseline::empty("0.1.0");
        held.set("slop", series(&[("src/a.rs", 3), ("src/gone.rs", 2)]));
        held.set("dupdeps", series(&[("serde", 2)]));
        let windows = series(&[("src/gone.rs#negate", 1)]);
        held.gates.insert("mutest@windows".to_string(), windows);
        assert_eq!(forgotten(&held, None, &|_| false), None, "nothing gone");
        let gone = |file: &str| ["src/gone.rs", "serde"].contains(&file);
        let mut report = GateReport::new("slop", Verdict::Pass, "chock run slop");
        report.tightened = Some(series(&[("src/a.rs", 1), ("src/gone.rs", 2)]));
        let settled = forgotten(&held, locked_in(&held, &[report], Gating::Commit), &gone).unwrap();
        assert_eq!(
            settled.said(),
            "lowered the record for slop (5 to 1), mutest@windows (1 to 0)"
        );
        assert_eq!(settled.record.gate("slop"), series(&[("src/a.rs", 1)]));
        assert_eq!(settled.record.gate("dupdeps"), series(&[("serde", 2)]));
        assert_eq!(
            settled.record.gates.get("mutest@windows"),
            Some(&Series::new())
        );
    }

    /// Only a commit forgets, and only a file that neither the disk nor the index holds.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_file_on_disk_or_in_the_index_keeps_its_record() {
        let files = [("src/a.rs", ""), ("src/deleted.rs", "")];
        let root = crate::testdir::repo("report-forgets", &files);
        std::fs::remove_file(root.join("src/deleted.rs")).unwrap();
        std::fs::write(root.join("src/new.rs"), "").unwrap();
        let mut was = Series::new();
        for file in ["src/a.rs", "src/deleted.rs", "src/new.rs", "src/gone.rs"] {
            was.set(file, 1);
        }
        let mut held = Baseline::empty("0.1.0");
        held.set("slop", was.clone());
        let mut lines = Series::new();
        lines.set("total", 5);
        held.set("lines", lines.clone());
        assert_eq!(
            forgets(&root, &held, None, Gating::Other),
            None,
            "outside a commit"
        );
        let settled = forgets(&root, &held, None, Gating::Commit).unwrap();
        was.0.remove("src/gone.rs");
        assert_eq!(settled.record.gate("slop"), was);
        assert_eq!(settled.record.gate("lines"), lines, "a total names no file");
        let bare = crate::testdir::make("report-forgets-bare");
        assert_eq!(
            forgets(&bare, &held, None, Gating::Commit),
            None,
            "no repository"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gain_waits_for_a_commit_and_a_first_record_with_nowhere_to_go_writes_nothing() {
        let dir = crate::testdir::make("report-settle");
        let mut held = crate::run::baseline::Baseline::empty("0.0.9");
        let mut was = crate::run::baseline::Series::new();
        was.set("src/a.rs", 4);
        held.set("slop", was.clone());
        let mut gained = GateReport::new("slop", Verdict::Pass, "chock run slop");
        let mut lower = crate::run::baseline::Series::new();
        lower.set("src/a.rs", 2);
        gained.tightened = Some(lower.clone());
        let written = || -> crate::run::baseline::Baseline {
            let text = std::fs::read_to_string(dir.join(crate::run::baseline::FILE)).unwrap();
            crate::project::document::parse(&text, "baseline").unwrap()
        };
        settle(
            &dir,
            &held,
            std::slice::from_ref(&gained),
            "0.1.0",
            Gating::Other,
        );
        assert!(
            !dir.join(crate::run::baseline::FILE).exists(),
            "a gain outside a commit waits for the pre-commit hook"
        );
        // A first record goes in with its unit, beside what the run held for the other gates.
        let mut first = GateReport::new("typos", Verdict::Pass, "chock run typos");
        first.tightened = Some(lower.clone());
        first.unit = Some("typo(s)".to_string());
        settle(
            &dir,
            &held,
            &[first.clone(), gained],
            "0.1.0",
            Gating::Other,
        );
        assert_eq!(written().chock, "0.1.0", "the chock that wrote the file");
        assert_eq!(written().gate("typos"), lower);
        assert_eq!(written().unit("typos"), Some("typo(s)"));
        assert_eq!(written().gate("slop"), was);
        let blocked = crate::testdir::make("report-settle-blocked");
        std::fs::write(blocked.join(".chock"), "not a directory").unwrap();
        settle(&blocked, &held, &[first], "0.1.0", Gating::Other);
        assert_eq!(
            std::fs::read_to_string(blocked.join(".chock")).unwrap(),
            "not a directory"
        );
    }

    /// A commit's run writes an old `crap` record again in the compact layout.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_commit_writes_an_old_crap_record_again_in_the_compact_layout() {
        let name = ".chock/crap-baseline.json";
        let old = "{\n  \"version\": 1,\n  \"diagnostics\": [],\n  \"entries\": [\n    {\n      \
                   \"crap\": 2.0\n    }\n  ]\n}\n";
        let root = crate::testdir::repo("report-settle-relaid", &[(name, old)]);
        let held = crate::run::baseline::Baseline::empty("0.0.9");
        settle(&root, &held, &[], "0.1.0", Gating::Other);
        assert_eq!(std::fs::read_to_string(root.join(name)).unwrap(), old);
        settle(&root, &held, &[], "0.1.0", Gating::Commit);
        assert_eq!(
            std::fs::read_to_string(root.join(name)).unwrap(),
            "{\n  \"version\": 1,\n  \"entries\": [\n    {\"crap\":2.0}\n  ]\n}\n"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a directory")]
    fn a_record_that_cannot_be_written_in_place_is_an_error_not_a_silence() {
        let dir = crate::testdir::make("report-remember-occupied");
        std::fs::create_dir_all(dir.join(LAST_RUN)).unwrap();
        let report = GateReport::new("slop", Verdict::Pass, "chock run slop");
        assert!(remember(&dir, std::slice::from_ref(&report), "0.1.0").is_err());
        assert!(
            dir.join(LAST_RUN).is_dir(),
            "what was in the way is left alone"
        );
    }
}
