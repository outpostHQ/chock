//! Every gate, grouped by what it reads: Outpost's check, another tool, the parsed source, what cargo
//! builds, the repository. A gate supplies numbers or a verdict; `run` does the rest, written once.

pub mod cargo;
pub mod coverage;
pub mod declared;
pub mod fixes;
pub mod metrics;
pub mod mutation;
pub mod outpost;
pub mod repo;
pub mod source;
pub mod text;
pub mod tools;
pub mod wiring;

use crate::run::baseline::Keys;
use crate::run::{Gate, Group, Kind};

static REGISTRY: &[Gate] = &[
    tools::TEST,
    tools::LINT,
    tools::DOC,
    cargo::modcheck::GATE,
    metrics::assertions::GATE,
    text::citations::GATE,
    repo::commits::GATE,
    cargo::manifest::GATE,
    cargo::placement::GATE,
    cargo::profile::GATE,
    cargo::features::GATE,
    repo::hygiene::GATE,
    source::GATE,
    metrics::duplication::GATE,
    outpost::duplicates::GATE,
    metrics::dead::GATE,
    repo::history::GATE,
    outpost::boundaries::GATE,
    outpost::padding::GATE,
    outpost::measures::GATE,
    outpost::intel::HAZARDS,
    outpost::intel::UNREFERENCED,
    outpost::intel::LENSES,
    outpost::intel::UNREAD,
    outpost::scan::GATE,
    tools::proof::GATE,
    tools::DEPS,
    tools::SORT,
    tools::dupdeps::GATE,
    tools::ACL,
    cargo::supply::GATE,
    tools::UNUSED,
    tools::TYPOS,
    tools::MSRV,
    text::slop::GATE,
    metrics::bigfiles::GATE,
    metrics::splits::GATE,
    metrics::lean::GATE,
    tools::binsize::GATE,
    metrics::complexity::GATE,
    metrics::nesting::GATE,
    tools::codeslop::GATE,
    metrics::unsafety::GATE,
    coverage::GATE,
    coverage::crap::GATE,
    tools::IDEMPOTENT,
    tools::miri::GATE,
    tools::MUTEST,
    tools::UNUSED_DEEP,
    tools::BSIZE,
    tools::FMT,
    declared::GATE,
    declared::BUILDS,
    text::phrases::GATE,
    wiring::GATE,
];

#[must_use]
pub fn registry() -> &'static [Gate] {
    REGISTRY
}

/// Whether a gate's tool runs on `os` at all: cackle refuses to compile off Linux, and Kani
/// publishes no Windows build. A run elsewhere leaves such a gate out and names it, never passes it.
#[must_use]
pub fn runs_on(gate: &str, os: &str) -> bool {
    match gate {
        "acl" => os == "linux",
        "proof" => os != "windows",
        _ => true,
    }
}

/// Ratchets whose count is the same on every machine and system, because they read source text or
/// every target's metadata. Only these lower their own record: the rest wait for `--lower`.
const SETTLES_ANYWHERE: [&str; 21] = [
    "assertions",
    "bigfiles",
    "citations",
    "complexity",
    "dupdeps",
    "duplication",
    "features",
    "lean",
    "manifest",
    "modcheck",
    "nesting",
    "phrases",
    "placement",
    "slop",
    "sort",
    "source",
    "splits",
    "supply",
    "typos",
    "unsafety",
    "unused",
];

/// Whether a run may lower this gate's record by itself. Coverage, mutation testing and binary size
/// measure lower on some machines than on others, and a record lowered on one fails CI on another.
#[must_use]
pub fn settles_anywhere(gate: &str) -> bool {
    SETTLES_ANYWHERE.contains(&gate)
}

/// Gates that count items but key each one by a name, not by the file it is in.
const KEYED_BY_NAME: [&str; 6] = [
    "boundaries",
    "commands",
    "commands-build",
    "dupdeps",
    "sort",
    "supply",
];

/// Whether the gate keeps a number for each file, which is what `clean_when_touched` holds to zero.
#[must_use]
pub fn holds_each_file(name: &str) -> bool {
    let by_item = find(name).is_some_and(|gate| match gate.kind {
        Kind::Debt { .. } => true,
        Kind::Ratchet { keys, .. } | Kind::AnnotatedRatchet { keys, .. } => keys == Keys::Items,
        Kind::Binary(_) => false,
    });
    by_item && !KEYED_BY_NAME.contains(&name)
}

/// Gates that build in a cargo target directory of their own and run no test suite. Cargo locks
/// a directory while it builds, so each can build beside the gates that run the suite.
const BUILDS_APART: [&str; 4] = ["binsize", "bsize", "proof", "acl"];

/// Whether `gate` builds apart under the project's `build` flags: `binsize` with a named profile
/// may share the suite's directory.
#[must_use]
pub fn builds_apart(gate: &str, build: &[String]) -> bool {
    let shared = gate == tools::binsize::GATE.name && build.iter().any(|flag| flag == "--profile");
    BUILDS_APART.contains(&gate) && !shared
}

/// Gates whose tool builds in a directory of its own under `target`, so each may take a lane of its
/// own beside the suite's once its recorded memory fits.
pub(crate) const BUILDS_ALONE: [&str; 2] = ["mutest", "miri"];

#[must_use]
pub fn find(name: &str) -> Option<&'static Gate> {
    REGISTRY.iter().find(|gate| gate.name == name)
}

/// The gates every project runs on each change: no instrument and no opt-in.
#[must_use]
pub fn enforced() -> Vec<&'static Gate> {
    REGISTRY
        .iter()
        .filter(|gate| matches!(gate.group, Group::Gates | Group::Quality | Group::Setup))
        .collect()
}

/// What a survey runs: every enforced gate that needs no compiler, so it can be pointed at a tree it
/// has never seen without a config, a baseline, or a build directory to write into.
#[must_use]
pub fn surveyable() -> Vec<&'static Gate> {
    // Setup gates ask about chock's own configuration, which a surveyed tree has none of: `wiring`
    // refused on all 127 open-source repositories surveyed and made every survey exit 2.
    enforced()
        .into_iter()
        .filter(|gate| !gate.builds && gate.group != Group::Setup)
        .collect()
}

/// Every gate chock can record a baseline for.
#[must_use]
pub fn ratchets() -> Vec<&'static Gate> {
    REGISTRY
        .iter()
        .filter(|gate| gate.counts_in().is_some())
        .collect()
}

/// The gates keeping a baseline of their own, outside `.chock/baseline.json`, and the file each keeps.
/// One list, because `init` and `baseline` both have to know or they wait on each other for ever.
pub const OWN_BASELINE: [(&str, OwnFile); 1] = [("crap", coverage::crap::baseline)];

/// The file a gate keeps for itself, named on the system that runs it.
pub type OwnFile = fn() -> String;

/// The file this gate keeps for itself on this system, where it keeps one.
#[must_use]
pub fn own_baseline(name: &str) -> Option<String> {
    OWN_BASELINE
        .iter()
        .find(|(gate, _)| *gate == name)
        .map(|(_, file)| file())
}

/// The debt the gate's own file holds under `root`; none for a gate that keeps no such file.
pub fn own_debt(
    root: &std::path::Path,
    name: &str,
) -> Result<Option<crate::run::debt::Debt>, String> {
    match name == coverage::crap::GATE.name {
        true => coverage::crap::debt(root),
        false => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::rerun;

    /// `rerun` is the one command AGENTS.md tells an agent to run, so it has to mean the same thing
    /// in every project. `just deps` meant `cargo machete` in the first tree that adopted chock.
    #[test]
    fn every_gate_reruns_through_chock_rather_than_a_recipe_the_project_defines() {
        for gate in registry() {
            let said = rerun(gate.name);
            assert_eq!(said, format!("chock run {}", gate.name));
            assert!(find(gate.name).is_some(), "{} does not resolve", gate.name);
        }
    }

    #[test]
    fn the_justfile_keeps_a_recipe_for_every_gate() {
        let justfile = include_str!("../../justfile");
        for gate in registry() {
            let want = format!("{}:", gate.name);
            assert!(
                justfile.lines().any(|line| line.starts_with(&want)),
                "the justfile has no `{want}` recipe"
            );
        }
    }

    #[test]
    fn only_a_gate_with_a_number_for_each_file_is_held_where_a_change_touches() {
        for name in KEYED_BY_NAME {
            assert!(
                find(name).is_some_and(|gate| gate.counts_in().is_some()),
                "{name} is no gate recording a number"
            );
            assert!(!holds_each_file(name), "{name} keys its record by a name");
        }
        for by_file in ["complexity", "nesting", "coverage", "phrases", "modcheck"] {
            assert!(
                holds_each_file(by_file),
                "{by_file} keeps a number for each file"
            );
        }
        for whole in [
            "binsize",
            "codeslop",
            "lenses",
            "measures",
            "test",
            "no-such-gate",
        ] {
            assert!(
                !holds_each_file(whole),
                "{whole} keeps no number for each file"
            );
        }
    }

    #[test]
    fn only_a_ratchet_reading_source_or_every_targets_metadata_lowers_its_own_record() {
        for name in SETTLES_ANYWHERE {
            assert!(
                find(name).is_some_and(|gate| gate.counts_in().is_some()),
                "{name} is no gate recording a number to lower"
            );
        }
        assert!(settles_anywhere("slop") && settles_anywhere("dupdeps"));
        for varies in [
            "coverage", "mutest", "binsize", "codeslop", "lenses", "commands",
        ] {
            assert!(
                !settles_anywhere(varies),
                "{varies} measures differently by machine"
            );
        }
    }

    #[test]
    fn acl_runs_on_linux_alone_proof_everywhere_but_windows_and_the_rest_anywhere() {
        let systems = ["linux", "macos", "windows"];
        let on = |gate: &str| -> Vec<&str> {
            systems.into_iter().filter(|os| runs_on(gate, os)).collect()
        };
        assert_eq!(on("acl"), ["linux"]);
        assert_eq!(on("proof"), ["linux", "macos"]);
        assert_eq!(on("test"), systems);
        assert!(
            find("acl").is_some() && find("proof").is_some(),
            "both names are gates"
        );
    }

    /// A survey runs on a tree that never adopted chock, so it cannot ask for a gate the project
    /// switched on, or start a compiler in another project's build directory.
    #[test]
    fn a_survey_runs_no_gate_that_needs_a_compiler_or_asks_about_chocks_own_setup() {
        let surveyed = surveyable();
        assert!(
            surveyed.iter().all(|gate| !gate.builds),
            "a compile snuck in"
        );
        let names: Vec<&str> = surveyed.iter().map(|gate| gate.name).collect();
        assert!(names.contains(&"slop"), "{names:?}");
        assert!(!names.contains(&"test"), "{names:?}");
        assert!(!names.contains(&"coverage"), "{names:?}");
        assert!(!names.contains(&"wiring"), "{names:?}");
    }

    #[test]
    fn every_registered_gate_has_a_unique_name_and_a_rerun_command() {
        let mut seen = std::collections::BTreeSet::new();
        for gate in registry() {
            assert!(seen.insert(gate.name), "{} is registered twice", gate.name);
            assert!(!rerun(gate.name).is_empty(), "{} has no rerun", gate.name);
            assert!(!gate.about.is_empty(), "{} has no description", gate.name);
        }
    }

    #[test]
    fn an_instrument_and_an_opt_in_are_both_left_out_of_the_enforced_set() {
        assert!(
            enforced()
                .iter()
                .all(|g| matches!(g.group, Group::Gates | Group::Quality | Group::Setup))
        );
        assert!(registry().iter().any(|g| g.group == Group::Instrument));
        assert!(registry().iter().any(|g| g.group == Group::OptIn));
    }

    #[test]
    fn every_ratchet_is_findable_by_the_name_it_registered() {
        for gate in ratchets() {
            assert_eq!(find(gate.name).map(|g| g.name), Some(gate.name));
        }
        assert!(ratchets().iter().any(|gate| gate.name == "mutest"));
    }

    #[test]
    fn a_gate_builds_apart_only_in_a_target_directory_no_suite_shares() {
        let none: Vec<String> = Vec::new();
        let profile = vec!["--profile".to_owned(), "dist".to_owned()];
        for name in BUILDS_APART {
            assert!(
                find(name).is_some_and(|gate| gate.builds),
                "{name} compiles nothing"
            );
            assert!(builds_apart(name, &none), "{name}");
        }
        assert!(!builds_apart("binsize", &profile));
        assert!(builds_apart("bsize", &profile));
        assert!(!builds_apart("test", &none) && !builds_apart("coverage", &none));
    }

    #[test]
    fn an_unknown_gate_name_finds_nothing() {
        assert!(find("no-such-gate").is_none());
    }

    /// Each gate says whether it compiles, so the hook script carries no list. A list becomes wrong
    /// when somebody switches a gate on.
    #[test]
    fn a_gate_says_for_itself_whether_answering_costs_a_compile() {
        let compiles: Vec<&str> = registry()
            .iter()
            .filter(|gate| gate.builds)
            .map(|gate| gate.name)
            .collect();
        assert!(compiles.contains(&"test"), "{compiles:?}");
        assert!(compiles.contains(&"lint"), "{compiles:?}");
        assert!(compiles.contains(&"msrv"), "{compiles:?}");
        assert!(!compiles.contains(&"slop"), "{compiles:?}");
        assert!(!compiles.contains(&"modcheck"), "{compiles:?}");
        assert!(!compiles.contains(&"source"), "{compiles:?}");
    }
}
