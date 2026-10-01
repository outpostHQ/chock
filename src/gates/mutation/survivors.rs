//! Reads mutest's totals and splits its surviving mutants into those that point at a missing test
//! and quiet ones that often no test could kill.

use std::collections::BTreeMap;

use crate::exec::strip_colour;
use crate::run::baseline::Series;
use crate::run::report::Finding;

/// What an undetected mutation says about the suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// It changed a decision and no test noticed, so there is a test to write.
    Telling,
    /// It removed or defaulted a value nothing observes; often the mutant behaves identically.
    Quiet,
}

/// Operators that change which way the code branches, so a survivor marks an untested boundary,
/// loop or match arm.
const TELLING: [&str; 11] = [
    "relational_op_eq_swap",
    "relational_op_invert",
    "eq_op_invert",
    "continue_break_swap",
    "range_limit_swap",
    "match_arm_delete",
    "match_guard_value",
    "logical_op_and_or_swap",
    "bool_expr_negate",
    "math_op_add_mul_swap",
    "unary_op_delete",
];

#[must_use]
pub fn signal_of(operator: &str) -> Signal {
    if TELLING.contains(&operator) {
        Signal::Telling
    } else {
        Signal::Quiet
    }
}

/// One mutation the suite let through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Survivor {
    pub operator: String,
    pub file: String,
    pub line: u32,
    pub what: String,
}

impl Survivor {
    #[must_use]
    pub fn signal(&self) -> Signal {
        signal_of(&self.operator)
    }

    /// The baseline key: file and operator, not line, since a line moves with every edit above it.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}#{}", self.file, self.operator)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Totals {
    pub undetected: u64,
    pub timed_out: u64,
    pub total: u64,
}

impl Totals {
    fn add(self, next: Self) -> Option<Self> {
        Some(Self {
            undetected: self.undetected.checked_add(next.undetected)?,
            timed_out: self.timed_out.checked_add(next.timed_out)?,
            total: self.total.checked_add(next.total)?,
        })
    }
}

/// Sums mutest's `mutations:` rows; the `safe`/`unsafe` rows repeat them. `None` when there are
/// no totals, an error when they are malformed.
pub(super) fn totals(output: &str) -> Result<Option<Totals>, String> {
    let mut found: Option<Totals> = None;
    for line in output.lines().map(strip_colour) {
        let Some(rest) = line.trim().strip_prefix("mutations:") else {
            continue;
        };
        let next = summary(rest.trim()).ok_or_else(|| {
            "mutest reported unreadable or inconsistent mutation totals".to_string()
        })?;
        found = Some(
            found
                .unwrap_or_default()
                .add(next)
                .ok_or("mutest mutation totals overflowed")?,
        );
    }
    Ok(found)
}

fn summary(line: &str) -> Option<Totals> {
    let (score, rest) = line.split_once(". ")?;
    let (detected, rest) = rest.split_once(" detected")?;
    let detected = number(detected)?;
    let (timed_out, crashed, rest) = unresolved(rest)?;
    let rest = rest.strip_prefix("; ")?;
    let (undetected, total) = rest.split_once(" undetected; ")?;
    let undetected = number(undetected)?;
    let total = number(total.strip_suffix(" total")?)?;
    let resolved = detected.checked_add(undetected)?;
    let counted = resolved.checked_add(timed_out)?.checked_add(crashed)?;
    (counted == total && valid_score(score, resolved)).then_some(Totals {
        undetected,
        timed_out,
        total,
    })
}

fn unresolved(rest: &str) -> Option<(u64, u64, &str)> {
    let Some(notes) = rest.strip_prefix(" (") else {
        return Some((0, 0, rest));
    };
    let (timed_out, notes) = notes.split_once(" timed out; ")?;
    let (crashed, rest) = notes.split_once(" crashed)")?;
    Some((number(timed_out)?, number(crashed)?, rest))
}

fn number(text: &str) -> Option<u64> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

fn valid_score(score: &str, resolved: u64) -> bool {
    if score == "none" {
        return resolved == 0;
    }
    let Some(number) = score.strip_suffix('%') else {
        return false;
    };
    number
        .parse::<f64>()
        .is_ok_and(|n| n.is_finite() && (0.0..=100.0).contains(&n))
}

/// Telling survivors, counted per file and operator. Quiet ones get no key, so a new one never
/// fails a build.
#[must_use]
pub fn telling(survivors: &[Survivor]) -> BTreeMap<String, u64> {
    let mut counted = BTreeMap::new();
    for survivor in survivors.iter().filter(|s| s.signal() == Signal::Telling) {
        *counted.entry(survivor.key()).or_insert(0) += 1;
    }
    counted
}

/// Each telling survivor under a key whose count rose past its record, so a rise names the
/// mutations a test has to catch.
#[must_use]
pub fn sites_over(survivors: &[Survivor], was: &Series) -> Vec<Finding> {
    let now = telling(survivors);
    survivors
        .iter()
        .filter(|survivor| {
            let key = survivor.key();
            now.get(&key)
                .is_some_and(|count| *count > was.get(&key).unwrap_or(0))
        })
        .map(|survivor| {
            let said = format!("no test caught: {} ({})", survivor.what, survivor.operator);
            Finding::at(&survivor.file, &said).line(survivor.line)
        })
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn mutation_counts_accept_digits_only_without_signs() {
        assert_eq!(number("0"), Some(0));
        assert_eq!(number("42"), Some(42));
        for invalid in ["", "+1", "-1", " 1", "1 ", "1a"] {
            assert_eq!(number(invalid), None, "{invalid}");
        }
    }

    const SUMMARY: &str = "mutations: 90.77%. 2243 detected (19 timed out; 0 crashed); 228 undetected; 2490 total\n\
                           safe: 90.77%. 2243 detected (19 timed out; 0 crashed); 228 undetected; 2490 total\n\
                         unsafe: none. 0 detected (0 timed out; 0 crashed); 0 undetected; 0 total\n";

    #[test]
    fn timeouts_and_crashes_are_exclusive_of_detected_mutations() {
        assert_eq!(
            totals(SUMMARY).unwrap(),
            Some(Totals {
                undetected: 228,
                timed_out: 19,
                total: 2490
            })
        );
        let crashed =
            "mutations: 80.00%. 4 detected (0 timed out; 1 crashed); 1 undetected; 6 total";
        assert_eq!(
            totals(crashed).unwrap(),
            Some(Totals {
                undetected: 1,
                timed_out: 0,
                total: 6
            })
        );
    }

    #[test]
    fn only_overall_rows_are_summed_across_targets() {
        assert_eq!(
            totals(&format!("{SUMMARY}{SUMMARY}")).unwrap(),
            Some(Totals {
                undetected: 456,
                timed_out: 38,
                total: 4980
            })
        );
    }

    #[test]
    fn an_explicit_zero_is_distinct_from_no_totals() {
        assert_eq!(
            totals("mutations: none. 0 detected (0 timed out; 0 crashed); 0 undetected; 0 total")
                .unwrap(),
            Some(Totals::default())
        );
        assert_eq!(totals("ran 2 out of 10 tests\n").unwrap(), None);
        assert_eq!(
            totals("mutations: 100%. 10 detected; 0 undetected; 10 total").unwrap(),
            Some(Totals {
                undetected: 0,
                timed_out: 0,
                total: 10
            })
        );
    }

    #[test]
    fn malformed_or_inconsistent_totals_are_not_a_clean_measurement() {
        for row in [
            "mutations:",
            "mutations: counts unavailable",
            "mutations: 100%. 10 detected; 1 undetected; 10 total",
            "mutations: 100%. 10 detected (1 timed out; 0 crashed); 0 undetected; 10 total",
            "mutations: 100%. 0 detected; x undetected; 0 total",
            "mutations: 100%. 0 detected; 0 undetected; 0 total trailing",
            "mutations: 100%. -1 detected; 0 undetected; 0 total",
            "mutations: none. 1 detected; 0 undetected; 1 total",
            "mutations: NaN%. 0 detected; 0 undetected; 0 total",
            "mutations: 100. 0 detected; 0 undetected; 0 total",
            "mutations: 101%. 0 detected; 0 undetected; 0 total",
            "mutations: 100%. 18446744073709551615 detected; 1 undetected; 0 total",
        ] {
            assert!(totals(row).is_err(), "{row}");
        }
    }

    #[test]
    fn an_overflow_across_complete_target_totals_is_refused() {
        let row = "mutations: 100%. 18446744073709551615 detected; 0 undetected; 18446744073709551615 total\n";
        assert_eq!(
            totals(&format!("{row}{row}")),
            Err("mutest mutation totals overflowed".to_string())
        );
    }

    #[test]
    fn colour_is_ignored_without_ignoring_a_malformed_second_summary() {
        let row = "\u{1b}[32mmutations:\u{1b}[0m 100%. 10 detected; 0 undetected; 10 total";
        assert_eq!(
            totals(row).unwrap(),
            Some(Totals {
                undetected: 0,
                timed_out: 0,
                total: 10
            })
        );
        assert!(totals(&format!("{row}\nmutations: missing counts")).is_err());
    }

    fn survivor(operator: &str, line: u32) -> Survivor {
        Survivor {
            operator: operator.to_string(),
            file: "src/a.rs".to_string(),
            line,
            what: "does a thing".to_string(),
        }
    }

    #[test]
    fn a_boundary_operator_is_worth_chasing_and_a_deleted_call_is_not() {
        assert_eq!(signal_of("relational_op_eq_swap"), Signal::Telling);
        assert_eq!(signal_of("continue_break_swap"), Signal::Telling);
        assert_eq!(signal_of("call_delete"), Signal::Quiet);
        assert_eq!(signal_of("call_value_default_shadow"), Signal::Quiet);
    }

    #[test]
    fn an_operator_nobody_has_classified_is_quiet_rather_than_a_failure() {
        assert_eq!(signal_of("some_operator_added_next_year"), Signal::Quiet);
    }

    #[test]
    fn a_survivor_is_keyed_by_file_and_operator_so_an_edit_above_it_is_not_new_debt() {
        let survivor = Survivor {
            operator: "eq_op_invert".to_string(),
            file: "src/a.rs".to_string(),
            line: 42,
            what: String::new(),
        };
        assert_eq!(survivor.key(), "src/a.rs#eq_op_invert");
    }

    #[test]
    fn telling_survivors_are_counted_per_file_and_operator() {
        let found = [
            survivor("continue_break_swap", 1),
            survivor("continue_break_swap", 9),
            survivor("eq_op_invert", 3),
        ];
        let counted = telling(&found);
        assert_eq!(counted.get("src/a.rs#continue_break_swap"), Some(&2));
        assert_eq!(counted.get("src/a.rs#eq_op_invert"), Some(&1));
    }

    #[test]
    fn a_quiet_survivor_is_read_but_never_keyed() {
        let found = [survivor("call_delete", 1)];
        assert_eq!(found[0].signal(), Signal::Quiet);
        assert_eq!(telling(&found), BTreeMap::new());
    }

    #[test]
    fn a_run_that_detected_everything_yields_nothing_to_chase() {
        assert_eq!(telling(&[]), BTreeMap::new());
    }

    #[test]
    fn a_count_past_its_record_names_each_telling_survivor_and_one_at_it_names_none() {
        let found = [
            survivor("eq_op_invert", 3),
            survivor("eq_op_invert", 7),
            survivor("continue_break_swap", 5),
            survivor("call_delete", 9),
        ];
        let was = Series(BTreeMap::from([
            ("src/a.rs#eq_op_invert".to_string(), 1),
            ("src/a.rs#continue_break_swap".to_string(), 1),
        ]));
        let at = |operator: &str, line: u32| {
            let said = format!("no test caught: does a thing ({operator})");
            Finding::at("src/a.rs", &said).line(line)
        };
        let twice = [at("eq_op_invert", 3), at("eq_op_invert", 7)];
        assert_eq!(sites_over(&found, &was), twice);
        assert_eq!(
            sites_over(&found, &Series::default()),
            [
                twice[0].clone(),
                twice[1].clone(),
                at("continue_break_swap", 5)
            ]
        );
    }
}
