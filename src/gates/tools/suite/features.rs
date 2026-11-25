//! The configured features that a run of one package keeps. cargo refuses a feature that no
//! selected package can take, so the workspace's whole list stops a run of one package.

use crate::project::workspace::{Dependency, Metadata, Package};

/// The options a run of `package` passes: `--package` for `--workspace` or `--all`, and each feature
/// list cut to what it can take. The members are read only where there is a list to cut.
pub(super) fn options(
    options: &[String],
    package: Option<&str>,
    members: impl FnOnce() -> Result<Metadata, String>,
) -> Result<Vec<String>, String> {
    let Some(package) = package else {
        return Ok(options.to_vec());
    };
    let whole = |arg: &&String| matches!(arg.as_str(), "--workspace" | "--all");
    let kept: Vec<String> = options.iter().filter(|arg| !whole(arg)).cloned().collect();
    let listed = kept
        .iter()
        .any(|arg| arg.starts_with("--features") || arg == "-F");
    let kept = if listed {
        for_package(kept, package, &members()?)
    } else {
        kept
    };
    Ok([vec!["--package".to_string(), package.to_string()], kept].concat())
}

/// `options` with each feature list cut to the features that the member `package` names can take,
/// by cargo's rule. Unchanged where no member has that name, so that cargo's own error names it.
fn for_package(options: Vec<String>, package: &str, metadata: &Metadata) -> Vec<String> {
    let name = package.split('@').next().unwrap_or(package);
    let members = &metadata.workspace_members;
    let selected: Vec<&Package> = (metadata.packages.iter())
        .filter(|it| it.name == name && (members.is_empty() || members.contains(&it.id)))
        .collect();
    if selected.is_empty() {
        return options;
    }
    let kept = |list: &str| -> String {
        let named = list.split([',', ' ']).filter(|it| !it.is_empty());
        let taken = named.filter(|it| selected.iter().any(|member| takes(member, it)));
        taken.collect::<Vec<&str>>().join(",")
    };
    let mut narrowed = Vec::new();
    let mut words = options.into_iter();
    while let Some(word) = words.next() {
        if let Some(list) = word.strip_prefix("--features=") {
            narrowed.extend(flagged("--features", &kept(list)));
        } else if matches!(word.as_str(), "--features" | "-F") {
            let list = words.next().unwrap_or_default();
            narrowed.extend(flagged(&word, &kept(&list)));
        } else {
            narrowed.push(word);
        }
    }
    narrowed
}

/// The flag and its list, or nothing where no feature is left: cargo needs no empty list.
fn flagged(flag: &str, list: &str) -> Vec<String> {
    if list.is_empty() {
        return Vec::new();
    }
    vec![flag.to_string(), list.to_string()]
}

/// Whether cargo takes `feature` for `member`: a feature or optional dependency of its own, the
/// same as `member/feat`, or `dep/feat` for a dependency that it declares.
fn takes(member: &Package, feature: &str) -> bool {
    let own = |name: &str| {
        member.features.contains_key(name)
            || (member.dependencies.iter()).any(|dep| dep.optional && in_toml(dep) == name)
    };
    let Some((dep, feat)) = feature.split_once('/') else {
        return own(feature);
    };
    // `dep?/feat` turns `feat` on only where something else turns `dep` on.
    let dep = dep.strip_suffix('?').unwrap_or(dep);
    (member.dependencies.iter()).any(|it| in_toml(it) == dep) || (dep == member.name && own(feat))
}

/// The name a manifest gives a dependency, which a feature uses: its rename, else its package.
fn in_toml(dep: &Dependency) -> &str {
    dep.rename.as_deref().unwrap_or(&dep.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace shaped like corbin's, as `cargo metadata --no-deps` reads it: `corbin` depends
    /// on `corbin-ui`, `corbin-ts` depends on neither, and `corbin-ui` has `test`.
    fn corbin() -> Metadata {
        serde_json::from_str(
            r#"{"workspace_members":["c","ui","ts"],"packages":[
                {"id":"c","name":"corbin","features":{"server":[],"test":[],"content":[]},
                 "dependencies":[{"name":"corbin-ui","kind":null},
                                 {"name":"tokio","rename":"rt","optional":true}]},
                {"id":"ui","name":"corbin-ui","features":{"test":[]},"dependencies":[]},
                {"id":"ts","name":"corbin-ts","features":{"docs":[]},
                 "dependencies":[{"name":"serde","kind":"dev"}]}]}"#,
        )
        .unwrap()
    }

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
    }

    const ASKED: &str = "--no-fail-fast --features corbin/server,corbin/test,corbin-ui/test,docs";

    #[test]
    fn one_package_keeps_only_the_features_cargo_lets_through_for_it() {
        let read = corbin();
        let both = "--no-fail-fast --features corbin/server,corbin/test,corbin-ui/test";
        let cases = [
            (ASKED, "corbin-ts", "--no-fail-fast --features docs"),
            (ASKED, "corbin", both),
            (
                ASKED,
                "corbin-ui@0.1.0",
                "--no-fail-fast --features corbin-ui/test",
            ),
            ("-F corbin/server --locked", "corbin-ts", "--locked"),
            (
                "--features=serde/derive,rt",
                "corbin-ts",
                "--features serde/derive",
            ),
            (ASKED, "nowhere", ASKED),
            (
                "--features rt,corbin/rt,rt?/full,x",
                "corbin",
                "--features rt,corbin/rt,rt?/full",
            ),
        ];
        for (asked, package, kept) in cases {
            let narrowed = for_package(words(asked), package, &read);
            assert_eq!(narrowed, words(kept), "{asked} under --package {package}");
        }
    }
}
