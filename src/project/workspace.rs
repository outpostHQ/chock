//! What `cargo metadata` says about a workspace: one set of types every gate reads it through.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The whole answer of `cargo metadata`, as far as a gate reads it.
#[derive(Debug, Default, Deserialize)]
pub struct Metadata {
    /// The directory that holds the workspace's root `Cargo.toml`.
    #[serde(default)]
    pub workspace_root: String,
    /// The package id of each workspace member.
    #[serde(default)]
    pub workspace_members: Vec<String>,
    /// Every package in the dependency graph, the members included.
    pub packages: Vec<Package>,
}

/// One package: a workspace member or a dependency.
#[derive(Debug, Default, Deserialize)]
pub struct Package {
    /// The id that `workspace_members` and the dependency graph name it by.
    pub id: String,
    /// The name its `Cargo.toml` gives it.
    #[serde(default)]
    pub name: String,
    /// The path of its `Cargo.toml`.
    #[serde(default)]
    pub manifest_path: String,
    /// Each feature, with the features and dependencies it turns on.
    #[serde(default)]
    pub features: BTreeMap<String, Vec<String>>,
    /// The dependencies its `Cargo.toml` declares.
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    /// Its library, binaries, tests, examples, benches and build script.
    #[serde(default)]
    pub targets: Vec<Target>,
    /// The Rust release its `rust-version` promises, as Cargo wrote it.
    #[serde(default)]
    pub rust_version: Option<String>,
    /// The registries it may go to; `Some(vec![])` is `publish = false`.
    #[serde(default)]
    pub publish: Option<Vec<String>>,
}

/// One dependency as a `Cargo.toml` declares it.
#[derive(Debug, Default, Deserialize)]
pub struct Dependency {
    /// The name of the package it asks for.
    pub name: String,
    /// The version requirement, such as `^1.0`.
    #[serde(default)]
    pub req: String,
    /// Where it comes from, such as a registry; `None` for a path dependency.
    #[serde(default)]
    pub source: Option<String>,
    /// The name the code uses for it, where `package = "..."` renames it.
    #[serde(default)]
    pub rename: Option<String>,
    /// Whether only a feature turns it on.
    #[serde(default)]
    pub optional: bool,
    /// `None` for a normal dependency, else `dev` or `build`.
    #[serde(default)]
    pub kind: Option<String>,
    /// The features of it that this dependency turns on.
    #[serde(default)]
    pub features: Vec<String>,
    /// Whether it keeps the default features of the package.
    #[serde(default)]
    pub uses_default_features: bool,
}

/// One target of a package, such as its library or a test.
#[derive(Debug, Default, Deserialize)]
pub struct Target {
    /// The target's name, such as the binary's or the test file's.
    #[serde(default)]
    pub name: String,
    /// What it builds, such as `lib`, `bin`, `test` or `custom-build`.
    #[serde(default)]
    pub kind: Vec<String>,
    /// The features it needs before cargo builds it.
    #[serde(default, rename = "required-features")]
    pub required_features: Vec<String>,
    /// cargo omits `doc` for a target rustdoc documents, which is every one not opted out.
    #[serde(default = "documented")]
    pub doc: bool,
}

fn documented() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_cargo_leaves_out_takes_its_default_and_doc_defaults_to_documented() {
        let read: Metadata = serde_json::from_str(
            r#"{"packages":[{"id":"a","targets":[{"name":"a","kind":["lib"]}],
               "dependencies":[{"name":"serde","kind":"dev"}]}]}"#,
        )
        .unwrap();
        let package = &read.packages[0];
        assert_eq!((package.id.as_str(), package.publish.clone()), ("a", None));
        assert_eq!(package.targets[0].kind, ["lib"]);
        assert!(package.targets[0].doc);
        assert_eq!(package.dependencies[0].kind.as_deref(), Some("dev"));
        assert_eq!(read.workspace_members, Vec::<String>::new());
    }
}
