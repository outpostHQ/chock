//! The `deps` gate: `cargo deny check` for each section the project's `deny.toml` has, read into
//! findings, with the checks nobody asked for named beside the answer.

use std::collections::BTreeMap;
use std::path::Path;

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
    let deny = |checks: &[&str]| -> Result<Outcome, String> {
        let args: Vec<&str> = ["deny", "check"].iter().chain(checks).copied().collect();
        let out = exec::tool(&ctx.root, "cargo", &args)?;
        let mut outcome = with_unplaced(verdict(&out, &ctx.root), &out);
        let said = format!("{}{}", out.stderr, out.stdout);
        let read = |file: &str| std::fs::read_to_string(ctx.root.join(file)).ok();
        relocated(&mut outcome.findings, &said, &read);
        Ok(outcome)
    };
    let outcome = checked(&asked, &deny)?;
    Ok(with_unasked(outcome, &asked))
}

/// All the `asked` checks in one run, then each alone when that run gave nothing to read.
fn checked(
    asked: &[&str],
    deny: &dyn Fn(&[&str]) -> Result<Outcome, String>,
) -> Result<Outcome, String> {
    let whole = deny(asked)?;
    match unread(&whole) && asked.len() > 1 {
        true => alone(asked, whole, deny),
        false => Ok(whole),
    }
}

/// Each check alone, after a run of all of them gave nothing to read: cargo-deny 0.20.2 overflows
/// its stack in `bans` on a dev-dependency cycle, and that hid what the other checks found.
fn alone(
    asked: &[&str],
    whole: Outcome,
    deny: &dyn Fn(&[&str]) -> Result<Outcome, String>,
) -> Result<Outcome, String> {
    let mut found = Outcome::passed();
    let mut silent = Vec::new();
    for check in asked {
        let one = deny(&[check])?;
        if unread(&one) {
            silent.push((*check, one));
            continue;
        }
        found.passed &= one.passed;
        found.findings.extend(one.findings);
    }
    if silent.len() == asked.len() {
        return Ok(whole);
    }
    // Every other check passed, so the one that stopped is the whole answer, and cannot run.
    if found.passed {
        return Ok(silent.into_iter().next().map_or(found, |(_, one)| one));
    }
    let stopped = silent.iter().map(|(check, one)| stopped(check, one));
    found.findings.extend(stopped.collect::<Vec<_>>());
    Ok(found)
}

/// The finding for a check that gave nothing to read even alone, with the last line it wrote.
fn stopped(check: &str, one: &Outcome) -> Finding {
    let said = one.said.as_deref().unwrap_or_default();
    let last = said.lines().map(str::trim).rfind(|line| !line.is_empty());
    let message = format!(
        "`cargo deny check {check}` stopped with nothing chock could read, so {check} went \
         unchecked: {}",
        last.unwrap_or("it wrote nothing")
    );
    Finding::at(DENY, &message).item(check)
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

/// The lock file, whose lines cargo-deny numbers by a list of its own.
const LOCK: &str = "Cargo.lock";

/// Each finding on a lock file, moved from a line of cargo-deny's own list of crates to the line of
/// the file that names the same crate; to no line where the file has none, not to a wrong one.
fn relocated(findings: &mut [Finding], said: &str, read: &dyn Fn(&str) -> Option<String>) {
    let drawn = drawn(said);
    let mut locks = BTreeMap::new();
    for found in findings.iter_mut().filter(|found| is_lock(&found.file)) {
        let lock = locks
            .entry(found.file.clone())
            .or_insert_with(|| read(&found.file));
        let entry = drawn.iter().find(|(line, ..)| Some(*line) == found.line);
        let line = |&(_, name, version): &(u32, &str, &str)| named(lock.as_deref()?, name, version);
        found.line = entry.and_then(line);
    }
}

fn is_lock(file: &str) -> bool {
    Path::new(file).file_name().is_some_and(|name| name == LOCK)
}

/// Each line cargo-deny drew under the place of a lock file: its number in cargo-deny's list, then
/// the name and version of the crate on it.
fn drawn(said: &str) -> Vec<(u32, &str, &str)> {
    let under_lock = said.split("┌─ ").skip(1).filter(|place| {
        let at = place.lines().next().unwrap_or_default();
        at.rsplitn(3, ':').nth(2).is_some_and(is_lock)
    });
    under_lock.flat_map(str::lines).filter_map(listed).collect()
}

/// `70 │ ╭ getrandom 0.2.17 registry+…`: the number of a line in cargo-deny's list, and its crate.
fn listed(line: &str) -> Option<(u32, &str, &str)> {
    let (number, rest) = line.trim().split_once(" │ ")?;
    let mut words = rest.trim_start_matches(['│', '╭', ' ']).split_whitespace();
    Some((number.parse().ok()?, words.next()?, words.next()?))
}

/// The line of `lock` that names the crate `name` at `version`, counted from 1.
fn named(lock: &str, name: &str, version: &str) -> Option<u32> {
    let (name, version) = (
        format!("name = \"{name}\""),
        format!("version = \"{version}\""),
    );
    let pair = |(first, next): (&str, &str)| first == name && next == version;
    let at = lock.lines().zip(lock.lines().skip(1)).position(pair)?;
    u32::try_from(at + 1).ok()
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

    /// Real cargo-deny 0.20.2 output for two `getrandom`s; its line 70 is in a list it made.
    const DUPLICATE: &str = "warning[duplicate]: found 2 duplicate entries for crate 'getrandom'\n   \u{250c}\u{2500} /w/proj/Cargo.lock:70:1\n   \u{2502}\n70 \u{2502} \u{256d} getrandom 0.2.17 registry+https://github.com/rust-lang/crates.io-index\n71 \u{2502} \u{2502} getrandom 0.3.4 registry+https://github.com/rust-lang/crates.io-index\n   \u{2502} \u{2570}\u{2500}\u{2500}\n   \u{251c} getrandom v0.2.17\n     \u{2514}\u{2500}\u{2500} ring v0.17.14\n";

    /// The lock file that output is about, where each `getrandom` is named far from line 70.
    const LOCKED: &str = "version = 4\n\n[[package]]\nname = \"getrandom\"\nversion = \"0.2.17\"\n\n\
                          [[package]]\nname = \"getrandom\"\nversion = \"0.3.4\"\n";

    #[test]
    fn a_finding_on_the_lock_file_is_at_the_line_that_names_its_crate() {
        let lines = |said: &str, lock: &'static str| {
            let out = exec::Output::of(Some(0), "", said);
            let mut found = verdict(&out, Path::new("/w/proj")).findings;
            relocated(&mut found, said, &|file| {
                (file == LOCK).then(|| lock.to_string())
            });
            found.iter().map(|found| found.line).collect::<Vec<_>>()
        };
        assert_eq!(lines(DUPLICATE, LOCKED), [Some(4)]);
        assert_eq!(lines(&DUPLICATE.replace(":70:", ":71:"), LOCKED), [Some(8)]);
        // A lock that names no such crate gives no line, and another file keeps its own.
        assert_eq!(lines(DUPLICATE, "version = 4\n"), [None]);
        assert_eq!(lines(&DUPLICATE.replace(LOCK, DENY), LOCKED), [Some(70)]);
        // A line drawn under another file is no crate, though it has the same number.
        let policy = "warning[a]: b\n \u{250c}\u{2500} /w/proj/deny.toml:70:1\n70 \u{2502} x = 1\n";
        assert_eq!(
            lines(&format!("{policy}{DUPLICATE}"), LOCKED),
            [Some(70), Some(4)]
        );
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

    /// The run scm saw: `bans` overflows its stack, and an advisory went unreported with it.
    #[test]
    fn a_check_that_crashes_alone_hides_no_finding_of_the_others() {
        let asked = ["advisories", "bans", "sources"];
        let crashed = || {
            let mut out = Outcome::failed(Vec::new());
            out.said = Some("thread 'main' has overflowed its stack\nfatal runtime error\n".into());
            out
        };
        let advisory = || Outcome::failed(vec![Finding::at("Cargo.lock", "zip 8.6.0").item("A-1")]);
        let deny = |checks: &[&str]| -> Result<Outcome, String> {
            Ok(match checks {
                ["advisories"] => advisory(),
                ["bans"] => crashed(),
                _ => Outcome::passed(),
            })
        };
        let found = alone(&asked, crashed(), &deny).unwrap();
        assert!(!found.passed);
        assert_eq!(
            rendered(&found.findings),
            [
                "Cargo.lock: A-1: zip 8.6.0",
                "deny.toml: bans: `cargo deny check bans` stopped with nothing chock could read, so \
                 bans went unchecked: fatal runtime error"
            ]
        );
        let clean = |checks: &[&str]| -> Result<Outcome, String> {
            Ok(if checks == ["bans"] {
                crashed()
            } else {
                Outcome::passed()
            })
        };
        let refused = alone(&asked, Outcome::failed(Vec::new()), &clean).unwrap();
        assert_eq!(
            refused,
            crashed(),
            "the others passed, so the crash is the answer"
        );
        let mut whole = Outcome::failed(Vec::new());
        whole.said = Some("every check together".into());
        let all = |_: &[&str]| -> Result<Outcome, String> { Ok(crashed()) };
        assert_eq!(alone(&asked, whole.clone(), &all).unwrap(), whole);
    }

    /// One call answers, unless several checks ran together and left nothing to read.
    #[test]
    fn only_a_silent_run_of_several_checks_runs_each_check_again() {
        let asked = ["advisories", "bans", "sources"];
        let crashed = || {
            let mut out = Outcome::failed(Vec::new());
            out.said = Some("thread 'main' has overflowed its stack\n".into());
            out
        };
        let advisory = || Outcome::failed(vec![Finding::at("Cargo.lock", "zip 8.6.0").item("A-1")]);
        let calls = &std::cell::Cell::new(0);
        let deny = |checks: &[&str]| -> Result<Outcome, String> {
            calls.set(calls.get() + 1);
            Ok(match checks {
                ["advisories"] => advisory(),
                ["bans"] | [_, _, _] => crashed(),
                _ => Outcome::passed(),
            })
        };
        let found = checked(&asked, &deny).unwrap();
        assert_eq!(calls.get(), 4);
        assert_eq!(found, alone(&asked, crashed(), &deny).unwrap());
        calls.set(0);
        // The second call of each would answer otherwise, so only one call may happen.
        let first = |answer: fn() -> Outcome| {
            move |_: &[&str]| -> Result<Outcome, String> {
                calls.set(calls.get() + 1);
                Ok(if calls.get() == 1 {
                    answer()
                } else {
                    Outcome::passed()
                })
            }
        };
        assert_eq!(checked(&asked, &first(advisory)).unwrap(), advisory());
        assert_eq!(calls.replace(0), 1);
        assert_eq!(checked(&["bans"], &first(crashed)).unwrap(), crashed());
        assert_eq!(calls.get(), 1);
    }
}
