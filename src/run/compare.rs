//! What a run's comparison with its record says, finding by finding.

use crate::run::baseline::{Change, Keys, Series};
use crate::run::measurement::Details;
use crate::run::report::{Finding, Verdict};

/// What the comparison says, once it is known to be possible. Each of its three questions goes
/// only to the key kinds it fits, and only the first two can fail the gate.
pub(super) fn compared(
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
        .map(|change| finding_for(change, unit).detailed(details.get(change.key())))
        .chain(dropped.iter().map(|key| dropped_finding(key, was, unit)))
        .chain(added.iter().map(|key| added_finding(key, now, gate, unit)))
        .collect();
    (verdict, findings)
}

/// A key is a path when it looks like one, so the finding lands where an editor can open it.
pub(super) fn placed(key: &str, message: &str) -> Finding {
    match key.split_once('#') {
        Some((path, name)) => Finding::at(path, message).item(name),
        None if key.contains('/') || key.contains(".rs") => Finding::at(key, message),
        None => Finding::at("", message).item(key),
    }
}

pub(super) fn finding_for(change: &Change, unit: &str) -> Finding {
    let (message, held) = match change {
        Change::Grew { was, now, .. } => {
            (format!("{now} {unit}, over the recorded {was}"), Some(*was))
        }
        Change::New { now, .. } => (format!("{now} {unit}, not in the baseline"), None),
    };
    placed(change.key(), &message).numbers(change.now(), held)
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
