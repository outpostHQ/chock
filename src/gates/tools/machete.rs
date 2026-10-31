//! Dependencies a crate declares and does not use, as `cargo machete` reports them, less any the
//! crate's sources name where machete does not look, such as a macro argument.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::exec;
use crate::project;
use crate::run::Ctx;
use crate::run::baseline::Series;
use crate::run::report::Finding;

/// Each dependency cargo-machete finds unused and no source names, keyed `manifest#name`.
/// # Errors
/// A cargo-machete that cannot run, or a manifest chock cannot read.
pub fn unused(ctx: &Ctx) -> Result<Series, String> {
    // machete honours `.gitignore` only inside git, so `target/` is skipped explicitly.
    let out = exec::tool(&ctx.root, "cargo", &["machete", "--skip-target-dir"])?;
    let objected = read_machete(&out, &ctx.root)?;
    let sources = sources_for(ctx, &objected)?;
    without_unified(unnamed_anywhere(&objected, &sources), &ctx.root)
}

/// `unnamed` less each entry whose features Cargo unifies. Only an entry needs the resolved graph,
/// so a tree with none left does not ask Cargo for it.
fn without_unified(unnamed: Series, root: &Path) -> Result<Series, String> {
    if unnamed.is_empty() {
        return Ok(unnamed);
    }
    let unified = unified(&project::resolved(root)?, root)?;
    let mut kept = Series::new();
    for (key, count) in unnamed.0.iter().filter(|(key, _)| !unified.contains(*key)) {
        kept.set(key, *count);
    }
    Ok(kept)
}

/// Each entry, as `manifest#name`, that turns on features of a package its crate also reaches
/// through another dependency. Cargo unifies features, so such an entry is in use.
pub(super) fn unified(resolved: &str, root: &Path) -> Result<BTreeSet<String>, String> {
    let read: serde_json::Value = serde_json::from_str(resolved)
        .map_err(|e| format!("cargo metadata printed a graph chock cannot read: {e}"))?;
    let unread = || "cargo metadata printed no resolved dependency graph".to_string();
    let nodes = read["resolve"]["nodes"].as_array().ok_or_else(unread)?;
    let edges: BTreeMap<&str, Vec<&str>> = nodes
        .iter()
        .filter_map(|node| {
            let deps = node["deps"].as_array().into_iter().flatten();
            Some((
                node["id"].as_str()?,
                deps.filter_map(|dep| dep["pkg"].as_str()).collect(),
            ))
        })
        .collect();
    let packages = read["packages"].as_array().ok_or_else(unread)?;
    let name_of: BTreeMap<&str, &str> = packages
        .iter()
        .filter_map(|package| Some((package["id"].as_str()?, package["name"].as_str()?)))
        .collect();
    let members: BTreeSet<&str> = read["workspace_members"]
        .as_array()
        .ok_or_else(unread)?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    let mut found = BTreeSet::new();
    for package in packages.iter().filter(|package| {
        package["id"]
            .as_str()
            .is_some_and(|id| members.contains(id))
    }) {
        let (Some(id), Some(manifest)) =
            (package["id"].as_str(), package["manifest_path"].as_str())
        else {
            continue;
        };
        let direct = edges.get(id).map(Vec::as_slice).unwrap_or_default();
        let entries = package["dependencies"].as_array().into_iter().flatten();
        for entry in entries.filter(|entry| {
            entry["features"]
                .as_array()
                .is_some_and(|on| !on.is_empty())
        }) {
            let Some(name) = entry["name"].as_str() else {
                continue;
            };
            let targets = direct.iter().filter(|pkg| name_of.get(*pkg) == Some(&name));
            if targets
                .into_iter()
                .any(|target| reached_otherwise(target, direct, &edges))
            {
                let key = entry["rename"].as_str().unwrap_or(name);
                let shown = project::relative(root, Path::new(manifest));
                found.insert(format!("{shown}#{key}"));
            }
        }
    }
    Ok(found)
}

/// Whether a direct dependency other than `target` leads to it in the resolved graph.
fn reached_otherwise(target: &str, direct: &[&str], edges: &BTreeMap<&str, Vec<&str>>) -> bool {
    let mut seen = BTreeSet::new();
    let mut next: Vec<&str> = direct
        .iter()
        .copied()
        .filter(|pkg| *pkg != target)
        .collect();
    while let Some(pkg) = next.pop() {
        if !seen.insert(pkg) {
            continue;
        }
        let deps = edges.get(pkg).map(Vec::as_slice).unwrap_or_default();
        if deps.contains(&target) {
            return true;
        }
        next.extend(deps);
    }
    false
}

/// The sources of each crate machete named, read once per manifest and not once per finding.
fn sources_for(ctx: &Ctx, objected: &Series) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut sources = BTreeMap::new();
    for manifest in manifests_in(objected) {
        let read = crate_sources(ctx, &manifest)?;
        sources.insert(manifest, read);
    }
    Ok(sources)
}

/// The manifests a run objected about, each once.
fn manifests_in(objected: &Series) -> BTreeSet<String> {
    objected
        .0
        .keys()
        .filter_map(|key| key.split('#').next())
        .map(str::to_string)
        .collect()
}

/// Keyed `manifest#dependency`. An objection that names no dependency is an error, not a pass.
fn read_machete(out: &exec::Output, root: &Path) -> Result<Series, String> {
    let mut series = Series::new();
    if !exec::objected(out, "cargo machete")? {
        return Ok(series);
    }
    for found in unused_deps(&out.stdout, root) {
        // `unused_deps` names the dependency on every finding, so the key always has both parts.
        let dependency = found.item.unwrap_or_default();
        series.set(&format!("{}#{dependency}", found.file), 1);
    }
    if series.0.is_empty() {
        return Err("cargo machete objected without naming a dependency".to_string());
    }
    Ok(series)
}

/// Parses machete's text, which has no JSON mode: a package header, indented names under it, then
/// advice after a blank line.
fn unused_deps(stdout: &str, root: &Path) -> Vec<Finding> {
    let mut found = Vec::new();
    let mut manifest: Option<String> = None;
    for line in stdout.lines() {
        if let Some((_, rest)) = line.split_once(" -- ") {
            manifest = rest.trim().strip_suffix(':').map(str::to_string);
        } else if line.trim().is_empty() {
            // The advice block below the list is prose about the tool, not about this tree.
            manifest = None;
        } else if let Some(at) = &manifest {
            let at = at.trim_start_matches("./");
            let shown = project::relative(root, Path::new(at));
            found.push(
                Finding::at(&shown, "nothing in this crate references this dependency")
                    .item(line.trim()),
            );
        }
    }
    found
}

/// Drops each finding whose dependency the crate's sources do name, such as in a macro argument.
fn unnamed_anywhere(objected: &Series, sources: &BTreeMap<String, Vec<String>>) -> Series {
    let mut kept = Series::new();
    for (key, count) in &objected.0 {
        let (manifest, dependency) = key.split_once('#').unwrap_or((key.as_str(), ""));
        let held = sources.get(manifest).map(Vec::as_slice).unwrap_or_default();
        if !names_crate(held, dependency) {
            kept.set(key, *count);
        }
    }
    kept
}

/// Whether any source names the dependency as Rust spells it, hyphens as underscores, matching
/// whole identifiers only.
pub(crate) fn names_crate(sources: &[String], dependency: &str) -> bool {
    let ident = dependency.replace('-', "_");
    if ident.is_empty() {
        return false;
    }
    sources.iter().any(|src| mentions(src, &ident))
}

/// A whole-identifier match, so a longer name that starts with this one does not count.
fn mentions(src: &str, ident: &str) -> bool {
    src.match_indices(ident).any(|(at, _)| {
        let before = src[..at].chars().next_back();
        let after = src[at + ident.len()..].chars().next();
        !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char)
    })
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The sources of the crate behind `manifest`, found by cargo's layout.
fn crate_sources(ctx: &Ctx, manifest: &str) -> Result<Vec<String>, String> {
    let found = project::walked(
        &ctx.listing,
        &ctx.root,
        &|name| !project::SKIPPED.contains(&name),
        &|name, _| Path::new(name).extension().is_some_and(|ext| ext == "rs"),
    )?;
    let mut out = Vec::new();
    for path in &found {
        if owns(manifest, &project::relative(&ctx.root, path)) {
            out.extend(std::fs::read_to_string(path).ok());
        }
    }
    Ok(out)
}

/// Whether this manifest's crate is the one cargo compiles that file as part of.
fn owns(manifest: &str, shown: &str) -> bool {
    const HOLDERS: [&str; 4] = ["src", "tests", "benches", "examples"];
    let dir = manifest
        .strip_suffix("Cargo.toml")
        .unwrap_or(manifest)
        .trim_end_matches('/');
    let Some(rest) = project::under(shown, dir) else {
        return false;
    };
    rest == "build.rs"
        || HOLDERS
            .iter()
            .any(|holder| project::under(rest, holder).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new("/w/proj")
    }

    fn ran(stdout: &str, code: Option<i32>) -> exec::Output {
        exec::Output {
            code,
            stdout: stdout.to_string(),
            stderr: "it broke".to_string(),
            truncated: false,
        }
    }

    /// A run whose output chock cut off at its limit.
    fn cut_off(stdout: &str, code: Option<i32>) -> exec::Output {
        exec::Output {
            truncated: true,
            ..ran(stdout, code)
        }
    }

    fn ctx_of(files: &[(&str, &str)]) -> crate::testdir::Held {
        crate::testdir::Held::tree("gate-machete", files)
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_crates_sources_are_read_from_its_own_directories_and_no_neighbours() {
        let ctx = ctx_of(&[
            ("Cargo.toml", ""),
            ("crates/a/Cargo.toml", ""),
            ("crates/a/src/lib.rs", "use tree_sitter_yaml as y;\n"),
            ("crates/a/notes.rs", "regex is unused here\n"),
            ("crates/b/Cargo.toml", ""),
            ("crates/b/src/lib.rs", "use regex::Regex;\n"),
        ]);
        let read = crate_sources(&ctx, "crates/a/Cargo.toml").unwrap();
        assert!(names_crate(&read, "tree-sitter-yaml"));
        assert!(
            !names_crate(&read, "regex"),
            "a neighbour's source was read"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_sources_read_for_a_run_are_keyed_by_the_manifest_they_belong_to() {
        let ctx = ctx_of(&[
            ("crates/a/Cargo.toml", ""),
            ("crates/a/src/lib.rs", "use serde::Serialize;\n"),
            ("crates/b/Cargo.toml", ""),
            ("crates/b/src/lib.rs", "fn f() {}\n"),
        ]);
        let mut objected = Series::new();
        objected.set("crates/a/Cargo.toml#serde", 1);
        objected.set("crates/b/Cargo.toml#serde", 1);
        let read = sources_for(&ctx, &objected).unwrap();
        let kept = unnamed_anywhere(&objected, &read);
        assert_eq!(kept.get("crates/a/Cargo.toml#serde"), None);
        assert_eq!(kept.get("crates/b/Cargo.toml#serde"), Some(1));
    }

    /// Real cargo-machete 0.9.2 output.
    #[test]
    fn an_unused_dependency_is_named_with_the_manifest_that_declares_it() {
        let out = "cargo-machete found the following unused dependencies in this directory:\n\
                   machbad -- ./Cargo.toml:\n\tserde\n\tregex\n\n\
                   If you believe cargo-machete has detected an unused dependency incorrectly,\n\
                   `[package.metadata.cargo-machete]` section of the appropriate Cargo.toml.\n";
        let found: Vec<String> = unused_deps(out, root())
            .iter()
            .map(Finding::render)
            .collect();
        assert_eq!(
            found,
            [
                "Cargo.toml: serde: nothing in this crate references this dependency",
                "Cargo.toml: regex: nothing in this crate references this dependency"
            ]
        );
    }

    #[test]
    fn the_advice_below_the_list_names_no_dependency() {
        let out = "cargo-machete found the following unused dependencies in this directory:\n\
                   \nFor example:\n\n[package.metadata.cargo-machete]\nignored = [\"prost\"]\n";
        assert_eq!(unused_deps(out, root()), vec![]);
    }

    #[test]
    fn a_dependency_is_attributed_to_the_member_that_declares_it() {
        let out = "one -- ./crates/a/Cargo.toml:\n\tserde\n\n\
                   two -- ./crates/b/Cargo.toml:\n\tregex\n";
        let found: Vec<String> = unused_deps(out, root())
            .iter()
            .map(|f| format!("{}:{}", f.file, f.item.clone().unwrap_or_default()))
            .collect();
        assert_eq!(
            found,
            ["crates/a/Cargo.toml:serde", "crates/b/Cargo.toml:regex"]
        );
    }

    #[test]
    fn a_tree_with_no_unused_dependency_passes() {
        let out = ran(
            "cargo-machete didn't find any unused dependencies\n",
            Some(0),
        );
        assert_eq!(read_machete(&out, root()).unwrap(), Series::new());
    }

    #[test]
    fn an_unused_dependency_is_keyed_by_the_manifest_that_declares_it_and_the_name() {
        let out = ran("chock -- /w/proj/Cargo.toml:\n\tserde\n\n", Some(1));
        let series = read_machete(&out, root()).unwrap();
        assert_eq!(series.get("Cargo.toml#serde"), Some(1));
    }

    #[test]
    fn output_machete_had_cut_short_stops_the_gate_rather_than_reporting_half() {
        let out = cut_off("chock -- /w/proj/Cargo.toml:\n\tserde\n", Some(1));
        let err = read_machete(&out, root()).unwrap_err();
        assert!(err.contains("more than chock keeps"), "{err}");
    }

    #[test]
    fn a_tool_that_objected_and_named_no_dependency_measured_nothing() {
        let out = ran("cargo-machete found the following unused dep", Some(1));
        let err = read_machete(&out, root()).unwrap_err();
        assert!(err.contains("without naming a dependency"), "{err}");
    }

    #[test]
    fn a_dependency_named_only_as_a_macro_argument_is_not_reported_as_unused() {
        let mut objected = Series::new();
        objected.set("crates/language/Cargo.toml#tree-sitter-yaml", 1);
        let sources = BTreeMap::from([(
            "crates/language/Cargo.toml".to_string(),
            vec!["conditional_lang!(tree_sitter_yaml, \"tree-sitter-yaml\")\n".to_string()],
        )]);
        assert_eq!(unnamed_anywhere(&objected, &sources), Series::new());
    }

    #[test]
    fn a_dependency_the_crates_source_never_names_is_left_exactly_as_the_tool_reported_it() {
        let mut objected = Series::new();
        objected.set("Cargo.toml#regex", 1);
        let sources = BTreeMap::from([(
            "Cargo.toml".to_string(),
            vec!["fn f() { serde_json::from_str(\"{}\"); }\n".to_string()],
        )]);
        assert_eq!(
            unnamed_anywhere(&objected, &sources).get("Cargo.toml#regex"),
            Some(1)
        );
    }

    #[test]
    fn every_manifest_a_run_objected_about_is_named_once_whatever_it_objected_to() {
        let mut objected = Series::new();
        objected.set("crates/a/Cargo.toml#serde", 1);
        objected.set("crates/a/Cargo.toml#regex", 1);
        objected.set("crates/b/Cargo.toml#serde", 1);
        let named: Vec<String> = manifests_in(&objected).into_iter().collect();
        assert_eq!(named, ["crates/a/Cargo.toml", "crates/b/Cargo.toml"]);
    }

    #[test]
    fn a_manifest_owns_the_files_cargo_compiles_as_part_of_its_own_crate() {
        assert!(owns("crates/a/Cargo.toml", "crates/a/src/lib.rs"));
        assert!(owns("crates/a/Cargo.toml", "crates/a/build.rs"));
        assert!(owns("crates/a/Cargo.toml", "crates/a/tests/end_to_end.rs"));
        assert!(owns("Cargo.toml", "src/main.rs"));
        assert!(!owns("Cargo.toml", "crates/a/src/lib.rs"));
        assert!(!owns("crates/a/Cargo.toml", "crates/b/src/lib.rs"));
        // Beside the manifest but in none of cargo's source directories.
        assert!(!owns("crates/a/Cargo.toml", "crates/a/notes.rs"));
    }

    /// An empty name would match every source and so forgive the finding.
    #[test]
    fn a_finding_naming_no_dependency_is_never_forgiven() {
        assert!(!names_crate(&["fn f() {}\n".to_string()], ""));
        let mut objected = Series::new();
        objected.set("Cargo.toml", 1);
        let sources = BTreeMap::from([("Cargo.toml".to_string(), vec!["fn f() {}\n".to_string()])]);
        assert_eq!(
            unnamed_anywhere(&objected, &sources).get("Cargo.toml"),
            Some(1)
        );
    }

    #[test]
    fn a_longer_name_that_starts_with_the_dependency_does_not_forgive_it() {
        assert!(!names_crate(
            &["use serde_json::Value;\n".to_string()],
            "serde"
        ));
        assert!(names_crate(
            &["use serde::Serialize;\n".to_string()],
            "serde"
        ));
        assert!(names_crate(&["#[serde(default)]\n".to_string()], "serde"));
    }

    #[test]
    fn a_dependency_named_only_inside_a_string_literal_still_counts_as_reached() {
        let named = ["#[serde(with = \"serde_regex\")]\n".to_string()];
        assert!(names_crate(&named, "serde-regex"));
    }

    #[test]
    fn a_manifest_with_no_sources_read_for_it_keeps_every_finding_it_had() {
        let mut objected = Series::new();
        objected.set("Cargo.toml#regex", 1);
        let read = unnamed_anywhere(&objected, &BTreeMap::new());
        assert_eq!(read.get("Cargo.toml#regex"), Some(1));
    }

    /// The resolved graph of a crate whose manifest names `syn`, and `proc-macro2` with `on`.
    fn graph(on: &str, rename: &str) -> String {
        format!(
            r#"{{"workspace_members":["app"],
            "packages":[{{"id":"app","name":"app","manifest_path":"/w/proj/crates/app/Cargo.toml",
              "dependencies":[{{"name":"syn","rename":null,"features":[]}},
                {{"name":"proc-macro2","rename":{rename},"features":[{on}]}},
                {{"name":"memchr","rename":null,"features":["std"]}}]}},
              {{"id":"syn","name":"syn","manifest_path":"/r/syn/Cargo.toml","dependencies":[]}},
              {{"id":"pm","name":"proc-macro2","manifest_path":"/r/pm/Cargo.toml","dependencies":[]}},
              {{"id":"memchr","name":"memchr","manifest_path":"/r/memchr/Cargo.toml","dependencies":[]}}],
            "resolve":{{"nodes":[{{"id":"app","deps":[{{"pkg":"syn"}},{{"pkg":"pm"}},{{"pkg":"memchr"}}]}},
              {{"id":"syn","deps":[{{"pkg":"pm"}}]}},{{"id":"pm","deps":[]}},{{"id":"memchr","deps":[]}}]}}}}"#
        )
    }

    /// Contract: the graph is read only for an entry, and an entry it cannot judge stays.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_resolved_graph_is_read_only_for_an_entry_left_to_judge() {
        assert_eq!(without_unified(Series::new(), root()), Ok(Series::new()));
        let mut ghost = Series::new();
        ghost.set("Cargo.toml#ghost", 1);
        assert!(
            without_unified(ghost.clone(), root()).is_err(),
            "/w/proj holds no project"
        );
        let manifest = "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n\
                        edition = \"2021\"\n[workspace]\n";
        let tree = crate::testdir::tree(
            "machete-without-unified",
            &[("Cargo.toml", manifest), ("src/lib.rs", "")],
        );
        assert_eq!(without_unified(ghost.clone(), &tree), Ok(ghost));
    }

    /// A member with no manifest, an entry with no name and a package reached twice each stop
    /// only their own part of the walk.
    #[test]
    fn a_part_of_the_graph_chock_cannot_read_or_has_walked_skips_only_that_part() {
        let resolved = r#"{"workspace_members":["odd","app"],
            "packages":[{"id":"odd","name":"odd","dependencies":[]},
              {"id":"app","name":"app","manifest_path":"/w/proj/crates/app/Cargo.toml",
              "dependencies":[{"features":["x"]},{"name":"pm","rename":null,"features":["y"]}]},
              {"id":"a","name":"a"},{"id":"b","name":"b"},{"id":"c","name":"c"},
              {"id":"d","name":"d"},{"id":"pm","name":"pm"}],
            "resolve":{"nodes":[{"id":"app","deps":[{"pkg":"a"},{"pkg":"b"},{"pkg":"pm"}]},
              {"id":"a","deps":[{"pkg":"d"},{"pkg":"c"}]},{"id":"b","deps":[{"pkg":"c"}]},
              {"id":"c","deps":[]},{"id":"d","deps":[{"pkg":"pm"}]},{"id":"pm","deps":[]}]}}"#;
        assert_eq!(
            unified(resolved, root()).unwrap(),
            BTreeSet::from(["crates/app/Cargo.toml#pm".to_string()])
        );
    }

    /// An entry with features, whose package another dependency also pulls in, is used: Cargo
    /// unifies the features. One with no features stays unused. Mutant: drop the features test.
    #[test]
    fn an_entry_that_only_turns_on_features_of_a_package_another_dependency_pulls_in_is_used() {
        let used = unified(&graph(r#""span-locations""#, "null"), root()).unwrap();
        assert_eq!(
            used,
            BTreeSet::from(["crates/app/Cargo.toml#proc-macro2".to_string()])
        );
        let renamed = unified(&graph(r#""span-locations""#, r#""pm2""#), root()).unwrap();
        assert_eq!(
            renamed,
            BTreeSet::from(["crates/app/Cargo.toml#pm2".to_string()])
        );
        // No features: the entry adds nothing, and `memchr` has features but nothing else reaches it.
        assert!(unified(&graph("", "null"), root()).unwrap().is_empty());
        assert!(unified("{}", root()).is_err());
    }
}
