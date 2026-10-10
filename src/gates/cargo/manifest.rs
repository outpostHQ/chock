//! The `manifest` gate: every dependency names an immutable version or revision. Existing unpinned
//! ones are debt keyed by package and dependency, so accepting one cannot hide a new one.

use crate::project::workspace::{Dependency, Metadata};

use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Inspection, Kind};

pub const GATE: Gate = Gate {
    name: "manifest",
    about: "every dependency names an immutable version or revision",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Debt {
        inspect,
        unit: "unpinned dependency declaration(s)",
    },
};

const MANIFEST: &str = "Cargo.toml";

impl Dependency {
    /// The name the manifest uses: the rename when there is one.
    fn key(&self) -> &str {
        self.rename.as_deref().unwrap_or(&self.name)
    }
}

fn inspect(ctx: &Ctx) -> Result<Inspection, String> {
    faults(&crate::project::metadata(&ctx.root)?).map(Inspection::debt)
}

/// The unpinned dependencies of workspace members, read from `cargo metadata` rather than the TOML
/// so inline tables, `[dependencies.x]` sections and workspace inheritance arrive resolved.
pub fn faults(metadata_json: &str) -> Result<Vec<Finding>, String> {
    let metadata: Metadata = serde_json::from_str(metadata_json)
        .map_err(|e| format!("cargo metadata produced something unreadable: {e}"))?;
    let mut found = Vec::new();
    // A transitive dependency's manifest is not this project's to fix.
    for package in metadata
        .packages
        .iter()
        .filter(|p| metadata.workspace_members.contains(&p.id))
    {
        for dependency in &package.dependencies {
            if let Some(problem) = fault_in(dependency) {
                found.push(Finding::at(MANIFEST, &problem).item(&format!(
                    "{}: {}",
                    package.name,
                    dependency.key()
                )));
            }
        }
    }
    Ok(found)
}

fn fault_in(dependency: &Dependency) -> Option<String> {
    let source = dependency.source.as_deref().unwrap_or_default();
    if source.starts_with("git+") && !pinned_to_a_revision(source) {
        return Some(format!(
            "git dependency \"{}\" names no full commit revision, and a branch or tag can be force-pushed",
            dependency.key()
        ));
    }
    if source.starts_with("registry+") && dependency.req == "*" {
        return Some(format!(
            "registry dependency \"{}\" accepts any version, so two machines can resolve it differently",
            dependency.key()
        ));
    }
    None
}

/// True when a git source's only query parameter is `rev` holding a full 40-character hash. A short
/// rev is ambiguous, and a branch beside it can move.
#[must_use]
pub fn pinned_to_a_revision(source: &str) -> bool {
    let Some((_, query_and_fragment)) = source.split_once('?') else {
        return false;
    };
    let query = query_and_fragment.split('#').next().unwrap_or_default();
    let mut parameters = query.split('&');
    let Some(parameter) = parameters.next() else {
        return false;
    };
    if parameters.next().is_some() {
        return false;
    }
    let Some(("rev", revision)) = parameter.split_once('=') else {
        return false;
    };
    revision.len() == 40 && revision.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    const REV: &str = "0123456789abcdef0123456789abcdef01234567";

    fn metadata(dependencies: &str) -> String {
        format!(
            r#"{{"packages":[{{"id":"me","name":"demo","dependencies":[{dependencies}]}}],
               "workspace_members":["me"]}}"#
        )
    }

    fn dep(name: &str, req: &str, source: &str) -> String {
        format!(r#"{{"name":"{name}","req":"{req}","source":"{source}"}}"#)
    }

    #[test]
    fn a_git_dependency_pinned_to_a_full_revision_passes() {
        let source = format!("git+https://example.com/x?rev={REV}#{REV}");
        assert_eq!(faults(&metadata(&dep("x", "*", &source))).unwrap(), vec![]);
    }

    #[test]
    fn a_git_dependency_on_a_branch_is_reported() {
        let source = "git+https://example.com/x?branch=main#abc";
        let found = faults(&metadata(&dep("x", "^1", source))).unwrap();
        assert_eq!(
            found[0].render(),
            "Cargo.toml: demo: x: git dependency \"x\" names no full commit revision, \
             and a branch or tag can be force-pushed"
        );
    }

    #[test]
    fn a_git_dependency_with_no_query_at_all_is_reported() {
        let found = faults(&metadata(&dep("x", "^1", "git+https://example.com/x"))).unwrap();
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn a_short_revision_is_not_a_pin() {
        assert!(!pinned_to_a_revision("git+https://e.com/x?rev=0123456"));
    }

    #[test]
    fn a_revision_that_is_not_hexadecimal_is_not_a_pin() {
        let source = format!("git+https://e.com/x?rev={}", "z".repeat(40));
        assert!(!pinned_to_a_revision(&source));
    }

    #[test]
    fn a_revision_alongside_a_branch_is_not_a_pin_because_the_branch_moves() {
        let source = format!("git+https://e.com/x?branch=main&rev={REV}");
        assert!(!pinned_to_a_revision(&source));
    }

    #[test]
    fn a_registry_dependency_accepting_any_version_is_reported() {
        let found = faults(&metadata(&dep("x", "*", "registry+https://crates.io"))).unwrap();
        assert!(
            found[0].render().contains("accepts any version"),
            "{found:?}"
        );
    }

    #[test]
    fn a_registry_dependency_with_a_real_requirement_passes() {
        let found = faults(&metadata(&dep("x", "^1.2", "registry+https://crates.io"))).unwrap();
        assert_eq!(found, vec![]);
    }

    #[test]
    fn a_path_dependency_carries_no_source_and_is_not_judged() {
        let text = metadata(r#"{"name":"x","req":"*","source":null}"#);
        assert_eq!(faults(&text).unwrap(), vec![]);
    }

    #[test]
    fn a_renamed_dependency_is_reported_under_the_name_the_manifest_uses() {
        let text = metadata(
            r#"{"name":"real","req":"*","source":"registry+https://crates.io","rename":"alias"}"#,
        );
        let found = faults(&text).unwrap();
        assert_eq!(found[0].item.as_deref(), Some("demo: alias"));
    }

    #[test]
    fn a_dependency_of_a_package_outside_the_workspace_is_not_this_projects_problem() {
        let text = format!(
            r#"{{"packages":[{{"id":"other","name":"vendored","dependencies":[{}]}}],
               "workspace_members":["me"]}}"#,
            dep("x", "*", "registry+https://crates.io")
        );
        assert_eq!(faults(&text).unwrap(), vec![]);
    }

    #[test]
    fn every_offending_dependency_is_reported_not_only_the_first() {
        let text = metadata(&format!(
            "{},{}",
            dep("a", "*", "registry+https://crates.io"),
            dep("b", "^1", "git+https://e.com/b?branch=main")
        ));
        let found = faults(&text).unwrap();
        assert_eq!(
            found
                .iter()
                .filter_map(|f| f.item.clone())
                .collect::<Vec<_>>(),
            vec!["demo: a".to_string(), "demo: b".to_string()]
        );
    }

    #[test]
    fn declared_dependency_debt_has_stable_keys_without_a_line_number() {
        let text = metadata(&dep("unpinned", "*", "registry+https://crates.io"));
        let inspected = Inspection::debt(faults(&text).unwrap());
        assert_eq!(inspected.blockers, Vec::new());
        assert_eq!(inspected.debt[0].item.as_deref(), Some("demo: unpinned"));
        assert_eq!(GATE.counts_in(), Some("unpinned dependency declaration(s)"));
    }

    #[test]
    fn manifest_metadata_that_is_not_json_is_refused_rather_than_read_as_clean() {
        assert!(faults("not json").is_err());
    }
}
