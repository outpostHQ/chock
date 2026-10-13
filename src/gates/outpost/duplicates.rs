//! Copied production code as Outpost finds it: by shape rather than syntax, in every language.
//! The `duplication` gate is the Rust-only syntax-tree check.

use crate::run::baseline::Keys;
use crate::run::report::{Detail, Place};
use crate::run::{Ctx, Details, Gate, Group, Kind, Measurement};

pub const GATE: Gate = Gate {
    name: "duplicates",
    about: "shipped code duplicated elsewhere in the tree, by shape rather than by syntax; needs \
            `outpost`",
    group: Group::OptIn,
    builds: false,
    reads: Some(crate::run::verdicts::OUTPOST),
    kind: Kind::AnnotatedRatchet {
        measure,
        keys: Keys::Items,
        unit: "duplicated line(s)",
    },
};

/// Counted in lines rather than copies, since lines are what folding a duplicate recovers.
const MEASURE: &str = "duplicate_lines_production";

/// Where Outpost names the other copies of each group.
const GROUPS: &str = "duplicate_groups_production";

fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    measured(super::once(ctx)?)
}

/// The lines of each copy, and where its other copies are, where Outpost names them.
fn measured(check: &super::Check) -> Result<Measurement, String> {
    let mut measured = Measurement::of(check.series(MEASURE)?, Vec::new());
    if let Ok(groups) = check.measure(GROUPS) {
        measured.details = copies(&groups.findings);
    }
    Ok(measured)
}

/// Each group's other copies, from a message such as "`walked` is repeated in 3 places, also
/// src/a.rs:4, src/b.rs:9; fold the copies into one", under the key its lines are counted by.
fn copies(groups: &[super::Finding]) -> Details {
    let mut details = Details::new();
    for group in groups {
        let listed = group
            .message
            .as_deref()
            .and_then(|said| said.split_once(", also "));
        let Some((places, _)) = listed.and_then(|(_, rest)| rest.rsplit_once(';')) else {
            continue;
        };
        let place = |place: &str| {
            let (file, line) = place.rsplit_once(':')?;
            Some(Place::at("copy", file, line.parse().ok()?))
        };
        let places = places.split(", ").filter_map(place).collect();
        let detail = Detail {
            line: group.line,
            places,
            fix: None,
        };
        details.insert(group.key(), detail);
    }
    details
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;
    use crate::run::baseline::Series;

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
        let series = measured(&check).unwrap().series;
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
    fn a_duplicates_measure_outpost_stopped_reporting_is_a_gate_that_could_not_run() {
        let check = checked_from(r#"{"contract": 1, "measures": []}"#);
        assert!(
            check
                .series(MEASURE)
                .unwrap_err()
                .starts_with("outpost no longer reports")
        );
    }

    /// Each group names its other copies at the key its lines are counted by.
    #[test]
    fn a_duplicate_names_where_its_other_copies_are() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "duplicate_lines_production", "verdict": "passed", "findings": [
                {"file": "src/a.rs", "line": 4, "item": "walked", "measured": 5}]},
              {"measure": "duplicate_groups_production", "verdict": "passed", "findings": [
                {"file": "src/a.rs", "line": 4, "item": "walked", "message":
                 "`walked` is repeated in 3 places, also src/b.rs:9, src/c.rs:12; fold the copies into one"},
                {"file": "src/d.rs", "line": 2, "item": "bad", "message":
                 "`bad` is repeated in 2 places, also src/e.rs; fold the copies into one"},
                {"file": "src/f.rs", "line": 1, "item": "plain", "message": "no list here"}]}]}"#,
        );
        let measured = measured(&check).unwrap();
        let walked = Detail {
            line: Some(4),
            places: vec![
                Place::at("copy", "src/b.rs", 9),
                Place::at("copy", "src/c.rs", 12),
            ],
            fix: None,
        };
        let bad = Detail {
            line: Some(2),
            places: Vec::new(),
            fix: None,
        };
        let expected = Details::from([
            ("src/a.rs#walked".to_string(), walked),
            ("src/d.rs#bad".to_string(), bad),
        ]);
        assert_eq!(measured.details, expected);
    }

    /// An Outpost that does not name the copies still counts their lines.
    #[test]
    fn a_duplicate_with_no_groups_still_counts_its_lines() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "duplicate_lines_production", "verdict": "passed", "findings": [
                {"file": "src/a.rs", "line": 4, "item": "walked", "measured": 5}]}]}"#,
        );
        let measured = measured(&check).unwrap();
        assert_eq!(measured.details, Details::new());
        assert_eq!(measured.series.get("src/a.rs#walked"), Some(5));
    }
}
