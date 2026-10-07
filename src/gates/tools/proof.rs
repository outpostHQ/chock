//! The `proof` gate: every kani harness in the workspace must verify.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::exec;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

pub const GATE: Gate = Gate {
    name: "proof",
    about: "every kani harness still verifies; needs harnesses to have been written",
    group: Group::OptIn,
    builds: true,
    // The verdict depends only on the tree and the cargo and kani versions, so it can be recalled.
    reads: Some(crate::run::verdicts::Reads::tree_and(&["cargo", "kani"]).versioned("proof-v2")),
    kind: Kind::Binary(check),
};

#[derive(Debug, Deserialize)]
struct Listing {
    #[serde(default, rename = "standard-harnesses")]
    standard: serde_json::Value,
    #[serde(default, rename = "contract-harnesses")]
    contract: serde_json::Value,
}

fn check(ctx: &Ctx) -> Result<Outcome, String> {
    ctx.default_build("cargo kani")?;
    clear_listing(&ctx.root)?;
    let listed = exec::run("cargo", &invocation(&ctx.features, &LIST), &ctx.root)
        .map_err(|e| e.to_string())?;
    let harnesses = read_harnesses(&listed, || read_listing(&ctx.root))?;
    // Each harness is one solver on one core, eight at most, within this lane's share of the cap.
    let jobs = crate::exec::budget::cap().clamp(1, 8).to_string();
    let out = exec::run(
        "cargo",
        &invocation(&ctx.features, &verify(&jobs)),
        &ctx.root,
    )
    .map_err(|e| e.to_string())?;
    read_verdict(&out, &harnesses)
}

const LIST: [&str; 3] = ["list", "--format", "json"];

/// Kani verifies harnesses side by side only with terse output.
fn verify(jobs: &str) -> [&str; 4] {
    ["--output-format", "terse", "--jobs", jobs]
}

/// The listing and the verification build the same features.
fn invocation<'a>(features: &'a [String], then: &[&'a str]) -> Vec<&'a str> {
    // Without `--workspace`, a virtual workspace lists only cargo's default members.
    let mut argv = vec!["kani", "--workspace"];
    argv.extend(features.iter().map(String::as_str));
    argv.extend(then);
    argv
}

/// Removes the old listing file, so a run that writes none cannot be read as the last one.
fn clear_listing(root: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(root.join(LISTING)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove the old {LISTING}: {e}")),
    }
}

fn read_harnesses(
    listed: &exec::Output,
    listing: impl Fn() -> Result<String, String>,
) -> Result<BTreeSet<String>, String> {
    if !listed.success() || listed.truncated {
        return Err(format!(
            "cargo kani could not completely list harnesses: {}",
            listed.why_it_failed()
        ));
    }
    listed_harnesses(&listing()?)
}

fn listed_harnesses(text: &str) -> Result<BTreeSet<String>, String> {
    let harnesses = harness_names(text)?;
    if harnesses.is_empty() {
        return Err(
            "no #[kani::proof] harnesses, so kani would verify nothing and exit 0 regardless"
                .to_string(),
        );
    }
    Ok(harnesses)
}

/// Judges the run; exit zero counts only with a complete summary for every listed harness.
fn read_verdict(out: &exec::Output, expected: &BTreeSet<String>) -> Result<Outcome, String> {
    let harnesses = verified_summaries(out, expected)?;
    let judged = findings(&harnesses);
    if !judged.is_empty() {
        return Ok(Outcome::failed(judged));
    }
    finished(out)
}

fn verified_summaries(
    out: &exec::Output,
    expected: &BTreeSet<String>,
) -> Result<Vec<Harness>, String> {
    if out.truncated {
        return Err("kani output was truncated, so not every harness can be accounted for".into());
    }
    let harnesses = summarised(&out.stdout)?;
    complete(&harnesses, expected)?;
    Ok(harnesses)
}

/// Each harness at fault. If no harness is at fault, each failed check, except the panic that a
/// `should_panic` harness expects.
fn findings(harnesses: &[Harness]) -> Vec<Finding> {
    let judged: Vec<Finding> = harnesses.iter().filter_map(Harness::fault).collect();
    if !judged.is_empty() {
        return judged;
    }
    harnesses
        .iter()
        .filter(|held| !held.panicked_as_expected())
        .flat_map(|held| &held.failed_checks)
        .map(|message| Finding::at("", message).item("kani"))
        .collect()
}

fn finished(out: &exec::Output) -> Result<Outcome, String> {
    if out.success() {
        return Ok(Outcome::passed());
    }
    Err(format!(
        "kani did not finish successfully: {}",
        out.why_it_failed()
    ))
}

fn complete(harnesses: &[Harness], expected: &BTreeSet<String>) -> Result<(), String> {
    if expected.is_empty() {
        return Err("kani has no expected harnesses to verify".to_string());
    }
    let observed: BTreeSet<String> = harnesses.iter().map(|h| h.name.clone()).collect();
    let missing: Vec<&str> = expected.difference(&observed).map(String::as_str).collect();
    let unexpected: Vec<&str> = observed.difference(expected).map(String::as_str).collect();
    if !missing.is_empty() || !unexpected.is_empty() {
        return Err(format!(
            "kani harness summaries do not match the listing; missing: [{}]; unexpected: [{}]",
            missing.join(", "),
            unexpected.join(", ")
        ));
    }
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Harness {
    name: String,
    summary: Option<Checks>,
    covers: Option<(u64, u64)>,
    /// What kani concluded after `VERIFICATION:- `, with the note it adds for a `should_panic`.
    verdict: Option<String>,
    failed_checks: Vec<String>,
}

/// Kani's verdict on a `#[kani::should_panic]` harness that panicked: it passes by failing.
const EXPECTED_PANIC: &str = "SUCCESSFUL (encountered one or more panics as expected)";

#[derive(Debug, PartialEq, Eq)]
struct Checks {
    failed: u64,
    total: u64,
    unreachable: u64,
}

impl Checks {
    fn failure(&self, expected_panic: bool) -> Option<String> {
        if self.failed > 0 && !expected_panic {
            return Some(format!("{} of {} checks failed", self.failed, self.total));
        }
        if self.total > 0 && self.unreachable == self.total {
            return Some(format!(
                "vacuous: all {} checks unreachable, so its assumptions cannot hold",
                self.total
            ));
        }
        None
    }

    fn empty(&self) -> Option<String> {
        (self.total == 0).then(|| "nothing was checked, so verifying proved nothing".to_string())
    }
}

impl Harness {
    fn fault(&self) -> Option<Finding> {
        let summary = self.summary.as_ref()?;
        summary
            .failure(self.panicked_as_expected())
            .or_else(|| self.refused())
            .or_else(|| self.uncovered())
            .or_else(|| summary.empty())
            .map(|message| Finding::at("", &message).item(&self.name))
    }

    /// Whether kani passed this `should_panic` harness because it panicked as expected.
    fn panicked_as_expected(&self) -> bool {
        self.verdict.as_deref() == Some(EXPECTED_PANIC)
    }

    /// Kani's failing verdict, which also catches a `should_panic` harness that never panicked.
    fn refused(&self) -> Option<String> {
        self.verdict
            .as_deref()
            .filter(|verdict| verdict.starts_with("FAILED"))
            .map(|verdict| format!("kani's verdict is {verdict}"))
    }

    fn uncovered(&self) -> Option<String> {
        let (satisfied, declared) = self.covers?;
        (satisfied < declared).then(|| format!(
            "{} of {declared} cover properties never satisfied, so a declared case is unreachable",
            declared - satisfied
        ))
    }

    fn read(&mut self, said: &str) -> Result<(), String> {
        let Some(rest) = said.strip_prefix("** ") else {
            return self.noted(said);
        };
        self.record(rest).ok_or_else(|| {
            format!(
                "kani reported an unreadable or duplicate summary for {}: {said}",
                self.name
            )
        })
    }

    /// What kani says of a harness besides its counts: its verdict, and each check it failed.
    fn noted(&mut self, said: &str) -> Result<(), String> {
        if let Some(verdict) = said.strip_prefix("VERIFICATION:- ") {
            return self.concluded(verdict);
        }
        if let Some(check) = said.strip_prefix("Failed Checks: ").map(str::trim) {
            self.failed_checks
                .extend((!check.is_empty()).then(|| check.to_string()));
        }
        Ok(())
    }

    fn concluded(&mut self, verdict: &str) -> Result<(), String> {
        match self.verdict.replace(verdict.to_string()) {
            None => Ok(()),
            Some(_) => Err(format!(
                "kani reported a second verdict for {}: {verdict}",
                self.name
            )),
        }
    }

    fn record(&mut self, rest: &str) -> Option<()> {
        if let Some(counts) = counted(rest, " cover properties satisfied") {
            return self.covers.replace(counts).is_none().then_some(());
        }
        self.summary.replace(checks(rest)?).is_none().then_some(())
    }
}

fn summarised(stdout: &str) -> Result<Vec<Harness>, String> {
    let mut reading = Reading::default();
    for line in stdout.lines().map(exec::strip_colour) {
        reading.line(line.trim())?;
    }
    finished_summaries(reading.found)
}

/// Kani's output, harness by harness. Verifying side by side, kani puts `Thread N: ` before each
/// harness it starts and before each result, and prints a result in one piece.
#[derive(Default)]
struct Reading {
    found: Vec<Harness>,
    names: BTreeSet<String>,
    /// The harness each thread verifies.
    threads: BTreeMap<String, usize>,
    /// The harness a line without a thread belongs to.
    current: Option<usize>,
}

impl Reading {
    fn line(&mut self, said: &str) -> Result<(), String> {
        let (thread, said) = threaded(said);
        if let Some(rest) = said.strip_prefix("Checking harness ") {
            self.found.push(started_harness(rest, &mut self.names)?);
            self.current = Some(self.found.len() - 1);
            self.threads
                .extend(thread.map(|n| (n.to_string(), self.found.len() - 1)));
            return Ok(());
        }
        if let Some(n) = thread {
            let held = self.threads.get(n).ok_or_else(|| {
                format!("kani reported a result on thread {n} before it started a harness there")
            })?;
            self.current = Some(*held);
        }
        match self.current.and_then(|held| self.found.get_mut(held)) {
            Some(held) => held.read(said),
            None if said.starts_with("** ") => {
                Err("kani reported a summary without a harness identity".to_string())
            }
            None => Ok(()),
        }
    }
}

/// The thread a line names and the rest of the line, or no thread and the whole line.
fn threaded(said: &str) -> (Option<&str>, &str) {
    let named = said
        .strip_prefix("Thread ")
        .and_then(|rest| rest.split_once(':'))
        .filter(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    match named {
        Some((n, rest)) => (Some(n), rest.trim()),
        None => (None, said),
    }
}

fn started_harness(rest: &str, names: &mut BTreeSet<String>) -> Result<Harness, String> {
    let name = rest
        .strip_suffix("...")
        .filter(|name| !name.is_empty())
        .ok_or_else(|| format!("kani reported an unreadable harness identity: {rest}"))?;
    if !names.insert(name.to_string()) {
        return Err(format!(
            "kani reported a duplicate harness identity: {name}"
        ));
    }
    Ok(Harness {
        name: name.to_string(),
        ..Harness::default()
    })
}

fn finished_summaries(found: Vec<Harness>) -> Result<Vec<Harness>, String> {
    if let Some(harness) = found.iter().find(|h| h.summary.is_none()) {
        return Err(format!(
            "kani reported no check summary for {}",
            harness.name
        ));
    }
    Ok(found)
}

fn counted(rest: &str, tail: &str) -> Option<(u64, u64)> {
    let (count, total) = rest.strip_suffix(tail)?.split_once(" of ")?;
    let count: u64 = count.parse().ok()?;
    let total: u64 = total.parse().ok()?;
    (count <= total).then_some((count, total))
}

fn checks(rest: &str) -> Option<Checks> {
    let (counts, unreachable) = match rest.split_once(" (") {
        Some((counts, note)) => (counts, note.strip_suffix(" unreachable)")?.parse().ok()?),
        None => (rest, 0),
    };
    let (failed, total) = counted(counts, " failed")?;
    (unreachable <= total - failed).then_some(Checks {
        failed,
        total,
        unreachable,
    })
}

const LISTING: &str = "kani-list.json";

fn read_listing(root: &std::path::Path) -> Result<String, String> {
    std::fs::read_to_string(root.join(LISTING))
        .map_err(|e| format!("cargo kani wrote no {LISTING}: {e}"))
}

fn harness_names(listing_json: &str) -> Result<BTreeSet<String>, String> {
    let listing: Listing = serde_json::from_str(listing_json)
        .map_err(|e| format!("cargo kani listed harnesses unreadably: {e}"))?;
    let mut names = BTreeSet::new();
    for section in [&listing.standard, &listing.contract] {
        if !section.is_null() {
            names_in(section, &mut names)?;
        }
    }
    Ok(names)
}

/// Kani's maps locate harnesses by source file; every array element is a harness identity.
fn names_in(value: &serde_json::Value, names: &mut BTreeSet<String>) -> Result<(), String> {
    match value {
        serde_json::Value::Object(map) => {
            for nested in map.values() {
                names_in(nested, names)?;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                listed_name(item, names)?;
            }
        }
        _ => return Err("kani listed an unreadable harness collection".to_string()),
    }
    Ok(())
}

fn listed_name(item: &serde_json::Value, names: &mut BTreeSet<String>) -> Result<(), String> {
    let name = item
        .as_str()
        .filter(|name| !name.trim().is_empty())
        .ok_or("kani listed an unreadable harness identity")?;
    if !names.insert(name.to_string()) {
        return Err(format!("kani listed a duplicate harness identity: {name}"));
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap or panic in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_check_summary_is_empty_not_a_vacuous_failure() {
        let empty = Checks {
            failed: 0,
            total: 0,
            unreachable: 0,
        };
        assert_eq!(empty.failure(false), None);
        assert_eq!(
            empty.empty().as_deref(),
            Some("nothing was checked, so verifying proved nothing")
        );
        let unreachable = Checks {
            failed: 0,
            total: 2,
            unreachable: 2,
        };
        assert_eq!(
            unreachable.failure(false).as_deref(),
            Some("vacuous: all 2 checks unreachable, so its assumptions cannot hold")
        );
        assert_eq!(unreachable.empty(), None);
    }

    // Recorded from Kani; all four harnesses exit zero, including the three vacuous ones.
    const MEASURED: &str = "\
Checking harness proofs::a_cover_that_cannot_be_satisfied...
SUMMARY:
 ** 0 of 2 failed
 ** 0 of 1 cover properties satisfied
VERIFICATION:- SUCCESSFUL
Checking harness proofs::vacuous_by_contradictory_assumptions...
SUMMARY:
 ** 0 of 2 failed (2 unreachable)
VERIFICATION:- SUCCESSFUL
Checking harness proofs::vacuous_by_a_false_assumption...
SUMMARY:
 ** 0 of 2 failed (2 unreachable)
VERIFICATION:- SUCCESSFUL
Checking harness proofs::doubling_never_exceeds_the_range...
SUMMARY:
 ** 0 of 2 failed
VERIFICATION:- SUCCESSFUL
Manual Harness Summary:
Complete - 4 successfully verified harnesses, 0 failures, 4 total.
";
    const ONE: &str = r#"{"standard-harnesses":{"src/a.rs":["one"]},"contract-harnesses":{}}"#;
    const HONEST: &str =
        "Checking harness one...\nSUMMARY:\n ** 0 of 2 failed\nVERIFICATION:- SUCCESSFUL\n";

    fn ran(code: Option<i32>, stdout: &str, stderr: &str) -> exec::Output {
        exec::Output {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            truncated: false,
        }
    }

    fn expected(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn verdict_of(stdout: &str, code: i32) -> Result<Outcome, String> {
        read_verdict(&ran(Some(code), stdout, ""), &expected(&["one"]))
    }

    #[test]
    fn a_harness_whose_assumptions_cannot_hold_is_not_a_proof() {
        let names = expected(&[
            "proofs::a_cover_that_cannot_be_satisfied",
            "proofs::vacuous_by_contradictory_assumptions",
            "proofs::vacuous_by_a_false_assumption",
            "proofs::doubling_never_exceeds_the_range",
        ]);
        let outcome = read_verdict(&ran(Some(0), MEASURED, ""), &names).unwrap();
        assert!(!outcome.passed);
        let said: Vec<String> = outcome.findings.iter().map(Finding::render).collect();
        assert_eq!(
            said,
            [
                "proofs::a_cover_that_cannot_be_satisfied: 1 of 1 cover properties never satisfied, so a declared case is unreachable",
                "proofs::vacuous_by_contradictory_assumptions: vacuous: all 2 checks unreachable, so its assumptions cannot hold",
                "proofs::vacuous_by_a_false_assumption: vacuous: all 2 checks unreachable, so its assumptions cannot hold",
            ]
        );
    }

    #[test]
    fn every_expected_harness_must_have_a_complete_reachable_summary() {
        assert_eq!(verdict_of(HONEST, 0).unwrap(), Outcome::passed());
        let why = read_verdict(&ran(Some(0), HONEST, ""), &expected(&["one", "two"])).unwrap_err();
        assert!(why.contains("missing: [two]"), "{why}");
        let why = verdict_of(&HONEST.replace("one...", "two..."), 0).unwrap_err();
        assert!(why.contains("missing: [one]"), "{why}");
        assert!(why.contains("unexpected: [two]"), "{why}");
    }

    #[test]
    fn no_output_is_not_proof_even_when_kani_exits_zero() {
        let why = verdict_of("", 0).unwrap_err();
        assert!(why.contains("missing: [one]"), "{why}");
        assert!(read_verdict(&ran(Some(0), "", ""), &BTreeSet::new()).is_err());
    }

    #[test]
    fn a_harness_with_zero_checks_is_a_measured_vacuous_proof() {
        let outcome = verdict_of(&HONEST.replace("0 of 2", "0 of 0"), 0).unwrap();
        assert_eq!(
            outcome.findings,
            [Finding::at("", "nothing was checked, so verifying proved nothing").item("one")]
        );
        assert!(!outcome.passed);
    }

    /// Kani 0.68's four answers, captured from a crate holding one harness of each.
    const SHOULD_PANIC: &str = "\
Checking harness proofs::holds...
SUMMARY:
 ** 0 of 3 failed
VERIFICATION:- SUCCESSFUL
Checking harness proofs::fails_without_panicking...
SUMMARY:
 ** 1 of 1 failed
Failed Checks: null pointer dereference occurred
 File: \"src/lib.rs\", line 29, in proofs::fails_without_panicking
VERIFICATION:- FAILED (encountered failures other than panics, which were unexpected)
Checking harness proofs::never_panics...
SUMMARY:
 ** 0 of 2 failed
VERIFICATION:- FAILED (encountered no panics, but at least one was expected)
Checking harness proofs::panics_as_expected...
SUMMARY:
 ** 1 of 2 failed
Failed Checks: \"x is below the maximum\"
 File: \"src/lib.rs\", line 4, in checked
VERIFICATION:- SUCCESSFUL (encountered one or more panics as expected)
Complete - 2 successfully verified harnesses, 2 failures, 4 total.
";

    #[test]
    fn a_should_panic_harness_is_judged_by_kanis_verdict_and_not_its_failed_checks() {
        let names = expected(&[
            "proofs::holds",
            "proofs::fails_without_panicking",
            "proofs::never_panics",
            "proofs::panics_as_expected",
        ]);
        let outcome = read_verdict(&ran(Some(1), SHOULD_PANIC, ""), &names).unwrap();
        let unexpected =
            "kani's verdict is FAILED (encountered no panics, but at least one was expected)";
        assert_eq!(
            outcome.findings,
            [
                Finding::at("", "1 of 1 checks failed").item("proofs::fails_without_panicking"),
                Finding::at("", unexpected).item("proofs::never_panics"),
            ]
        );
        assert!(!outcome.passed);
    }

    #[test]
    fn a_harness_that_panicked_as_expected_passes_and_its_panic_is_no_finding() {
        let witness = SHOULD_PANIC
            .find("Checking harness proofs::panics_as_expected")
            .unwrap();
        let passing = format!("{HONEST}{}", &SHOULD_PANIC[witness..]);
        let names = expected(&["one", "proofs::panics_as_expected"]);
        let outcome = read_verdict(&ran(Some(0), &passing, ""), &names).unwrap();
        assert_eq!(outcome.findings, []);
        assert!(outcome.passed);
    }

    #[test]
    fn an_expected_panic_excuses_the_failed_count_and_nothing_else() {
        let failing = Checks {
            failed: 1,
            total: 2,
            unreachable: 0,
        };
        assert_eq!(
            failing.failure(false).as_deref(),
            Some("1 of 2 checks failed")
        );
        assert_eq!(failing.failure(true), None);
        let mut witness = Harness {
            name: "w".to_string(),
            summary: Some(failing),
            ..Harness::default()
        };
        witness
            .noted(&format!("VERIFICATION:- {EXPECTED_PANIC}"))
            .unwrap();
        assert!(witness.panicked_as_expected());
        assert_eq!(witness.fault(), None);
        assert!(
            witness
                .noted("VERIFICATION:- SUCCESSFUL")
                .unwrap_err()
                .contains("second verdict")
        );
        let mut plain = Harness {
            name: "p".to_string(),
            summary: witness.summary.take(),
            ..Harness::default()
        };
        plain.noted("VERIFICATION:- SUCCESSFUL").unwrap();
        assert!(!plain.panicked_as_expected());
        assert_eq!(
            plain.fault(),
            Some(Finding::at("", "1 of 2 checks failed").item("p"))
        );
    }

    /// Kani 0.68 with `--jobs`: both harnesses start before either result arrives.
    const SIDE_BY_SIDE: &str = "\
Thread 0: Checking harness fails...
Thread 1: Checking harness holds...
Thread 1:
VERIFICATION RESULT:
 ** 0 of 1 failed
VERIFICATION:- SUCCESSFUL
Thread 0:
VERIFICATION RESULT:
 ** 1 of 2 failed
Failed Checks: assertion failed: x < 200
VERIFICATION:- FAILED
Thread 1: Checking harness later...
Thread 1:
 ** 0 of 3 failed
VERIFICATION:- SUCCESSFUL
";

    #[test]
    fn a_result_verified_side_by_side_belongs_to_the_harness_its_thread_started() {
        let names = expected(&["fails", "holds", "later"]);
        let outcome = read_verdict(&ran(Some(1), SIDE_BY_SIDE, ""), &names).unwrap();
        assert_eq!(
            outcome.findings,
            [Finding::at("", "1 of 2 checks failed").item("fails")]
        );
        let why = summarised("Thread 2: \n ** 0 of 1 failed\n").unwrap_err();
        assert!(why.contains("thread 2"), "{why}");
        let why = summarised(" ** 0 of 1 failed\n").unwrap_err();
        assert!(why.contains("without a harness identity"), "{why}");
    }

    #[test]
    fn a_failed_check_is_kept_and_an_empty_one_is_not() {
        let mut held = Harness::default();
        held.noted("Failed Checks: overflow in add").unwrap();
        held.noted("Failed Checks:   ").unwrap();
        held.noted("SUMMARY:").unwrap();
        assert_eq!(held.failed_checks, ["overflow in add"]);
        assert_eq!(held.verdict, None);
    }

    #[test]
    fn a_failed_check_is_named_without_calling_the_run_a_success() {
        let outcome = verdict_of(&HONEST.replace("0 of 2", "1 of 2"), 1).unwrap();
        assert_eq!(
            outcome.findings,
            [Finding::at("", "1 of 2 checks failed").item("one")]
        );
        assert!(!outcome.passed);
    }

    #[test]
    fn an_announced_harness_without_a_summary_is_unmeasured_not_vacuous() {
        let why =
            verdict_of("Checking harness one...\nVERIFICATION:- SUCCESSFUL\n", 0).unwrap_err();
        assert_eq!(why, "kani reported no check summary for one");
    }

    #[test]
    fn duplicate_harnesses_or_summaries_cannot_replace_missing_work() {
        assert!(
            verdict_of(&format!("{HONEST}{HONEST}"), 0)
                .unwrap_err()
                .contains("duplicate harness")
        );
        assert!(
            verdict_of(&format!("{HONEST} ** 0 of 2 failed\n"), 0)
                .unwrap_err()
                .contains("duplicate summary")
        );
        let covers = format!(
            "{HONEST} ** 1 of 1 cover properties satisfied\n ** 1 of 1 cover properties satisfied\n"
        );
        assert!(
            verdict_of(&covers, 0)
                .unwrap_err()
                .contains("duplicate summary")
        );
    }

    #[test]
    fn malformed_or_inconsistent_summary_numbers_never_prove_a_harness() {
        for summary in [
            "x of 2 failed",
            "3 of 2 failed",
            "0 of 2 failed (3 unreachable)",
            "1 of 2 failed (2 unreachable)",
            "0 of 2 failed ignored",
            "0 of 2 failed (x unreachable)",
            "2 of 1 cover properties satisfied",
            "0 of 2 something else",
        ] {
            let text = HONEST.replace("0 of 2 failed", summary);
            assert!(verdict_of(&text, 0).is_err(), "{summary}");
        }
        assert!(verdict_of(" ** 0 of 2 failed\n", 0).is_err());
        assert!(verdict_of("Checking harness ...\n ** 0 of 2 failed\n", 0).is_err());
    }

    #[test]
    fn truncated_or_abnormally_terminated_output_is_not_a_proof() {
        let mut out = ran(Some(0), HONEST, "");
        out.truncated = true;
        assert!(
            read_verdict(&out, &expected(&["one"]))
                .unwrap_err()
                .contains("truncated")
        );
        let why =
            read_verdict(&ran(None, HONEST, "solver stopped"), &expected(&["one"])).unwrap_err();
        assert!(why.contains("solver stopped"), "{why}");
    }

    #[test]
    fn a_nonzero_run_can_report_named_failures_but_cannot_invent_checked_harnesses() {
        let outcome = verdict_of(&format!("{HONEST}Failed Checks: overflow in add\n"), 1).unwrap();
        assert_eq!(
            outcome.findings,
            [Finding::at("", "overflow in add").item("kani")]
        );
        assert!(!outcome.passed);
        assert!(verdict_of("VERIFICATION:- FAILED\n", 1).is_err());
        assert!(verdict_of(HONEST, 1).is_err());
    }

    #[test]
    fn colour_does_not_change_a_harness_identity_or_summary() {
        let coloured = format!("\u{1b}[32m{}\u{1b}[0m...", "one");
        let text = HONEST.replace("one...", &coloured);
        assert_eq!(verdict_of(&text, 0).unwrap(), Outcome::passed());
    }

    #[test]
    fn progress_before_the_first_harness_and_satisfied_covers_are_not_failures() {
        let text = format!("Compiling fixture\n{HONEST} ** 1 of 1 cover properties satisfied\n");
        assert_eq!(verdict_of(&text, 0).unwrap(), Outcome::passed());
        assert_eq!(Harness::default().fault(), None);
    }

    #[test]
    fn names_from_standard_and_contract_harnesses_are_preserved() {
        let json = r#"{"standard-harnesses":{"src/a.rs":["one","two"]},"contract-harnesses":{"src/b.rs":["three"]}}"#;
        assert_eq!(
            harness_names(json).unwrap(),
            expected(&["one", "two", "three"])
        );
    }

    #[test]
    fn a_listing_with_no_harnesses_is_distinct_from_an_unreadable_listing() {
        assert_eq!(
            harness_names(r#"{"standard-harnesses":{},"contract-harnesses":{}}"#),
            Ok(BTreeSet::new())
        );
        assert_eq!(
            harness_names(r#"{"standard-harnesses":{}}"#),
            Ok(BTreeSet::new())
        );
        for text in [
            "not json",
            r#"{"standard-harnesses":{"a":[1]}}"#,
            r#"{"standard-harnesses":{"a":[""]}}"#,
            r#"{"standard-harnesses":{"a":1}}"#,
            r#"{"standard-harnesses":{"a":["same"],"b":["same"]}}"#,
        ] {
            assert!(harness_names(text).is_err(), "{text}");
        }
    }

    #[test]
    fn a_listing_failure_or_truncation_is_refused_before_any_file_is_read() {
        let out = ran(Some(1), "", "kani is not installed");
        let why = read_harnesses(&out, || panic!("must not read a failed listing")).unwrap_err();
        assert!(why.contains("kani is not installed"), "{why}");
        let mut out = ran(Some(0), "", "");
        out.truncated = true;
        assert!(read_harnesses(&out, || panic!("must not read an incomplete listing")).is_err());
    }

    #[test]
    fn the_listing_returns_identities_and_rejects_no_expected_work() {
        assert_eq!(
            read_harnesses(&ran(Some(0), "", ""), || Ok(ONE.to_string())).unwrap(),
            expected(&["one"])
        );
        let why = read_harnesses(&ran(Some(0), "", ""), || Ok("{}".into())).unwrap_err();
        assert!(why.contains("no #[kani::proof] harnesses"), "{why}");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_listing_is_read_only_from_a_file_created_after_old_state_is_removed() {
        let root = crate::testdir::make("proof-listing");
        std::fs::write(root.join(LISTING), ONE).unwrap();
        clear_listing(&root).unwrap();
        assert!(read_harnesses(&ran(Some(0), "", ""), || read_listing(&root)).is_err());
        clear_listing(&root).unwrap();
        std::fs::write(root.join(LISTING), ONE).unwrap();
        assert_eq!(
            harness_names(&read_listing(&root).unwrap()).unwrap(),
            expected(&["one"])
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_listing_that_cannot_be_removed_is_a_refusal() {
        let root = crate::testdir::make("proof-listing-directory");
        std::fs::create_dir(root.join(LISTING)).unwrap();
        assert!(
            clear_listing(&root)
                .unwrap_err()
                .contains("cannot remove the old kani-list.json")
        );
    }

    #[test]
    fn listing_and_verification_build_the_same_configured_features() {
        let features = ["--no-default-features", "--features", "testkit,extra"].map(String::from);
        assert_eq!(
            invocation(&features, &verify("4")),
            [
                "kani",
                "--workspace",
                "--no-default-features",
                "--features",
                "testkit,extra",
                "--output-format",
                "terse",
                "--jobs",
                "4"
            ]
        );
        assert_eq!(
            invocation(&features, &LIST),
            [
                "kani",
                "--workspace",
                "--no-default-features",
                "--features",
                "testkit,extra",
                "list",
                "--format",
                "json"
            ]
        );
        assert_eq!(
            invocation(&[], &LIST),
            ["kani", "--workspace", "list", "--format", "json"]
        );
    }

    #[test]
    fn the_gate_is_asked_for_by_name_rather_than_run_by_default() {
        assert_eq!(GATE.group, Group::OptIn);
        assert_eq!(GATE.name, "proof");
    }
}
