//! What `cargo metadata` says about a workspace: one set of types every gate reads it through.

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct Metadata {
    #[serde(default)]
    pub workspace_root: String,
    #[serde(default)]
    pub workspace_members: Vec<String>,
    pub packages: Vec<Package>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Package {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub manifest_path: String,
    #[serde(default)]
    pub features: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub targets: Vec<Target>,
    /// The registries it may go to; `Some(vec![])` is `publish = false`.
    #[serde(default)]
    pub publish: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Dependency {
    pub name: String,
    #[serde(default)]
    pub req: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub rename: Option<String>,
    #[serde(default)]
    pub optional: bool,
    /// `None` for a normal dependency, else `dev` or `build`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub uses_default_features: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct Target {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: Vec<String>,
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
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
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
