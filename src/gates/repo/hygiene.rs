//! Secret-bearing files, credential literals and build output among the files the repository
//! tracks.

use std::path::Path;

#[cfg(test)]
use crate::exec;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

pub const GATE: Gate = Gate {
    name: "hygiene",
    about: "the repository tracks a secret-bearing file, a credential literal or build output",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Binary(check),
};

/// The largest file scanned for credentials; anything bigger is data, not configuration.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Cargo's default build directory; a `build.target-dir` override is not read.
const BUILD_DIR: &str = "target";

fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let paths = crate::project::vcs::tracked(&ctx.root, ctx.vcs)?;
    let findings = faults_of(
        &paths,
        &|path| read_text(&ctx.root, path),
        &|path| ctx.root.join(path).exists(),
        ctx.vcs,
    );
    if findings.is_empty() {
        Ok(Outcome::passed())
    } else {
        Ok(Outcome::failed(findings))
    }
}

/// Every rule over a list of tracked paths, with file reading injected so tests need no repository.
#[must_use]
pub fn faults(paths: &[String], read: &dyn Fn(&str) -> Option<String>) -> Vec<Finding> {
    faults_of(paths, read, &|_| true, Some(crate::project::vcs::Kind::Git))
}

/// `faults`, also told which paths are still on disk (a missing one is being removed) and which
/// system holds the tree (so a fix names its command).
#[must_use]
pub fn faults_of(
    paths: &[String],
    read: &dyn Fn(&str) -> Option<String>,
    present: &dyn Fn(&str) -> bool,
    held: Option<crate::project::vcs::Kind>,
) -> Vec<Finding> {
    let mut found = Vec::new();
    for path in paths {
        // A file reported by name is not also scanned: the fix is the same whatever it holds.
        if let Some(finding) = secret_file(path, held) {
            found.push(finding);
            continue;
        }
        found.extend(credentials_in(path, read));
    }
    found.extend(build_output(paths, present, held));
    found
}

/// A file whose name marks it as secret-bearing, reported by name without reading it.
fn secret_file(path: &str, held: Option<crate::project::vcs::Kind>) -> Option<Finding> {
    if non_production(path) || !is_secret_name(file_name(path)) {
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

/// One credential shape: a fixed prefix, a minimum run of admissible bytes after it, and a name.
/// Entropy alone never matches.
struct Credential {
    name: &'static str,
    prefix: &'static str,
    tail: usize,
    admits: fn(u8) -> bool,
}

/// Prefixes with a long fixed tail. `sk-` and the PEM header are left out: both match ordinary
/// source too often.
const CREDENTIALS: [Credential; 4] = [
    Credential {
        name: "AWS access key id",
        prefix: "AKIA",
        tail: 16,
        admits: |byte| byte.is_ascii_uppercase() || byte.is_ascii_digit(),
    },
    Credential {
        name: "GitHub personal access token",
        prefix: "ghp_",
        tail: 36,
        admits: |byte| byte.is_ascii_alphanumeric(),
    },
    Credential {
        name: "GitHub fine-grained token",
        prefix: "github_pat_",
        tail: 22,
        admits: |byte| byte.is_ascii_alphanumeric() || byte == b'_',
    },
    Credential {
        name: "Slack bot token",
        prefix: "xoxb-",
        tail: 10,
        admits: |byte| byte.is_ascii_alphanumeric() || byte == b'-',
    },
];

/// Credential literals in a configuration file. Source is skipped: a match there is rarely a leak,
/// and this gate has no way to suppress one.
fn credentials_in(path: &str, read: &dyn Fn(&str) -> Option<String>) -> Vec<Finding> {
    let name = file_name(path);
    let extension = name.rsplit_once('.').map(|(_, extension)| extension);
    let configuration = extension.is_some_and(|extension| CONFIGURATION.contains(&extension));
    if non_production(path) || is_template(name) || !configuration {
        return Vec::new();
    }
    let Some(text) = read(path) else {
        return Vec::new();
    };
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| credential_in(line).map(|found| (index, found)))
        .map(|(index, found)| {
            Finding::at(path, "a credential literal: remove it and rotate the key")
                .line(u32::try_from(index + 1).unwrap_or(u32::MAX))
                .item(found)
        })
        .collect()
}

/// Configuration file extensions, where a key-shaped string is a key rather than an identifier.
const CONFIGURATION: [&str; 10] = [
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
];

/// The first credential a line holds, by name. The value is never carried out of here.
fn credential_in(line: &str) -> Option<&'static str> {
    CREDENTIALS
        .iter()
        .find(|credential| holds(line, credential))
        .map(|credential| credential.name)
}

/// Whether the line holds this credential: its prefix at a word boundary, with a long enough tail.
fn holds(line: &str, credential: &Credential) -> bool {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(offset) = line[from..].find(credential.prefix) {
        let start = from + offset;
        let after_boundary =
            start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        let tail_start = start + credential.prefix.len();
        let tail = bytes[tail_start..]
            .iter()
            .take_while(|byte| (credential.admits)(**byte))
            .count();
        if after_boundary && tail >= credential.tail {
            return true;
        }
        from = tail_start;
    }
    false
}

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

/// The text of a regular UTF-8 file within the size limit; a symlink could leave the tree.
fn read_text(root: &Path, path: &str) -> Option<String> {
    let file = root.join(path);
    let about = std::fs::symlink_metadata(&file).ok()?;
    if !about.is_file() || about.len() > MAX_FILE_BYTES {
        return None;
    }
    String::from_utf8(std::fs::read(&file).ok()?).ok()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    /// Assembled at run time so this file holds no literal the gate would report.
    fn aws_key() -> String {
        format!("AKIA{}", "A1B2C3D4E5F6G7H8")
    }

    fn tracked_paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|path| (*path).to_string()).collect()
    }

    fn reads(text: &str) -> impl Fn(&str) -> Option<String> + '_ {
        move |_| Some(text.to_string())
    }

    /// The context of a tree whose files git lists.
    fn listed_by_git(root: &Path) -> Ctx {
        Ctx {
            vcs: crate::project::vcs::live_holder(root),
            ..Ctx::at(root)
        }
    }

    fn rendered(findings: &[Finding]) -> Vec<String> {
        findings.iter().map(Finding::render).collect()
    }

    #[test]
    fn a_private_key_committed_to_the_tree_is_reported() {
        let found = faults(&tracked_paths(&["certs/server.pem"]), &|_| None);
        assert_eq!(
            rendered(&found),
            vec![
                "certs/server.pem: tracked, and the name marks it as secret-bearing: \
                 `git rm --cached` it, rotate what it holds, and ignore the name"
            ]
        );
    }

    #[test]
    fn every_name_on_the_closed_list_is_reported() {
        for name in [
            ".env",
            ".env.local",
            ".env.production",
            "deploy.key",
            "id_rsa",
            "credentials.json",
            "src/server.pem",
        ] {
            let found = faults(&tracked_paths(&[name]), &|_| None);
            assert_eq!(
                found.iter().map(|f| f.file.as_str()).collect::<Vec<_>>(),
                [name]
            );
        }
    }

    #[test]
    fn a_committed_placeholder_is_not_a_leak() {
        for name in [
            ".env.example",
            ".env.template",
            ".env.sample",
            "server.example.pem",
        ] {
            assert_eq!(faults(&tracked_paths(&[name]), &|_| None), vec![], "{name}");
        }
    }

    #[test]
    fn a_name_that_merely_reads_like_a_secret_is_left_alone() {
        for name in [
            "src/environment.rs",
            "src/keyboard.rs",
            "src/monkey.rs",
            "docs/credentials.json.md",
            "src/env",
        ] {
            assert_eq!(faults(&tracked_paths(&[name]), &|_| None), vec![], "{name}");
        }
    }

    #[test]
    fn a_certificate_under_a_test_directory_is_the_fixture_not_the_leak() {
        for path in [
            "tests/fixtures/server.pem",
            "crates/api/testdata/ca.pem",
            "examples/demo.key",
            "benches/keys/id_rsa",
        ] {
            assert_eq!(faults(&tracked_paths(&[path]), &|_| None), vec![], "{path}");
        }
    }

    #[test]
    fn the_same_certificate_outside_a_test_directory_is_reported() {
        let found = faults(&tracked_paths(&["crates/api/src/ca.pem"]), &|_| None);
        assert_eq!(
            found.iter().map(|f| f.file.as_str()).collect::<Vec<_>>(),
            ["crates/api/src/ca.pem"]
        );
    }

    #[test]
    fn a_file_reported_by_its_name_is_never_quoted_back() {
        let text = format!("AWS_ACCESS_KEY_ID={}\n", aws_key());
        let found = faults(&tracked_paths(&[".env"]), &reads(&text));
        assert_eq!(
            found.iter().map(|f| f.file.as_str()).collect::<Vec<_>>(),
            [".env"]
        );
        assert!(!found[0].message.contains(&aws_key()), "{found:?}");
    }

    #[test]
    fn a_file_reported_by_its_name_does_not_stop_the_scan_at_the_one_behind_it() {
        let text = format!("key = \"{}\"\n", aws_key());
        let found = faults(&tracked_paths(&[".env", "deploy/aws.toml"]), &reads(&text));
        assert_eq!(
            rendered(&found),
            vec![
                ".env: tracked, and the name marks it as secret-bearing: \
                 `git rm --cached` it, rotate what it holds, and ignore the name",
                "deploy/aws.toml:1: AWS access key id: a credential literal: \
                 remove it and rotate the key",
            ]
        );
    }

    #[test]
    fn an_aws_key_in_a_committed_config_file_is_reported_at_its_line() {
        let text = format!("region = \"eu-west-1\"\nkey = \"{}\"\n", aws_key());
        let found = faults(&tracked_paths(&["deploy/aws.toml"]), &reads(&text));
        assert_eq!(
            rendered(&found),
            vec![
                "deploy/aws.toml:2: AWS access key id: a credential literal: \
                 remove it and rotate the key"
            ]
        );
    }

    #[test]
    fn the_finding_names_the_pattern_and_never_the_value() {
        let text = format!("key = \"{}\"\n", aws_key());
        let found = faults(&tracked_paths(&["deploy/aws.toml"]), &reads(&text));
        assert_eq!(found[0].item.as_deref(), Some("AWS access key id"));
        assert!(!found[0].render().contains(&aws_key()), "{found:?}");
    }

    #[test]
    fn every_pattern_in_the_closed_list_is_recognised() {
        let cases = [
            (aws_key(), "AWS access key id"),
            (
                format!("ghp_{}", "a".repeat(36)),
                "GitHub personal access token",
            ),
            (
                format!("github_pat_{}", "b".repeat(22)),
                "GitHub fine-grained token",
            ),
            (format!("xoxb-{}", "1".repeat(10)), "Slack bot token"),
        ];
        for (value, name) in cases {
            assert_eq!(
                credential_in(&format!("token = \"{value}\"")),
                Some(name),
                "{name}"
            );
        }
    }

    #[test]
    fn a_prefix_inside_a_longer_identifier_is_not_a_credential() {
        assert_eq!(credential_in(&format!("S{}", aws_key())), None);
        assert_eq!(credential_in(&format!("doughp_{}", "a".repeat(36))), None);
    }

    #[test]
    fn a_short_tail_is_prose_rather_than_a_key() {
        assert_eq!(credential_in("AKIA123"), None);
        assert_eq!(credential_in("xoxb-bot"), None);
    }

    #[test]
    fn entropy_alone_never_triggers() {
        assert_eq!(credential_in("9f8e7d6c5b4a39281706f5e4d3c2b1a0"), None);
        assert_eq!(credential_in("-----BEGIN RSA PRIVATE KEY-----"), None);
    }

    #[test]
    fn a_credential_shaped_literal_in_rust_source_is_not_this_gates_business() {
        let text = format!("const KEY: &str = \"{}\";\n", aws_key());
        assert_eq!(
            faults(&tracked_paths(&["src/deploy.rs"]), &reads(&text)),
            vec![]
        );
    }

    #[test]
    fn a_config_file_that_is_test_material_or_a_template_is_not_scanned() {
        let text = format!("key = \"{}\"\n", aws_key());
        for path in [
            "tests/data/aws.toml",
            "deploy/aws.example.toml",
            "examples/aws.toml",
        ] {
            assert_eq!(
                faults(&tracked_paths(&[path]), &reads(&text)),
                vec![],
                "{path}"
            );
        }
    }

    #[test]
    fn every_line_that_holds_one_is_reported_not_only_the_first() {
        let text = format!("a = \"{0}\"\nb = \"ok\"\nc = \"{0}\"\n", aws_key());
        let found = faults(&tracked_paths(&["deploy/aws.toml"]), &reads(&text));
        assert_eq!(
            found.iter().filter_map(|f| f.line).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn a_file_the_reader_cannot_return_is_skipped_rather_than_failing_the_gate() {
        assert_eq!(
            faults(&tracked_paths(&["deploy/aws.toml"]), &|_| None),
            vec![]
        );
    }

    #[test]
    fn a_file_tracked_under_the_build_directory_is_reported() {
        let found = faults(
            &tracked_paths(&["target/debug/chock", "target/debug/chock.d", "src/lib.rs"]),
            &|_| None,
        );
        assert_eq!(
            rendered(&found),
            vec![
                "target: git tracks 2 file(s) under the directory cargo builds into: \
                 `git rm -r --cached target` and ignore it"
            ]
        );
    }

    #[test]
    fn build_output_the_tree_no_longer_holds_is_a_removal_rather_than_debt() {
        let paths = tracked_paths(&["target/debug/a", "target/debug/b"]);
        let gone = faults_of(
            &paths,
            &|_| None,
            &|_| false,
            Some(crate::project::vcs::Kind::Git),
        );
        assert_eq!(gone, vec![]);
        let carried = faults_of(
            &paths,
            &|_| None,
            &|_| true,
            Some(crate::project::vcs::Kind::Git),
        );
        assert_eq!(
            rendered(&carried),
            vec![
                "target: git tracks 2 file(s) under the directory cargo builds into: \
                 `git rm -r --cached target` and ignore it"
            ]
        );
    }

    #[test]
    fn the_command_a_finding_names_is_the_one_holding_this_tree() {
        let paths = tracked_paths(&["target/debug/a"]);
        let said = rendered(&faults_of(
            &paths,
            &|_| None,
            &|_| true,
            Some(crate::project::vcs::Kind::Outpost),
        ));
        assert_eq!(
            said,
            vec![
                "target: outpost tracks 1 file(s) under the directory cargo builds into: \
                 `outpost rm -r target` and ignore it"
            ]
        );
    }

    #[test]
    fn a_secret_file_finding_names_the_command_of_the_system_holding_the_tree() {
        let said = rendered(&faults_of(
            &tracked_paths(&[".env"]),
            &|_| None,
            &|_| true,
            Some(crate::project::vcs::Kind::Outpost),
        ));
        assert_eq!(
            said,
            vec![
                ".env: tracked, and the name marks it as secret-bearing: copy it out, then \
                 `outpost rm` it, rotate what it holds, and ignore the name"
            ]
        );
    }

    #[test]
    fn a_source_path_that_merely_contains_target_is_not_build_output() {
        let paths = tracked_paths(&["src/target/mod.rs", "src/target.rs", "crates/a/target/x.rs"]);
        assert_eq!(faults(&paths, &|_| None), vec![]);
    }

    #[test]
    fn a_tree_with_nothing_wrong_yields_nothing() {
        let paths = tracked_paths(&["Cargo.toml", "src/lib.rs", ".env.example", ".gitignore"]);
        assert_eq!(faults(&paths, &reads("name = \"demo\"\n")), vec![]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_path_that_is_not_a_regular_file_is_not_read() {
        let root = crate::testdir::make("hygiene-read");
        std::fs::create_dir_all(root.join("deploy")).unwrap();
        assert_eq!(read_text(&root, "deploy"), None);
        assert_eq!(read_text(&root, "deploy/absent.toml"), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_exactly_at_the_size_limit_is_read_and_one_byte_past_it_is_not() {
        let root = crate::testdir::make("hygiene-size");
        let limit = usize::try_from(MAX_FILE_BYTES).unwrap();
        let head = "region = \"eu-west-1\"\n";
        let padded = |size: usize| format!("{head}{}", "x".repeat(size - head.len()));
        std::fs::write(root.join("at.toml"), padded(limit)).unwrap();
        std::fs::write(root.join("over.toml"), padded(limit + 1)).unwrap();
        assert_eq!(
            read_text(&root, "at.toml")
                .as_deref()
                .and_then(|text| text.lines().next()),
            Some(head.trim_end())
        );
        assert_eq!(read_text(&root, "over.toml"), None);
    }

    fn git(root: &Path, args: &[&str]) {
        let out = exec::run("git", args, root).unwrap();
        assert!(out.success(), "git {args:?}: {}", out.stderr);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_enumeration_is_what_git_tracks_and_not_what_the_directory_holds() {
        let root = crate::testdir::make("hygiene-tracked");
        std::fs::write(root.join("server.pem"), "placeholder\n").unwrap();
        std::fs::write(root.join(".env"), "TOKEN=placeholder\n").unwrap();
        git(&root, &["init", "--quiet"]);
        git(&root, &["add", "server.pem"]);
        let ctx = listed_by_git(&root);
        let outcome = check(&ctx).unwrap();
        assert!(!outcome.passed);
        assert_eq!(
            outcome
                .findings
                .iter()
                .map(|f| f.file.as_str())
                .collect::<Vec<_>>(),
            ["server.pem"]
        );
    }

    /// `git ls-files` reads the index, so the test writes the entries into it, not to disk.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_tree_of_exactly_what_one_pass_reads_is_enumerated_rather_than_refused() {
        let root = crate::testdir::make("hygiene-cap");
        git(&root, &["init", "--quiet"]);
        std::fs::write(root.join("blob"), "placeholder\n").unwrap();
        let hashed = exec::run("git", &["hash-object", "-w", "blob"], &root).unwrap();
        let blob = hashed.stdout.trim();
        let mut entries = String::new();
        for n in 0..crate::project::vcs::MAX_TRACKED {
            entries.push_str(&format!("100644 {blob} 0\tf/{n:05}.txt\n"));
        }
        std::fs::write(root.join("entries"), entries).unwrap();
        let listed = std::fs::File::open(root.join("entries")).unwrap();
        let wrote = std::process::Command::new("git")
            .args(["update-index", "--index-info"])
            .current_dir(&root)
            .stdin(listed)
            .status()
            .unwrap();
        assert!(wrote.success(), "{wrote:?}");
        let held = Some(crate::project::vcs::Kind::Git);
        let paths = crate::project::vcs::tracked(&root, held).unwrap();
        assert_eq!(paths.first().map(String::as_str), Some("f/00000.txt"));
        assert_eq!(paths.last().map(String::as_str), Some("f/49999.txt"));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_tracked_tree_with_nothing_to_report_passes() {
        let root = crate::testdir::make("hygiene-clean");
        std::fs::write(root.join(".env.example"), "TOKEN=\n").unwrap();
        git(&root, &["init", "--quiet"]);
        git(&root, &["add", "-A"]);
        let ctx = listed_by_git(&root);
        assert_eq!(check(&ctx).unwrap(), Outcome::passed());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn outside_a_git_repository_the_gate_cannot_run_rather_than_passing() {
        let root = crate::testdir::make("hygiene-no-repo");
        std::fs::write(root.join(".git"), "gitdir: /nowhere-chock-hygiene\n").unwrap();
        let ctx = listed_by_git(&root);
        let reason = check(&ctx).unwrap_err();
        assert!(
            reason.starts_with("git ls-files read no repository here:"),
            "{reason}"
        );
    }

    #[test]
    fn the_gate_is_a_pass_fail_check_named_hygiene() {
        assert_eq!(GATE.name, "hygiene");
        assert_eq!(crate::run::rerun(GATE.name), "chock run hygiene");
        assert_eq!(GATE.group, Group::Quality);
        assert!(
            matches!(GATE.kind, Kind::Binary(_)),
            "hygiene reports a verdict, not a number"
        );
    }
}
