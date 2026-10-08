//! Which compiling gates run side by side, and the records of the last run that plan it.

use super::report::{self, GateReport, Run};
use super::workers::on_workers;
use super::{Ctx, Gate, Told, in_turn};

/// With jobs for two lanes or more, the gates that compile, in the lanes `lanes_of` plans.
/// Otherwise every slot stays empty.
pub(super) fn in_lanes(gates: &[&Gate], ctx: &Ctx, told: Told) -> Vec<Option<GateReport>> {
    let room = room(crate::exec::budget::available_mb().unwrap_or(0));
    let lanes = lanes_of(gates, ctx, &recorded_peaks(&ctx.root), room);
    let width = crate::exec::budget::lanes(ctx.jobs, lanes.len());
    let lanes = if width > 1 { lanes } else { Vec::new() };
    on_workers(gates, &lanes, width, &|gate| {
        let _share = crate::exec::budget::Lane::enter();
        in_turn(gate, ctx, told)
    })
}

/// The MB the lanes may plan for: a quarter stays free for what the record did not see.
fn room(available_mb: u64) -> u64 {
    available_mb / 4 * 3
}

/// The lanes as indexes into `gates`: the compiling gates that share the suite's target directory,
/// those that build apart (so no two release builds run at once), then one per gate `alone` frees.
pub(super) fn lanes_of(
    gates: &[&Gate],
    ctx: &Ctx,
    peaks: &[(String, u64)],
    room: u64,
) -> Vec<Vec<usize>> {
    let (apart, beside): (Vec<usize>, Vec<usize>) = gates
        .iter()
        .enumerate()
        .filter(|(_, gate)| gate.builds)
        .map(|(at, _)| at)
        .partition(|&at| crate::gates::builds_apart(gates[at].name, &ctx.build));
    let peak = |at: &usize| {
        peaks
            .iter()
            .find(|(name, _)| name == gates[*at].name)
            .map(|(_, mb)| *mb)
    };
    let alone = alone(
        &beside,
        &apart,
        &|at| crate::gates::BUILDS_ALONE.contains(&gates[at].name),
        &peak,
        room,
    );
    let rest = beside
        .into_iter()
        .filter(|at| !alone.contains(at))
        .collect();
    [
        vec![rest, apart],
        alone.into_iter().map(|at| vec![at]).collect(),
    ]
    .concat()
}

/// The gates of `beside` that build alone, each on a lane of its own, while all the peaks that
/// would run at once fit in `room` MB. A lane runs one gate at a time, so it holds its largest.
fn alone(
    beside: &[usize],
    apart: &[usize],
    builds_alone: &dyn Fn(usize) -> bool,
    peak: &dyn Fn(&usize) -> Option<u64>,
    room: u64,
) -> Vec<usize> {
    let most = |lane: &[usize], out: &[usize]| {
        // A gate with no record adds nothing to its lane's largest.
        let held = lane.iter().filter(|at| !out.contains(at)).filter_map(peak);
        held.max().unwrap_or(0)
    };
    let mut alone: Vec<usize> = Vec::new();
    for &at in beside.iter().filter(|at| builds_alone(**at)) {
        let tried = [alone.as_slice(), &[at]].concat();
        // With no record there is no lane: the sum is `None`.
        let held = tried.iter().map(peak).sum::<Option<u64>>();
        if held.is_some_and(|mb| mb + most(beside, &tried) + most(apart, &[]) <= room) {
            alone = tried;
        }
    }
    alone
}

/// Each gate's report as the last run left it, or none.
fn last_run(root: &std::path::Path) -> impl Iterator<Item = GateReport> {
    let text = std::fs::read_to_string(root.join(report::LAST_RUN)).unwrap_or_default();
    let last = serde_json::from_str::<Run>(&text).ok();
    last.into_iter().flat_map(|last| last.gates)
}

/// Each gate's peak memory in MB as the last run left it, or none.
fn recorded_peaks(root: &std::path::Path) -> Vec<(String, u64)> {
    let gates = last_run(root);
    gates
        .filter_map(|gate| Some((gate.gate, gate.peak_mb?)))
        .collect()
}

/// Each test's time in ms as the last run of `gate` left it, or none.
pub(crate) fn recorded_tests(
    root: &std::path::Path,
    gate: &str,
) -> std::collections::BTreeMap<String, u64> {
    let mut gates = last_run(root);
    gates
        .find(|report| report.gate == gate)
        .map(|report| report.tests_ms)
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;
    use crate::run::report::Verdict;

    #[test]
    fn a_quarter_of_what_is_free_stays_free() {
        assert_eq!(room(100), 75);
        assert_eq!(room(0), 0);
    }

    #[test]
    fn a_gate_runs_alone_only_while_every_peak_that_would_run_at_once_fits() {
        let mb = [Some(30), Some(10), Some(20), Some(5)];
        let alone_at = |at: usize| at != 1;
        let fits = |room| alone(&[0, 1, 2], &[3], &alone_at, &|at: &usize| mb[*at], room);
        assert_eq!(fits(65), [0, 2], "30 + 20 + 10 + 5");
        assert_eq!(
            fits(64),
            [0],
            "30 + 20 + 5, the suite's lane holding 20 while 0 runs"
        );
        assert!(fits(54).is_empty(), "either one alone needs 55");
        let unrecorded = |at: &usize| mb[*at].filter(|_| *at != 0);
        let free = alone(&[0, 1, 2], &[3], &alone_at, &unrecorded, 1000);
        assert_eq!(free, [2], "a gate with no record stays in its lane");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_peaks_lanes_are_planned_by_are_those_the_last_run_left() {
        let dir = crate::testdir::make("run-recorded-peaks");
        assert!(recorded_peaks(&dir).is_empty(), "no run yet");
        let mut held = GateReport::new("mutest", Verdict::Pass, "chock run mutest");
        held.peak_mb = Some(9);
        let mut timed = GateReport::new("miri", Verdict::Pass, "chock run miri");
        timed.tests_ms.insert("chock a".to_string(), 7);
        report::remember(&dir, &[held, timed], "0.2.0").unwrap();
        assert_eq!(recorded_peaks(&dir), [("mutest".to_string(), 9)]);
        assert_eq!(recorded_tests(&dir, "miri")["chock a"], 7);
        assert!(recorded_tests(&dir, "mutest").is_empty());
    }
}
