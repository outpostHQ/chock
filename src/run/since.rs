//! What changed since the last run, said where a gate's verdict changed. A pass that turns into a
//! trip, with no edit anybody remembers, otherwise reads as chock contradicting itself.
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::run::report::{GateReport, Run, Verdict};

/// Most files one line names; the rest are counted.
const NAMED: usize = 3;

/// Each gate's verdict in the last run, and when it was taken, as seconds since the Unix epoch.
/// Read before this run writes its own record over it.
#[must_use]
pub fn before(root: &Path) -> BTreeMap<String, (Verdict, u64)> {
    let text = std::fs::read_to_string(root.join(crate::run::report::LAST_RUN)).unwrap_or_default();
    let Ok(last) = serde_json::from_str::<Run>(&text) else {
        return BTreeMap::new();
    };
    last.gates
        .into_iter()
        .filter_map(|report| Some((report.gate, (report.verdict, report.ran_at?))))
        .collect()
}

/// One line for each gate that passed in the last run and trips now, or the reverse, with the
/// files `modified` names since that verdict. `now` is seconds since the Unix epoch.
#[must_use]
pub fn told(
    before: &BTreeMap<String, (Verdict, u64)>,
    reports: &[GateReport],
    modified: &dyn Fn(SystemTime) -> Option<Vec<String>>,
    now: u64,
) -> Vec<String> {
    let mut lines = Vec::new();
    for report in reports {
        let Some(&(was, stamped)) = before.get(&report.gate) else {
            continue;
        };
        if !flipped(was, report.verdict) {
            continue;
        }
        let taken = UNIX_EPOCH.checked_add(Duration::from_secs(stamped));
        let mut changed = taken.and_then(modified);
        if let Some(files) = changed.as_mut() {
            files.sort();
        }
        let ago = now.saturating_sub(stamped);
        lines.push(line(
            &report.gate,
            (was, report.verdict),
            ago,
            changed.as_deref(),
        ));
    }
    lines
}

/// Whether a verdict went from a pass to a trip or back. A gate that could not run measured
/// nothing, so it changed nothing worth explaining.
fn flipped(was: Verdict, is: Verdict) -> bool {
    matches!(
        (was, is),
        (Verdict::Pass, Verdict::Tripped) | (Verdict::Tripped, Verdict::Pass)
    )
}

fn line(gate: &str, (was, is): (Verdict, Verdict), ago: u64, changed: Option<&[String]>) -> String {
    let why = match changed {
        None => String::new(),
        Some([]) => "; no file in the tree changed since then, so a tool, the config, the record \
                     or git did"
            .to_string(),
        Some(files) => {
            let named: Vec<&str> = files.iter().take(NAMED).map(String::as_str).collect();
            let more = files.len().saturating_sub(NAMED);
            let rest = if more == 0 {
                String::new()
            } else {
                format!(" and {more} more")
            };
            format!("; changed since then: {}{rest}", named.join(", "))
        }
    };
    format!(
        "chock: {gate} was {was} {} and is {is} now{why}",
        ago_text(ago)
    )
}

/// A time in the past without a calendar or a time zone, which chock does not carry.
fn ago_text(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds} s ago"),
        60..3_600 => format!("{} min ago", seconds / 60),
        3_600..86_400 => format!("{} h ago", seconds / 3_600),
        _ => {
            let days = usize::try_from(seconds / 86_400).unwrap_or(usize::MAX);
            format!("{} ago", crate::run::report::plural(days, "day"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(gate: &str, verdict: Verdict) -> GateReport {
        GateReport::new(gate, verdict, &format!("chock run {gate}"))
    }

    fn last(gate: &str, verdict: Verdict, at: u64) -> BTreeMap<String, (Verdict, u64)> {
        BTreeMap::from([(gate.to_string(), (verdict, at))])
    }

    /// testlint passed, a test without an assertion was written, and the commit tripped.
    #[test]
    fn a_pass_that_trips_now_names_each_file_modified_since_the_pass() {
        let modified = |at: SystemTime| {
            assert_eq!(at, UNIX_EPOCH + Duration::from_secs(1_000));
            Some(vec!["src/media/capture.rs".to_string()])
        };
        let said = told(
            &last("testlint", Verdict::Pass, 1_000),
            &[report("testlint", Verdict::Tripped)],
            &modified,
            1_240,
        );
        assert_eq!(
            said,
            [
                "chock: testlint was ok 4 min ago and is TRIPPED now; changed since then: \
              src/media/capture.rs"
            ]
        );
    }

    #[test]
    fn a_gate_with_the_same_verdict_or_no_earlier_one_says_nothing() {
        let named = |_: SystemTime| Some(vec!["src/a.rs".to_string()]);
        let passed = last("lint", Verdict::Pass, 1);
        assert_eq!(
            told(&passed, &[report("lint", Verdict::Pass)], &named, 9),
            Vec::<String>::new()
        );
        let tripped = last("lint", Verdict::Tripped, 1);
        assert_eq!(
            told(&tripped, &[report("lint", Verdict::Tripped)], &named, 9),
            Vec::<String>::new()
        );
        assert_eq!(
            told(&passed, &[report("slop", Verdict::Tripped)], &named, 9),
            Vec::<String>::new()
        );
        let refused = report("lint", Verdict::CannotRun);
        assert_eq!(told(&passed, &[refused], &named, 9), Vec::<String>::new());
    }

    /// A gate with no earlier verdict, or with the same one, does not stop the reading of the gates
    /// after it. Mutant: `break` for either `continue` loses the flip of `lint`.
    #[test]
    fn a_gate_that_says_nothing_does_not_hide_a_later_flip() {
        let named = |_: SystemTime| Some(vec!["src/a.rs".to_string()]);
        let mut before = last("lint", Verdict::Pass, 1);
        before.extend(last("fmt", Verdict::Pass, 1));
        let alone = told(&before, &[report("lint", Verdict::Tripped)], &named, 9);
        assert_eq!(
            alone,
            ["chock: lint was ok 8 s ago and is TRIPPED now; changed since then: src/a.rs"]
        );
        let reports = [
            report("slop", Verdict::Tripped),
            report("fmt", Verdict::Pass),
            report("lint", Verdict::Tripped),
        ];
        assert_eq!(told(&before, &reports, &named, 9), alone);
    }

    #[test]
    fn a_trip_that_passes_now_is_said_too_with_the_files_sorted_and_counted_past_three() {
        let named = |_: SystemTime| Some(["e", "d", "c", "b", "a"].map(String::from).to_vec());
        let said = told(
            &last("slop", Verdict::Tripped, 0),
            &[report("slop", Verdict::Pass)],
            &named,
            7_200,
        );
        assert_eq!(
            said,
            [
                "chock: slop was TRIPPED 2 h ago and is ok now; changed since then: a, b, c and 2 more"
            ]
        );
    }

    /// A flip with nothing modified is what a reader most needs told: the cause is not the code.
    #[test]
    fn a_flip_with_no_file_modified_says_the_cause_is_outside_the_tree() {
        let none = |_: SystemTime| Some(Vec::new());
        let said = told(
            &last("deps", Verdict::Pass, 0),
            &[report("deps", Verdict::Tripped)],
            &none,
            5,
        );
        assert_eq!(
            said,
            [
                "chock: deps was ok 5 s ago and is TRIPPED now; no file in the tree changed since \
              then, so a tool, the config, the record or git did"
            ]
        );
        let unread = |_: SystemTime| None;
        let said = told(
            &last("deps", Verdict::Pass, 0),
            &[report("deps", Verdict::Tripped)],
            &unread,
            5,
        );
        assert_eq!(said, ["chock: deps was ok 5 s ago and is TRIPPED now"]);
    }

    #[test]
    fn a_past_time_reads_in_the_largest_whole_unit() {
        assert_eq!(ago_text(59), "59 s ago");
        assert_eq!(ago_text(60), "1 min ago");
        assert_eq!(ago_text(3_599), "59 min ago");
        assert_eq!(ago_text(3_600), "1 h ago");
        assert_eq!(ago_text(86_400), "1 day ago");
        assert_eq!(ago_text(3 * 86_400), "3 days ago");
    }

    #[test]
    fn the_last_run_is_read_by_gate_with_its_time_and_an_unread_record_is_empty() {
        let dir = crate::testdir::make("since-before");
        assert!(before(&dir).is_empty());
        let mut kept = report("testlint", Verdict::Pass);
        kept.ran_at = Some(1_000);
        crate::run::report::remember(&dir, &[kept], "0.1.0").unwrap();
        let held = before(&dir);
        let (verdict, at) = held.get("testlint").copied().unwrap();
        assert_eq!(verdict, Verdict::Pass);
        assert!(at > 1_000, "remember stamps the time it wrote: {at}");
    }
}
