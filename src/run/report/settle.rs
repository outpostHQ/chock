//! How a run settles its records: which a gate lowers, which it locks in, and which it forgets.

use std::collections::BTreeMap;

use super::{GateReport, remember};
use crate::run::baseline::{Baseline, Series};

/// What a run leaves in the record file: each gate's first record, and each lower number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    /// The baseline as this run leaves it.
    pub record: Baseline,
    /// Gates that had no record before this run.
    pub first: Vec<String>,
    /// Gates whose record this run lowered.
    pub lowered: Vec<String>,
    /// Gates that measured less outside a commit; the pre-commit hook lowers their record.
    pub waiting: Vec<String>,
    /// Each lowered or waiting record's total before and after, by the name it is said under.
    pub moved: BTreeMap<String, (u64, u64)>,
    /// Each key of a waiting record the next commit lowers or drops, as `gate key 9 to 4`.
    pub next: Vec<String>,
    /// Each key a newer chock measures that this run records first, as `gate key`.
    pub measures: Vec<String>,
}

/// Names said in full up to this many, such as the keys the next commit lowers or drops.
const NAMED: usize = 10;

/// The names in full up to `NAMED`, then how many more.
#[must_use]
pub fn some_of(names: &[String]) -> String {
    match names.len().saturating_sub(NAMED) {
        0 => names.join(", "),
        more => format!("{} and {more} more", names[..NAMED].join(", ")),
    }
}

/// Each key of `gate` that `now` holds lower than `was`, or not at all.
pub(super) fn lowering(gate: &str, was: &Series, now: &Series) -> Vec<String> {
    let lower = |(key, was): (&String, &u64)| match now.get(key) {
        Some(now) if now < *was => Some(format!("{gate} {key} {was} to {now}")),
        Some(_) => None,
        None => Some(format!("{gate} {key} {was} to none")),
    };
    was.0.iter().filter_map(lower).collect()
}

impl Settled {
    pub(super) fn new(held: &Baseline) -> Self {
        Self {
            record: held.clone(),
            first: Vec::new(),
            lowered: Vec::new(),
            waiting: Vec::new(),
            moved: BTreeMap::new(),
            next: Vec::new(),
            measures: Vec::new(),
        }
    }

    fn take(&mut self, held: &Baseline, report: &GateReport, series: &Series, gating: Gating) {
        if held.has(&report.gate) {
            let total = |series: &Series| series.0.values().sum::<u64>();
            let now = (total(&held.gate(&report.gate)), total(series));
            self.moved.insert(report.gate.clone(), now);
        }
        // Work in progress can lose an item and get it back, so only a commit moves a record down.
        if held.has(&report.gate) && gating != Gating::Commit {
            self.waiting.push(report.gate.clone());
            let was = held.gate(&report.gate);
            self.next.extend(lowering(&report.gate, &was, series));
            return;
        }
        self.record.set(&report.gate, series.clone());
        if held.has(&report.gate) {
            self.lowered.push(report.gate.clone());
            return;
        }
        // The unit goes beside a first record, as `chock baseline` writes it.
        if let Some(unit) = &report.unit {
            self.record.units.insert(report.gate.clone(), unit.clone());
        }
        self.first.push(report.gate.clone());
    }

    /// Records each key a newer chock measures, at any run, as a new gate takes its first record.
    fn add(&mut self, report: &GateReport) {
        if report.new_measures.is_empty() {
            return;
        }
        let mut series = self.record.gate(&report.gate);
        for (key, count) in &report.new_measures.0 {
            series.set(key, *count);
            self.measures.push(format!("{} {key}", report.gate));
        }
        self.record.set(&report.gate, series);
    }

    fn writes(&self) -> bool {
        !(self.first.is_empty() && self.lowered.is_empty() && self.measures.is_empty())
    }

    /// Writes the record where this run changed it, and says what it did. At a commit the file
    /// goes into the commit, unless it holds a change nobody staged.
    pub(super) fn written(
        self,
        root: &std::path::Path,
        chock_version: &str,
        gating: Gating,
        measured: &[String],
    ) -> String {
        let index = crate::project::vcs::commit_index(root);
        self.written_into(root, chock_version, gating, measured, index.as_deref())
    }

    /// `written`, staging into `index`, the one the commit's hook was given.
    pub(super) fn written_into(
        mut self,
        root: &std::path::Path,
        chock_version: &str,
        gating: Gating,
        measured: &[String],
        index: Option<&std::path::Path>,
    ) -> String {
        if !self.writes() {
            return self.said();
        }
        // The file names the chock that wrote it, as `chock baseline` leaves it.
        self.record.chock = self.record.stamp(chock_version, measured);
        let file = crate::run::baseline::FILE;
        let staging = (gating == Gating::Commit)
            .then(|| crate::project::vcs::Staging::before(root, index, file))
            .flatten();
        let rendered = crate::project::document::render(&self.record);
        let written = crate::project::document::write(&root.join(file), &rendered);
        match written.map(|()| staging.and_then(|staging| staging.add(root, file))) {
            Ok(Some(true)) => format!("{}; {file} is in this commit", self.said()),
            Ok(Some(false)) => format!(
                "{}; {file} is in this commit; run `git add {file}` after the commit",
                self.said()
            ),
            Ok(None) => format!("{}; commit {file} with this change", self.said()),
            Err(error) => format!("could not write {file}, so this run recorded nothing: {error}"),
        }
    }

    /// What was written, in the words the run prints: first records, then lowered ones.
    #[must_use]
    pub fn said(&self) -> String {
        let part = |what: &str, gates: &[String]| {
            (!gates.is_empty()).then(|| format!("{what} {}", gates.join(", ")))
        };
        // `take` and `forgotten` keep both totals of each gate they list.
        let by = |gates: &[String]| -> Vec<String> {
            let moved = |gate: &String| {
                let totals = self.moved.get(gate);
                let said = totals.map(|(was, now)| format!(" ({was} to {now})"));
                format!("{gate}{}", said.unwrap_or_default())
            };
            gates.iter().map(moved).collect()
        };
        let next = some_of(&self.next);
        let next = if next.is_empty() {
            next
        } else {
            format!(": {next}")
        };
        let waiting = part("measured less for", &by(&self.waiting)).map(|said| {
            format!("{said}, and the pre-commit hook lowers the record at the next commit{next}")
        });
        let measures = some_of(&self.measures);
        let measures = (!measures.is_empty())
            .then(|| format!("wrote the first record of a new measure for {measures}"));
        [
            part("wrote the first record for", &self.first),
            measures,
            part("lowered the record for", &by(&self.lowered)),
            waiting,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("; ")
    }
}

/// Whether a run gates a commit, the one run that moves a record down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gating {
    /// The run gates a commit, so a lower number goes into the record and into the commit.
    Commit,
    /// Any other run, whose lower number waits for the next commit.
    Other,
}

/// What this run writes into the record: each first record, and each lower number it measured
/// when it gates a commit; `None` when it has nothing to write or to say.
#[must_use]
pub fn locked_in(held: &Baseline, reports: &[GateReport], gating: Gating) -> Option<Settled> {
    let mut settled = Settled::new(held);
    for report in reports {
        if let Some(series) = &report.tightened {
            settled.take(held, report, series, gating);
        }
        settled.add(report);
    }
    (!settled.said().is_empty()).then_some(settled)
}

/// What a finished run leaves behind: its record for `chock explain`, and each first record and
/// each gain written into the baseline for the change to commit. CI writes neither.
#[expect(
    clippy::print_stderr,
    reason = "the run tells the user on stderr what it recorded"
)]
pub fn settle(
    root: &std::path::Path,
    held: &Baseline,
    reports: &[GateReport],
    chock_version: &str,
    gating: Gating,
) {
    remember_or_say(root, reports, chock_version);
    for said in settles(root, held, reports, chock_version, gating) {
        eprintln!("chock: {said}");
    }
}

/// Writes each first record and each gain into the records, and returns what it says of them.
pub(super) fn settles(
    root: &std::path::Path,
    held: &Baseline,
    reports: &[GateReport],
    chock_version: &str,
    gating: Gating,
) -> Vec<String> {
    // The run measured the work tree. A record that held a change the commit leaves out would be
    // wrong for the commit, so the record then moves as it does outside a commit.
    let left = if gating == Gating::Commit {
        crate::run::baseline::left_out(root)
    } else {
        Vec::new()
    };
    let committing = if left.is_empty() {
        gating
    } else {
        Gating::Other
    };
    let mut said = crate::gates::coverage::crap::relaid(root, committing);
    let file = crate::run::baseline::FILE;
    if let Some(settled) = forgets(root, held, locked_in(held, reports, committing), committing) {
        if !left.is_empty() && !settled.waiting.is_empty() {
            said.push(lowered_none(&left));
        }
        let measured: Vec<String> = reports
            .iter()
            .filter(|report| report.measured.is_some())
            .map(|report| report.gate.clone())
            .collect();
        said.push(settled.written(root, chock_version, committing, &measured));
    } else if gating == Gating::Commit && crate::project::vcs::unstaged(root, file) {
        // A commit its message refused leaves the record it wrote unstaged; a retry writes none.
        said.push(format!(
            "{file} holds a record that this commit leaves out; stage it to commit it"
        ));
    }
    said
}

/// Why a commit lowered no record: the tree it measured is not the tree it commits. CI then fails
/// each record that stays above what the commit holds.
fn lowered_none(paths: &[String]) -> String {
    format!(
        "this commit leaves out {}, so no record moved down, and CI fails each record above what \
         the commit holds; stage those changes, or stash them with `git stash -k -u`",
        some_of(paths)
    )
}

/// `settled`, and at a commit, less each record of a file that neither the disk nor the repository
/// holds any more: a file that comes back at the same path starts again from no record.
pub(super) fn forgets(
    root: &std::path::Path,
    held: &Baseline,
    settled: Option<Settled>,
    gating: Gating,
) -> Option<Settled> {
    if gating != Gating::Commit {
        return settled;
    }
    let holder = crate::project::vcs::live_holder(root);
    let Ok(tracked) = crate::project::vcs::tracked(root, holder) else {
        return settled;
    };
    let tracked: std::collections::HashSet<String> = tracked.into_iter().collect();
    forgotten(held, settled, &|file| {
        !tracked.contains(file) && !root.join(file).exists()
    })
}

/// `settled`, less each key of a `gone` file in every gate that keeps a number for each file, on
/// every system; `None` when it has nothing to write or to say.
pub(super) fn forgotten(
    held: &Baseline,
    settled: Option<Settled>,
    gone: &dyn Fn(&str) -> bool,
) -> Option<Settled> {
    let mut settled = settled.unwrap_or_else(|| Settled::new(held));
    let Settled {
        record,
        lowered,
        moved,
        ..
    } = &mut settled;
    for (name, series) in &mut record.gates {
        let gate = name.split_once('@').map_or(name.as_str(), |(gate, _)| gate);
        let (before, total) = (series.0.len(), series.0.values().sum::<u64>());
        if crate::gates::holds_each_file(gate) {
            series
                .0
                .retain(|key, _| !gone(crate::run::baseline::file_of(key)));
        }
        if series.0.len() == before {
            continue;
        }
        let now = series.0.values().sum::<u64>();
        let said = lowered
            .iter()
            .find(|was| crate::run::baseline::kept_here(was) == *name);
        if let Some(was) = said {
            moved.entry(was.clone()).or_insert((total, now)).1 = now;
        } else {
            lowered.push(name.clone());
            moved.insert(name.clone(), (total, now));
        }
    }
    (!settled.said().is_empty()).then_some(settled)
}

/// `remember`, saying so on stderr when the record could not be written: `chock explain` then
/// answers from an older run, and a reader has to be told which.
#[expect(
    clippy::print_stderr,
    reason = "the run tells the user on stderr that it kept no record"
)]
pub fn remember_or_say(root: &std::path::Path, reports: &[GateReport], chock_version: &str) {
    if let Err(error) = remember(root, reports, chock_version) {
        eprintln!(
            "chock: this run was not recorded, so `chock explain` shows an older one: {error}"
        );
    }
}

/// Each report with the time it was taken, so an old pass does not read as a fresh one.
pub(super) fn stamped(reports: &[GateReport]) -> Vec<GateReport> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .ok();
    reports
        .iter()
        .map(|report| GateReport {
            ran_at: now,
            ..report.clone()
        })
        .collect()
}
