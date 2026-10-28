//! Every rule over the tracked tree: secret-bearing names, credential literals, hidden
//! characters, `.gitmodules` values, links into `.git`, and tracked build output.

use std::path::PathBuf;

use super::{Credential, credential_in, git, hidden, holds};
use crate::run::report::Finding;

/// Cargo's default build directory; a `build.target-dir` override is not read.
const BUILD_DIR: &str = "target";

/// The tree as the rules see it: each file's text, whether a path is still on disk (a missing one
/// is being removed), and where a tracked link points.
pub(super) struct Tree<'a> {
    pub(super) read: &'a dyn Fn(&str) -> Option<String>,
    pub(super) present: &'a dyn Fn(&str) -> bool,
    pub(super) link: &'a dyn Fn(&str) -> Option<PathBuf>,
}

/// `faults` over a whole `Tree`, also told which system holds it (so a fix names its command).
pub(super) fn faults_of(
    paths: &[String],
    tree: &Tree<'_>,
    held: Option<crate::project::vcs::Kind>,
    declared: &[Credential<'_>],
) -> Vec<Finding> {
    let mut found = Vec::new();
    for path in paths {
        // A file reported by name is not also scanned: the fix is the same whatever it holds.
        if let Some(finding) = secret_file(path, held) {
            found.push(finding);
            continue;
        }
        if let Some(target) = (tree.link)(path) {
            found.extend(git::link(path, &target));
            continue;
        }
        // Test material is skipped, as this gate has no way to suppress a match.
        let Some(text) = (tree.read)(path).filter(|_| !non_production(path)) else {
            continue;
        };
        found.extend(credentials_in(path, &text, declared));
        found.extend(hidden::found(path, &text));
        if path == ".gitmodules" {
            found.extend(git::modules(path, &text));
        }
    }
    found.extend(build_output(paths, tree.present, held));
    found
}

/// A file whose name marks it as secret-bearing, reported by name without reading it.
fn secret_file(path: &str, held: Option<crate::project::vcs::Kind>) -> Option<Finding> {
    if non_production(path) || !(is_secret_name(file_name(path)) || is_cargo_credentials(path)) {
        return None;
    }
    Some(Finding::at(
        path,
        &format!(
            "tracked, and the name marks it as secret-bearing: {}, \
             rotate what it holds, and ignore the name",
            untrack_secret(held)
        ),
    ))
}

/// The untrack step for the system holding the tree. `outpost rm` deletes the file, unlike
/// `git rm --cached`, so the secret is copied out first.
fn untrack_secret(held: Option<crate::project::vcs::Kind>) -> &'static str {
    match held {
        Some(crate::project::vcs::Kind::Outpost) => "copy it out, then `outpost rm` it",
        _ => "`git rm --cached` it",
    }
}

/// Whether a file name is on a closed list of secret-bearing names; there is no heuristic.
fn is_secret_name(name: &str) -> bool {
    if is_template(name) {
        return false;
    }
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name == "id_rsa"
        || name == "credentials.json"
        || name == ".git-credentials"
}

/// The registry tokens `cargo login` writes, under `.cargo` with or without `.toml`.
fn is_cargo_credentials(path: &str) -> bool {
    let mut parts = path.rsplit('/');
    matches!(parts.next(), Some("credentials" | "credentials.toml"))
        && parts.next() == Some(".cargo")
}

/// Whether the name marks a committed placeholder: an `example`, `template` or `sample` part.
fn is_template(name: &str) -> bool {
    name.split('.')
        .any(|part| matches!(part, "example" | "template" | "sample"))
}

/// Whether the path is under a test, fixture, example or bench directory, where a key is a fixture.
fn non_production(path: &str) -> bool {
    path.split('/').any(|segment| {
        matches!(
            segment,
            "tests" | "fixtures" | "testdata" | "examples" | "benches"
        )
    })
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Credential literals: a known shape in a configuration file, a declared shape in any file.
/// Templates are skipped, as this gate has no way to suppress a match.
fn credentials_in(path: &str, text: &str, declared: &[Credential<'_>]) -> Vec<Finding> {
    let name = file_name(path);
    let extension = name.rsplit_once('.').map(|(_, extension)| extension);
    let configuration = extension.is_some_and(|extension| CONFIGURATION.contains(&extension));
    if is_template(name) || !configuration && declared.is_empty() {
        return Vec::new();
    }
    let shape = |line: &str| {
        let known: Option<&str> = credential_in(line).filter(|_| configuration);
        let own = || declared.iter().find(|credential| holds(line, credential));
        known.or_else(|| own().map(|credential| credential.name))
    };
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| shape(line).map(|found| (index, found)))
        .map(|(index, found)| {
            Finding::at(path, "a credential literal: remove it and rotate the key")
                .line(u32::try_from(index + 1).unwrap_or(u32::MAX))
                .item(found)
        })
        .collect()
}

/// Configuration file extensions, where a key-shaped string is a key rather than an identifier.
const CONFIGURATION: [&str; 12] = [
    "json",
    "yaml",
    "yml",
    "toml",
    "ini",
    "conf",
    "cfg",
    "properties",
    "tfvars",
    "sh",
    "npmrc",
    "pypirc",
];

/// Tracked build output still on disk, as one finding for the whole directory. A tracked path gone
/// from disk is being removed.
fn build_output(
    paths: &[String],
    present: &dyn Fn(&str) -> bool,
    held: Option<crate::project::vcs::Kind>,
) -> Option<Finding> {
    let prefix = format!("{BUILD_DIR}/");
    let count = paths
        .iter()
        .filter(|path| path.starts_with(&prefix) && present(path))
        .count();
    (count > 0).then(|| {
        Finding::at(
            BUILD_DIR,
            &format!(
                "{} tracks {count} file(s) under the directory cargo builds into: \
                 `{}` and ignore it",
                named(held),
                untracks(held)
            ),
        )
    })
}

/// The system holding the tree, so a finding does not name git in a repository git does not hold.
fn named(held: Option<crate::project::vcs::Kind>) -> &'static str {
    match held {
        Some(crate::project::vcs::Kind::Outpost) => "outpost",
        _ => "git",
    }
}

/// The command that removes build output, for the system holding the tree.
fn untracks(held: Option<crate::project::vcs::Kind>) -> String {
    match held {
        Some(crate::project::vcs::Kind::Outpost) => format!("outpost rm -r {BUILD_DIR}"),
        _ => format!("git rm -r --cached {BUILD_DIR}"),
    }
}
