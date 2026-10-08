//! The `wiring` gate: reports gates that would pass on this tree but are switched off.

use crate::project::config::Config;
use crate::run::baseline::Baseline;
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
    let Some(config) =
        crate::project::document::read::<Config>(&ctx.root).map_err(|e| e.to_string())?
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

/// The gates `unwired` names at `root` in the context a run builds, so `init` switches on what
/// this gate would refuse. None where `wiring` is off: there the choice is the project's.
pub(crate) fn passing_and_off(root: &std::path::Path, config: &Config) -> Vec<String> {
    if !config.is_on(GATE.name) {
        return Vec::new();
    }
    unwired(
        config,
        &Ctx {
            vcs: crate::project::vcs::holding(root, Some(config)),
            root: root.to_path_buf(),
            baseline: crate::project::document::read::<Baseline>(root)
                .ok()
                .flatten()
                .unwrap_or_else(|| Baseline::empty(env!("CARGO_PKG_VERSION"))),
            ..Ctx::from_config(Some(config))
        },
        &|gate, ctx| run::run_one(gate, ctx).verdict,
    )
    .into_iter()
    .filter_map(|finding| finding.item)
    .collect()
}

/// Switched-off gates that would pass. Only gates that need no compile are tried, and not this one.
fn unwired(
    config: &Config,
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

    /// Returns the scratch guard too: `Ctx` cannot hold it, and dropping it deletes the directory.
    fn ctx() -> (Ctx, crate::testdir::Scratch) {
        let dir = crate::testdir::make("wiring");
        let held = Ctx::at(&dir);
        (held, dir)
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
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
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_that_would_fail_is_not_reported() {
        let (held, _dir) = ctx();
        let found = unwired(&Config::of([""; 0]), &held, &|_, _| Verdict::Tripped);
        assert_eq!(found, vec![]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_that_could_not_run_is_not_reported() {
        let (held, _dir) = ctx();
        let found = unwired(&Config::of([""; 0]), &held, &|_, _| Verdict::CannotRun);
        assert_eq!(found, vec![]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_that_costs_a_compile_is_never_offered() {
        let (held, _dir) = ctx();
        let found = unwired(&Config::of([""; 0]), &held, &|_, _| Verdict::Pass);
        let names: Vec<String> = found.iter().filter_map(|f| f.item.clone()).collect();
        for expensive in ["test", "lint", "msrv", "codeslop", "binsize"] {
            assert!(!names.contains(&expensive.to_string()), "{expensive}");
        }
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn an_opt_in_gate_is_never_demanded() {
        let (held, _dir) = ctx();
        let found = unwired(&Config::of([""; 0]), &held, &|_, _| Verdict::Pass);
        let names: Vec<String> = found.iter().filter_map(|f| f.item.clone()).collect();
        for chosen in ["padding", "hazards", "unreferenced", "history", "miri"] {
            assert!(!names.contains(&chosen.to_string()), "{chosen}");
        }
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn this_gate_never_reports_itself() {
        let (held, _dir) = ctx();
        let found = unwired(&Config::of([""; 0]), &held, &|_, _| Verdict::Pass);
        let names: Vec<String> = found.iter().filter_map(|f| f.item.clone()).collect();
        assert!(!names.contains(&GATE.name.to_string()), "{names:?}");
    }
}
