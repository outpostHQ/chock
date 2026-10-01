//! The `supply` gate: dependencies whose build scripts or proc-macros run unsandboxed on the
//! machine doing the build.

use crate::project::workspace::{Metadata, Package};

use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

pub const GATE: Gate = Gate {
    name: "supply",
    about: "dependencies that execute code during a build, by crate and kind",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "crate(s) running code at build time",
    },
};

fn measure(ctx: &Ctx) -> Result<Series, String> {
    running_code(&project::resolved(&ctx.root)?)
}

/// cargo's target-kind names for code that runs at build time.
const BUILD_SCRIPT: &str = "custom-build";
const PROC_MACRO: &str = "proc-macro";

/// Counts each non-workspace crate with a build script or proc-macro, keyed by crate and kind.
/// A ratchet, not a ban: real projects already have dozens, so only a new one fails.
pub fn running_code(metadata_json: &str) -> Result<Series, String> {
    let metadata: Metadata = serde_json::from_str(metadata_json)
        .map_err(|e| format!("cargo metadata printed a graph chock cannot read: {e}"))?;
    let mut series = Series::new();
    // Workspace members are the project's own code, which other gates judge.
    let theirs = metadata
        .packages
        .iter()
        .filter(|p| !metadata.workspace_members.contains(&p.id));
    for key in theirs.flat_map(keys_for) {
        series.set(&key, 1);
    }
    Ok(series)
}

/// One key per build-time kind, so a crate that is both is named under each.
fn keys_for(package: &Package) -> Vec<String> {
    package
        .targets
        .iter()
        .flat_map(|target| target.kind.iter())
        .filter_map(|kind| match kind.as_str() {
            BUILD_SCRIPT => Some(format!("{}#build-script", package.name)),
            PROC_MACRO => Some(format!("{}#proc-macro", package.name)),
            _ => None,
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

    fn graph(packages: &str, members: &str) -> String {
        format!(r#"{{"packages":[{packages}],"workspace_members":[{members}]}}"#)
    }

    fn package(id: &str, name: &str, kinds: &[&str]) -> String {
        let targets: Vec<String> = kinds
            .iter()
            .map(|k| format!(r#"{{"kind":["{k}"]}}"#))
            .collect();
        format!(
            r#"{{"id":"{id}","name":"{name}","targets":[{}]}}"#,
            targets.join(",")
        )
    }

    #[test]
    fn a_dependency_with_a_build_script_is_counted_under_its_own_name() {
        let json = graph(&package("serde 1.0", "serde", &["lib", "custom-build"]), "");
        let found = running_code(&json).unwrap();
        assert_eq!(found.get("serde#build-script"), Some(1));
        assert_eq!(found.get("serde#proc-macro"), None);
    }

    #[test]
    fn a_proc_macro_dependency_is_counted_under_its_own_kind() {
        let json = graph(&package("sd 1.0", "serde_derive", &["proc-macro"]), "");
        let found = running_code(&json).unwrap();
        assert_eq!(found.get("serde_derive#proc-macro"), Some(1));
    }

    #[test]
    fn a_dependency_that_runs_nothing_at_build_time_contributes_no_key() {
        let json = graph(&package("x 1.0", "plain", &["lib"]), "");
        assert_eq!(running_code(&json).unwrap(), Series::new());
    }

    #[test]
    fn a_build_script_in_this_workspace_is_not_somebody_elses_code() {
        let json = graph(
            &package("mine 0.1", "mine", &["lib", "custom-build"]),
            r#""mine 0.1""#,
        );
        assert_eq!(running_code(&json).unwrap(), Series::new());
    }

    #[test]
    fn a_crate_that_is_both_is_counted_once_under_each_kind() {
        let json = graph(
            &package("b 1.0", "both", &["proc-macro", "custom-build"]),
            "",
        );
        let found = running_code(&json).unwrap();
        assert_eq!(found.get("both#proc-macro"), Some(1));
        assert_eq!(found.get("both#build-script"), Some(1));
    }

    #[test]
    fn a_graph_chock_cannot_read_stops_the_gate_rather_than_counting_none() {
        assert!(running_code("not json").is_err());
    }
}
