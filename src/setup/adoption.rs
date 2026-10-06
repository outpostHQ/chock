//! Decides which checks `init` turns on from their first reports. A check that could not be
//! measured stays on. Nothing here runs a tool or writes a file.

use std::fmt::Write as _;

use crate::project::config::Config;
use crate::run::report::Verdict;
use crate::run::{self, Group};

/// What `init` decided about one gate, with the detail it prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Already green, so it is on from the start.
    On(String),
    /// Off until asked for by name: too slow, or needs a second toolchain.
    AskedFor,
    /// An instrument that reports a number rather than gating.
    Reports,
    /// Not green yet, so left off with what it found.
    Found(String),
    /// A core check that failed; it stays on.
    Failing(String),
    /// Could not be measured here; it stays on so later runs keep reporting that.
    Unmeasurable(String),
}

impl Decision {
    #[must_use]
    pub fn enables(&self) -> bool {
        matches!(self, Self::On(_) | Self::Failing(_) | Self::Unmeasurable(_))
    }

    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::On(d) | Self::Found(d) | Self::Failing(d) | Self::Unmeasurable(d) => d,
            Self::AskedFor => "opt-in",
            Self::Reports => "reports a number; it never trips",
        }
    }

    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::On(_) | Self::Failing(_) => "on",
            Self::AskedFor | Self::Reports => "off",
            Self::Found(_) => "FOUND",
            Self::Unmeasurable(_) => "on, error",
        }
    }
}

/// The decision for a gate's first report. A failed core check or a tool failure keeps it on.
pub(crate) fn adoption(gate: &run::Gate, report: &crate::run::report::GateReport) -> Decision {
    match report.verdict {
        Verdict::Pass => Decision::On("already green".to_string()),
        Verdict::Tripped if gate.group == Group::Gates => Decision::Failing(format!(
            "a required gate, and it trips today: {}",
            found(report.findings.len())
        )),
        Verdict::Tripped => Decision::Found(found(report.findings.len())),
        Verdict::CannotRun => Decision::Unmeasurable(
            report
                .cannot_run_reason
                .clone()
                .unwrap_or_else(|| "no reason given".to_string()),
        ),
    }
}

fn found(count: usize) -> String {
    match count {
        0 => "fails, with nothing to point at".to_string(),
        1 => "1 finding".to_string(),
        n => format!("{n} findings"),
    }
}

/// A line per gate, one line for all the opt-in gates, then a note on any that is on but failing.
#[must_use]
pub fn render_decisions(rows: &[(&str, Decision)]) -> String {
    let mut out = String::new();
    for (name, decision) in rows.iter().filter(|(_, d)| *d != Decision::AskedFor) {
        let _ = writeln!(
            out,
            "  {:<9} {:<12} {}",
            decision.label(),
            name,
            decision.detail()
        );
    }
    out.push_str(&opt_in(rows));
    out.push_str(&attention(rows));
    out
}

/// The opt-in gates on one line: each is still named, and all are off for the one reason.
fn opt_in(rows: &[(&str, Decision)]) -> String {
    let names: Vec<&str> = rows
        .iter()
        .filter(|(_, decision)| *decision == Decision::AskedFor)
        .map(|(name, _)| *name)
        .collect();
    if names.is_empty() {
        return String::new();
    }
    format!(
        "  {:<9} {}, each slow or in need of its own tool: {}. `chock enable GATE` switches one on.\n",
        Decision::AskedFor.label(),
        crate::run::report::plural(names.len(), "opt-in gate"),
        names.join(", ")
    )
}

/// The note naming gates that are on but failing or unmeasured, or nothing.
fn attention(rows: &[(&str, Decision)]) -> String {
    let blocked: Vec<&str> = rows
        .iter()
        .filter(|(_, decision)| {
            matches!(decision, Decision::Failing(_) | Decision::Unmeasurable(_))
        })
        .map(|(name, _)| *name)
        .collect();
    if blocked.is_empty() {
        return String::new();
    }
    format!(
        "On does not mean passed. These gates stay on and need attention: {}. \
         `chock run {}` shows what each one needs.\n",
        blocked.join(", "),
        blocked.join(" ")
    )
}

/// The config `init` saves: every check its decision enables, and each `Found` one recorded as
/// left off with what it found.
pub(crate) fn selected(rows: &[(&str, Decision)]) -> Config {
    let mut config = Config::of(
        rows.iter()
            .filter(|(_, decision)| decision.enables())
            .map(|(name, _)| *name),
    );
    for (name, decision) in rows {
        if let Decision::Found(found) = decision {
            config
                .left_off
                .insert((*name).to_string(), format!("init found {found}"));
        }
    }
    config
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::gates;

    #[test]
    fn required_and_unmeasurable_checks_stay_enabled_without_claiming_a_pass() {
        assert!(Decision::On("already green".to_string()).enables());
        assert!(!Decision::Found("47 findings".to_string()).enables());
        assert!(Decision::Unmeasurable("no lcov".to_string()).enables());
        assert!(
            Decision::Failing("a required gate, and it trips today: 1 finding".to_string())
                .enables()
        );
        assert!(!Decision::AskedFor.enables());
        assert!(!Decision::Reports.enables());
    }

    #[test]
    fn a_missing_tool_cannot_remove_any_core_check_from_the_saved_selection() {
        for gate in gates::registry()
            .iter()
            .filter(|gate| gate.group == Group::Gates)
        {
            let rerun = run::rerun(gate.name);
            let report =
                crate::run::report::GateReport::cannot_run(gate.name, &rerun, "tool missing");
            let decision = adoption(gate, &report);
            assert_eq!(decision, Decision::Unmeasurable("tool missing".to_string()));
            let config = selected(&[(gate.name, decision)]);
            let saved =
                crate::project::document::parse::<Config>(&config.render(), "config.json").unwrap();
            assert_eq!(
                saved.enabled,
                std::collections::BTreeSet::from([gate.name.to_string()])
            );
        }
    }

    #[test]
    fn a_failing_core_check_keeps_its_findings_and_remains_enabled() {
        for gate in gates::registry()
            .iter()
            .filter(|gate| gate.group == Group::Gates)
        {
            let mut report = crate::run::report::GateReport::new(
                gate.name,
                Verdict::Tripped,
                &run::rerun(gate.name),
            );
            report.findings = vec![crate::run::report::Finding::at(
                "src/lib.rs",
                "this check failed",
            )];
            let decision = adoption(gate, &report);
            assert_eq!(
                decision,
                Decision::Failing("a required gate, and it trips today: 1 finding".to_string())
            );
            let printed = render_decisions(&[(gate.name, decision.clone())]);
            assert!(
                printed.contains("a required gate, and it trips today:"),
                "{printed}"
            );
            assert!(printed.contains(&run::rerun(gate.name)), "{printed}");
            assert!(selected(&[(gate.name, decision)]).is_on(gate.name));
        }
    }

    #[test]
    fn a_check_left_off_for_its_debt_says_so_in_the_config_and_enabling_it_clears_that() {
        let rows = [
            ("slop", Decision::Found("3 findings".to_string())),
            ("lint", Decision::On("already green".to_string())),
            ("mutest", Decision::AskedFor),
        ];
        let mut config = selected(&rows);
        assert_eq!(
            config.left_off,
            std::collections::BTreeMap::from([(
                "slop".to_string(),
                "init found 3 findings".to_string()
            )])
        );
        assert_eq!(config.undecided(&["slop", "lint", "typos"]), ["typos"]);
        config.enable("slop");
        assert_eq!(config.left_off, std::collections::BTreeMap::new());
        assert!(config.is_on("slop"));
    }

    #[test]
    fn measured_quality_debt_and_an_unmeasurable_quality_check_are_not_the_same_decision() {
        let gate = &gates::cargo::features::GATE;
        let mut report =
            crate::run::report::GateReport::new(gate.name, Verdict::Tripped, "chock run features");
        report.findings = vec![crate::run::report::Finding::at(
            "Cargo.toml",
            "unused feature",
        )];
        assert_eq!(
            adoption(gate, &report),
            Decision::Found("1 finding".to_string())
        );
        let refused = crate::run::report::GateReport::cannot_run(
            gate.name,
            "chock run features",
            "metadata failed",
        );
        assert!(adoption(gate, &refused).enables());
        report.verdict = Verdict::Pass;
        assert_eq!(
            adoption(gate, &report),
            Decision::On("already green".to_string())
        );
    }

    #[test]
    fn a_refusal_without_a_reason_still_retains_the_check() {
        let gate = &gates::tools::TEST;
        let report =
            crate::run::report::GateReport::new(gate.name, Verdict::CannotRun, "chock run test");
        assert_eq!(
            adoption(gate, &report),
            Decision::Unmeasurable("no reason given".to_string())
        );
    }

    #[test]
    fn retained_failures_are_not_mixed_with_explicit_opt_in_choices() {
        let rows = [
            (
                "test",
                Decision::Failing("a required gate, and it trips today: 1 finding".to_string()),
            ),
            ("lint", Decision::Unmeasurable("missing clippy".to_string())),
            ("mutest", Decision::AskedFor),
            ("bsize", Decision::Reports),
        ];
        assert_eq!(
            selected(&rows).enabled,
            std::collections::BTreeSet::from(["lint".to_string(), "test".to_string()])
        );
        let report = render_decisions(&rows);
        assert!(report.contains("`chock run test lint` shows"), "{report}");
    }

    #[test]
    fn a_gate_that_is_not_green_is_reported_with_what_it_found() {
        let rows = [("clippy", Decision::Found(found(47)))];
        assert_eq!(
            render_decisions(&rows),
            "  FOUND     clippy       47 findings\n"
        );
    }

    #[test]
    fn a_single_finding_is_not_reported_in_the_plural() {
        assert_eq!(found(1), "1 finding");
        assert_eq!(found(2), "2 findings");
    }

    #[test]
    fn a_gate_that_fails_with_nothing_to_point_at_still_says_it_failed() {
        assert_eq!(found(0), "fails, with nothing to point at");
    }

    #[test]
    fn a_gate_that_could_not_be_measured_is_not_reported_as_clean() {
        let rows = [("crap", Decision::Unmeasurable("no lcov.info".to_string()))];
        assert_eq!(
            render_decisions(&rows),
            "  on, error crap         no lcov.info\n\
             On does not mean passed. These gates stay on and need attention: crap. \
             `chock run crap` shows what each one needs.\n"
        );
    }

    #[test]
    fn a_costly_gate_is_named_rather_than_assumed() {
        let rows = [
            ("mutation", Decision::AskedFor),
            ("test", Decision::On("already green".to_string())),
            ("miri", Decision::AskedFor),
        ];
        assert_eq!(
            render_decisions(&rows),
            "  on        test         already green\n  off       2 opt-in gates, each slow or in need of its own tool: mutation, miri. `chock enable GATE` switches one on.\n"
        );
        assert_eq!(Decision::AskedFor.detail(), "opt-in");
    }
}
