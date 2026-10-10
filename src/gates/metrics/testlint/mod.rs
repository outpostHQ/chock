//! Defects in the tests themselves, which the compiler cannot see: a test that asserts nothing, an
//! assertion that cannot fail, a skip with no reason, a name that states no claim.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::gates::cargo::modcheck;
use crate::gates::metrics::prodlines;
use crate::project;
use crate::run::report::{Finding, Place};
use crate::run::{Ctx, Gate, Group, Inspection, Kind};
use crate::tokens::{Lexed, Tok};

mod per_test;
mod scan;

use scan::{FileScan, Helper, TestItem};

pub const GATE: Gate = Gate {
    name: "testlint",
    about: "tests that assert nothing, cannot fail, are skipped with no reason or are named for no claim, per file and rule",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Debt {
        inspect,
        unit: "test defect(s)",
    },
};

const NO_ASSERTION: &str = "no-assertion";
const AMBIGUOUS: &str = "ambiguous-test-name";
const PIPEFAIL: &str = "harness-without-pipefail";

/// Most tests of the same name that one finding lists; its message counts them all.
const TWINS_SHOWN: usize = 3;

/// The root directories whose scripts run a build or a test suite.
const HARNESS_DIRS: [&str; 2] = ["bin", "scripts"];

fn inspect(ctx: &Ctx) -> Result<Inspection, String> {
    let targets = modcheck::reach(&ctx.root)?;
    let sources = prodlines::sources(ctx)?.into_iter();
    let shown = sources.map(|path| (project::relative(&ctx.root, &path), path));
    let mut scans = Vec::new();
    for (shown, path) in shown.filter(|(shown, _)| targets.compiles(shown)) {
        let raw = std::fs::read_to_string(&path).map_err(|e| format!("{shown}: {e}"))?;
        scans.push(scan::scan_source(&shown, &raw));
    }
    let mut found: Vec<Finding> = scans.iter().flat_map(per_test::in_file).collect();
    found.extend(assertionless(&scans));
    found.extend(ambiguous_names(&scans));
    found.extend(shell_harnesses(ctx)?);
    found.sort_by(|a, b| (&a.file, a.line, &a.item).cmp(&(&b.file, b.line, &b.item)));
    Ok(Inspection::debt(found))
}

fn line(at: usize) -> u32 {
    u32::try_from(at).unwrap_or(u32::MAX)
}

pub(crate) fn finding(file: &str, at: usize, rule: &str, detail: &str) -> Finding {
    Finding::at(file, detail).item(rule).line(line(at))
}

fn waived(t: &TestItem, rule: &str) -> bool {
    let reasoned = |w: &scan::Waiver| w.rule == rule && !w.reason.is_empty();
    t.waivers.iter().any(reasoned)
}

/// Each test that holds nothing that can fail it, in its own body or in a helper it calls.
fn assertionless(scans: &[FileScan]) -> Vec<Finding> {
    let helpers = asserting_helpers(scans);
    let mut found = Vec::new();
    for s in scans {
        let asserts = |t: &TestItem| has_an_assertion(&s.lexed, t.body.clone(), &helpers);
        let silent = |t: &&TestItem| !waived(t, NO_ASSERTION) && !asserts(t);
        found.extend(s.tests.iter().filter(silent).map(|t| {
            let detail = format!("`{}` asserts nothing", t.name);
            finding(&s.file, t.line, NO_ASSERTION, &detail)
        }));
    }
    found
}

/// Each helper of the tree with the file that holds it.
fn helpers(scans: &[FileScan]) -> impl Iterator<Item = (&FileScan, &Helper)> {
    scans
        .iter()
        .flat_map(|s| s.helpers.iter().map(move |h| (s, h)))
}

/// Whether the tokens hold what fails a helper: an assert or a panic macro, or the name of a
/// helper in `known`.
fn fails(lexed: &Lexed, body: Range<usize>, known: &BTreeSet<&str>) -> bool {
    fails_by_macro(lexed, body.clone()) || names_one(lexed, body, known)
}

/// The names of the helpers a test asserts through: a function of the test code that holds an
/// assert or a panic, or names a helper that does.
fn asserting_helpers(scans: &[FileScan]) -> BTreeSet<&str> {
    let (mut names, mut grew) = (BTreeSet::new(), true);
    while grew {
        let failing = helpers(scans).filter(|(s, h)| fails(&s.lexed, h.body.clone(), &names));
        let all: BTreeSet<&str> = failing.map(|(_, h)| h.name.as_str()).collect();
        grew = all.len() > names.len();
        names = all;
    }
    names
}

/// The macros that stop a test with no condition.
const PANICS: [&str; 4] = ["panic", "unreachable", "todo", "unimplemented"];

/// Whether the tokens hold a macro that fails a test: an assert of any family, or a panic.
fn fails_by_macro(lexed: &Lexed, body: Range<usize>) -> bool {
    body.into_iter().any(|i| {
        let name = lexed.ident(i);
        invoked(lexed, i) && (name.contains("assert") || PANICS.contains(&name))
    })
}

/// Whether the tokens name one of `helpers`: a call, or the name alone as one argument, which
/// hands the helper to a runner.
fn names_one(lexed: &Lexed, body: Range<usize>, helpers: &BTreeSet<&str>) -> bool {
    let comma = |i: usize| lexed.is_punct(i, ',');
    let alone = |i: usize| {
        let starts = lexed.opens(i - 1, '(') || comma(i - 1);
        let ends = lexed.toks.get(i + 1) == Some(&Tok::Close(')')) || comma(i + 1);
        starts && ends
    };
    let named = |i: usize| lexed.opens(i + 1, '(') || alone(i);
    body.into_iter()
        .any(|i| helpers.contains(lexed.ident(i)) && named(i))
}

/// Whether the identifier at `i` is a macro call: a `!` with nothing between.
fn invoked(lexed: &Lexed, i: usize) -> bool {
    lexed.is_punct(i + 1, '!') && lexed.at[i].end == lexed.at[i + 1].start
}

/// Whether the tokens hold a shape that can fail a test: an assert or panic macro, an `unwrap`,
/// a call to a function named for checking, or the name of a helper that asserts.
fn has_an_assertion(lexed: &Lexed, body: Range<usize>, helpers: &BTreeSet<&str>) -> bool {
    const CHECKS: [&str; 4] = ["require", "expect", "verify", "must_"];
    let named_for_checking = |i: usize| {
        let name = lexed.ident(i);
        let checks = name.contains("assert")
            || matches!(name, "unwrap" | "unwrap_err")
            || CHECKS.iter().any(|prefix| name.starts_with(prefix));
        checks && (lexed.opens(i + 1, '(') || invoked(lexed, i))
    };
    fails(lexed, body.clone(), helpers) || body.into_iter().any(named_for_checking)
}

/// Each test whose name another test has: a name filter runs both, and a failure line names
/// neither.
fn ambiguous_names(scans: &[FileScan]) -> Vec<Finding> {
    let mut by_name: BTreeMap<&str, Vec<(&str, &TestItem)>> = BTreeMap::new();
    for s in scans {
        for t in &s.tests {
            let named = by_name.entry(t.name.as_str()).or_default();
            named.push((s.file.as_str(), t));
        }
    }
    let mut found = Vec::new();
    for (name, tests) in &by_name {
        for (file, t) in tests.iter().filter(|(_, t)| !waived(t, AMBIGUOUS)) {
            let others = tests.iter().filter(|(_, other)| !std::ptr::eq(*other, *t));
            let place =
                |(at, other): &(&str, &TestItem)| Place::at("same name", at, line(other.line));
            let detail = format!("`{name}` is the name of {} tests", tests.len());
            let mut twin = finding(file, t.line, AMBIGUOUS, &detail);
            twin.places = others.take(TWINS_SHOWN).map(place).collect();
            found.extend((!twin.places.is_empty()).then_some(twin));
        }
    }
    found
}

/// Whether `shown` is a file directly in a harness directory at the root.
fn is_harness(shown: &str) -> bool {
    let direct = |(dir, file): (&str, &str)| HARNESS_DIRS.contains(&dir) && !file.contains('/');
    shown.split_once('/').is_some_and(direct)
}

/// A shell script that pipes and never sets `pipefail`: a failed build piped into `tail` reads as
/// a pass.
fn hides_a_failed_pipe(text: &str) -> bool {
    const SHELLS: [&str; 5] = ["sh", "bash", "dash", "ksh", "zsh"];
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    let shell =
        first.starts_with("#!") && first.split([' ', '/']).any(|word| SHELLS.contains(&word));
    let mut code = lines.filter(|line| !line.trim_start().starts_with('#'));
    let pipes = code.any(|line| line.contains(" | ") || line.trim_end().ends_with(" |"));
    shell && pipes && !text.contains("pipefail")
}

/// Each harness script whose pipe hides a failure. One that is not text is a compiled tool,
/// which holds no pipe to judge.
fn shell_harnesses(ctx: &Ctx) -> Result<Vec<Finding>, String> {
    let shown = |path: &Path| project::relative(&ctx.root, path);
    let enter = |dir: &str| HARNESS_DIRS.contains(&dir);
    let harness = |_: &str, path: &Path| is_harness(&shown(path));
    let files = project::walked(&ctx.listing, &ctx.root, &enter, &harness)?;
    let text = |path: &Path| std::fs::read_to_string(path).unwrap_or_default();
    let hiding = files.iter().filter(|path| hides_a_failed_pipe(&text(path)));
    let detail = "a pipe in this script hides a failed command: it does not `set -o pipefail`";
    let named = |path: &PathBuf| finding(&shown(path), 1, PIPEFAIL, detail);
    Ok(hiding.map(named).collect())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    /// What the tests of these files break, the rules that need every file included.
    fn found_in(files: &[(&str, &str)]) -> Vec<String> {
        let scan = |(file, src): &(&str, &str)| scan::scan_source(file, src);
        let scans: Vec<FileScan> = files.iter().map(scan).collect();
        let mut all = assertionless(&scans);
        all.extend(scans.iter().flat_map(per_test::in_file));
        Finding::rendered(&all)
    }

    fn found(src: &str) -> Vec<String> {
        found_in(&[("src/thing.rs", src)])
    }

    /// What a test named `reads_a_file` with this attribute block and body breaks.
    fn broken(attrs: &str, body: &str) -> Vec<String> {
        found(&format!("{attrs}\nfn reads_a_file() {{ {body} }}\n"))
    }

    #[test]
    fn a_test_that_only_checks_that_nothing_failed_asserts_nothing() {
        let src = "#[tokio::test]\nasync fn pushes_and_pulls() -> R { push().await?; Ok(()) }\n";
        let expected = "src/thing.rs:1: no-assertion: `pushes_and_pulls` asserts nothing";
        assert_eq!(found(src), [expected]);
    }

    #[test]
    fn every_shape_that_can_fail_a_test_is_an_assertion() {
        let bodies = "assert!(seen); assert_eq!(a, b); prop_assert!(seen); panic!(\"no\"); \
            unreachable!(); todo!(); unimplemented!(); verify!(seen); read().unwrap(); \
            read().unwrap_err(); read().expect(\"a file\"); command().debug_assert(); \
            started.assert(); require_some_interleaving(&S, \"x\", f); verify(seen); \
            must_fail(seen);";
        for body in bodies.split_inclusive(';') {
            assert_eq!(broken("#[test]", body), [""; 0], "{body}");
        }
    }

    #[test]
    fn a_test_asserts_through_a_helper_of_the_test_code() {
        let says = "fn says(out: &str) { assert!(out.contains(\"x\")); }\n";
        let test = "#[test]\nfn reads_a_file() { says(&read()); }\n";
        let gated = format!("#[cfg(test)]\nmod tests {{\n{says}{test}}}\n");
        assert_eq!(found(&gated), [""; 0]);
        assert_eq!(
            found_in(&[("tests/it.rs", &format!("{says}{test}"))]),
            [""; 0]
        );
        let apart = [("tests/it.rs", test), ("tests/common/mod.rs", says)];
        assert_eq!(found_in(&apart), [""; 0]);
    }

    #[test]
    fn a_test_asserts_through_a_helper_it_hands_to_a_runner() {
        let says = "fn says(out: &str) { assert!(out.contains(\"x\")); }\n";
        for body in ["run(says).await", "run(1, says, 2)", "run(says, 2)"] {
            let src = format!("{says}#[test]\nfn reads_a_file() {{ {body} }}\n");
            assert_eq!(found_in(&[("tests/it.rs", &src)]), [""; 0], "{body}");
        }
    }

    #[test]
    fn a_name_of_a_helper_inside_a_larger_argument_hands_nothing_on() {
        let says = "fn says(out: &str) { assert!(out.contains(\"x\")); }\n";
        for body in [
            "run(&says)",
            "run(says.len())",
            "run(1, says.len(), 2)",
            "let says = 1;",
        ] {
            let src = format!("{says}#[test]\nfn reads_a_file() {{ {body} }}\n");
            let expected = "tests/it.rs:2: no-assertion: `reads_a_file` asserts nothing";
            assert_eq!(found_in(&[("tests/it.rs", &src)]), [expected], "{body}");
        }
    }

    #[test]
    fn a_helper_that_calls_a_helper_that_asserts_is_followed_to_the_end() {
        let chain = "pub fn outer() { inner() }\npub fn inner() { middle() }\npub fn middle() { unreachable!(\"no\") }\n";
        let test = "#[test]\nfn reads_a_file() { outer(); }\n";
        let apart = [("tests/common/mod.rs", chain), ("tests/it.rs", test)];
        assert_eq!(found_in(&apart), [""; 0]);
        assert_eq!(found_in(&[apart[1], apart[0]]), [""; 0]);
    }

    #[test]
    fn a_function_that_is_no_asserting_helper_of_the_test_code_asserts_nothing() {
        let test = "#[test]\nfn reads_a_file() { says(&read()); }\n";
        for helper in [
            "fn says(out: &str) { out.parse::<u8>().unwrap(); }\n",
            "fn says(out: &str) { expect_text(out); }\n",
            "fn says(out: &str) { other(out) }\n",
            "fn says(out: &str) { let assert = out; drop(assert); }\n",
            "fn other(out: &str) { assert!(out.is_empty()); }\n",
            "fn other() { let says = 1; assert_eq!(says, 1); }\n",
        ] {
            let expected = "tests/it.rs:2: no-assertion: `reads_a_file` asserts nothing";
            let src = format!("{helper}{test}");
            assert_eq!(found_in(&[("tests/it.rs", &src)]), [expected], "{helper}");
        }
        let says = "fn says(out: &str) { assert!(out.contains(\"x\")); }\n";
        let production = format!("{says}{test}");
        let expected = "src/thing.rs:2: no-assertion: `reads_a_file` asserts nothing";
        assert_eq!(found(&production), [expected]);
    }

    #[test]
    fn a_word_that_only_looks_like_an_assertion_is_not_one() {
        let bodies = "let s = \"assert!(seen)\"; drop(s);|read()?;|read().unwrap_or(1);|\
            let unwrap = 1; let expect = unwrap;|panic(seen);|let assert = [1]; run(assert);|\
            let todo = 1; if todo != 2 { run(); }";
        for body in bodies.split('|') {
            let expected = "src/thing.rs:1: no-assertion: `reads_a_file` asserts nothing";
            assert_eq!(broken("#[test]", body), [expected], "{body}");
        }
    }

    #[test]
    fn an_assertion_that_cannot_fail_is_reported_with_its_text() {
        let shape = |body: &str| {
            let said = broken("#[test]", body).join("\n");
            let shape = said
                .strip_prefix("src/thing.rs:1: tautological-assertion: `reads_a_file` contains ");
            shape.map(str::to_string).unwrap_or(said)
        };
        assert_eq!(shape("assert!(true);"), "`assert!(true)`");
        assert_eq!(shape("assert!( !false );"), "`assert!(!false)`");
        assert_eq!(shape("assert_eq!(seen, seen);"), "`assert_eq!(seen, seen)`");
        assert_eq!(
            shape("assert_ne![a.b, a.b, \"why\"];"),
            "`assert_ne!(a.b, a.b, \"why\")`"
        );
        assert_eq!(
            shape("assert!(seen); assert_eq!(a, a);"),
            "`assert_eq!(a, a)`"
        );
    }

    #[test]
    fn an_assertion_the_code_under_test_can_change_is_not_a_tautology() {
        let bodies = "assert_ne!(default_seed(), default_seed());|assert_eq!(v![1], v![1]);|\
            assert_eq!(key(&m(\"a\")), key(&m(\"b\")));|assert_eq!(a, b);|assert_eq!(a, );|\
            assert!(truth);|assert_eq!(true);|assert!(a, a);|check!(true); assert!(seen);|\
            let assert_eq = !(a, a); assert!(seen);|debug_assert_eq!(a, a);";
        for body in bodies.split('|') {
            assert_eq!(broken("#[test]", body), [""; 0], "{body}");
        }
    }

    #[test]
    fn a_skip_must_say_why() {
        let skip = |attr: &str| broken(&format!("#[test]\n{attr}"), "assert!(seen);").join("\n");
        let no =
            "src/thing.rs:1: ignore-without-reason: `reads_a_file` is #[ignore] with no reason";
        let empty = "src/thing.rs:1: ignore-without-reason: `reads_a_file` is #[ignore] with an empty reason";
        assert_eq!(skip("#[ignore]"), no);
        assert_eq!(skip("#[ignore = \"\"]"), empty);
        assert_eq!(skip("#[ignore = \"  \"]"), empty);
        assert_eq!(skip("#[ignore = \"needs a bucket\"]\n#[ignore]"), no);
        assert_eq!(skip("#[ignore = \"needs a bucket\"]"), "");
        assert_eq!(skip("#[ignored]"), "");
        assert_eq!(skip("/// #[ignore]"), "");
    }

    #[test]
    fn a_should_panic_must_name_the_panic_it_expects() {
        let panics = |attr: &str| broken(&format!("{attr}\n#[test]"), "run().unwrap();").join("\n");
        let any = "src/thing.rs:1: should-panic-without-expected: `reads_a_file` accepts any panic, including its own setup's";
        assert_eq!(panics("#[should_panic]"), any);
        assert_eq!(panics("#[should_panic(expected = \"\")]"), any);
        assert_eq!(panics("#[should_panic(expected = \"out of range\")]"), "");
        assert_eq!(panics("#[should_panic = \"out of range\"]"), "");
    }

    #[test]
    fn a_name_that_states_no_claim_is_a_placeholder_or_one_word() {
        let named = |name: &str| {
            let said = found(&format!("#[test]\nfn {name}() {{ assert!(seen); }}\n")).join("\n");
            said.replace("src/thing.rs:1: ", "")
        };
        assert_eq!(
            named("it_works"),
            "placeholder-test-name: `it_works` names no behaviour"
        );
        assert_eq!(
            named("test_1"),
            "placeholder-test-name: `test_1` names no behaviour"
        );
        assert_eq!(
            named("test_basic"),
            "placeholder-test-name: `test_basic` names no behaviour"
        );
        assert_eq!(
            named("smoke_test"),
            "placeholder-test-name: `smoke_test` names no behaviour"
        );
        let one = "one-word-test-name: `test_read` is one word — state what it proves";
        assert_eq!(named("test_read"), one);
        let bare = "one-word-test-name: `__roundtrip_` is one word — state what it proves";
        assert_eq!(named("__roundtrip_"), bare);
        assert_eq!(named("reads_file"), "");
        assert_eq!(named("test_reads_a_file_test"), "");
    }

    #[test]
    fn a_call_that_leaves_the_run_directory_is_named() {
        const SAID: &str = "src/thing.rs:1: escapes-the-run-dir: `reads_a_file` calls ";
        let said = |body: &str| broken("#[test]", &format!("{body} assert!(seen);")).join("\n");
        let calls = |body: &str| said(body).replace(SAID, "");
        assert_eq!(calls("let d = std::env::temp_dir();"), "`env::temp_dir()`");
        let moved = "std::env::set_current_dir(&d).unwrap();";
        assert_eq!(calls(moved), "`set_current_dir()`");
        let first = "dirs::home_dir(); env::temp_dir();";
        assert_eq!(calls(first), "`env::temp_dir()`");
        assert_eq!(calls("dirs::data_dir();"), "`dirs::data_dir()`");
        assert_eq!(calls("dirs::config_dir();"), "`dirs::config_dir()`");
        let owned = "let d = scratch.temp_dir();|let d = mine::temp_dir();|\
            let d = env::temp_dir;|let home_dir = dirs::home_dir;|let s = \"env::temp_dir()\";";
        for body in owned.split('|') {
            assert_eq!(calls(body), "", "{body}");
        }
    }

    #[test]
    fn a_waiver_with_a_reason_silences_its_rule_and_no_other() {
        let waived = "#[test]\n// test-lint: allow(no-assertion) — compiling is the claim\nfn it_works() { run(); }\n";
        let expected = "src/thing.rs:1: placeholder-test-name: `it_works` names no behaviour";
        assert_eq!(found(waived), [expected]);
    }

    #[test]
    fn a_waiver_with_no_reason_silences_nothing_and_is_a_finding() {
        let src = "#[test]\nfn reads_a_file() {\n    run(); // test-lint: allow(no-assertion)\n}\n";
        let expected = [
            "src/thing.rs:1: no-assertion: `reads_a_file` asserts nothing",
            "src/thing.rs:3: waiver-without-reason: `test-lint: allow(no-assertion)` states no reason",
        ];
        assert_eq!(found(src), expected);
    }

    #[test]
    fn two_tests_with_one_name_each_name_the_other() {
        let scan = |file: &str, src: &str| scan::scan_source(file, src);
        let scans = [
            scan(
                "tests/a.rs",
                "#[test]\nfn round_trips() {}\n#[test]\nfn alone() {}\n",
            ),
            scan(
                "tests/b.rs",
                "\n#[test]\nfn round_trips() {}\nmod m {\n#[test]\nfn round_trips() {}\n}\n",
            ),
            scan(
                "tests/c.rs",
                "#[test]\n// test-lint: allow(ambiguous-test-name) — one per backend\nfn round_trips() {}\n",
            ),
            scan("tests/d.rs", "#[test]\nfn round_trips() {}\n"),
        ];
        let twins = ambiguous_names(&scans);
        let lines: Vec<String> = twins.iter().flat_map(Finding::lines).collect();
        let message = "ambiguous-test-name: `round_trips` is the name of 5 tests";
        let expected = [
            format!("tests/a.rs:1: {message}"),
            "  same name: tests/b.rs:2".to_string(),
            "  same name: tests/b.rs:5".to_string(),
            "  same name: tests/c.rs:1".to_string(),
            format!("tests/b.rs:2: {message}"),
            "  same name: tests/a.rs:1".to_string(),
            "  same name: tests/b.rs:5".to_string(),
            "  same name: tests/c.rs:1".to_string(),
            format!("tests/b.rs:5: {message}"),
            "  same name: tests/a.rs:1".to_string(),
            "  same name: tests/b.rs:2".to_string(),
            "  same name: tests/c.rs:1".to_string(),
            format!("tests/d.rs:1: {message}"),
            "  same name: tests/a.rs:1".to_string(),
            "  same name: tests/b.rs:2".to_string(),
            "  same name: tests/b.rs:5".to_string(),
        ];
        assert_eq!(lines, expected);
    }

    #[test]
    fn a_harness_is_a_file_directly_in_a_script_directory_at_the_root() {
        for (shown, harness) in [
            ("bin/test", true),
            ("scripts/ci.sh", true),
            ("bin/lib/common.sh", false),
            ("src/bin/tool.rs", false),
            ("check.sh", false),
            ("tools/bin", false),
        ] {
            assert_eq!(is_harness(shown), harness, "{shown}");
        }
    }

    #[test]
    fn a_shell_script_that_pipes_without_pipefail_hides_a_failure() {
        let hides = hides_a_failed_pipe;
        assert!(hides("#!/usr/bin/env bash\ncargo test | tail -5\n"));
        assert!(hides("#!/bin/sh\ncargo build 2>&1 |\n  tail -5\n"));
        assert!(!hides("#!/bin/sh\nset -o pipefail\ncargo test | tail\n"));
        assert!(!hides("#!/bin/bash\ncargo test\n"));
        assert!(!hides("#!/bin/sh\n # a | b\ncargo test || exit 1\n"));
        assert!(!hides("#!/usr/bin/env python3\nprint(a | b)\n"));
        assert!(!hides("cargo test | tail -5\n"));
        assert!(!hides("bash\ncargo test | tail -5\n"));
        assert!(!hides(""));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_is_judged_file_by_file_then_across_files_then_its_harnesses() {
        let piped = "#!/bin/bash\ncargo test | tail -5\n";
        let ctx = crate::testdir::Held::tree(
            "testlint-inspect",
            &[
                ("Cargo.toml", ""),
                ("src/lib.rs", "\n#[test]\nfn reads_a_file() { run(); }\n"),
                (
                    "tests/it.rs",
                    "#[test]\nfn reads_a_file() { assert!(seen); }\n#[test]\nfn it_works() { assert!(true); }\n",
                ),
                ("src/notes.rs", "fn broken( {\n"),
                ("tests/ui/pass.rs", "#[test]\nfn test() {}\n"),
                ("scripts/ci", piped),
                ("bin/check", piped),
                (
                    "bin/safe",
                    "#!/bin/bash\nset -o pipefail\ncargo test | tail -5\n",
                ),
                ("bin/deep/check", piped),
                ("check", piped),
            ],
        );
        let inspection = inspect(&ctx).unwrap();
        assert_eq!(Finding::rendered(&inspection.blockers), [""; 0]);
        let pipe = "harness-without-pipefail: a pipe in this script hides a failed command: it does not `set -o pipefail`";
        let twin = "ambiguous-test-name: `reads_a_file` is the name of 2 tests";
        let expected = [
            format!("bin/check:1: {pipe}"),
            format!("scripts/ci:1: {pipe}"),
            format!("src/lib.rs:2: {twin}"),
            "src/lib.rs:2: no-assertion: `reads_a_file` asserts nothing".to_string(),
            format!("tests/it.rs:1: {twin}"),
            "tests/it.rs:3: placeholder-test-name: `it_works` names no behaviour".to_string(),
            "tests/it.rs:3: tautological-assertion: `it_works` contains `assert!(true)`"
                .to_string(),
        ];
        assert_eq!(Finding::rendered(&inspection.debt), expected);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_source_that_cannot_be_read_stops_the_gate_and_is_named() {
        let files = [("Cargo.toml", ""), ("src/lib.rs", "")];
        let ctx = crate::testdir::Held::tree("testlint-unread", &files);
        std::fs::write(ctx.root.join("src/lib.rs"), [0xff, 0xfe]).unwrap();
        let err = inspect(&ctx).map(|found| found.debt).unwrap_err();
        assert!(err.starts_with("src/lib.rs: "), "{err}");
    }
}
