//! Which of a crate's features reach code that ships. A lint allowed under
//! `cfg(any(test, feature = "f"))` is test-only when nothing shipped turns `f` on.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::project::workspace::{Metadata, Package};

/// Every feature a shipped build can enable, keyed by crate directory. A crate absent from the map
/// has every feature counted as shipping.
pub type Reached = BTreeMap<String, BTreeSet<String>>;

/// Which features ship, read from `cargo metadata --no-deps`; a member matching `not_shipped`
/// enables nothing.
pub fn reached(metadata: &str, not_shipped: &[String]) -> Result<Reached, String> {
    let read: Metadata = serde_json::from_str(metadata)
        .map_err(|e| format!("cargo metadata produced something unreadable: {e}"))?;
    let root = Path::new(&read.workspace_root);
    let members: Vec<&Package> = read
        .packages
        .iter()
        .filter(|package| read.workspace_members.contains(&package.id))
        .collect();

    let mut seeds: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for package in &members {
        let manifest = crate::project::relative(root, Path::new(&package.manifest_path));
        if !crate::project::ships(&manifest, not_shipped) {
            continue;
        }
        // Building the member enables its own default features.
        seeds
            .entry(package.name.as_str())
            .or_default()
            .insert("default".to_string());
        for edge in package.dependencies.iter().filter(|d| d.kind.is_none()) {
            let held = seeds.entry(edge.name.as_str()).or_default();
            held.extend(edge.features.iter().cloned());
            if edge.uses_default_features {
                held.insert("default".to_string());
            }
        }
    }

    Ok(members
        .iter()
        .filter_map(|package| {
            let manifest = crate::project::relative(root, Path::new(&package.manifest_path));
            let dir = crate::project::crate_dir(&manifest)?;
            let from = seeds
                .get(package.name.as_str())
                .cloned()
                .unwrap_or_default();
            Some((dir, closure(&from, &package.features)))
        })
        .collect())
}

/// Everything the seeds reach through the package's own feature table. An entry naming another
/// crate (`dep:x`, `x/y`) leaves this package, so only bare names continue the walk.
fn closure(seeds: &BTreeSet<String>, table: &BTreeMap<String, Vec<String>>) -> BTreeSet<String> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    let mut rest: Vec<String> = seeds.iter().cloned().collect();
    while let Some(name) = rest.pop() {
        if !found.insert(name.clone()) {
            continue;
        }
        for entry in table.get(&name).into_iter().flatten() {
            if !entry.contains('/') && !entry.starts_with("dep:") {
                rest.push(entry.clone());
            }
        }
    }
    found
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;

    /// Only a dev-dependency and the `outpost-agent-canary` member turn `test-utils` on.
    const WORKSPACE: &str = r#"{
      "workspace_root": "/w",
      "workspace_members": ["core 0.1.0", "cli 0.1.0", "canary 0.1.0"],
      "packages": [
        {"id": "core 0.1.0", "name": "outpost-core", "manifest_path": "/w/crates/core/Cargo.toml",
         "features": {"default": ["semantic"], "semantic": [], "test-utils": ["fixtures"],
                      "fixtures": [], "loom-tests": []},
         "dependencies": []},
        {"id": "cli 0.1.0", "name": "outpost-cli", "manifest_path": "/w/crates/cli/Cargo.toml",
         "features": {},
         "dependencies": [
           {"name": "outpost-core", "kind": null, "features": ["semantic"], "uses_default_features": false},
           {"name": "outpost-core", "kind": "dev", "features": ["test-utils"], "uses_default_features": false}
         ]},
        {"id": "canary 0.1.0", "name": "outpost-agent-canary",
         "manifest_path": "/w/crates/outpost-agent-canary/Cargo.toml",
         "features": {},
         "dependencies": [
           {"name": "outpost-core", "kind": null, "features": ["test-utils"], "uses_default_features": false}
         ]}
      ]
    }"#;

    #[test]
    fn a_feature_only_a_dev_dependency_turns_on_never_reaches_shipped_code() {
        let held = reached(WORKSPACE, &["crates/outpost-agent-canary".to_string()]).unwrap();
        let core = &held["crates/core"];
        assert!(!core.contains("test-utils"), "{core:?}");
        assert!(!core.contains("fixtures"), "{core:?}");
        // What a normal edge and the package's own default do reach.
        assert!(core.contains("semantic"));
        assert!(core.contains("default"));
    }

    #[test]
    fn a_feature_a_shipped_member_turns_on_reaches_shipped_code() {
        let held = reached(WORKSPACE, &[]).unwrap();
        assert!(held["crates/core"].contains("test-utils"));
        // And whatever that feature itself enables, through the package's own table.
        assert!(held["crates/core"].contains("fixtures"));
    }

    #[test]
    fn an_edge_that_takes_default_features_reaches_what_default_reaches() {
        let tree = r#"{"workspace_root": "/w", "workspace_members": ["a 0.1.0", "b 0.1.0"],
          "packages": [
            {"id": "a 0.1.0", "name": "a", "manifest_path": "/w/a/Cargo.toml",
             "features": {"default": ["wire"], "wire": [], "helper": []}, "dependencies": []},
            {"id": "b 0.1.0", "name": "b", "manifest_path": "/w/b/Cargo.toml", "features": {},
             "dependencies": [
               {"name": "a", "kind": null, "features": [], "uses_default_features": true}]}]}"#;
        let held = reached(tree, &[]).unwrap();
        assert!(held["a"].contains("wire"));
        assert!(!held["a"].contains("helper"));
    }

    #[test]
    fn a_feature_no_edge_and_no_default_names_is_test_only() {
        let held = reached(WORKSPACE, &[]).unwrap();
        assert!(!held["crates/core"].contains("loom-tests"));
    }

    #[test]
    fn a_members_own_default_features_ship_because_building_it_enables_them() {
        let alone = r#"{"workspace_root": "/w", "workspace_members": ["a 0.1.0"], "packages": [
          {"id": "a 0.1.0", "name": "a", "manifest_path": "/w/Cargo.toml",
           "features": {"default": ["shipped"], "shipped": [], "helper": []}, "dependencies": []}]}"#;
        let held = reached(alone, &[]).unwrap();
        assert!(held[""].contains("shipped"));
        assert!(!held[""].contains("helper"));
    }

    #[test]
    fn a_feature_entry_naming_another_crate_does_not_reach_a_feature_of_this_one() {
        let table = BTreeMap::from([
            (
                "default".to_string(),
                vec!["serde/std".to_string(), "dep:rand".to_string()],
            ),
            ("std".to_string(), Vec::new()),
        ]);
        let found = closure(&BTreeSet::from(["default".to_string()]), &table);
        assert_eq!(found, BTreeSet::from(["default".to_string()]));
    }

    #[test]
    fn a_feature_table_that_points_at_itself_still_finishes() {
        let table = BTreeMap::from([
            ("a".to_string(), vec!["b".to_string()]),
            ("b".to_string(), vec!["a".to_string()]),
        ]);
        let found = closure(&BTreeSet::from(["a".to_string()]), &table);
        assert_eq!(found, BTreeSet::from(["a".to_string(), "b".to_string()]));
    }

    #[test]
    fn metadata_that_cannot_be_read_is_a_failure_rather_than_an_empty_answer() {
        assert!(reached("not json", &[]).is_err());
    }
}
