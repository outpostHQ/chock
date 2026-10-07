//! Whether each concept lives in one directory, read from Outpost's tree-wide reference graph.

use crate::run::baseline::{Keys, Series};
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Measurement};

pub const GATE: Gate = Gate {
    name: "boundaries",
    about: "a definition whose callers are spread past the directory that owns it, and a directory \
            holding more than one concern; needs `outpost`",
    group: Group::OptIn,
    builds: false,
    reads: Some(crate::run::verdicts::OUTPOST),
    kind: Kind::AnnotatedRatchet {
        measure,
        keys: Keys::Items,
        unit: "file(s) off one concern, tree-wide",
    },
};

/// The two tree-wide totals, the sites behind a total past its record, and the modularity caveat.
fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let check = super::once(ctx)?;
    let series = graded(check)?;
    let mut findings = caveat(check);
    findings.extend(check.sites_over(&series, &ctx.record(GATE.name)));
    Ok(Measurement::of(series, findings))
}

/// A note when modularity is under Outpost's floor, where the totals move as files are added.
fn caveat(check: &super::Check) -> Vec<Finding> {
    check
        .measure(FOREIGN)
        .ok()
        .filter(|held| held.below_modularity_floor)
        .and_then(|held| held.modularity)
        .map(|modularity| {
            Finding::at(
                "",
                &format!(
                    "Outpost's modularity for this tree is {modularity:.2}, under its 0.3 floor: \
                     these totals are one partition among several and move as files are added"
                ),
            )
            .item("modularity")
        })
        .into_iter()
        .collect()
}

/// How many of a directory's coupled files sit outside its largest group.
const FOREIGN: &str = "mixed_directory_foreign_files_production";

/// Groups spread across directories, counted rather than keyed by hub, since hubs re-partition
/// when a file moves.
const SCATTERED: &str = "scattered_groups_production";

/// Two tree-wide rows, not per directory, so a file moving between directories is not new debt.
fn graded(check: &super::Check) -> Result<Series, String> {
    let mut series = Series::new();
    for name in [FOREIGN, SCATTERED] {
        series.set(name, tallied(check.measure(name)?)?);
    }
    Ok(series)
}

/// Outpost's own total for the measure; a missing total is an error, never a zero.
fn tallied(measured: &super::Measure) -> Result<u64, String> {
    measured.measured.ok_or_else(|| {
        format!(
            "outpost reported `{}` as measured with no total, so nothing measured it",
            measured.measure
        )
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::exec;

    fn checked(rows: &str) -> super::super::Check {
        let out = exec::Output {
            code: Some(0),
            stdout: format!(r#"{{"contract":1,"measures":[{rows}]}}"#),
            stderr: String::new(),
            truncated: false,
        };
        super::super::checked(&out).unwrap()
    }

    /// The two rows, each with what goes before its findings: a total, and any flags.
    fn rows(mixed: &str, scattered: &str) -> String {
        format!(
            r#"{{"measure":"{FOREIGN}","verdict":"passed",{mixed}"findings":[]}},
               {{"measure":"{SCATTERED}","verdict":"passed",{scattered}"findings":[]}}"#
        )
    }

    fn total(measured: u64) -> String {
        format!(r#""measured":{measured},"#)
    }

    fn at(directory: &str, measured: u64) -> String {
        format!(r#"{{"file":"{directory}","measured":{measured}}}"#)
    }

    #[test]
    fn the_tree_records_the_count_of_files_outside_the_concern_of_their_directory() {
        let found = graded(&checked(&rows(&total(35), &total(0)))).unwrap();
        assert_eq!(found.get(FOREIGN), Some(35));
    }

    #[test]
    fn a_directory_that_re_clustered_does_not_move_the_tree_wide_count() {
        let before = graded(&checked(&rows(&total(33), &total(0)))).unwrap();
        let after = graded(&checked(&rows(&total(33), &total(0)))).unwrap();
        assert_eq!(before.get(FOREIGN), Some(33));
        assert_eq!(after.get(FOREIGN), before.get(FOREIGN));
        assert_eq!(after.get("src#mixed"), None);
    }

    #[test]
    fn a_tree_with_every_concept_in_one_place_records_both_rows_at_zero() {
        let found = graded(&checked(&rows(&total(0), &total(0)))).unwrap();
        assert_eq!(found.get(FOREIGN), Some(0));
        assert_eq!(found.get(SCATTERED), Some(0));
    }

    #[test]
    fn groups_spread_across_directories_are_counted_rather_than_named() {
        let spread = format!(
            r#"{{"measure":"{FOREIGN}","verdict":"passed","measured":4,"findings":[]}},
               {{"measure":"{SCATTERED}","verdict":"passed","measured":2,"findings":[{},{}]}}"#,
            at("a/hub.rs", 7),
            at("b/hub.rs", 3)
        );
        let found = graded(&checked(&spread)).unwrap();
        assert_eq!(found.get(SCATTERED), Some(2));
        assert_eq!(found.get("a/hub.rs#mixed"), None);
    }

    #[test]
    fn a_measure_reported_with_no_total_stops_the_gate() {
        for (mixed, scattered, missing) in [
            (String::new(), total(0), FOREIGN),
            (total(0), String::new(), SCATTERED),
        ] {
            let why = graded(&checked(&rows(&mixed, &scattered))).unwrap_err();
            assert!(why.contains("no total"), "{why}");
            assert!(why.contains(missing), "{why}");
        }
    }

    #[test]
    fn a_tree_under_the_modularity_floor_is_measured_and_says_so() {
        let flagged = r#""below_modularity_floor":true,"modularity":0.1718,"measured":15,"#;
        let ctx = Ctx::for_root(
            std::path::PathBuf::from("/nowhere"),
            crate::run::baseline::Baseline::default(),
        );
        assert!(
            ctx.checked
                .set(Ok(checked(&rows(flagged, &total(1)))))
                .is_ok()
        );
        let measured = measure(&ctx).unwrap();
        assert_eq!(
            (measured.series.get(FOREIGN), measured.series.get(SCATTERED)),
            (Some(15), Some(1))
        );
        let said: Vec<(Option<&str>, bool)> = measured
            .findings
            .iter()
            .map(|note| (note.item.as_deref(), note.message.contains("0.17")))
            .collect();
        assert_eq!(said, [(Some("modularity"), true)]);
        assert_eq!(caveat(&checked(&rows(&total(15), &total(1)))), Vec::new());
    }

    #[test]
    fn a_measure_outpost_no_longer_reports_stops_the_gate() {
        let only_one =
            format!(r#"{{"measure":"{FOREIGN}","verdict":"passed","measured":0,"findings":[]}}"#);
        assert!(graded(&checked(&only_one)).is_err());
    }

    #[test]
    fn a_measure_that_could_not_run_stops_the_gate() {
        let refused = format!(
            r#"{{"measure":"{FOREIGN}","verdict":"cannot_run","cannot_run_reason":"no graph","findings":[]}},
               {{"measure":"{SCATTERED}","verdict":"passed","findings":[]}}"#
        );
        assert!(graded(&checked(&refused)).is_err());
    }
}
