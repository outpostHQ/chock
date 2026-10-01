//! Complexity × uncoverage per function, via cargo-crap. Pass or fail, not a ratchet: cargo-crap
//! owns the baseline and the comparison, and `chock baseline` only records the file.

use std::path::Path;

use crate::exec;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

/// Under `.chock/` with chock's other committed state, so it never overwrites a project's own file.
/// Each system keeps its own, as each keeps its own records in `.chock/baseline.json`.
#[must_use]
pub fn baseline() -> String {
    kept_on(std::env::consts::OS)
}

/// Linux keeps the plain name, as every system did before each kept its own.
fn kept_on(system: &str) -> String {
    match system {
        "linux" => ".chock/crap-baseline.json".to_string(),
        _ => format!(".chock/crap-baseline@{system}.json"),
    }
}

/// The lcov file the `coverage` gate writes, named once so both gates read the same file.
pub use super::FILE as COVERAGE;

pub const GATE: Gate = Gate {
    name: "crap",
    about: "complexity x uncoverage per function, against this system's .chock/crap-baseline",
    group: Group::Quality,
    builds: true,
    reads: None,
    kind: Kind::Binary(check),
};

/// Both inputs are in place before cargo-crap runs, since it reads a missing lcov as an empty one.
fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let baseline = baseline();
    missing(&ctx.root, &baseline, "run `chock baseline`")?;
    crate::gates::coverage::ensure(ctx)?;
    let out = exec::run(
        "cargo",
        &[
            "crap",
            "--lcov",
            COVERAGE,
            "--workspace",
            "--baseline",
            &baseline,
            "--fail-regression",
            "--format",
            "json",
        ],
        &ctx.root,
    )
    .map_err(|e| e.to_string())?;
    if out.success() {
        return Ok(Outcome::passed());
    }
    Ok(Outcome::failed(regressions(&out.stdout, &ctx.root)?))
}

/// cargo-crap's JSON report: only the fields this gate reads.
#[derive(serde::Deserialize)]
struct Delta {
    entries: Vec<Scored>,
}

/// What cargo-crap says about one function: only the fields this gate reads.
#[derive(serde::Deserialize)]
struct Scored {
    file: String,
    function: String,
    line: u32,
    crap: f64,
    /// Only `regressed` is reported; cargo-crap does not fail over a `new` function.
    status: String,
    baseline_crap: Option<f64>,
}

fn missing(root: &Path, name: &str, remedy: &str) -> Result<(), String> {
    if root.join(name).is_file() {
        return Ok(());
    }
    Err(format!("no {name} — {remedy}"))
}

/// The regressed functions in cargo-crap's JSON report; none at all is an error.
fn regressions(json: &str, root: &Path) -> Result<Vec<Finding>, String> {
    let delta: Delta = serde_json::from_str(json)
        .map_err(|e| format!("cargo-crap produced a report chock cannot read: {e}"))?;
    let found: Vec<Finding> = delta
        .entries
        .iter()
        .filter(|entry| entry.status == "regressed")
        .map(|entry| {
            let against = match entry.baseline_crap {
                Some(was) => format!("was {was:.1}"),
                None => "not in the baseline".to_string(),
            };
            Finding::at(
                &crate::project::relative(root, Path::new(&entry.file)),
                &format!("CRAP {:.1}, {against}", entry.crap),
            )
            .line(entry.line)
            .item(&entry.function)
        })
        .collect();
    if found.is_empty() {
        return Err(
            "cargo-crap failed but named no regression, so nothing was measured".to_string(),
        );
    }
    Ok(found)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::run::baseline::Baseline;

    fn ctx_in(dir: std::path::PathBuf) -> Ctx {
        Ctx::for_root(dir, Baseline::empty("0.1.0"))
    }

    #[test]
    /// Only a missing baseline stops the gate early; the gate makes coverage itself.
    fn a_tree_with_no_baseline_says_which_step_was_skipped() {
        let dir = crate::testdir::make("crap-no-baseline-file");
        let ctx = ctx_in(dir.to_path_buf());
        assert_eq!(
            check(&ctx),
            Err(format!("no {} — run `chock baseline`", baseline()))
        );
    }

    #[test]
    fn a_tree_with_coverage_but_no_baseline_asks_for_the_baseline() {
        let dir = crate::testdir::make("crap-no-baseline");
        std::fs::write(dir.join(COVERAGE), "TN:\n").unwrap();
        assert_eq!(
            check(&ctx_in(dir.to_path_buf())),
            Err(format!("no {} — run `chock baseline`", baseline()))
        );
    }

    #[test]
    fn each_system_keeps_its_own_baseline_and_linux_keeps_the_plain_name() {
        assert_eq!(kept_on("linux"), ".chock/crap-baseline.json");
        assert_eq!(kept_on("macos"), ".chock/crap-baseline@macos.json");
        assert_eq!(baseline(), kept_on(std::env::consts::OS));
        assert_eq!(GATE.name, "crap");
    }

    const REPORT: &str = r#"{"entries":[
      {"file":"/w/src/a.rs","function":"parse","line":12,"cyclomatic":9.0,"coverage":0.0,
       "crap":90.0,"status":"regressed","baseline_crap":42.0},
      {"file":"/w/src/b.rs","function":"fresh","line":3,"cyclomatic":5.0,"coverage":0.0,
       "crap":30.0,"status":"new","baseline_crap":null},
      {"file":"/w/src/c.rs","function":"steady","line":7,"cyclomatic":2.0,"coverage":100.0,
       "crap":2.0,"status":"unchanged","baseline_crap":2.0}]}"#;

    #[test]
    fn only_a_function_that_got_worse_is_reported_with_both_numbers() {
        let found = regressions(REPORT, Path::new("/w")).unwrap();
        let rendered: Vec<String> = found.iter().map(Finding::render).collect();
        assert_eq!(rendered, ["src/a.rs:12: parse: CRAP 90.0, was 42.0"]);
    }

    #[test]
    fn a_report_that_names_no_regression_is_a_gate_that_could_not_run() {
        let quiet = r#"{"entries":[]}"#;
        assert_eq!(
            regressions(quiet, Path::new("/w")),
            Err("cargo-crap failed but named no regression, so nothing was measured".to_string())
        );
        assert!(
            regressions("not json", Path::new("/w"))
                .unwrap_err()
                .starts_with("cargo-crap produced a report chock cannot read")
        );
    }
}
