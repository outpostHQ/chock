//! Every measure Outpost reports, as one census keyed by measure name, so a measure that stops
//! arriving is noticed.

use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

pub const GATE: Gate = Gate {
    name: "measures",
    about: "every measure outpost reports, counted per measure; needs `outpost`",
    group: Group::OptIn,
    builds: false,
    reads: Some(crate::run::verdicts::OUTPOST),
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Census,
        unit: "counted",
    },
};

fn measure(ctx: &Ctx) -> Result<Series, String> {
    let (ran, withheld) = counted(super::once(ctx)?)?;
    judged(ran, &withheld, ctx.baseline.recorded(GATE.name))
}

/// Every measure's own total keyed by name, zeros included, and each withheld row with its reason.
fn counted(check: &super::Check) -> Result<(Series, Vec<(String, String)>), String> {
    let mut series = Series::new();
    let mut withheld = Vec::new();
    for held in &check.measures {
        let ran = match held.ran() {
            Ok(ran) => ran,
            Err(why) => {
                withheld.push((held.measure.clone(), why));
                continue;
            }
        };
        let count = ran.measured.ok_or_else(|| {
            format!(
                "outpost reported `{}` as measured with no number, so nothing measured it",
                ran.measure
            )
        })?;
        series.set(&ran.measure, count);
    }
    Ok((series, withheld))
}

/// The census to compare. Beside a withheld row, a regression among the rows that ran still trips;
/// with none, or with no record to regress from, the census is an error.
fn judged(
    mut ran: Series,
    withheld: &[(String, String)],
    held: Option<&Series>,
) -> Result<Series, String> {
    if withheld.is_empty() {
        return Ok(ran);
    }
    let why: Vec<&str> = withheld.iter().map(|(_, why)| why.as_str()).collect();
    let Some(held) = held else {
        return Err(format!(
            "{}; with no record, the census needs every measure",
            why.join("; ")
        ));
    };
    if ran.regressions(held, Keys::Census).is_empty() {
        return Err(format!(
            "{}; the other {} measure(s) hold",
            why.join("; "),
            ran.len()
        ));
    }
    // Withheld rows keep their recorded number, so the trip names only the regression.
    for (measure, _) in withheld {
        if let Some(was) = held.get(measure) {
            ran.set(measure, was);
        }
    }
    Ok(ran)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]
mod tests {
    use super::{counted, judged};
    use crate::run::baseline::{Keys, Series};

    fn checked_from(json: &str) -> crate::gates::outpost::Check {
        crate::gates::outpost::checked(&crate::exec::Output {
            code: Some(0),
            stdout: json.to_string(),
            stderr: String::new(),
            truncated: false,
        })
        .unwrap()
    }

    #[test]
    fn a_measure_at_zero_holds_a_row_like_any_other() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "dead_items", "verdict": "passed", "measured": 0},
              {"measure": "sparse", "verdict": "passed", "measured": 122}]}"#,
        );
        let (series, withheld) = counted(&check).unwrap();
        assert_eq!(series.get("dead_items"), Some(0));
        assert_eq!(series.get("sparse"), Some(122));
        assert_eq!(withheld, Vec::new());
        assert_eq!(
            judged(series.clone(), &withheld, Some(&Series::new())),
            Ok(series.clone())
        );
        assert_eq!(judged(series.clone(), &withheld, None), Ok(series));
    }

    #[test]
    fn a_census_with_a_withheld_row_is_no_first_record() {
        let (ran, withheld) = counted(&checked_from(WITHHELD)).unwrap();
        assert_eq!(
            judged(ran, &withheld, None).unwrap_err(),
            "outpost could not measure `commit_message_faults`: no remote named `origin`; \
             with no record, the census needs every measure"
        );
    }

    const WITHHELD: &str = r#"{"contract": 1, "measures": [
      {"measure": "sparse", "verdict": "passed", "measured": 9},
      {"measure": "commit_message_faults", "verdict": "cannot_run",
       "cannot_run_reason": "no remote named `origin`"}]}"#;

    #[test]
    fn a_withheld_row_leaves_the_census_unmeasured_while_the_rest_hold() {
        let (ran, withheld) = counted(&checked_from(WITHHELD)).unwrap();
        let mut held = Series::new();
        held.set("sparse", 9);
        assert_eq!(
            judged(ran, &withheld, Some(&held)).unwrap_err(),
            "outpost could not measure `commit_message_faults`: no remote named `origin`; \
             the other 1 measure(s) hold"
        );
    }

    #[test]
    fn a_regression_among_the_rows_that_ran_still_trips_beside_a_withheld_row() {
        let (ran, withheld) = counted(&checked_from(WITHHELD)).unwrap();
        let mut held = Series::new();
        held.set("sparse", 6);
        held.set("commit_message_faults", 2);
        let now = judged(ran, &withheld, Some(&held)).unwrap();
        assert_eq!(
            (now.get("sparse"), now.get("commit_message_faults")),
            (Some(9), Some(2))
        );
        assert_eq!(
            now.regressions(&held, Keys::Census)
                .iter()
                .map(crate::run::baseline::Change::key)
                .collect::<Vec<_>>(),
            ["sparse"]
        );
        assert_eq!(now.stopped_measuring(&held), Vec::<String>::new());
    }

    #[test]
    fn the_gate_counts_the_check_the_run_already_holds() {
        let ctx = crate::run::Ctx::for_root(
            std::path::PathBuf::from("/nowhere"),
            crate::run::baseline::Baseline::default(),
        );
        let check = checked_from(
            r#"{"contract": 1, "measures": [{"measure": "sparse", "verdict": "passed", "measured": 9}]}"#,
        );
        assert!(ctx.checked.set(Ok(check)).is_ok());
        assert_eq!(super::measure(&ctx).unwrap().get("sparse"), Some(9));
    }

    #[test]
    fn rows_after_a_withheld_row_are_still_counted() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "commit_message_faults", "verdict": "cannot_run",
               "cannot_run_reason": "no remote named `origin`"},
              {"measure": "sparse", "verdict": "passed", "measured": 9}]}"#,
        );
        let (ran, withheld) = counted(&check).unwrap();
        assert_eq!(ran.get("sparse"), Some(9));
        assert_eq!(
            withheld,
            [(
                "commit_message_faults".to_string(),
                "outpost could not measure `commit_message_faults`: no remote named `origin`"
                    .to_string()
            )]
        );
    }

    #[test]
    fn a_passing_measure_with_no_number_is_a_gate_that_could_not_measure() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [{"measure": "sparse", "verdict": "passed"}]}"#,
        );
        let why = counted(&check).unwrap_err();
        assert!(why.contains("sparse"), "{why}");
        assert!(why.contains("no number"), "{why}");
    }
}
