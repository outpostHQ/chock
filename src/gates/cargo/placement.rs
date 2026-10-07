//! The `placement` gate: each dependency is declared where a fresh clone can reach it, and in the
//! section for the builds that use it. Either mistake builds locally and breaks elsewhere.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::gates::tools::machete::names_crate;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Inspection, Kind};

pub const GATE: Gate = Gate {
    name: "placement",
    about: "every dependency is declared where a clone can build it, for the builds that use it",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Debt {
        inspect,
        unit: "misplaced dependency declaration(s)",
    },
};

fn inspect(ctx: &Ctx) -> Result<Inspection, String> {
    let read = crate::project::metadata(&ctx.root)?;
    let repository = crate::project::vcs::repository_root(&ctx.root, ctx.vcs);
    let mut found = outside(&read, repository.as_deref())?;
    found.extend(test_only(&crates(&ctx.root, &read)?));
    Ok(Inspection::debt(found))
}

/// Path dependencies outside the repository, which build only where the sibling checkout exists.
pub fn outside(metadata_json: &str, repository: Option<&Path>) -> Result<Vec<Finding>, String> {
    let Some(repository) = repository else {
        return Ok(Vec::new());
    };
    let metadata = parsed(metadata_json)?;
    Ok(members(&metadata)
        .flat_map(|package| {
            let manifest = crate::project::relative(repository, &manifest_of(package));
            let name = package["name"].as_str().unwrap_or_default().to_string();
            dependencies(package)
                .filter_map(|dep| Some((key(dep), dep["path"].as_str()?)))
                .filter(|(_, path)| !Path::new(path).starts_with(repository))
                .map(move |(dep, path)| {
                    Finding::at(
                        &manifest,
                        &format!(
                            "path dependency \"{dep}\" is at {path}, outside the repository, so a \
                             clone cannot build it"
                        ),
                    )
                    .item(&format!("{name}: {dep}"))
                })
        })
        .collect())
}

/// Parsed as a `Value`: derived structs for this rule would exceed the binary's size budget.
fn parsed(metadata_json: &str) -> Result<Value, String> {
    serde_json::from_str(metadata_json)
        .map_err(|e| format!("cargo metadata produced something unreadable: {e}"))
}

fn members(metadata: &Value) -> impl Iterator<Item = &Value> {
    let ids = metadata["workspace_members"].as_array();
    metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(move |package| ids.is_some_and(|ids| ids.contains(&package["id"])))
}

fn dependencies(package: &Value) -> impl Iterator<Item = &Value> {
    package["dependencies"].as_array().into_iter().flatten()
}

fn manifest_of(package: &Value) -> PathBuf {
    PathBuf::from(package["manifest_path"].as_str().unwrap_or_default())
}

/// The name the manifest uses for a dependency: the rename when there is one.
fn key(dependency: &Value) -> String {
    dependency["rename"]
        .as_str()
        .or_else(|| dependency["name"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// A workspace member: its `[dependencies]` names and its source text, split into production and
/// test code.
#[derive(Debug, Default)]
pub struct Crate {
    pub manifest: String,
    pub package: String,
    pub normal: Vec<String>,
    pub production: Vec<String>,
    pub tests: Vec<String>,
}

/// `[dependencies]` entries only test code names. One nothing names is left to the `unused` gate.
#[must_use]
pub fn test_only(crates: &[Crate]) -> Vec<Finding> {
    crates
        .iter()
        .flat_map(|held| {
            held.normal
                .iter()
                .filter(|dep| !names_crate(&held.production, dep) && names_crate(&held.tests, dep))
                .map(|dep| {
                    Finding::at(
                        &held.manifest,
                        &format!(
                            "\"{dep}\" is declared for every build but only test code names it; \
                             it belongs in [dev-dependencies], or nowhere"
                        ),
                    )
                    .item(&format!("{}: {dep}", held.package))
                })
        })
        .collect()
}

/// Every workspace member with its sources sorted: a file goes to the deepest crate holding it, and
/// `tests/`, `benches/`, `examples/`, a test-only file and a `#[cfg(test)]` module are test code.
pub fn crates(root: &Path, metadata_json: &str) -> Result<Vec<Crate>, String> {
    let metadata = parsed(metadata_json)?;
    let members: Vec<&Value> = members(&metadata).collect();
    let manifests: Vec<PathBuf> = members.iter().map(|package| manifest_of(package)).collect();
    let dirs: Vec<PathBuf> = manifests
        .iter()
        .map(|manifest| manifest.parent().map(Path::to_path_buf).unwrap_or_default())
        .collect();
    let files = rust_files(root)?;
    let mut read = Vec::new();
    for ((package, manifest), dir) in members.iter().zip(&manifests).zip(&dirs) {
        let mut held = Crate {
            manifest: crate::project::relative(root, manifest),
            package: package["name"].as_str().unwrap_or_default().to_string(),
            // `kind` is `null` for `[dependencies]`, `dev` or `build` for the others.
            normal: dependencies(package)
                .filter(|dep| dep["kind"].is_null())
                .map(key)
                .collect(),
            ..Crate::default()
        };
        for file in files.iter().filter(|file| owner(file, &dirs) == Some(dir)) {
            sort(&mut held, dir, file)?;
        }
        read.push(held);
    }
    Ok(read)
}

fn rust_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    crate::project::walk(
        root,
        &|name| !crate::project::SKIPPED.contains(&name),
        &|name, _| name.ends_with(".rs"),
    )
}

/// The deepest crate directory holding a file, so a nested crate's sources are never its parent's.
fn owner<'a>(file: &Path, dirs: &'a [PathBuf]) -> Option<&'a PathBuf> {
    dirs.iter()
        .filter(|dir| file.starts_with(dir))
        .max_by_key(|dir| dir.components().count())
}

/// Adds a file's text to the crate's production or test code. A file that does not parse counts as
/// production, where it can hide a finding but never make one.
fn sort(held: &mut Crate, dir: &Path, file: &Path) -> Result<(), String> {
    let src = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let within = file.strip_prefix(dir).unwrap_or(file);
    let first = within
        .components()
        .next()
        .map(|part| part.as_os_str().to_string_lossy());
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let testing = matches!(first.as_deref(), Some("tests" | "benches" | "examples"))
        || crate::gates::metrics::prodlines::is_test_file(&name);
    if testing {
        held.tests.push(src);
        return Ok(());
    }
    match crate::gates::metrics::prodlines::split(&src) {
        Ok((production, tests)) => {
            held.production.push(production);
            held.tests.push(tests);
        }
        Err(_) => held.production.push(src),
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

    fn held(normal: &[&str], production: &str, tests: &str) -> Crate {
        Crate {
            manifest: "crates/a/Cargo.toml".to_string(),
            package: "a".to_string(),
            normal: normal.iter().map(|dep| (*dep).to_string()).collect(),
            production: vec![production.to_string()],
            tests: vec![tests.to_string()],
        }
    }

    #[test]
    fn a_dependency_only_tests_name_is_reported_and_one_production_names_is_not() {
        let found: Vec<String> = test_only(&[held(
            &["serde", "tempfile", "never-named"],
            "use serde::Serialize;\n",
            "use tempfile::tempdir;\nuse serde_json as _;\n",
        )])
        .iter()
        .map(Finding::render)
        .collect();
        assert_eq!(
            found,
            [
                "crates/a/Cargo.toml: a: tempfile: \"tempfile\" is declared for every build but only \
              test code names it; it belongs in [dev-dependencies], or nowhere"
            ]
        );
    }

    #[test]
    fn a_path_dependency_outside_the_repository_is_one_a_clone_cannot_build() {
        let text = r#"{"workspace_members":["me"],"packages":[{"id":"me","name":"demo",
          "manifest_path":"/work/app/crates/demo/Cargo.toml","dependencies":[
            {"name":"core","path":"/work/lib/crates/core"},
            {"name":"sibling","path":"/work/app/crates/sibling"},
            {"name":"serde","kind":null}]}]}"#;
        let repository = Path::new("/work/app");
        let found: Vec<String> = outside(text, Some(repository))
            .unwrap()
            .iter()
            .map(Finding::render)
            .collect();
        assert_eq!(
            found,
            [
                "crates/demo/Cargo.toml: demo: core: path dependency \"core\" is at \
              /work/lib/crates/core, outside the repository, so a clone cannot build it"
            ]
        );
        assert_eq!(
            outside(text, None).unwrap(),
            vec![],
            "no repository, nothing to say"
        );
        assert!(outside("not json", Some(repository)).is_err());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn test_modules_test_directories_and_nested_crates_are_sorted_where_they_belong() {
        let dir = crate::testdir::make("placement-sorted");
        for (path, text) in [
            (
                "src/lib.rs",
                "use serde::S;\n#[cfg(test)]\nmod tests {\n    use tempfile::t;\n}\n",
            ),
            ("src/broken.rs", "fn (\n"),
            ("tests/it.rs", "use proptest::p;\n"),
            ("inner/src/lib.rs", "use tempfile::t;\n"),
        ] {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let metadata = format!(
            r#"{{"workspace_members":["a","i"],"packages":[
              {{"id":"a","name":"a","manifest_path":"{0}/Cargo.toml","dependencies":[
                {{"name":"serde","kind":null}},{{"name":"tempfile","kind":null}},
                {{"name":"proptest","kind":null}},{{"name":"criterion","kind":"dev"}}]}},
              {{"id":"i","name":"i","manifest_path":"{0}/inner/Cargo.toml","dependencies":[
                {{"name":"tempfile","kind":null}}]}}]}}"#,
            dir.display().to_string().replace('\\', "\\\\")
        );
        let read = crates(&dir, &metadata).unwrap();
        let found: Vec<Option<String>> = test_only(&read).iter().map(|f| f.item.clone()).collect();
        assert_eq!(
            found,
            [
                Some("a: tempfile".to_string()),
                Some("a: proptest".to_string())
            ]
        );
        assert!(crates(&dir, "not json").is_err());
    }
}
