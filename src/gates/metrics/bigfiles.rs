//! The file-size ratchet: a Rust file over the budget may shrink but never grow, and no other file
//! may cross it.

use crate::gates::metrics::prodlines;
use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

/// Production lines a file may hold before it is listed and held to its own count.
pub const BUDGET: usize = 1000;

pub const GATE: Gate = Gate {
    name: "bigfiles",
    about: "a Rust file over 1000 production lines grew, or a new one crossed the budget",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "production lines",
    },
};

fn measure(ctx: &Ctx) -> Result<Series, String> {
    over_budget(ctx, BUDGET)
}

/// The files over the budget, each with its count; a file under it is left out so it may grow.
fn over_budget(ctx: &Ctx, budget: usize) -> Result<Series, String> {
    let mut series = Series::new();
    for (path, lines) in prodlines::measure(ctx)? {
        if lines > budget {
            let count = u64::try_from(lines).unwrap_or(u64::MAX);
            series.set(&project::relative(&ctx.root, &path), count);
        }
    }
    Ok(series)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn tree(name: &str, files: &[(&str, &str)]) -> crate::testdir::Held {
        let root = crate::testdir::make(name);
        for (rel, body) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let held = Ctx::for_root(
            root.to_path_buf(),
            crate::run::baseline::Baseline::empty("0.1.0"),
        );
        crate::testdir::Held::new(root, held)
    }

    /// Three production lines: two `fn` lines and the empty line a trailing newline leaves.
    fn two_functions(name: &str) -> crate::testdir::Held {
        tree(name, &[("src/lib.rs", "fn a() {}\nfn b() {}\n")])
    }

    #[test]
    fn a_file_under_the_budget_is_absent_from_the_series() {
        let root = two_functions("bigfiles-under");
        assert_eq!(over_budget(&root, 5).unwrap(), Series::new());
    }

    #[test]
    fn a_file_exactly_at_the_budget_is_not_over_it() {
        let root = two_functions("bigfiles-exact");
        assert_eq!(over_budget(&root, 3).unwrap(), Series::new());
    }

    #[test]
    fn a_file_over_the_budget_is_recorded_with_its_own_count() {
        let root = two_functions("bigfiles-over");
        assert_eq!(over_budget(&root, 2).unwrap().get("src/lib.rs"), Some(3));
    }

    #[test]
    fn a_key_is_the_path_relative_to_the_root_with_forward_slashes() {
        let root = tree(
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
    fn what_is_recorded_is_the_production_count_and_not_the_length_of_the_file() {
        let root = tree(
            "bigfiles-production",
            &[(
                "src/lib.rs",
                "fn a() {}\n#[cfg(test)]\nmod tests {\nfn t() {}\nfn u() {}\nfn v() {}\n}\n",
            )],
        );
        assert_eq!(over_budget(&root, 1).unwrap().get("src/lib.rs"), Some(2));
    }

    #[test]
    fn a_file_gated_test_only_from_another_file_is_not_measured_at_all() {
        let root = tree(
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
    fn a_file_the_parser_rejects_stops_the_gate_rather_than_scoring_it_zero() {
        // The manifest matters: only a file some crate compiles stops the gate.
        let root = tree(
            "bigfiles-unparsable",
            &[("Cargo.toml", ""), ("src/lib.rs", "fn broken( {\n")],
        );
        let err = over_budget(&root, 0).unwrap_err();
        assert!(err.starts_with("src/lib.rs: line 1:"), "{err}");
    }

    #[test]
    fn the_budget_the_gate_runs_with_is_a_thousand_production_lines() {
        let over = "fn f() {}\n".repeat(1000);
        let under = "fn g() {}\n".repeat(999);
        let root = tree(
            "bigfiles-budget",
            &[
                ("src/big.rs", over.as_str()),
                ("src/small.rs", under.as_str()),
            ],
        );
        let series = measure(&root).unwrap();
        assert_eq!(series.get("src/big.rs"), Some(1001));
        assert_eq!(series.get("src/small.rs"), None);
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
