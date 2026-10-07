//! The file-size ratchet: a Rust file over the budget may shrink but never grow, and no other file
//! may cross it.

use crate::gates::metrics::splits;
use crate::run::baseline::Keys;
use crate::run::{Ctx, Gate, Group, Kind, Measurement};

/// Production lines a file may hold before it is listed and held to its own count.
pub const BUDGET: usize = 1000;

pub const GATE: Gate = Gate {
    name: "bigfiles",
    about: "a Rust file over 1000 production lines grew, or a new one crossed the budget",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::AnnotatedRatchet {
        measure,
        keys: Keys::Items,
        unit: "production lines",
    },
};

fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    read(ctx, BUDGET)
}

/// The files over the budget, each with its count and the parts that could leave it; a file under
/// it is left out so it may grow.
fn read(ctx: &Ctx, budget: usize) -> Result<Measurement, String> {
    splits::measured(ctx, &|lines| lines > budget, &|file, _| Some(file.lines))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::run::baseline::Series;
    use crate::testdir::Held;

    fn over_budget(ctx: &Ctx, budget: usize) -> Result<Series, String> {
        read(ctx, budget).map(|read| read.series)
    }

    /// Three production lines: two `fn` lines and the empty line a trailing newline leaves.
    fn two_functions(name: &str) -> crate::testdir::Held {
        Held::tree(name, &[("src/lib.rs", "fn a() {}\nfn b() {}\n")])
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_under_the_budget_is_absent_from_the_series() {
        let root = two_functions("bigfiles-under");
        assert_eq!(over_budget(&root, 5).unwrap(), Series::new());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_exactly_at_the_budget_is_not_over_it() {
        let root = two_functions("bigfiles-exact");
        assert_eq!(over_budget(&root, 3).unwrap(), Series::new());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_over_the_budget_is_recorded_with_its_own_count() {
        let root = two_functions("bigfiles-over");
        assert_eq!(over_budget(&root, 2).unwrap().get("src/lib.rs"), Some(3));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_key_is_the_path_relative_to_the_root_with_forward_slashes() {
        let root = Held::tree(
            "bigfiles-key",
            &[("src/deep/inner.rs", "fn a() {}\nfn b() {}\n")],
        );
        let series = over_budget(&root, 1).unwrap();
        assert_eq!(
            series.0.keys().collect::<Vec<_>>(),
            vec!["src/deep/inner.rs"]
        );
        assert_eq!(series.get("src/deep/inner.rs"), Some(3));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn what_is_recorded_is_the_production_count_and_not_the_length_of_the_file() {
        let root = Held::tree(
            "bigfiles-production",
            &[(
                "src/lib.rs",
                "fn a() {}\n#[cfg(test)]\nmod tests {\nfn t() {}\nfn u() {}\nfn v() {}\n}\n",
            )],
        );
        assert_eq!(over_budget(&root, 1).unwrap().get("src/lib.rs"), Some(2));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_gated_test_only_from_another_file_is_not_measured_at_all() {
        let root = Held::tree(
            "bigfiles-gated",
            &[
                ("src/lib.rs", "#[cfg(test)]\nmod fixtures;\n"),
                ("src/fixtures.rs", "fn f() {}\nfn g() {}\nfn h() {}\n"),
            ],
        );
        let series = over_budget(&root, 0).unwrap();
        assert_eq!(series.get("src/lib.rs"), Some(1));
        assert_eq!(series.get("src/fixtures.rs"), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_the_parser_rejects_stops_the_gate_rather_than_scoring_it_zero() {
        let root = Held::tree("bigfiles-unparsable", &crate::testdir::UNPARSABLE);
        let err = over_budget(&root, 0).unwrap_err();
        assert!(err.starts_with("src/lib.rs: line 1:"), "{err}");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_budget_the_gate_runs_with_is_a_thousand_production_lines() {
        // Blank lines count and parse at no cost: a thousand lines of code took minutes under Miri.
        let over = format!("fn f() {{}}\n{}", "\n".repeat(999));
        let under = format!("fn g() {{}}\n{}", "\n".repeat(998));
        let root = Held::tree(
            "bigfiles-budget",
            &[
                ("src/big.rs", over.as_str()),
                ("src/small.rs", under.as_str()),
            ],
        );
        let series = measure(&root).unwrap().series;
        assert_eq!(series.get("src/big.rs"), Some(1001));
        assert_eq!(series.get("src/small.rs"), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_over_the_budget_names_the_part_that_could_leave_it() {
        // As in the budget test above: blank lines count the same and parse at no cost under Miri.
        let body = format!("    let _ = 0;\n{}", "\n".repeat(149));
        let src = format!(
            "pub fn entry() {{\n    a();\n    other();\n}}\nfn a() {{\n    a1();\n}}\n\
             fn a1() {{\n{body}}}\nfn other() {{\n{body}}}\n"
        );
        let root = Held::tree("bigfiles-part", &[("src/big.rs", src.as_str())]);
        let mut read = read(&root, 100).unwrap();
        let told = read.details.remove("src/big.rs").unwrap();
        assert_eq!(
            told.fix.as_deref(),
            Some("move `a` with the items only it uses (2 items, 155 lines) to `src/big/a.rs`")
        );
    }

    #[test]
    fn the_gate_is_a_ratchet_over_items_counted_in_production_lines() {
        assert_eq!(GATE.name, "bigfiles");
        assert_eq!(crate::run::rerun(GATE.name), "chock run bigfiles");
        assert_eq!(GATE.group, Group::Quality);
        match GATE.kind {
            Kind::Ratchet { keys, unit, .. } | Kind::AnnotatedRatchet { keys, unit, .. } => {
                assert_eq!(keys, Keys::Items);
                assert_eq!(unit, "production lines");
            }
            Kind::Binary(_) | Kind::Debt { .. } => {
                panic!("bigfiles reports a number, not a verdict")
            }
        }
    }
}
