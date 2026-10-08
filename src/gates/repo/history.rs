//! Credentials in any commit, found by `outpost scan secrets --history`: deleting a key later does
//! not unpublish it, since every clone carries every commit.

use serde::Deserialize;

use crate::exec;
use crate::gates::outpost;
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
    // Counted before the expensive walk, whose coverage is judged against this count.
    let held = vcs::commits(&ctx.root, ctx.vcs)?;
    let walk = ["scan", "secrets", "--history", "--json"];
    let found = leaks(&outpost::spawn(&ctx.root, &walk)?, held, &ctx.accepted)?;
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
                "outpost walked {walked} of the {held} commits this repository holds, so \
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
    if let Some(entry) = accepted.iter().find(|entry| vague(entry)) {
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

fn vague(entry: &Accepted) -> bool {
    entry.commit.len() < SHORTEST || entry.reason.trim().is_empty()
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
            "outpost walked 3 of the 1277 commits this repository holds, so 1274 went unscanned; \
             `outpost git import` brings the rest in"
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
    fn output_in_a_shape_chock_does_not_know_stops_the_gate() {
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
}
