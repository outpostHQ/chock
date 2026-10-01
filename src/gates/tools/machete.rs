//! Dependencies a crate declares and does not use, as `cargo machete` reports them, less any the
//! crate's sources name where machete does not look, such as a macro argument.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::exec;
use crate::project;
use crate::run::Ctx;
use crate::run::baseline::Series;
use crate::run::report::Finding;

pub fn unused(ctx: &Ctx) -> Result<Series, String> {
    // machete honours `.gitignore` only inside git, so `target/` is skipped explicitly.
    let out = exec::tool(&ctx.root, "cargo", &["machete", "--skip-target-dir"])?;
    let objected = read_machete(&out, &ctx.root)?;
    let sources = sources_for(ctx, &objected)?;
    Ok(unnamed_anywhere(&objected, &sources))
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
        &|name, _| name.ends_with(".rs"),
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
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new("/w/proj")
    }

    fn ran(stdout: &str, code: Option<i32>, truncated: bool) -> exec::Output {
        exec::Output {
            code,
            stdout: stdout.to_string(),
            stderr: "it broke".to_string(),
            truncated,
        }
    }

    fn ctx_of(files: &[(&str, &str)]) -> crate::testdir::Held {
        let dir = crate::testdir::make("gate-machete");
        for (name, text) in files {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let held = Ctx::for_root(
            dir.to_path_buf(),
            crate::run::baseline::Baseline::empty("0.1.0"),
        );
        crate::testdir::Held::new(dir, held)
    }

    #[test]
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
            false,
        );
        assert_eq!(read_machete(&out, root()).unwrap(), Series::new());
    }

    #[test]
    fn an_unused_dependency_is_keyed_by_the_manifest_that_declares_it_and_the_name() {
        let out = ran("chock -- /w/proj/Cargo.toml:\n\tserde\n\n", Some(1), false);
        let series = read_machete(&out, root()).unwrap();
        assert_eq!(series.get("Cargo.toml#serde"), Some(1));
    }

    #[test]
    fn output_machete_had_cut_short_stops_the_gate_rather_than_reporting_half() {
        let out = ran("chock -- /w/proj/Cargo.toml:\n\tserde\n", Some(1), true);
        let err = read_machete(&out, root()).unwrap_err();
        assert!(err.contains("more than chock keeps"), "{err}");
    }

    #[test]
    fn a_tool_that_objected_and_named_no_dependency_measured_nothing() {
        let out = ran(
            "cargo-machete found the following unused dep",
            Some(1),
            false,
        );
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
}
