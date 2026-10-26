//! Reads mutest's totals and splits its surviving mutants into those that point at a missing test
//! and quiet ones that often no test could kill.

use std::collections::BTreeMap;

use serde::Deserialize;

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
    /// What the mutation changes, from which a proof's copy of the file is rebuilt.
    pub edits: Vec<Edit>,
}

/// One change a mutation makes, as mutest writes it: where it goes, and the code it puts there.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Edit {
    pub location: Location,
    pub substitute: Substitute,
}

/// Where an edit goes: `kind` is `replace`, `insert_before` or `insert_after`, of `span`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Location {
    pub kind: String,
    pub span: Span,
}

/// The code an edit puts in: `kind` is `expr`, `stmt` or `local`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Substitute {
    pub kind: String,
    pub replacement: String,
}

/// A span of a file from the workspace root: its first place and the place just past it, each a
/// line and a column in characters, counted from 1.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Span {
    pub path: String,
    pub begin: (u32, u32),
    pub end: (u32, u32),
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
    pub(super) fn add(self, next: Self) -> Option<Self> {
        Some(Self {
            undetected: self.undetected.checked_add(next.undetected)?,
            timed_out: self.timed_out.checked_add(next.timed_out)?,
            total: self.total.checked_add(next.total)?,
        })
    }
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

    fn survivor(operator: &str, line: u32) -> Survivor {
        Survivor {
            operator: operator.to_string(),
            file: "src/a.rs".to_string(),
            line,
            what: "does a thing".to_string(),
            edits: Vec::new(),
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
            edits: Vec::new(),
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
