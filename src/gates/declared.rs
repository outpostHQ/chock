//! Gates that run the checks a project declares for itself under `commands` in its config.

use std::collections::BTreeMap;

use crate::exec;
use crate::project::config::Command;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Inspection, Kind};

/// The declared commands that need no compiler, so a commit waits for them.
pub const GATE: Gate = Gate {
    name: "commands",
    about: "the project's own declared checks that need no compiler",
    group: Group::OptIn,
    builds: false,
    reads: None,
    kind: Kind::Debt {
        inspect: quick,
        unit: "declared finding(s)",
    },
};

/// The declared commands that compile, so the push runs them rather than every commit.
pub const BUILDS: Gate = Gate {
    name: "commands-build",
    about: "the project's own declared checks that need a compiler",
    group: Group::OptIn,
    builds: true,
    reads: None,
    kind: Kind::Debt {
        inspect: building,
        unit: "declared finding(s)",
    },
};

fn quick(ctx: &Ctx) -> Result<Inspection, String> {
    inspect(ctx, false)
}

fn building(ctx: &Ctx) -> Result<Inspection, String> {
    inspect(ctx, true)
}

fn inspect(ctx: &Ctx, builds: bool) -> Result<Inspection, String> {
    let chosen: Vec<&Command> = ctx
        .commands
        .iter()
        .filter(|command| command.builds == builds)
        .collect();
    if chosen.is_empty() {
        return Err(format!(
            "no command with `builds: {builds}` is declared under `commands` in {}",
            crate::project::config::FILE
        ));
    }
    let mut found = Inspection::default();
    for command in chosen {
        let Some((program, args)) = command.run.split_first() else {
            return Err(format!("`{}` names no command to run", command.name));
        };
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        judge(command, &exec::tool(&ctx.root, program, &args)?, &mut found)?;
    }
    Ok(found)
}

/// Each count becomes that many findings, so a printed `u64::MAX` would exhaust memory.
const MOST_COUNTED: u64 = 100_000;

/// A failed pass/fail command blocks outright. A counting one must exit zero and print one JSON
/// object of counts; anything else is an error, never a count of zero.
fn judge(command: &Command, out: &exec::Output, found: &mut Inspection) -> Result<(), String> {
    if out.truncated {
        return Err(format!(
            "`{}` said more than chock reads, so any answer would be partial",
            command.name
        ));
    }
    if !command.counts {
        if !out.success() {
            let why = format!("failed: {}", out.why_it_failed());
            found
                .blockers
                .push(Finding::at("", &why).item(&command.name));
        }
        return Ok(());
    }
    if !out.success() {
        return Err(format!(
            "`{}` did not count: {}",
            command.name,
            out.why_it_failed()
        ));
    }
    let counts: BTreeMap<String, u64> = serde_json::from_str(out.stdout.trim())
        .map_err(|e| format!("`{}` printed no object of counts: {e}", command.name))?;
    let total = counts
        .values()
        .try_fold(0_u64, |sum, count| sum.checked_add(*count));
    if !total.is_some_and(|total| total <= MOST_COUNTED) {
        return Err(format!(
            "`{}` counted more than the {MOST_COUNTED} findings chock holds in memory",
            command.name
        ));
    }
    let said = format!("counted by `{}`", command.name);
    for (key, count) in counts {
        found
            .debt
            .extend((0..count).map(|_| Finding::at(&command.name, &said).item(&key)));
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn declared(name: &str, run: &[&str], counts: bool, builds: bool) -> Command {
        Command {
            name: name.to_string(),
            run: run.iter().map(|arg| (*arg).to_string()).collect(),
            counts,
            builds,
        }
    }

    fn said(code: i32, stdout: &str) -> exec::Output {
        exec::Output {
            code: Some(code),
            stdout: stdout.to_string(),
            stderr: String::new(),
            truncated: false,
        }
    }

    fn judged(command: &Command, out: &exec::Output) -> Result<Inspection, String> {
        let mut found = Inspection::default();
        judge(command, out, &mut found).map(|()| found)
    }

    fn said_by(findings: &[Finding]) -> Vec<(String, String)> {
        findings
            .iter()
            .map(|finding| {
                (
                    finding.item.clone().unwrap_or_default(),
                    finding.message.clone(),
                )
            })
            .collect()
    }

    /// How many findings each `file#item` key holds: what the ratchet will compare.
    fn tallied(findings: &[Finding]) -> BTreeMap<String, usize> {
        let mut keys = BTreeMap::<String, usize>::new();
        for finding in findings {
            let item = finding.item.clone().unwrap_or_default();
            *keys.entry(format!("{}#{item}", finding.file)).or_default() += 1;
        }
        keys
    }

    #[test]
    fn a_pass_fail_command_blocks_only_when_it_fails() {
        let check = declared("shipped-config", &["true"], false, true);
        assert_eq!(judged(&check, &said(0, "")), Ok(Inspection::default()));
        let broken = said(101, "error[E0425]: not found");
        let failed = judged(&check, &broken).unwrap();
        assert_eq!(
            said_by(&failed.blockers),
            [(
                "shipped-config".to_string(),
                format!("failed: {}", broken.why_it_failed())
            )]
        );
        assert_eq!(failed.debt, Vec::new());
    }

    #[test]
    fn a_counting_command_charges_each_key_its_count() {
        let docs = declared("doc-check", &["true"], true, false);
        let found = judged(&docs, &said(0, "{\"stale\": 2, \"absent\": 1}\n")).unwrap();
        assert_eq!(
            tallied(&found.debt),
            BTreeMap::from([
                ("doc-check#absent".to_string(), 1),
                ("doc-check#stale".to_string(), 2)
            ])
        );
        assert_eq!(found.blockers, Vec::new());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri did not end this test in 19 minutes")]
    fn a_count_that_is_not_an_answer_refuses_rather_than_reading_as_zero() {
        let docs = declared("doc-check", &["true"], true, false);
        assert_eq!(
            judged(&docs, &said(0, "all good")).unwrap_err(),
            "`doc-check` printed no object of counts: expected value at line 1 column 1"
        );
        assert!(
            judged(&docs, &said(2, "{}"))
                .unwrap_err()
                .starts_with("`doc-check` did not count: ")
        );
        let cut = exec::Output {
            truncated: true,
            ..said(0, "{}")
        };
        assert_eq!(
            judged(&docs, &cut).unwrap_err(),
            "`doc-check` said more than chock reads, so any answer would be partial"
        );
        let refused = "`doc-check` counted more than the 100000 findings chock holds in memory";
        for printed in [
            "{\"a\": 100001}".to_string(),
            format!("{{\"a\": {}, \"b\": 1}}", u64::MAX),
        ] {
            assert_eq!(
                judged(&docs, &said(0, &printed)).unwrap_err(),
                refused,
                "{printed}"
            );
        }
        let at_the_bound = judged(&docs, &said(0, "{\"a\": 99999, \"b\": 1}")).unwrap();
        assert_eq!(
            tallied(&at_the_bound.debt),
            BTreeMap::from([
                ("doc-check#a".to_string(), 99_999),
                ("doc-check#b".to_string(), 1)
            ])
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn each_gate_runs_only_its_own_half_and_refuses_when_it_has_none() {
        let dir = crate::testdir::make("declared-commands");
        let ctx = Ctx {
            commands: vec![
                declared("quick", &["sh", "-c", "exit 0"], false, false),
                declared("slow", &["sh", "-c", "exit 3"], false, true),
                declared(
                    "counted",
                    &["sh", "-c", "echo '{\"warn\": 1}'"],
                    true,
                    false,
                ),
            ],
            ..Ctx::for_root(
                dir.to_path_buf(),
                crate::run::baseline::Baseline::empty("0.1.0"),
            )
        };
        let quick = quick(&ctx).unwrap();
        assert_eq!(quick.blockers, Vec::new());
        assert_eq!(
            said_by(&quick.debt),
            [("warn".to_string(), "counted by `counted`".to_string())]
        );
        let slow = building(&ctx).unwrap();
        assert_eq!(
            slow.blockers
                .iter()
                .map(|finding| finding.item.clone())
                .collect::<Vec<_>>(),
            [Some("slow".to_string())]
        );
        let none = Ctx::for_root(
            dir.to_path_buf(),
            crate::run::baseline::Baseline::empty("0.1.0"),
        );
        assert_eq!(
            quick_or_reason(&none),
            "no command with `builds: false` is declared under `commands` in .chock/config.json"
        );
        let empty = Ctx {
            commands: vec![declared("nothing", &[], false, false)],
            ..Ctx::for_root(
                dir.to_path_buf(),
                crate::run::baseline::Baseline::empty("0.1.0"),
            )
        };
        assert_eq!(quick_or_reason(&empty), "`nothing` names no command to run");
    }

    fn quick_or_reason(ctx: &Ctx) -> String {
        quick(ctx)
            .map(|found| format!("{found:?}"))
            .unwrap_or_else(|why| why)
    }
}
