//! The suite's gates: `test` runs it once, and `idempotent` runs it a second time in the same
//! tree.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{spans, verdict, whole};
use crate::exec;
use crate::project::config::Command;
use crate::run::report::Finding;
use crate::run::{Ctx, Outcome};

pub(super) fn test(ctx: &Ctx) -> Result<Outcome, String> {
    let (out, changed) = first_run(ctx)?;
    let mut outcome = read_suite(&out, &ctx.root);
    outcome.passed &= changed.is_empty();
    outcome
        .findings
        .extend(changed.iter().map(|path| Finding::at(path, CHANGED)));
    Ok(outcome)
}

/// What a file says that the suite changed: a test that writes the file it compares with cannot
/// fail.
const CHANGED: &str = "the suite changed this file, which the repository holds: a test that \
                       writes the file it should compare with passes without a check. If you \
                       edited the file during the run, run `chock run test` again";

/// The suite's outcome: the failing tests or worsened measures it names, else compiler spans.
pub(super) fn read_suite(out: &exec::Output, root: &Path) -> Outcome {
    if out.success() {
        return Outcome::passed();
    }
    let mut found: Vec<Finding> = failing_tests(out)
        .iter()
        .map(|test| Finding::at("", &failure(out, test, "this test failed")).item(test))
        .collect();
    found.extend(worsened(out));
    match found.is_empty() {
        // Nothing named a test or a measure, so the build failed; rustc's spans say where.
        true => verdict(out, root),
        false => Outcome::failed(whole(found, out)),
    }
}

/// Every measure `outpost check` says got worse, for a runner that calls outpost as a stage.
fn worsened(out: &exec::Output) -> Vec<Finding> {
    let mut lines: BTreeSet<String> = BTreeSet::new();
    for text in [&out.stdout, &out.stderr] {
        lines.extend(
            text.lines()
                .map(|line| exec::strip_colour(line).trim().to_string())
                .filter(|line| line.starts_with("worse ")),
        );
    }
    lines.iter().filter_map(|line| worse(line)).collect()
}

/// `worse <measure> <from> -> <to>`; a line without the `->` is prose and names nothing.
pub(super) fn worse(line: &str) -> Option<Finding> {
    let rest = line.strip_prefix("worse ")?;
    let (measure, moved) = rest.split_once(' ')?;
    let (from, to) = moved.split_once(" -> ")?;
    let message = format!("got worse: {} to {}", from.trim(), to.trim());
    Some(Finding::at("", &message).item(measure))
}

/// Every test given a failing verdict, from both streams: nextest's lines, or libtest's own where
/// nextest named none, as under a runner that calls `cargo test`.
pub(super) fn failing_tests(out: &exec::Output) -> BTreeSet<String> {
    // A set, since the same line can reach both streams.
    let named = |verdict: fn(&str) -> Option<String>| -> BTreeSet<String> {
        [&out.stdout, &out.stderr]
            .into_iter()
            .flat_map(|text| text.lines())
            .filter_map(|line| verdict(&exec::strip_colour(line)))
            .collect()
    };
    Some(named(failed_test))
        .filter(|nextest| !nextest.is_empty())
        .unwrap_or_else(|| named(libtest_failure))
}

/// `test <path> ... FAILED`, or the header over what a failed test printed, `---- <path> stdout
/// ----`, which still names a test whose own output split the first.
pub(super) fn libtest_failure(line: &str) -> Option<String> {
    let line = line.trim();
    let between = |start: &str, end: &str| line.strip_prefix(start)?.strip_suffix(end);
    between("test ", " ... FAILED")
        .or_else(|| between("---- ", " stdout ----"))
        .map(|named| named.trim().to_string())
}

/// `what`, with the panic of `test` when either stream holds it: `panicked at <where>:` over its
/// message, or the older one-line `panicked at '<message>', <where>`.
pub(super) fn failure(out: &exec::Output, test: &str, what: &str) -> String {
    // libtest names a test's thread by its path; nextest puts its binary first.
    let path = test.rsplit(' ').next().unwrap_or(test);
    let head = format!("thread '{path}' panicked at ");
    let panic = [&out.stderr, &out.stdout].into_iter().find_map(|text| {
        let lines = plain_lines(text);
        let at = lines.iter().position(|line| line.starts_with(&head))?;
        let place = &lines[at][head.len()..];
        let place = place.strip_suffix(':').unwrap_or(place);
        let said: Vec<&str> = (lines[at + 1..].iter().map(String::as_str))
            .take_while(|line| !line.is_empty() && !line.starts_with("note:"))
            .take(TAIL)
            .collect();
        Some(if said.is_empty() {
            format!("panicked at {place}")
        } else {
            format!("panicked at {place}: {}", said.join(" "))
        })
    });
    let Some(panic) = panic else {
        return what.to_string();
    };
    let mut quoted: String = panic.chars().take(PANIC_CHARS).collect();
    if quoted.len() < panic.len() {
        quoted.push('…');
    }
    format!("{what}: {quoted}")
}

/// The most of a panic message a finding quotes; an `assert_eq!` on a large value prints pages.
pub(super) const PANIC_CHARS: usize = 300;

/// Each line of a tool's output without its colour codes or the indent a runner adds.
fn plain_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| exec::strip_colour(line).trim().to_string())
        .collect()
}

/// `FAIL [   0.005s] <binary-id> <test path>`. `FLAKY` is not a failure: the test passed on retry.
pub(super) fn failed_test(line: &str) -> Option<String> {
    marked(line, &["FAIL", "TIMEOUT", "ABORT", "SIGSEGV", "LEAK-FAIL"])
}

/// Every test nextest stopped at its time limit, from both streams.
pub(super) fn timed_out_tests(out: &exec::Output) -> BTreeSet<String> {
    [&out.stdout, &out.stderr]
        .into_iter()
        .flat_map(|text| text.lines())
        .filter_map(|line| marked(&exec::strip_colour(line), &["TIMEOUT"]))
        .collect()
}

/// The test a nextest verdict line names, when the line starts with one of `verdicts`.
fn marked(line: &str, verdicts: &[&str]) -> Option<String> {
    let trimmed = line.trim();
    let rest = verdicts
        .iter()
        .find_map(|mark| trimmed.strip_prefix(mark))?;
    let (_duration, named) = rest.trim_start().strip_prefix('[')?.split_once(']')?;
    let named = uncounted(named.trim());
    (!named.is_empty()).then(|| named.to_string())
}

/// The name after the progress count newer nextest prints first: `(  12/1783) chock tests::x`.
pub(super) fn uncounted(named: &str) -> &str {
    let counted = named
        .strip_prefix('(')
        .and_then(|rest| rest.split_once(')'))
        .filter(|(count, _)| {
            count
                .chars()
                .all(|c| c.is_ascii_digit() || c == '/' || c == ' ')
        });
    counted.map_or(named, |(_count, after)| after.trim_start())
}

/// Runs the suite again in the same tree. Failing only the second time means runs share state, such
/// as a file left behind.
pub(super) fn idempotent(ctx: &Ctx) -> Result<Outcome, String> {
    // Reuses the run `test` already took when both gates are on.
    let (first, _) = first_run(ctx)?;
    if !first.success() {
        return Err("the suite fails on its first run, so a second says nothing".to_string());
    }
    let second = suite(ctx)?;
    if second.success() {
        return Ok(Outcome::passed());
    }
    Ok(Outcome::failed(failed_twice(&second, &ctx.root)))
}

/// What the second run says, most specific first: a compiler span, else the tests that failed, else
/// that it failed at all.
pub(super) fn failed_twice(second: &exec::Output, root: &Path) -> Vec<Finding> {
    let mut spanned = spans(&second.stderr, root);
    spanned.extend(spans(&second.stdout, root));
    if !spanned.is_empty() {
        return spanned;
    }
    let named = failing_tests(second);
    if !named.is_empty() {
        return named
            .iter()
            .map(|test| {
                let said = failure(
                    second,
                    test,
                    "passed on the first run and failed on the second",
                );
                Finding::at("", &said).item(test)
            })
            .collect();
    }
    // The last lines say where the run stopped: a runner's stage, or the error that ended it.
    let ends: Vec<String> = [("stdout", &second.stdout), ("stderr", &second.stderr)]
        .into_iter()
        .filter_map(|(stream, text)| {
            let lines: Vec<String> = (plain_lines(text).into_iter())
                .filter(|line| !line.is_empty())
                .collect();
            let last = lines[lines.len().saturating_sub(TAIL)..].join(" | ");
            (!last.is_empty()).then(|| format!("; {stream} ends: {last}"))
        })
        .collect();
    let said = format!(
        "the suite passed once and failed the second time in the same tree{}",
        ends.concat()
    );
    vec![Finding::at("", &said).item("idempotent")]
}

/// How many of a failed run's last lines a finding quotes, from each stream.
const TAIL: usize = 3;

/// The suite's first run in this invocation, shared by every gate that needs one, with the files
/// the repository holds that it changed.
fn first_run(ctx: &Ctx) -> Result<(exec::Output, Vec<String>), String> {
    ctx.suite.get_or_init(|| watched(ctx)).clone()
}

/// Runs the suite, and names each file the repository holds whose bytes differ after it.
fn watched(ctx: &Ctx) -> Result<(exec::Output, Vec<String>), String> {
    let before = changes(ctx);
    let out = suite(ctx)?;
    let changed = match (before, changes(ctx)) {
        (Some(before), Some(after)) => crate::project::differing(&before, &after),
        _ => Vec::new(),
    };
    Ok((out, changed))
}

/// Each file the repository holds that differs from its last commit, with a hash of its bytes, or
/// `None` once it is deleted; `None` where no repository says which files it holds.
fn changes(ctx: &Ctx) -> Option<BTreeMap<String, Option<u64>>> {
    let dirty = crate::project::vcs::dirty(&ctx.root, ctx.vcs).ok()?;
    let held: Vec<String> = dirty
        .into_iter()
        .filter(|path| !written_on_purpose(&ctx.commands, path))
        .collect();
    let hashes = crate::project::contents(&ctx.root, &held);
    let hashed = held.into_iter().map(|path| {
        let hash = hashes.get(&path).copied();
        (path, hash)
    });
    Some(hashed.collect())
}

/// A file that a run writes by design: the lockfile cargo settles, a report a gate asks for,
/// chock's own state, or what a declared command names among its `outputs`.
fn written_on_purpose(commands: &[Command], path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let declared = commands.iter().flat_map(|it| &it.outputs).any(|output| {
        let output = output.trim_end_matches('/');
        let rest = path.strip_prefix(output);
        rest.is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    });
    name == "Cargo.lock"
        || crate::project::PRODUCED.contains(&name)
        || path.split('/').any(|part| part == ".chock")
        || declared
}

/// Runs whatever the project's `runner` setting names, since some suites need their own setup.
pub(super) fn suite(ctx: &Ctx) -> Result<exec::Output, String> {
    let (program, args) = ctx
        .runner
        .argv
        .split_first()
        .ok_or("the configured `runner` names no command")?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    exec::tool(&ctx.root, program, &args)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn a_file_a_run_writes_by_design_is_not_watched() {
        let schema = Command {
            name: "schema".to_string(),
            run: vec!["scripts/gen-schema".to_string()],
            counts: false,
            builds: false,
            outputs: vec!["schema/".to_string(), "docs/cli.md".to_string()],
        };
        let commands = [schema];
        let by_design = [
            "Cargo.lock",
            "crates/a/Cargo.lock",
            "lcov.info",
            ".chock/baseline.json",
            "schema/config.json",
            "docs/cli.md",
        ];
        for path in by_design {
            assert!(written_on_purpose(&commands, path), "{path}");
        }
        for path in ["schemas.json", "docs/cli.md.bak", "src/lib.rs"] {
            assert!(!written_on_purpose(&commands, path), "{path}");
        }
    }

    /// A suite that changes a file the repository holds fails at that file. One that leaves each
    /// file as it found it passes, an edit from before the run included.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_suite_that_changes_a_file_the_repository_holds_fails() {
        let files = [("src/a.rs", "a"), ("src/b.rs", "b")];
        let root = crate::testdir::repo("suite-changes", &files);
        let edit = || std::fs::write(root.join("src/a.rs"), "edited before the run").unwrap();
        let ran = |argv: &[&str], vcs| {
            let baseline = crate::run::baseline::Baseline::empty("0.1.0");
            let mut ctx = Ctx::for_root(root.to_path_buf(), baseline);
            ctx.vcs = vcs;
            ctx.runner.argv = argv.iter().map(|arg| (*arg).to_string()).collect();
            let outcome = test(&ctx).unwrap();
            (outcome.passed, outcome.findings)
        };
        let git = Some(crate::project::vcs::Kind::Git);
        let restores = ["git", "checkout", "-q", "HEAD", "--", "src/a.rs"];
        edit();
        let changed = vec![Finding::at("src/a.rs", CHANGED)];
        assert_eq!(ran(&restores, git), (false, changed));
        edit();
        assert_eq!(ran(&["git", "status", "--short"], git), (true, Vec::new()));
        assert_eq!(
            ran(&restores, None),
            (true, Vec::new()),
            "no repository to ask"
        );
    }
}
