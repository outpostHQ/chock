//! Secret-bearing files, credential literals, build output, hidden characters and files that
//! steer git, among the files the repository tracks.

mod git;
mod hidden;
mod tree;

use std::path::Path;

use crate::project::config::Secret;

#[cfg(test)]
use crate::exec;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};
use tree::{Tree, faults_of};

/// The `hygiene` gate: secrets, build output and hidden characters in tracked files.
pub const GATE: Gate = Gate {
    name: "hygiene",
    about: "the repository tracks a secret-bearing file, a credential literal, build output, a \
            character no reader sees, or a file that steers git",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Binary(check),
};

/// The largest file scanned for credentials; anything bigger is data, not configuration.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let declared = declared(&ctx.secrets)?;
    let paths = crate::project::vcs::tracked(&ctx.root, ctx.vcs)?;
    let tree = Tree {
        read: &|path| read_text(&ctx.root, path),
        present: &|path| ctx.root.join(path).exists(),
        link: &|path| std::fs::read_link(ctx.root.join(path)).ok(),
    };
    let findings = faults_of(&paths, &tree, ctx.vcs, &declared);
    if findings.is_empty() {
        Ok(Outcome::passed())
    } else {
        Ok(Outcome::failed(findings))
    }
}

/// Every rule over a list of tracked paths, with file reading injected so tests need no repository.
#[must_use]
pub fn faults(paths: &[String], read: &dyn Fn(&str) -> Option<String>) -> Vec<Finding> {
    let tree = Tree {
        read,
        present: &|_| true,
        link: &|_| None,
    };
    faults_of(paths, &tree, Some(crate::project::vcs::Kind::Git), &[])
}

/// One credential shape: a fixed prefix, a minimum run of admissible bytes after it, and a name.
/// Entropy alone never matches.
pub(crate) struct Credential<'a> {
    name: &'a str,
    prefix: &'a str,
    tail: usize,
    admits: fn(u8) -> bool,
}

/// The shortest prefix and tail a declared secret may have, so that it cannot match prose.
const SHORTEST_PREFIX: usize = 3;
const SHORTEST_TAIL: usize = 8;

/// The shapes listed under `secrets`. Their tail admits letters, digits, `_` and `-`.
pub(crate) fn declared(secrets: &[Secret]) -> Result<Vec<Credential<'_>>, String> {
    secrets
        .iter()
        .map(|secret| {
            let named = &secret.name;
            if named.trim().is_empty() {
                return Err(format!(
                    "the `secrets` entry `{}` gives no `name`",
                    secret.prefix
                ));
            }
            if secret.prefix.chars().count() < SHORTEST_PREFIX || secret.tail < SHORTEST_TAIL {
                return Err(format!(
                    "the secret `{named}` needs a `prefix` of at least {SHORTEST_PREFIX} \
                     characters and a `tail` of at least {SHORTEST_TAIL}, or it matches prose"
                ));
            }
            Ok(Credential {
                name: named,
                prefix: &secret.prefix,
                tail: secret.tail,
                admits: |byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-',
            })
        })
        .collect()
}

impl Credential<'_> {
    pub(crate) fn name(&self) -> &str {
        self.name
    }

    pub(crate) fn prefix(&self) -> &str {
        self.prefix
    }
}

/// Prefixes with a long fixed tail. `sk-` and the PEM header are left out: both match ordinary
/// source too often.
const CREDENTIALS: [Credential<'static>; 11] = [
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
    Credential {
        name: "GitHub OAuth token",
        prefix: "gho_",
        tail: 36,
        admits: |byte| byte.is_ascii_alphanumeric(),
    },
    Credential {
        name: "GitHub app user token",
        prefix: "ghu_",
        tail: 36,
        admits: |byte| byte.is_ascii_alphanumeric(),
    },
    Credential {
        name: "GitHub app installation token",
        prefix: "ghs_",
        tail: 36,
        admits: |byte| byte.is_ascii_alphanumeric(),
    },
    Credential {
        name: "GitHub refresh token",
        prefix: "ghr_",
        tail: 36,
        admits: |byte| byte.is_ascii_alphanumeric(),
    },
    Credential {
        name: "crates.io API token",
        prefix: "cio",
        tail: 32,
        admits: |byte| byte.is_ascii_alphanumeric(),
    },
    Credential {
        name: "npm access token",
        prefix: "npm_",
        tail: 36,
        admits: |byte| byte.is_ascii_alphanumeric(),
    },
    Credential {
        name: "PyPI API token",
        prefix: "pypi-AgEIcHlwaS5vcmc",
        tail: 50,
        admits: |byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_',
    },
];

/// The first credential a line holds, by name. The value is never carried out of here.
fn credential_in(line: &str) -> Option<&'static str> {
    CREDENTIALS
        .iter()
        .find(|credential| holds(line, credential))
        .map(|credential| credential.name)
}

/// The tails that vendors print in their own documentation, which no account holds: that of AWS's
/// example key id, and the one GitHub's API description gives each kind of token.
const DOCUMENTED: [&str; 2] = ["IOSFODNN7EXAMPLE", "16C7e42F292c6912E7710c838347Ae178B4a"];

/// Whether the line holds this credential: its prefix at a word boundary, with a long enough tail.
pub(crate) fn holds(line: &str, credential: &Credential<'_>) -> bool {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(offset) = line[from..].find(credential.prefix) {
        let start = from + offset;
        let after_boundary =
            start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        let tail_start = start + credential.prefix.len();
        if after_boundary && undocumented_tail(&bytes[tail_start..], credential) >= credential.tail
        {
            return true;
        }
        from = tail_start;
    }
    false
}

/// The length of the credential's tail at the start of `bytes`; 0 for a tail a vendor documents.
fn undocumented_tail(bytes: &[u8], credential: &Credential<'_>) -> usize {
    let tail = bytes
        .iter()
        .take_while(|byte| (credential.admits)(**byte))
        .count();
    let documented = DOCUMENTED
        .iter()
        .any(|example| &bytes[..tail] == example.as_bytes());
    if documented { 0 } else { tail }
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
mod tests {
    use std::fmt::Write as _;

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

    /// Whether the test's files are on disk.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum OnDisk {
        /// Each file is on disk.
        Present,
        /// No file is on disk.
        Missing,
    }

    /// The findings over paths whose files hold nothing, each `on_disk` or not, in a tree `held`.
    fn unread(paths: &[String], on_disk: OnDisk, held: crate::project::vcs::Kind) -> Vec<Finding> {
        let present = on_disk == OnDisk::Present;
        let tree = Tree {
            read: &|_| None,
            present: &|_| present,
            link: &|_| None,
        };
        faults_of(paths, &tree, Some(held), &[])
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
        let names = ".env .env.local .env.production deploy.key id_rsa credentials.json \
                     src/server.pem .git-credentials .cargo/credentials .cargo/credentials.toml";
        for name in names.split_whitespace() {
            let found = faults(&tracked_paths(&[name]), &|_| None);
            let files: Vec<&str> = found.iter().map(|f| f.file.as_str()).collect();
            assert_eq!(files, [name]);
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
            (format!("gho_{}", "c".repeat(36)), "GitHub OAuth token"),
            (format!("ghu_{}", "d".repeat(36)), "GitHub app user token"),
            (
                format!("ghs_{}", "e".repeat(36)),
                "GitHub app installation token",
            ),
            (format!("ghr_{}", "f".repeat(36)), "GitHub refresh token"),
            (format!("cio{}", "g1".repeat(16)), "crates.io API token"),
            (format!("npm_{}", "h2".repeat(18)), "npm access token"),
            (
                format!("pypi-AgEIcHlwaS5vcmc{}", "i-_".repeat(17)),
                "PyPI API token",
            ),
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
    fn the_example_a_vendor_documents_is_not_a_credential() {
        // Assembled at run time, as `aws_key` is, so no secret scanner reads a token in this file.
        let token = |end: char| format!("ghu_{}{end}", "16C7e42F292c6912E7710c838347Ae178B4");
        let lines = [
            "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE".to_string(),
            format!("\"token\": \"{}\"", token('a')),
            token('b'),
        ];
        let found: Vec<_> = lines.iter().map(|line| credential_in(line)).collect();
        assert_eq!(found, [None, None, Some("GitHub app user token")]);
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
        assert_eq!(
            unread(&paths, OnDisk::Missing, crate::project::vcs::Kind::Git),
            vec![]
        );
        assert_eq!(
            rendered(&unread(
                &paths,
                OnDisk::Present,
                crate::project::vcs::Kind::Git
            )),
            vec![
                "target: git tracks 2 file(s) under the directory cargo builds into: \
                 `git rm -r --cached target` and ignore it"
            ]
        );
    }

    #[test]
    fn the_command_a_finding_names_is_the_one_holding_this_tree() {
        let paths = tracked_paths(&["target/debug/a"]);
        let said = rendered(&unread(
            &paths,
            OnDisk::Present,
            crate::project::vcs::Kind::Outpost,
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
        let held = crate::project::vcs::Kind::Outpost;
        let said = rendered(&unread(&tracked_paths(&[".env"]), OnDisk::Present, held));
        let fix = "copy it out, then `outpost rm` it, rotate what it holds, and ignore the name";
        let want = format!(".env: tracked, and the name marks it as secret-bearing: {fix}");
        assert_eq!(said, [want]);
    }

    #[test]
    fn a_source_path_that_merely_contains_target_is_not_build_output() {
        let paths = tracked_paths(&["src/target/mod.rs", "src/target.rs", "crates/a/target/x.rs"]);
        assert_eq!(faults(&paths, &|_| None), vec![]);
    }

    #[test]
    fn a_cargo_file_named_credentials_elsewhere_is_left_alone() {
        let paths = tracked_paths(&["src/credentials.rs", "docs/cargo/credentials"]);
        assert_eq!(faults(&paths, &|_| None), vec![]);
    }

    #[test]
    fn a_token_in_the_file_npm_or_pip_reads_is_reported() {
        let text = format!("//registry.npmjs.org/:_authToken=npm_{}\n", "a1".repeat(18));
        let found = faults(&tracked_paths(&[".npmrc", "web/.pypirc"]), &reads(&text));
        assert_eq!(
            rendered(&found),
            [
                ".npmrc:1: npm access token: a credential literal: remove it and rotate the key",
                "web/.pypirc:1: npm access token: a credential literal: remove it and rotate the key"
            ]
        );
    }

    #[test]
    fn a_hidden_character_is_reported_in_any_tracked_file_but_test_material() {
        let paths = tracked_paths(&["tests/bidi.rs", "src/lib.rs", "AGENTS.md"]);
        let found = faults(&paths, &reads("ok\nlet a\u{202E} = 1;\n"));
        let at: Vec<_> = found
            .iter()
            .map(|f| (f.file.as_str(), f.line, f.item.as_deref()))
            .collect();
        assert_eq!(
            at,
            [
                ("src/lib.rs", Some(2), Some(hidden::RULE)),
                ("AGENTS.md", Some(2), Some(hidden::RULE))
            ]
        );
    }

    #[test]
    fn only_the_gitmodules_git_reads_is_held_to_its_values() {
        let text = "[submodule \"a\"]\n\turl = --upload-pack=touch\n";
        let found = faults(
            &tracked_paths(&[".gitmodules", "docs/.gitmodules"]),
            &reads(text),
        );
        let at: Vec<_> = found
            .iter()
            .map(|f| (f.file.as_str(), f.line, f.item.as_deref()))
            .collect();
        assert_eq!(at, [(".gitmodules", Some(2), Some(git::MODULES))]);
    }

    #[test]
    fn a_tracked_link_into_git_is_reported_and_never_read() {
        let paths = tracked_paths(&["docs", "hooks"]);
        let link = |path: &str| {
            let target = if path == "hooks" {
                "../.git/hooks"
            } else {
                "../README.md"
            };
            Some(std::path::PathBuf::from(target))
        };
        let read = |_: &str| -> Option<String> { panic!("a link is never read") };
        let found = faults_of(
            &paths,
            &Tree {
                read: &read,
                present: &|_| true,
                link: &link,
            },
            None,
            &[],
        );
        let at: Vec<_> = found
            .iter()
            .map(|f| (f.file.as_str(), f.item.as_deref()))
            .collect();
        assert_eq!(at, [("hooks", Some(git::LINK))]);
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
            let _ = writeln!(entries, "100644 {blob} 0\tf/{n:05}.txt");
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

    fn secret(name: &str, prefix: &str, tail: usize) -> Secret {
        Secret {
            name: name.to_string(),
            prefix: prefix.to_string(),
            tail,
        }
    }

    /// A declared shape is read in source too, past a word boundary and with its whole tail.
    #[test]
    fn a_declared_secret_is_found_in_any_tracked_file_but_test_material_and_templates() {
        let secrets = [secret("Acme API key", "acme_", 12)];
        let declared = declared(&secrets).unwrap();
        let text = "let k = \"acme_ABCdef-123_xyz\";\nxacme_ABCdef123456 and acme_abc\n";
        let paths = tracked_paths(&["src/main.rs", "tests/a.rs", "conf.example.toml"]);
        let git = Some(crate::project::vcs::Kind::Git);
        let tree = Tree {
            read: &reads(text),
            present: &|_| true,
            link: &|_| None,
        };
        let said = rendered(&faults_of(&paths, &tree, git, &declared));
        assert_eq!(
            said,
            ["src/main.rs:1: Acme API key: a credential literal: remove it and rotate the key"]
        );
        assert_eq!(
            (declared[0].name(), declared[0].prefix()),
            ("Acme API key", "acme_")
        );
        assert_eq!(faults_of(&paths, &tree, git, &[]), []);
    }

    #[test]
    fn a_declared_secret_too_short_to_tell_from_prose_is_refused() {
        let weak = "needs a `prefix` of at least 3 characters and a `tail` of at least 8, or it \
                    matches prose";
        assert_eq!(
            declared(&[secret("k", "ac", 12)]).err(),
            Some(format!("the secret `k` {weak}"))
        );
        assert_eq!(
            declared(&[secret("k", "acme", 7)]).err(),
            Some(format!("the secret `k` {weak}"))
        );
        assert_eq!(
            declared(&[secret(" ", "acme", 8)]).err(),
            Some("the `secrets` entry `acme` gives no `name`".to_string())
        );
        let short = [secret("k", "acm", 8)];
        let shortest = declared(&short).unwrap();
        let named: Vec<(&str, &str)> = shortest.iter().map(|c| (c.name(), c.prefix())).collect();
        assert_eq!(named, [("k", "acm")]);
    }
}
