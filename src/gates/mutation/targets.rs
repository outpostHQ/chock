//! Checks that mutest wrote results for every target cargo tests, because a silently skipped
//! target reads as every mutation caught.

use std::collections::BTreeSet;

use serde_json::Value;

/// Fails when a target `cargo mutest run` builds has no results and mutest did not call it
/// ineligible. `ineligible` holds integration test names as mutest prints them.
pub(super) fn all_written(
    metadata: &str,
    written: &BTreeSet<String>,
    ineligible: &BTreeSet<String>,
) -> Result<(), String> {
    let read: Value = serde_json::from_str(metadata)
        .map_err(|error| format!("cargo metadata printed JSON chock cannot read: {error}"))?;
    // Without it every package is filtered out and the check passes having checked nothing.
    let members = read["workspace_default_members"].as_array().ok_or(
        "cargo metadata named no workspace_default_members, so what mutest builds is unknown",
    )?;
    let named = |at: &String| {
        ineligible
            .iter()
            .any(|name| at.ends_with(&format!("/tests/{}", name.replace('-', "_"))))
    };
    let missing: Vec<String> = read["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|package| members.contains(&package["id"]))
        .flat_map(|package| {
            let name = package["name"].as_str().unwrap_or_default();
            package["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(move |target| results_of(name, target))
        })
        .filter(|at| !written.contains(at) && !named(at))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "mutest wrote no results for {}, so their mutations were never evaluated",
        missing.join(", ")
    ))
}

/// Where mutest writes a target's results, or `None` when none are required: `test = false`,
/// required features, or a proc-macro. The main binary is the one named after the package.
fn results_of(package: &str, target: &Value) -> Option<String> {
    let gated = target["required-features"]
        .as_array()
        .is_some_and(|features| !features.is_empty());
    if target["test"].as_bool() != Some(true) || gated {
        return None;
    }
    let crate_name = target["name"].as_str()?.replace('-', "_");
    let kinds = target["kind"].as_array();
    let is = |kind: &str| kinds.is_some_and(|held| held.iter().any(|one| one == kind));
    if is("bin") && crate_name == package.replace('-', "_") {
        return Some(format!("{package}/bin"));
    }
    if is("bin") {
        return Some(format!("{package}/bins/{crate_name}"));
    }
    if is("test") {
        return Some(format!("{package}/tests/{crate_name}"));
    }
    ["lib", "rlib", "dylib", "cdylib", "staticlib"]
        .into_iter()
        .any(is)
        .then(|| format!("{package}/lib"))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    const METADATA: &str = r#"{"workspace_default_members": ["a 0.1.0"], "packages": [
      {"id": "a 0.1.0", "name": "my-app", "targets": [
        {"name": "my-app", "kind": ["lib"], "test": true},
        {"name": "my-app", "kind": ["bin"], "test": true},
        {"name": "tool-two", "kind": ["bin"], "test": true},
        {"name": "cli-flow", "kind": ["test"], "test": true},
        {"name": "commands", "kind": ["test"], "test": true},
        {"name": "gated", "kind": ["test"], "test": true, "required-features": ["slow"]},
        {"name": "tour", "kind": ["example"], "test": false},
        {"name": "speed", "kind": ["bench"], "test": false},
        {"name": "build-script-build", "kind": ["custom-build"], "test": false}]},
      {"id": "b 0.1.0", "name": "not-built", "targets": [
        {"name": "not-built", "kind": ["lib"], "test": true}]}]}"#;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn every_target_cargo_tests_is_named_where_mutest_writes_it() {
        let written = set(&[
            "my-app/lib",
            "my-app/bin",
            "my-app/bins/tool_two",
            "my-app/tests/cli_flow",
        ]);
        assert_eq!(all_written(METADATA, &written, &set(&["commands"])), Ok(()));
    }

    #[test]
    fn a_target_with_no_results_and_no_word_from_mutest_is_refused() {
        let written = set(&["my-app/lib", "my-app/bin", "my-app/tests/cli_flow"]);
        let refused = all_written(METADATA, &written, &set(&[])).unwrap_err();
        assert_eq!(
            refused,
            "mutest wrote no results for my-app/bins/tool_two, my-app/tests/commands, so their \
             mutations were never evaluated"
        );
    }

    #[test]
    fn metadata_that_does_not_say_what_cargo_builds_is_refused_rather_than_passed() {
        let refused = all_written(r#"{"packages": []}"#, &set(&[]), &set(&[])).unwrap_err();
        assert!(refused.contains("workspace_default_members"), "{refused}");
    }
}
