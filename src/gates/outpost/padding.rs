//! Production code longer than the tree's density explains, measured by Outpost with comments and
//! strings discarded.

use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

pub const GATE: Gate = Gate {
    name: "padding",
    about: "production code longer than this tree's own density explains; needs `outpost`",
    group: Group::OptIn,
    builds: false,
    reads: Some(crate::run::verdicts::OUTPOST),
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "line(s) over",
    },
};

const MEASURE: &str = "excess_lines_production";

fn measure(ctx: &Ctx) -> Result<Series, String> {
    over_the_bar(super::once(ctx)?)
}

/// Every entity over the bar, provided two runs can be compared against that bar.
fn over_the_bar(check: &super::Check) -> Result<Series, String> {
    comparable(check.measure(MEASURE)?.relative, check.bar.as_ref())?;
    check.series(MEASURE)
}

/// Whether two runs of this measure can be compared. An unpinned bar is this tree's median
/// density, so an entity nobody edited crosses it when anything else moves.
fn comparable(relative: bool, bar: Option<&super::Bar>) -> Result<(), String> {
    if !relative {
        return Ok(());
    }
    match bar {
        None => Err(
            "outpost reports no density bar, so what this was measured against cannot be \
                     read; a newer outpost carries one"
                .to_string(),
        ),
        Some(bar) if !bar.pinned => Err(format!(
            "this tree's own median density of {:.2} over {} entities is the bar, so it moves \
             whenever the tree does and no two runs compare: pin it with `outpost check --pin-bar`",
            bar.tree_median, bar.population
        )),
        Some(_) => Ok(()),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;

    fn checked_from(json: &str) -> crate::gates::outpost::Check {
        crate::gates::outpost::checked(&crate::exec::Output::of(Some(0), json, "")).unwrap()
    }

    fn bar(tree_median: f64, pinned: bool) -> crate::gates::outpost::Bar {
        crate::gates::outpost::Bar {
            tree_median,
            population: 724,
            pinned,
        }
    }

    #[test]
    fn a_measure_judged_against_an_unpinned_tree_median_cannot_be_ratcheted() {
        let why = comparable(true, Some(&bar(7.57, false))).unwrap_err();
        assert!(why.contains("outpost check --pin-bar"), "{why}");
        assert!(why.contains("7.57"), "{why}");
        assert!(why.contains("724"), "{why}");
    }

    #[test]
    fn a_pinned_bar_is_the_same_yardstick_twice_and_compares() {
        assert_eq!(comparable(true, Some(&bar(7.57, true))), Ok(()));
    }

    #[test]
    fn an_outpost_that_reports_no_bar_at_all_stops_the_gate() {
        let why = comparable(true, None).unwrap_err();
        assert!(why.contains("no density bar"), "{why}");
    }

    #[test]
    fn a_measure_that_is_not_relative_needs_no_bar_to_be_comparable() {
        assert_eq!(comparable(false, None), Ok(()));
    }

    #[test]
    fn an_entity_over_the_trees_density_is_keyed_by_its_name_and_valued_by_its_excess() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "excess_lines_production", "verdict": "passed", "findings": [
                {"file": "src/hooks.rs", "line": 28, "item": "hook_bodies", "measured": 7},
                {"file": "src/gates/profile.rs", "line": 105, "item": "free_wins", "measured": 1}]}]}"#,
        );
        let series = over_the_bar(&check).unwrap();
        assert_eq!(series.get("src/hooks.rs#hook_bodies"), Some(7));
        assert_eq!(series.get("src/gates/profile.rs#free_wins"), Some(1));
    }

    #[test]
    fn a_relative_measure_with_an_unpinned_bar_reaches_no_series() {
        let check = checked_from(
            r#"{"contract": 1,
              "bar": {"density": 7.5, "tree_median": 7.5, "population": 725, "pinned": false},
              "measures": [
              {"measure": "excess_lines_production", "verdict": "passed", "relative": true,
               "findings": [{"file": "src/a.rs", "item": "f", "measured": 4}]}]}"#,
        );
        let why = over_the_bar(&check).unwrap_err();
        assert!(why.contains("outpost check --pin-bar"), "{why}");
    }

    #[test]
    fn a_relative_measure_with_a_pinned_bar_reaches_its_series() {
        let check = checked_from(
            r#"{"contract": 1,
              "bar": {"density": 7.5, "tree_median": 7.5, "population": 725, "pinned": true},
              "measures": [
              {"measure": "excess_lines_production", "verdict": "passed", "relative": true,
               "findings": [{"file": "src/a.rs", "item": "f", "measured": 4}]}]}"#,
        );
        assert_eq!(over_the_bar(&check).unwrap().get("src/a.rs#f"), Some(4));
    }

    #[test]
    fn a_tree_at_its_own_density_records_nothing() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "excess_lines_production", "verdict": "passed", "findings": []}]}"#,
        );
        assert_eq!(check.series(MEASURE).unwrap(), Series::new());
    }

    #[test]
    fn a_padding_measure_outpost_stopped_reporting_is_a_gate_that_could_not_run() {
        let check = checked_from(r#"{"contract": 1, "measures": []}"#);
        assert_eq!(
            check.series(MEASURE).unwrap_err(),
            "outpost no longer reports `excess_lines_production`, so nothing measured it"
        );
    }

    #[test]
    fn a_measure_that_could_not_run_refuses_rather_than_reading_as_clean() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "excess_lines_production", "verdict": "cannot_run",
               "cannot_run_reason": "the grammar could not open the tree", "findings": []}]}"#,
        );
        assert!(
            check
                .series(MEASURE)
                .unwrap_err()
                .contains("could not open")
        );
    }
}
