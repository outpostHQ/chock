//! Credentials in any commit, found by `outpost scan secrets --history`: deleting a key later does
//! not unpublish it, since every clone carries every commit.

use serde::Deserialize;

use crate::exec;
use crate::gates::outpost;
use crate::gates::repo::hygiene::{self, Credential};
use crate::project::config::Accepted;
use crate::project::vcs;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

pub const GATE: Gate = Gate {
    name: "history",
    about: "a credential in a commit, whether or not it is still in the tree; needs `outpost`",
    group: Group::OptIn,
    builds: false,
    reads: None,
    kind: Kind::Binary(check),
};

/// One scanner finding, with only the fields this gate reads.
#[derive(Deserialize)]
struct Leak {
    rule: String,
    description: String,
    path: String,
    line: Option<u32>,
    redacted: Option<String>,
    commit: Option<String>,
    only_in_history: bool,
}

/// The directory of the Outpost repository whose commits this scanner walks.
const REPOSITORY: &str = ".outpost";

fn check(ctx: &Ctx) -> Result<Outcome, String> {
    // Asked of `holders`, not the directory: outpost skips a `.outpost` that is not a repository
    // and would scan one further up.
    if !vcs::holders(&ctx.root).contains(&vcs::Kind::Outpost) {
        return Err(format!(
            "no {REPOSITORY} repository here, so there is no history this scanner can walk"
        ));
    }
    let shapes = hygiene::declared(&ctx.secrets)?;
    if !shapes.is_empty() && !vcs::holders(&ctx.root).contains(&vcs::Kind::Git) {
        return Err(
            "the declared secrets need git's history, and git does not hold this tree".into(),
        );
    }
    // Counted before the expensive walk, whose coverage is judged against this count.
    let held = vcs::commits(&ctx.root, ctx.vcs)?;
    let walk = ["scan", "secrets", "--history", "--json"];
    let mut found = leaks(&outpost::spawn(&ctx.root, &walk)?, held, &ctx.accepted)?;
    for shape in &shapes {
        let args = pickaxe(shape);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let walked = exec::run("git", &args, &ctx.root).map_err(|e| e.to_string())?;
        let still = |path: &str| still_holds(&ctx.root, path, shape);
        found.extend(unaccepted(&added(&walked, shape, &still)?, &ctx.accepted)?);
    }
    if found.is_empty() {
        return Ok(Outcome::passed());
    }
    Ok(Outcome::failed(found))
}

/// `outpost scan secrets` output; only `mode` says whether it read history or the working tree.
#[derive(Deserialize)]
struct Scan {
    mode: String,
    #[serde(default)]
    findings: Vec<Leak>,
    #[serde(default)]
    commits_walked: u64,
}

/// The mode this gate asks for; a working-tree scan would leave published commits unread.
const WALKED: &str = "history";

impl Scan {
    /// The findings, or an error if this was not a history scan, or it walked no commits or fewer
    /// than `held`.
    fn findings(self, held: u64) -> Result<Vec<Leak>, String> {
        if self.mode != WALKED {
            return Err(format!(
                "outpost scanned {}, not the history chock asked for",
                self.mode
            ));
        }
        let walked = self.commits_walked;
        if walked == 0 {
            return Err("outpost walked no commits, so no history was scanned".to_string());
        }
        // Outpost mirroring git can hold fewer commits than git does.
        if walked < held {
            let unscanned = held - walked;
            return Err(format!(
                "outpost walked {walked} of the {held} commits that change a file here, so \
                 {unscanned} went unscanned; `outpost git import` brings the rest in"
            ));
        }
        Ok(self.findings)
    }
}

/// The leaks a full history scan found, less those the project accepted. A failed, truncated,
/// empty, unreadable or partial scan is an error rather than a clean answer.
fn leaks(out: &exec::Output, held: u64, accepted: &[Accepted]) -> Result<Vec<Finding>, String> {
    if !out.success() {
        return Err(format!(
            "outpost could not walk this history: {}",
            out.why_it_failed()
        ));
    }
    if out.truncated {
        return Err("outpost printed more than chock keeps; the findings would be partial".into());
    }
    let text = out.stdout.trim();
    if text.is_empty() {
        return Err("outpost printed nothing, so no commit was scanned".to_string());
    }
    let scan: Scan = serde_json::from_str(text)
        .map_err(|e| format!("outpost printed a scan chock cannot read: {e}"))?;
    unaccepted(&scan.findings(held)?, accepted)
}

/// The shortest start of a commit id an accepted leak may name: git's own short form.
const SHORTEST: usize = 7;

/// The leaks no entry in `accepted` names. An entry with no reason, or too little of its commit to
/// name one without doubt, is an error: it could accept a leak nobody reviewed.
fn unaccepted(found: &[Leak], accepted: &[Accepted]) -> Result<Vec<Finding>, String> {
    let vague = |entry: &&Accepted| entry.commit.len() < SHORTEST || entry.reason.trim().is_empty();
    if let Some(entry) = accepted.iter().find(vague) {
        return Err(format!(
            "the accepted {} leak in {} needs a reason and at least {SHORTEST} characters of its \
             commit",
            entry.rule, entry.path
        ));
    }
    Ok(found
        .iter()
        .filter(|leak| !accepted.iter().any(|entry| accepts(entry, leak)))
        .map(finding)
        .collect())
}

/// Whether the entry names this leak: its rule, its file, and the commit the scan names or a start
/// of that commit's id.
fn accepts(entry: &Accepted, leak: &Leak) -> bool {
    entry.rule == leak.rule
        && entry.path == leak.path
        && leak
            .commit
            .as_deref()
            .is_some_and(|commit| commit.starts_with(&entry.commit))
}

/// The `git log` that prints each commit's added and removed lines holding a shape's prefix, with
/// its paths from the project root.
fn pickaxe(shape: &Credential<'_>) -> Vec<String> {
    let special = |c: char| ".[]()*+?{}|^$\\".contains(c);
    let mut literal = String::new();
    for c in shape.prefix().chars() {
        if special(c) {
            literal.push('\\');
        }
        literal.push(c);
    }
    let fixed = [
        "log",
        "--all",
        "--relative",
        "-p",
        "-U0",
        "--no-color",
        "--format=%x00%H",
    ];
    let mut args: Vec<String> = fixed.map(str::to_string).to_vec();
    args.push(format!("-G{literal}"));
    args
}

/// Each line a commit added that holds the shape, read from `git log -p -U0` output. A failed or
/// truncated log is an error rather than a clean history.
fn added(
    out: &exec::Output,
    shape: &Credential<'_>,
    still: &dyn Fn(&str) -> bool,
) -> Result<Vec<Leak>, String> {
    if !out.success() {
        return Err(format!(
            "git could not walk this history: {}",
            out.why_it_failed()
        ));
    }
    if out.truncated {
        return Err(
            "git printed more history than chock keeps; the findings would be partial".into(),
        );
    }
    let mut found = Vec::new();
    let (mut commit, mut path, mut line, mut left) = (None, None, 0, 0);
    for text in out.stdout.lines() {
        if let Some(hash) = text.strip_prefix('\0') {
            (commit, path, left) = (Some(hash.trim()), None, 0);
        } else if left > 0
            && let Some(content) = text.strip_prefix('+')
        {
            if let (Some(commit), Some(path)) = (commit, path)
                && hygiene::holds(content, shape)
            {
                found.push(Leak {
                    rule: shape.name().to_string(),
                    description: shape.name().to_string(),
                    path: String::from(path),
                    line: Some(line),
                    redacted: None,
                    commit: Some(commit.to_string()),
                    only_in_history: !still(path),
                });
            }
            (line, left) = (line + 1, left - 1);
        } else if let Some(name) = text.strip_prefix("+++ ") {
            path = name.trim_matches('"').strip_prefix("b/");
        } else if let Some(hunk) = text.strip_prefix("@@ ") {
            (line, left) = adds(hunk);
        }
    }
    Ok(found)
}

/// The first line and the count of the added side of a hunk header, `-a,b +c,d @@`.
fn adds(hunk: &str) -> (u32, u32) {
    let Some(side) = hunk.split(' ').find_map(|part| part.strip_prefix('+')) else {
        return (0, 0);
    };
    let (first, count) = side.split_once(',').unwrap_or((side, "1"));
    (first.parse().unwrap_or(0), count.parse().unwrap_or(0))
}

/// Whether the file at `path` still holds the shape today.
fn still_holds(root: &std::path::Path, path: &str, shape: &Credential<'_>) -> bool {
    std::fs::read_to_string(root.join(path))
        .is_ok_and(|text| text.lines().any(|line| hygiene::holds(line, shape)))
}

fn finding(leak: &Leak) -> Finding {
    let redacted = leak.redacted.as_deref().unwrap_or("redacted");
    let message = match (&leak.commit, leak.only_in_history) {
        (Some(commit), true) => format!(
            "{}: {redacted} — in {commit} and no longer in the tree, so it is still published",
            leak.description
        ),
        (Some(commit), false) => format!("{}: {redacted} — since {commit}", leak.description),
        (None, _) => format!("{}: {redacted}", leak.description),
    };
    let mut found = Finding::at(&leak.path, &message).item(&leak.rule);
    if let Some(line) = leak.line {
        found = found.line(line);
    }
    found
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn said(stdout: &str) -> exec::Output {
        exec::Output::of(Some(0), stdout, "")
    }

    /// The commit count `scanned` reports walking, so passing it means the scan covered everything.
    const WHOLE: u64 = 3;

    fn scanned(findings: &str) -> exec::Output {
        said(&format!(
            r#"{{"mode":"history","findings":[{findings}],"commits_walked":3,"versions_scanned":4}}"#
        ))
    }

    /// A scanner that died partway still prints an envelope.
    #[test]
    fn a_scan_that_failed_is_not_a_history_with_no_secrets_in_it() {
        let walked = r#"{"mode":"history","findings":[],"commits_walked":3}"#;
        let refused = exec::Output::of(Some(2), walked, "pack is corrupt");
        let why = "outpost could not walk this history: pack is corrupt";
        assert_eq!(leaks(&refused, WHOLE, &[]), Err(why.to_string()));
    }

    #[test]
    fn output_chock_had_to_truncate_could_not_run() {
        let mut cut = scanned("");
        cut.truncated = true;
        assert_eq!(
            leaks(&cut, WHOLE, &[]),
            Err("outpost printed more than chock keeps; the findings would be partial".to_string())
        );
    }

    #[test]
    fn a_scan_that_found_nothing_over_every_commit_reports_nothing() {
        assert_eq!(leaks(&scanned(""), WHOLE, &[]), Ok(vec![]));
    }

    #[test]
    fn a_scan_covering_fewer_commits_than_the_repository_holds_could_not_run() {
        assert_eq!(
            leaks(&scanned(""), 1277, &[]).unwrap_err(),
            "outpost walked 3 of the 1277 commits that change a file here, so 1274 went \
             unscanned; `outpost git import` brings the rest in"
        );
    }

    /// Outpost can record commits the mirrored history lacks, so its count may exceed git's.
    #[test]
    fn a_scan_covering_more_commits_than_the_repository_holds_is_still_an_answer() {
        assert_eq!(leaks(&scanned(""), 2, &[]), Ok(vec![]));
    }

    #[test]
    fn a_scan_of_the_working_tree_is_not_the_history_this_gate_asked_for() {
        let json = r#"{"mode":"working_tree","findings":[],"files_read":79}"#;
        assert_eq!(
            leaks(&said(json), WHOLE, &[]),
            Err("outpost scanned working_tree, not the history chock asked for".to_string())
        );
    }

    #[test]
    fn a_scan_that_walked_no_commits_could_not_run() {
        let json = r#"{"mode":"history","findings":[],"commits_walked":0,"versions_scanned":0}"#;
        assert_eq!(
            leaks(&said(json), 0, &[]),
            Err("outpost walked no commits, so no history was scanned".to_string())
        );
    }

    #[test]
    fn empty_output_is_a_gate_that_could_not_run() {
        assert_eq!(
            leaks(&said("  \n"), WHOLE, &[]),
            Err("outpost printed nothing, so no commit was scanned".to_string())
        );
    }

    #[test]
    fn a_scan_in_a_shape_chock_does_not_know_stops_the_history_gate() {
        assert!(
            leaks(&said("not json"), WHOLE, &[])
                .unwrap_err()
                .starts_with("outpost printed a scan chock cannot read")
        );
    }

    #[test]
    fn a_leak_still_in_the_tree_is_reported_at_its_line() {
        let json = scanned(
            r#"{"rule":"aws-key","description":"an AWS access key","path":"src/a.rs",
                "line":12,"redacted":"AKIA…(20 chars)","commit":"abc1234",
                "only_in_history":false}"#,
        );
        assert_eq!(
            leaks(&json, WHOLE, &[]).unwrap()[0].render(),
            "src/a.rs:12: aws-key: an AWS access key: AKIA…(20 chars) — since abc1234"
        );
    }

    #[test]
    fn a_leak_only_a_commit_holds_says_that_removing_it_did_not_unpublish_it() {
        let json = scanned(
            r#"{"rule":"private-key-block","description":"a PEM private key",
                "path":"deploy/id_rsa","line":1,"redacted":"----…(31 chars)",
                "commit":"def5678","only_in_history":true}"#,
        );
        assert_eq!(
            leaks(&json, WHOLE, &[]).unwrap()[0].render(),
            "deploy/id_rsa:1: private-key-block: a PEM private key: ----…(31 chars) — in def5678 \
             and no longer in the tree, so it is still published"
        );
    }

    /// A fixture token and a real key, both added in a commit the scanner names in full.
    fn a_fixture_and_a_key() -> exec::Output {
        scanned(
            r#"{"rule":"jwt","description":"a JSON web token","path":"tests/key.jwt","line":1,
                "redacted":"eyJh…(310 chars)","commit":"abc1234def5678","only_in_history":false},
               {"rule":"aws-key","description":"an AWS access key","path":"src/a.rs","line":12,
                "redacted":"AKIA…(20 chars)","commit":"abc1234def5678","only_in_history":false}"#,
        )
    }

    /// The files whose leaks are still reported when the project accepts the one `entry` names.
    fn reported_with(
        commit: &str,
        path: &str,
        rule: &str,
        reason: &str,
    ) -> Result<Vec<String>, String> {
        let entry = Accepted {
            commit: commit.to_string(),
            path: path.to_string(),
            rule: rule.to_string(),
            reason: reason.to_string(),
        };
        let found = leaks(&a_fixture_and_a_key(), WHOLE, &[entry])?;
        Ok(found.into_iter().map(|leak| leak.file).collect())
    }

    const REVIEWED: &str = "signed by a throwaway key";

    #[test]
    fn an_accepted_leak_is_not_reported_and_the_others_still_are() {
        let key = Ok(vec!["src/a.rs".to_string()]);
        // The commit as `chock run history` names it, or as short as git's own short form.
        assert_eq!(
            reported_with("abc1234def5678", "tests/key.jwt", "jwt", REVIEWED),
            key
        );
        assert_eq!(
            reported_with("abc1234", "tests/key.jwt", "jwt", REVIEWED),
            key
        );
    }

    #[test]
    fn an_entry_that_differs_in_rule_file_or_commit_accepts_nothing() {
        let both = Ok(vec!["tests/key.jwt".to_string(), "src/a.rs".to_string()]);
        assert_eq!(
            reported_with("abc1234", "tests/key.jwt", "aws-key", REVIEWED),
            both
        );
        assert_eq!(
            reported_with("abc1234", "tests/old.jwt", "jwt", REVIEWED),
            both
        );
        assert_eq!(
            reported_with("abc1235", "tests/key.jwt", "jwt", REVIEWED),
            both
        );
    }

    #[test]
    fn an_entry_with_no_reason_or_too_little_of_its_commit_stops_the_gate() {
        let refused = Err(
            "the accepted jwt leak in tests/key.jwt needs a reason and at least 7 \
                           characters of its commit"
                .to_string(),
        );
        assert_eq!(
            reported_with("abc123", "tests/key.jwt", "jwt", REVIEWED),
            refused
        );
        assert_eq!(
            reported_with("abc1234", "tests/key.jwt", "jwt", "  "),
            refused
        );
    }

    fn acme() -> Vec<crate::project::config::Secret> {
        vec![crate::project::config::Secret {
            name: "Acme key".to_string(),
            prefix: "acme_".to_string(),
            tail: 12,
        }]
    }

    /// Built with `+` lines a header could be mistaken for: the hunk's count says which are lines.
    fn logged() -> String {
        let key = format!("acme_{}", "ABCDEFGHIJKL");
        [
            "\0aaaa1111",
            "",
            "diff --git a/conf.toml b/conf.toml",
            "--- /dev/null",
            "+++ b/conf.toml",
            "@@ -0,0 +1,3 @@",
            "+name = \"x\"",
            &format!("+key = \"{key}\""),
            &format!("++++ {key}M"),
            "\0bbbb2222",
            "",
            "--- a/conf.toml",
            "+++ b/conf.toml",
            "@@ -2 +2 @@",
            &format!("-key = \"{key}\""),
            &format!("+key = \"acme_{}\"", "ZZZZZZZZZZZZ"),
            "@@ -3 +2,0 @@",
            &format!("-+++ {key}M"),
            "--- a/gone.toml",
            "+++ /dev/null",
            "@@ -1 +0,0 @@",
            &format!("-{key}"),
        ]
        .join("\n")
    }

    #[test]
    fn each_line_a_commit_added_holding_a_declared_secret_is_a_leak() {
        let secrets = acme();
        let shapes = hygiene::declared(&secrets).unwrap();
        let out = said(&logged());
        let found = added(&out, &shapes[0], &|path| path == "kept.toml").unwrap();
        let places: Vec<(Option<&str>, &str, Option<u32>, bool)> = found
            .iter()
            .map(|leak| {
                (
                    leak.commit.as_deref(),
                    leak.path.as_str(),
                    leak.line,
                    leak.only_in_history,
                )
            })
            .collect();
        assert_eq!(
            places,
            [
                (Some("aaaa1111"), "conf.toml", Some(2), true),
                (Some("aaaa1111"), "conf.toml", Some(3), true),
                (Some("bbbb2222"), "conf.toml", Some(2), true),
            ]
        );
        assert_eq!(
            unaccepted(&found[..1], &[]).unwrap()[0].render(),
            "conf.toml:2: Acme key: Acme key: redacted — in aaaa1111 and no longer in the tree, \
             so it is still published"
        );
        let kept = added(&out, &shapes[0], &|_| true).unwrap();
        assert!(kept.iter().all(|leak| !leak.only_in_history));
    }

    #[test]
    fn a_git_log_that_failed_or_was_cut_short_is_no_clean_history() {
        let secrets = acme();
        let shapes = hygiene::declared(&secrets).unwrap();
        let failed = exec::Output::of(Some(128), "", "fatal: bad revision");
        assert_eq!(
            added(&failed, &shapes[0], &|_| true).err(),
            Some("git could not walk this history: fatal: bad revision".to_string())
        );
        let mut cut = said(&logged());
        cut.truncated = true;
        assert_eq!(
            added(&cut, &shapes[0], &|_| true).err(),
            Some("git printed more history than chock keeps; the findings would be partial".into())
        );
    }

    #[test]
    fn the_log_asks_for_lines_holding_the_prefix_as_a_literal() {
        let secrets = [crate::project::config::Secret {
            prefix: "a.b+".to_string(),
            ..acme().remove(0)
        }];
        let shapes = hygiene::declared(&secrets).unwrap();
        let args = pickaxe(&shapes[0]);
        assert_eq!(args.first().map(String::as_str), Some("log"));
        assert_eq!(args.last().map(String::as_str), Some("-Ga\\.b\\+"));
        assert!(args.iter().any(|arg| arg == "--relative"));
        assert_eq!(adds("-0,0 +1,3 @@"), (1, 3));
        assert_eq!(adds("-2 +2 @@"), (2, 1));
        assert_eq!(adds("-3 +2,0 @@"), (2, 0));
        assert_eq!(adds("no hunk"), (0, 0));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_still_holds_a_shape_when_one_of_its_lines_does() {
        let secrets = acme();
        let shapes = hygiene::declared(&secrets).unwrap();
        let text = format!("a = 1\nkey = acme_{}\n", "ABCDEFGHIJKL");
        let held = crate::testdir::Held::tree("history-still", &[("conf.toml", text.as_str())]);
        assert!(still_holds(&held.root, "conf.toml", &shapes[0]));
        assert!(!still_holds(&held.root, "gone.toml", &shapes[0]));
    }
}
