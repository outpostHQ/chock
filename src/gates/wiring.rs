//! The `wiring` gate: reports gates that would pass on this tree but are switched off.

use crate::run::report::{Finding, Verdict};
use crate::run::{self, Ctx, Gate, Group, Kind, Outcome};

pub const GATE: Gate = Gate {
    name: "wiring",
    about: "every gate that would pass on this tree, and is not opt-in, is switched on",
    group: Group::Setup,
    builds: false,
    reads: None,
    kind: Kind::Binary(check),
};

fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let Some(config) = crate::project::document::read::<crate::project::config::Config>(&ctx.root)
        .map_err(|e| e.to_string())?
    else {
        return Err(format!(
            "no {} — run `chock init --local`",
            crate::project::config::FILE
        ));
    };
    let found = unwired(&config, ctx, &|gate, ctx| run::run_one(gate, ctx).verdict);
    if found.is_empty() {
        return Ok(Outcome::passed());
    }
    Ok(Outcome::failed(found))
}

/// Switched-off gates that would pass. Only gates that need no compile are tried, and not this one.
fn unwired(
    config: &crate::project::config::Config,
    ctx: &Ctx,
    verdict: &dyn Fn(&'static Gate, &Ctx) -> Verdict,
) -> Vec<Finding> {
    super::registry()
        .iter()
        .filter(|gate| !gate.builds && gate.name != GATE.name)
        // Opt-in gates are a choice, and several need a tool that cannot be installed everywhere.
        .filter(|gate| !matches!(gate.group, Group::Instrument | Group::OptIn))
        .filter(|gate| !config.is_on(gate.name))
        .filter(|gate| verdict(gate, ctx) == Verdict::Pass)
        .map(|gate| Finding::at("", "passes on this tree and is switched off").item(gate.name))
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::project::config::Config;

    /// Returns the scratch guard too: `Ctx` cannot hold it, and dropping it deletes the directory.
    fn ctx() -> (Ctx, crate::testdir::Scratch) {
        let dir = crate::testdir::make("wiring");
        let held = Ctx::for_root(
            dir.to_path_buf(),
            crate::run::baseline::Baseline::empty("0.1.0"),
        );
        (held, dir)
    }

    #[test]
    fn a_gate_that_passes_and_is_switched_off_is_reported_by_name() {
        let config = Config::of(["slop"]);
        let (held, _dir) = ctx();
        let found = unwired(&config, &held, &|_, _| Verdict::Pass);
        let names: Vec<String> = found.iter().filter_map(|f| f.item.clone()).collect();
        assert!(names.contains(&"modcheck".to_string()), "{names:?}");
        assert!(!names.contains(&"slop".to_string()), "{names:?}");
    }

    /// A failing gate is debt that `init --local` deliberately leaves off.
    #[test]
    fn a_gate_that_would_fail_is_not_reported() {
        let (held, _dir) = ctx();
        let found = unwired(&Config::of([""; 0]), &held, &|_, _| Verdict::Tripped);
        assert_eq!(found, vec![]);
    }

    #[test]
    fn a_gate_that_could_not_run_is_not_reported() {
        let (held, _dir) = ctx();
        let found = unwired(&Config::of([""; 0]), &held, &|_, _| Verdict::CannotRun);
        assert_eq!(found, vec![]);
    }

    #[test]
    fn a_gate_that_costs_a_compile_is_never_offered() {
        let (held, _dir) = ctx();
        let found = unwired(&Config::of([""; 0]), &held, &|_, _| Verdict::Pass);
        let names: Vec<String> = found.iter().filter_map(|f| f.item.clone()).collect();
        for expensive in ["test", "lint", "msrv", "codeslop", "binsize"] {
            assert!(!names.contains(&expensive.to_string()), "{expensive}");
        }
    }

    #[test]
    fn an_opt_in_gate_is_never_demanded() {
        let (held, _dir) = ctx();
        let found = unwired(&Config::of([""; 0]), &held, &|_, _| Verdict::Pass);
        let names: Vec<String> = found.iter().filter_map(|f| f.item.clone()).collect();
        for chosen in ["padding", "hazards", "unreferenced", "history", "miri"] {
            assert!(!names.contains(&chosen.to_string()), "{chosen}");
        }
    }

    #[test]
    fn this_gate_never_reports_itself() {
        let (held, _dir) = ctx();
        let found = unwired(&Config::of([""; 0]), &held, &|_, _| Verdict::Pass);
        let names: Vec<String> = found.iter().filter_map(|f| f.item.clone()).collect();
        assert!(!names.contains(&GATE.name.to_string()), "{names:?}");
    }
}
