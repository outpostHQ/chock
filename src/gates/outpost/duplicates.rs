//! Copied production code as Outpost finds it: by shape rather than syntax, in every language.
//! The `duplication` gate is the Rust-only syntax-tree check.

use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

pub const GATE: Gate = Gate {
    name: "duplicates",
    about: "shipped code duplicated elsewhere in the tree, by shape rather than by syntax; needs \
            `outpost`",
    group: Group::OptIn,
    builds: false,
    reads: Some(crate::run::verdicts::OUTPOST),
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "duplicated line(s)",
    },
};

/// Counted in lines rather than copies, since lines are what folding a duplicate recovers.
const MEASURE: &str = "duplicate_lines_production";

fn measure(ctx: &Ctx) -> Result<Series, String> {
    super::once(ctx)?.series(MEASURE)
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

    #[test]
    fn a_duplicated_definition_is_keyed_by_its_name_and_valued_by_its_lines() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "duplicate_lines_production", "verdict": "passed", "findings": [
                {"file": "src/gates/prodlines.rs", "line": 280, "item": "walked", "measured": 5},
                {"file": "src/project.rs", "line": 38, "item": "metadata", "measured": 3}]}]}"#,
        );
        let series = check.series(MEASURE).unwrap();
        assert_eq!(series.get("src/gates/prodlines.rs#walked"), Some(5));
        assert_eq!(series.get("src/project.rs#metadata"), Some(3));
    }

    /// Test code is left out: fixtures are copied across tests by design.
    #[test]
    fn the_measure_read_is_the_production_half() {
        assert_eq!(MEASURE, "duplicate_lines_production");
    }

    #[test]
    fn a_tree_with_nothing_duplicated_records_nothing() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "duplicate_lines_production", "verdict": "passed", "findings": []}]}"#,
        );
        assert_eq!(check.series(MEASURE).unwrap(), Series::new());
    }

    #[test]
    fn a_comparison_that_could_not_run_refuses_rather_than_reading_as_clean() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "duplicate_lines_production", "verdict": "cannot_run",
               "cannot_run_reason": "no files were indexed", "findings": []}]}"#,
        );
        assert!(
            check
                .series(MEASURE)
                .unwrap_err()
                .contains("no files were indexed")
        );
    }

    #[test]
    fn a_measure_outpost_stopped_reporting_is_a_gate_that_could_not_run() {
        let check = checked_from(r#"{"contract": 1, "measures": []}"#);
        assert!(
            check
                .series(MEASURE)
                .unwrap_err()
                .starts_with("outpost no longer reports")
        );
    }
}
