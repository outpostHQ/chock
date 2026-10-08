//! The `deps` gate: `cargo deny check` for each section the project's `deny.toml` has, read into
//! findings, with the checks nobody asked for named beside the answer.

use super::{Note, located, note, unread, verdict};
use crate::exec;
use crate::run::report::Finding;
use crate::run::{Ctx, Outcome};

pub(super) fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let manifest = std::fs::read_to_string(ctx.root.join(DENY))
        .map_err(|e| format!("cannot read {DENY}, so no policy says what to check: {e}"))?;
    let asked = asked_for(&manifest);
    if asked.is_empty() {
        return Err(format!(
            "{DENY} declares no [advisories], [bans], [licenses] or [sources], so there is no \
             policy to hold anything to"
        ));
    }
    let mut args = vec!["deny", "check"];
    args.extend(&asked);
    let out = exec::tool(&ctx.root, "cargo", &args)?;
    let outcome = with_unplaced(verdict(&out, &ctx.root), &out);
    Ok(with_unasked(outcome, &asked))
}

/// A failed outcome, with cargo-deny's errors that point at no place; a pass keeps what it has.
fn with_unplaced(mut outcome: Outcome, out: &exec::Output) -> Outcome {
    if !outcome.passed {
        let texts = [out.stderr.as_str(), out.stdout.as_str()];
        outcome
            .findings
            .extend(texts.into_iter().flat_map(unplaced));
    }
    outcome
}

/// cargo-deny's coded errors with no place, such as a crate with no licence. rustc's unplaced
/// `error:` has no code and stays out: that one is a build that never ended.
fn unplaced(text: &str) -> Vec<Finding> {
    let mut found = Vec::new();
    let mut open: Option<Note> = None;
    for line in text.lines().map(str::trim) {
        if located(line).is_some() {
            open = None;
        } else if let Some(next) = note(line) {
            found.extend(open.replace(next).and_then(coded));
        }
    }
    found.extend(open.and_then(coded));
    found
}

/// What the owner of the crate does about a crate cargo-deny read no licence from.
const LICENSE: &str =
    " — if the crate is yours, add a `license` field under `[package]` in its Cargo.toml";

/// The finding for an error that names its rule. A warning is no failure, so it makes none.
fn coded(note: Note) -> Option<Finding> {
    let code = note.code.filter(|_| note.error)?;
    let advice = if code == "unlicensed" { LICENSE } else { "" };
    Some(Finding::at("", &format!("{}{advice}", note.message)).item(&code))
}

/// The outcome, and one finding for each cargo-deny check that did not run. A failure nothing was
/// read from keeps no finding, so it still reports as unable to run.
fn with_unasked(mut outcome: Outcome, asked: &[&str]) -> Outcome {
    if !unread(&outcome) {
        outcome.findings.extend(not_asked(asked));
    }
    outcome
}

/// The policy file a project writes for cargo-deny.
const DENY: &str = "deny.toml";

/// Each cargo-deny check, with the section of the policy file that asks for it.
const CHECKS: [(&str, &str); 4] = [
    ("[advisories]", "advisories"),
    ("[bans]", "bans"),
    ("[licenses]", "licenses"),
    ("[sources]", "sources"),
];

/// The cargo-deny checks the project wrote a section for; one with no policy rejects every crate.
#[must_use]
fn asked_for(manifest: &str) -> Vec<&'static str> {
    CHECKS
        .iter()
        .filter(|(section, _)| declares(manifest, section))
        .map(|(_, check)| *check)
        .collect()
}

/// The checks the policy file has no section for, so a pass does not read as all four.
fn not_asked(asked: &[&str]) -> Vec<Finding> {
    CHECKS
        .iter()
        .filter(|(_, check)| !asked.contains(check))
        .map(|(section, check)| {
            let message = format!("no {section} section, so cargo-deny did not check {check}");
            Finding::at(DENY, &message)
        })
        .collect()
}

/// A section heading on its own line, so `[bans.build]` does not read as `[bans]` and a mention
/// inside a comment does not read as a policy.
fn declares(manifest: &str, section: &str) -> bool {
    manifest.lines().map(str::trim).any(|line| line == section)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use std::path::Path;

    use super::*;

    /// Real cargo-deny 0.20.2 output for a crate with no `license` field.
    const UNLICENSED: &str = "warning[no-license-field]: license expression was not specified in manifest for crate 'tour = 0.1.0'\n \u{251c} tour v0.1.0\nwarning[unlicensed]: a valid license expression could not be retrieved for the crate\n  \u{250c}\u{2500} path+file:///w/proj#tour@0.1.0-synthesized.toml:2:9\n  \u{2502}\n2 \u{2502} name = \"tour\"\n  \u{2502}\nerror[unlicensed]: tour = 0.1.0 is unlicensed\n \u{251c} tour v0.1.0 (*)\n";

    fn rendered(found: &[Finding]) -> Vec<String> {
        found.iter().map(Finding::render).collect()
    }

    #[test]
    fn a_coded_error_that_points_at_no_place_is_still_a_finding() {
        assert_eq!(
            rendered(&unplaced(UNLICENSED)),
            vec![format!("unlicensed: tour = 0.1.0 is unlicensed{LICENSE}")]
        );
        let two = "error[banned]: crate a is banned\nerror[rejected]: crate b is rejected\n";
        assert_eq!(
            rendered(&unplaced(two)),
            vec!["banned: crate a is banned", "rejected: crate b is rejected"]
        );
        // No code: a build that never ended. A place: the span reader has it. A warning: no failure.
        assert_eq!(unplaced("error: could not compile `a`\n"), vec![]);
        let placed = "error[vulnerability]: known\n   \u{250c}\u{2500} /w/proj/Cargo.lock:42:1\n";
        assert_eq!(unplaced(placed), vec![]);
        assert_eq!(unplaced("warning[yanked]: an old release\n"), vec![]);
    }

    #[test]
    fn only_a_failed_outcome_gains_the_unplaced_errors() {
        let out = exec::Output::of(Some(1), "", UNLICENSED);
        assert_eq!(with_unplaced(Outcome::passed(), &out), Outcome::passed());
        let read = verdict(&out, Path::new("/w/proj"));
        assert!(unread(&read), "{:?}", read.findings);
        let whole = with_unplaced(read, &out);
        assert_eq!(
            rendered(&whole.findings),
            vec![format!("unlicensed: tour = 0.1.0 is unlicensed{LICENSE}")]
        );
        assert!(!unread(&whole));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_policy_file_that_is_absent_or_asks_for_nothing_cannot_be_measured() {
        let dir = crate::testdir::make("deps-no-policy");
        let ctx = Ctx::at(&dir);
        let absent = check(&ctx).unwrap_err();
        assert_eq!(
            absent.split(": ").next(),
            Some("cannot read deny.toml, so no policy says what to check")
        );
        std::fs::write(dir.join(DENY), "[graph]\nall-features = true\n").unwrap();
        assert_eq!(
            check(&ctx).unwrap_err(),
            "deny.toml declares no [advisories], [bans], [licenses] or [sources], so there is no \
             policy to hold anything to"
        );
    }

    #[test]
    fn only_the_checks_the_project_wrote_a_policy_for_are_run() {
        let ws = "[graph]\nall-features = true\n\n[advisories]\n\n[bans]\n\n[sources]\n";
        assert_eq!(asked_for(ws), vec!["advisories", "bans", "sources"]);
    }

    #[test]
    fn a_policy_naming_every_check_runs_every_check() {
        let all = "[advisories]\n[licenses]\n[bans]\n[sources]\n";
        assert_eq!(
            asked_for(all),
            vec!["advisories", "bans", "licenses", "sources"]
        );
    }

    #[test]
    fn a_nested_section_or_a_commented_one_is_not_a_policy() {
        assert_eq!(
            asked_for("[bans.build]\n[[bans.build.bypass]]\n"),
            Vec::<&str>::new()
        );
        assert_eq!(
            asked_for("# [licenses] was removed on purpose\n"),
            Vec::<&str>::new()
        );
        assert_eq!(asked_for("[advisories]"), vec!["advisories"]);
    }

    /// The false green this answers: a policy with no `[licenses]` passed as if licences held.
    #[test]
    fn a_check_with_no_section_is_named_beside_a_pass_or_a_trip_and_never_beside_silence() {
        let asked = ["advisories", "bans", "sources"];
        let gap = "deny.toml: no [licenses] section, so cargo-deny did not check licenses";
        let passed = with_unasked(Outcome::passed(), &asked);
        assert_eq!(
            (passed.passed, rendered(&passed.findings)),
            (true, vec![gap.to_string()])
        );
        let banned = Outcome::failed(vec![Finding::at("Cargo.lock", "banned")]);
        assert_eq!(
            rendered(&with_unasked(banned, &asked).findings),
            vec!["Cargo.lock: banned", gap]
        );
        let unread = Outcome::failed(Vec::new());
        assert_eq!(with_unasked(unread.clone(), &asked), unread);
        let every = ["advisories", "bans", "licenses", "sources"];
        assert_eq!(with_unasked(Outcome::passed(), &every), Outcome::passed());
    }
}
