//! The ratchet of measured gates: the first record of a measure, and the lock-in of a lowered one.

use super::compare::{compared, placed};
use super::{
    Ctx, Gate, Kind, Measurement, held_against, newly_measured, numbered, rekeyed, rerun,
    takes_first, touched,
};
use crate::run::baseline::{Keys, Series};
use crate::run::report::{Finding, GateReport, Verdict};

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
    let total = now.summed();
    let mut report = numbered(gate, Verdict::Pass, total, total, unit);
    report.findings = first_notes(gate, read.findings);
    if let Err(why) = touched(&mut report, gate, ctx, &now, &now, keys) {
        return GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &why);
    }
    report.tightened = Some(now);
    report
}

pub(super) fn measured_ratchet(
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
    let total = now.summed();
    let was = held_against(gate, ctx, &now);
    let added = newly_measured(gate, ctx, &now, &was);
    let mut against = was.clone();
    against.0.extend(added.0.clone());
    let regroup = regrouped(gate, &now, &against, keys);
    let recount = recounted(ctx.baseline.unit(gate.name), unit);
    if let Some(reason) = regroup.or(recount) {
        return GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &reason);
    }
    let (verdict, findings) = compared(&now, &against, keys, gate.name, unit, &read.details);
    let mut report = numbered(gate, verdict, total, against.summed(), unit);
    report.findings = findings;
    report.findings.extend(read.findings);
    if let Err(why) = touched(&mut report, gate, ctx, &now, &against, keys) {
        return GateReport::cannot_run(gate.name, rerun(gate.name).as_str(), &why);
    }
    lock_in(&mut report, gate, ctx, now.tightened(&was, keys), &was);
    report.new_measures = added;
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
                "{now} {unit} where the record holds {held}: a commit through the pre-commit hook \
                 lowers the record, or `chock baseline --lower`, and the change has to commit it"
            );
            placed(key, &said).numbers(now, Some(*held))
        })
        .collect()
}

/// Whether a baseline may carry these keys. Asked where a record is written as well as where one is
/// read, because a first run refuses before it measures and never reaches the comparison.
/// # Errors
/// The text names the first key that holds a path outside the project.
pub fn portable(series: &Series) -> Result<(), String> {
    match crate::project::unportable(series.0.keys().map(String::as_str)) {
        None => Ok(()),
        Some(key) => Err(format!(
            "measured `{key}`, which is not inside this project — a baseline is committed and read \
             elsewhere, so it cannot be keyed by one machine's paths"
        )),
    }
}
