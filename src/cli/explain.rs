//! The `list` and `explain` commands: the gates and their state, and the findings of the last run.

use std::process::ExitCode;

use super::{VERSION, cannot_run, configured, emit, root, stage_of, unknown_gate};
use crate::gates;
use crate::project::config::Config;
use crate::run;
use crate::run::report::{Format, LAST_RUN, Run};

pub(super) fn list_gates(format: Format) -> ExitCode {
    // Off a project or with no config, list the default set as on, as a run would.
    match configured() {
        Ok(config) => listed(format, config.as_ref()),
        Err(e) => cannot_run(&e),
    }
}

/// Each gate with its state, group, kind, stage and what it checks.
fn listed(format: Format, config: Option<&Config>) -> ExitCode {
    if format == Format::Json {
        let rows: Vec<serde_json::Value> = gates::registry()
            .iter()
            .map(|gate| {
                serde_json::json!({
                    "gate": gate.name,
                    "about": gate.about,
                    "group": format!("{:?}", gate.group).to_lowercase(),
                    "ratchet": gate.counts_in().is_some(),
                    "enabled": switched_on(gate, config),
                    "stage": stage_of(gate, config).name(),
                    "rerun": run::rerun(gate.name),
                })
            })
            .collect();
        let doc = serde_json::json!({ "chock": VERSION, "gates": rows });
        emit(&format!(
            "{}\n",
            serde_json::to_string_pretty(&doc).unwrap_or_default()
        ));
        return ExitCode::SUCCESS;
    }
    for gate in gates::registry() {
        let kind = if gate.counts_in().is_some() {
            "ratchet"
        } else {
            "pass/fail"
        };
        emit(&format!(
            "  {:<4} {:<12} {:<10} {:<9} {:<6} {}\n",
            if switched_on(gate, config) {
                "on"
            } else {
                "off"
            },
            gate.name,
            format!("{:?}", gate.group).to_lowercase(),
            kind,
            stage_of(gate, config).name(),
            gate.about
        ));
    }
    ExitCode::SUCCESS
}

/// Whether `chock run` would include this gate. Without a config, a run uses the enforced set.
fn switched_on(gate: &run::Gate, config: Option<&Config>) -> bool {
    match config {
        Some(config) => config.is_on(gate.name),
        None => gates::enforced().iter().any(|held| held.name == gate.name),
    }
}

pub(super) fn explain_or_owe(name: Option<&str>, format: Format) -> ExitCode {
    let Some(name) = name else {
        return match root().and_then(|root| run::debt::shown(&root, format)) {
            Ok(said) => {
                emit(&said);
                ExitCode::SUCCESS
            }
            Err(e) => cannot_run(&e),
        };
    };
    explain(name)
}

pub(super) fn explain(name: &str) -> ExitCode {
    if gates::find(name).is_none() {
        return cannot_run(&unknown_gate(name));
    }
    let root = match root() {
        Ok(r) => r,
        Err(e) => return cannot_run(&e),
    };
    let path = root.join(LAST_RUN);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return cannot_run(&format!(
            "no record of a run at {} — run `chock run` first",
            path.display()
        ));
    };
    let Ok(last) = serde_json::from_str::<Run>(&text) else {
        return cannot_run(&format!("{} is not a chock run record", path.display()));
    };
    let Some(report) = last.gates.iter().find(|g| g.gate == name) else {
        return cannot_run(&format!("the last run did not include `{name}`"));
    };
    emit(&format!("{}\n", report.summary()));
    // How long ago this gate ran, so nobody reads an old report as a fresh pass.
    if let Some(ago) = ago(report.ran_at, std::time::SystemTime::now()) {
        emit(&format!("  measured {ago}\n"));
    }
    for line in report.explained(&root) {
        emit(&format!("  {line}\n"));
    }
    emit(&crate::edited::told(&crate::edited::behind_held(
        &root, report,
    )));
    emit(&format!("\nre-run with: {}\n", report.rerun));
    ExitCode::from(report.verdict.code())
}

/// `now` is a parameter so a test can fix it. A record from a clock that is ahead gives `None`.
pub(super) fn ago(ran_at: Option<u64>, now: std::time::SystemTime) -> Option<String> {
    let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(ran_at?);
    Some(crate::setup::doctor::how_long(now.duration_since(at).ok()?))
}
