//! The suite in groups of tests, one interpreter to a group: each interpreter compiles the crate
//! again before its first test, so one to a test spent most of the run compiling.

use std::collections::BTreeMap;

use crate::exec;

/// Tests one interpreter runs: enough that its compile is a small share, few enough to spread.
const PER_GROUP: usize = 32;

/// The longest filter a rerun passes; past it, the whole part runs again, as Windows caps a command.
const LONGEST: usize = 16 * 1024;

/// Tests of one binary that one interpreter runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Group {
    /// nextest's name for the binary, which a rerun's filter names.
    binary: String,
    /// The cargo arguments that pick the binary.
    target: Vec<String>,
    tests: Vec<String>,
}

/// nextest's list in JSON from the run's own arguments, so the list holds the tests the run would.
pub(super) fn listing(args: &[String]) -> Vec<String> {
    let mut list: Vec<String> = args
        .iter()
        .filter(|arg| !["--no-tests=fail", "--no-fail-fast"].contains(&arg.as_str()))
        .cloned()
        .collect();
    if let Some(verb) = list.iter_mut().find(|arg| *arg == "run") {
        *verb = "list".to_string();
    }
    list.push("--message-format=json".to_string());
    list
}

/// Each binary's kept tests, at most `PER_GROUP` to a group, each group every n-th test so slow
/// neighbours part; `None` for an unreadable or empty list, which nextest then runs whole.
pub(super) fn grouped(json: &str) -> Option<Vec<Group>> {
    let list: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut groups = Vec::new();
    for (binary, suite) in list.get("rust-suites")?.as_object()? {
        let cases = suite
            .get("testcases")
            .and_then(serde_json::Value::as_object);
        let tests: Vec<String> = cases
            .into_iter()
            .flatten()
            .filter(|(_, case)| {
                case["ignored"] == false && case["filter-match"]["status"] == "matches"
            })
            .map(|(name, _)| name.clone())
            .collect();
        if tests.is_empty() {
            continue;
        }
        let (target, count) = (picked(suite)?, tests.len().div_ceil(PER_GROUP));
        groups.extend((0..count).map(|first| Group {
            binary: binary.clone(),
            target: target.clone(),
            tests: tests.iter().skip(first).step_by(count).cloned().collect(),
        }));
    }
    (!groups.is_empty()).then_some(groups)
}

/// The cargo arguments that pick a binary; `None` for a kind cargo has no flag for.
fn picked(suite: &serde_json::Value) -> Option<Vec<String>> {
    let package = suite.get("package-name")?.as_str()?;
    let name = suite.get("binary-name")?.as_str()?;
    let mut target = vec!["-p".to_string(), package.to_string()];
    match suite.get("kind")?.as_str()? {
        "lib" | "proc-macro" => target.push("--lib".to_string()),
        kind @ ("bin" | "test" | "bench" | "example") => {
            target.extend([format!("--{kind}"), name.to_string()]);
        }
        _ => return None,
    }
    Some(target)
}

/// What libtest takes for a group: each name matched whole, one test at a time, each test's time.
/// A harness that refuses one fails the group, which then runs under nextest.
const LIBTEST: [&str; 5] = [
    "--",
    "--exact",
    "--test-threads=1",
    "-Zunstable-options",
    "--report-time",
];

/// One group under libtest's own harness.
pub(super) fn command(group: &Group, asked: &[String]) -> Vec<String> {
    let mut args = ["+nightly", "miri", "test"].map(String::from).to_vec();
    args.extend(group.target.iter().cloned());
    args.extend_from_slice(asked);
    args.extend(LIBTEST.map(String::from));
    args.extend(group.tests.iter().cloned());
    args
}

/// Each test's time in ms, out of libtest's lines such as `test a::b ... ok <1.250s>`.
pub(super) fn times(group: &Group, stdout: &str) -> Vec<(String, u64)> {
    let each = stdout.lines().filter_map(|line| {
        let (test, said) = line.strip_prefix("test ")?.split_once(" ... ")?;
        let secs = said.split_once(" <")?.1.strip_suffix("s>")?.parse().ok()?;
        let ms = std::time::Duration::try_from_secs_f64(secs)
            .ok()?
            .as_millis();
        Some((key(&group.binary, test), u64::try_from(ms).ok()?))
    });
    each.collect()
}

/// The name a test's time is kept under: its binary's, then its own.
fn key(binary: &str, test: &str) -> String {
    format!("{binary} {test}")
}

/// Why a group did not pass: miri cannot run the suite, chock stopped it in one test, or the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Fault {
    Refused(String),
    Stopped(String),
    Failed,
}

/// Each test's time in ms, or why the group did not pass.
pub(super) type Ended = Result<Vec<(String, u64)>, Fault>;

/// How a group's run ended, from what it printed or from why chock stopped it.
pub(super) fn ended(group: &Group, ran: &Result<exec::Output, exec::ExecError>) -> Ended {
    match ran {
        Ok(out) if out.success() => Ok(times(group, &out.stdout)),
        Ok(out) => Err(super::never_ran(&out.stderr).map_or(Fault::Failed, Fault::Refused)),
        Err(stopped) => Err(stopped_on(group, stopped).map_or(Fault::Failed, Fault::Stopped)),
    }
}

/// The test a group was in when chock stopped it. libtest names a test before it runs, so the
/// last test line then has no verdict.
fn stopped_on(group: &Group, stopped: &exec::ExecError) -> Option<String> {
    let lines = stopped.reason.lines();
    let (named, verdict) = lines
        .filter_map(|line| line.strip_prefix("test ")?.split_once(" ... "))
        .next_back()?;
    let test = named.split_once(" - ").map_or(named, |(test, _)| test);
    let running = stopped.hung() && verdict.is_empty() && group.tests.iter().any(|own| own == test);
    running.then(|| test.to_string())
}

/// What nextest runs again: each group that did not pass, less the test chock stopped it in. With
/// them, each such test under its binary's name. A group miri refused ends the suite instead.
pub(super) fn left(grouped: &[Group], ended: &[Option<Ended>]) -> Result<Left, String> {
    let mut left = (Vec::new(), Vec::new());
    for (group, ended) in grouped.iter().zip(ended) {
        let mut group = group.clone();
        match ended {
            Some(Ok(_)) => continue,
            Some(Err(Fault::Refused(why))) => return Err(why.clone()),
            Some(Err(Fault::Stopped(test))) => {
                left.1.push(key(&group.binary, test));
                group.tests.retain(|own| own != test);
            }
            _ => {}
        }
        if !group.tests.is_empty() {
            left.0.push(group);
        }
    }
    Ok(left)
}

/// The groups nextest runs again, and the tests chock stopped, which no run names again.
pub(super) type Left = (Vec<Group>, Vec<String>);

/// Each group a lane of its own, the longest on record first, so the last to start ends soonest.
/// A group with a test on no record starts first, as it may be the longest.
pub(super) fn slowest_first(groups: &[Group], recorded: &BTreeMap<String, u64>) -> Vec<Vec<usize>> {
    let took = |group: &Group| -> Option<u64> {
        let each = group.tests.iter();
        each.map(|test| recorded.get(&key(&group.binary, test)).copied())
            .sum()
    };
    let mut order: Vec<(usize, Option<u64>)> = groups.iter().map(took).enumerate().collect();
    order.sort_by_key(|(_, took)| std::cmp::Reverse(took.unwrap_or(u64::MAX)));
    order.into_iter().map(|(at, _)| vec![at]).collect()
}

/// A nextest filter that picks the tests of `groups`, each in its own binary.
fn filter(groups: &[&Group]) -> String {
    let each = groups.iter().map(|group| {
        let tests: Vec<String> = group
            .tests
            .iter()
            .map(|test| format!("test(={test})"))
            .collect();
        format!("(binary_id(={}) & ({}))", group.binary, tests.join(" | "))
    });
    each.collect::<Vec<_>>().join(" | ")
}

/// The nextest run of the failed groups, one test to a process, which names each test that fails.
/// The filter holds only this part's tests, so the partition goes.
pub(super) fn rerun(args: &[String], failed: &[&Group]) -> Vec<String> {
    let filter = filter(failed);
    if filter.len() > LONGEST {
        return args.to_vec();
    }
    let mut kept: Vec<String> = args
        .iter()
        .filter(|arg| !arg.starts_with("--partition"))
        .cloned()
        .collect();
    kept.extend(["-E".to_string(), filter]);
    kept
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    /// Library tests of chock, named in `tests` with a space between them.
    fn group(binary: &str, tests: &str) -> Group {
        Group {
            binary: binary.to_string(),
            target: words("-p chock --lib"),
            tests: words(tests),
        }
    }

    #[test]
    fn the_list_is_asked_with_the_runs_own_selection_and_a_feature_named_run_is_kept() {
        let partition = "--features run --partition=count:2/6";
        let run = words(&format!(
            "+nightly miri nextest run --no-tests=fail --no-fail-fast {partition}"
        ));
        let list = words(&format!(
            "+nightly miri nextest list {partition} --message-format=json"
        ));
        assert_eq!(listing(&run), list);
    }

    fn suite(kind: &str, cases: &str) -> String {
        format!(
            r#"{{"package-name": "chock", "binary-name": "cli", "kind": "{kind}",
                 "testcases": {{{cases}}}}}"#
        )
    }

    fn case(name: &str) -> String {
        format!(r#""{name}": {{"ignored": false, "filter-match": {{"status": "matches"}}}}"#)
    }

    #[test]
    fn only_tests_that_match_and_are_not_ignored_are_grouped() {
        let left_out = r#""ignored": {"ignored": true, "filter-match": {"status": "matches"}},
            "other_part": {"ignored": false, "filter-match": {"status": "mismatch"}}"#;
        let cases = [case("a"), left_out.to_string()].join(",");
        let json = format!(
            r#"{{"rust-suites": {{"chock::cli": {}}}}}"#,
            suite("test", &cases)
        );
        let mut cli = group("chock::cli", "a");
        cli.target = words("-p chock --test cli");
        assert_eq!(grouped(&json).unwrap(), [cli]);
    }

    #[test]
    fn each_kind_of_binary_is_picked_by_its_own_cargo_flag() {
        for (kind, flags) in [
            ("lib", "--lib"),
            ("proc-macro", "--lib"),
            ("bin", "--bin cli"),
            ("test", "--test cli"),
            ("bench", "--bench cli"),
            ("example", "--example cli"),
        ] {
            let json = format!(r#"{{"rust-suites": {{"b": {}}}}}"#, suite(kind, &case("a")));
            let target = &grouped(&json).unwrap()[0].target;
            assert_eq!(*target, words(&format!("-p chock {flags}")), "{kind}");
        }
        let odd = format!(
            r#"{{"rust-suites": {{"b": {}}}}}"#,
            suite("cdylib", &case("a"))
        );
        assert_eq!(
            grouped(&odd),
            None,
            "a kind cargo cannot pick runs under nextest"
        );
        let idle = format!(
            r#"{{"rust-suites": {{"a": {}, "b": {}}}}}"#,
            suite("cdylib", ""),
            suite("lib", &case("a"))
        );
        let lib = [group("b", "a")];
        assert_eq!(
            grouped(&idle).unwrap(),
            lib,
            "a binary with no test is passed over"
        );
    }

    #[test]
    fn a_list_with_no_test_or_none_chock_can_read_runs_under_nextest() {
        let none = format!(r#"{{"rust-suites": {{"b": {}}}}}"#, suite("lib", ""));
        assert_eq!(grouped(&none), None, "so `--no-tests=fail` still decides");
        assert_eq!(grouped("not json"), None);
        assert_eq!(grouped(r#"{"rust-suites": []}"#), None);
        let bare = r#"{"rust-suites": {"b": {"package-name": "chock", "binary-name": "b", "kind": "lib"}}}"#;
        assert_eq!(
            grouped(bare),
            None,
            "a binary that lists no tests holds none"
        );
    }

    #[test]
    fn a_binary_with_more_tests_than_a_group_holds_is_dealt_out_so_neighbours_part() {
        let names: Vec<String> = (0..=PER_GROUP).map(|at| format!("t{at:02}")).collect();
        let cases: Vec<String> = names.iter().map(|name| case(name)).collect();
        let json = format!(
            r#"{{"rust-suites": {{"b": {}}}}}"#,
            suite("lib", &cases.join(","))
        );
        let dealt = |first: usize| {
            names
                .iter()
                .skip(first)
                .step_by(2)
                .cloned()
                .collect::<Vec<_>>()
        };
        let split = [
            group("b", &dealt(0).join(" ")),
            group("b", &dealt(1).join(" ")),
        ];
        assert_eq!(grouped(&json).unwrap(), split);
        assert!(split.iter().all(|group| group.tests.len() <= PER_GROUP));
    }

    #[test]
    fn a_group_runs_its_tests_by_exact_name_one_at_a_time_with_the_projects_flags() {
        let run = command(&group("chock", "a::b c"), &words("--features x"));
        let lib = "-p chock --lib --features x";
        assert_eq!(
            run,
            words(&format!(
                "+nightly miri test {lib} -- --exact --test-threads=1 -Zunstable-options \
                 --report-time a::b c"
            ))
        );
    }

    #[test]
    fn each_time_libtest_printed_is_kept_in_ms_under_the_tests_binary_and_name() {
        let out = "running 2 tests\ntest a::b ... ok <1.250s>\ntest c ... ok <0.000s>\n\
                   test d ... ok\ntest e ... ok <soon>\ntest f ... ok <-1.000s>\n\
                   test g ... ok <100000000000000000s>\ntest result: ok. 2 passed";
        let timed = [("chock a::b".to_string(), 1250), ("chock c".to_string(), 0)];
        assert_eq!(times(&group("chock", "a::b c"), out), timed);
    }

    /// cargo as chock failed to run it at `stage`, with what the failure quotes.
    fn stopped(stage: exec::Stage, reason: &str) -> exec::ExecError {
        exec::ExecError {
            program: "cargo".to_string(),
            stage,
            reason: reason.to_string(),
        }
    }

    /// What chock quotes of a group it stopped in test `b`: the end of each stream.
    const IN_B: &str = "no progress for 60s; cleanup requested\nstdout (partial capture) (bounded \
                        output):\ntest a ... ok <0.100s>\ntest b ... \nstderr (partial capture) \
                        (bounded output):\n     Running unittests src/lib.rs";

    #[test]
    fn the_test_a_stopped_group_was_in_is_the_last_one_libtest_named_with_no_verdict() {
        let both = group("chock", "a b");
        let named = |reason: &str| stopped_on(&both, &stopped(exec::Stage::Hung, reason));
        assert_eq!(named(IN_B), Some("b".to_string()));
        let panics = "test a ... ok\ntest b - should panic ... ";
        assert_eq!(named(panics), Some("b".to_string()), "the mode is no name");
        assert_eq!(
            named("test a ... \ntest b ... FAILED"),
            None,
            "the last ended"
        );
        assert_eq!(named("test a ... ok\ntest b ... ok <2.000s>\n"), None);
        assert_eq!(named("test c ... "), None, "not a test of this group");
        assert_eq!(
            named("   Compiling chock v0.4.0"),
            None,
            "the build stalled"
        );
        let lost = stopped(exec::Stage::Wait, IN_B);
        assert_eq!(
            stopped_on(&both, &lost),
            None,
            "chock did not stop this one"
        );
    }

    #[test]
    fn a_group_ends_with_its_times_or_with_why_it_did_not_pass() {
        let both = group("chock", "a b");
        let said = |code, stdout, stderr| Ok(exec::Output::of(Some(code), stdout, stderr));
        let timed = vec![("chock a".to_string(), 1250)];
        assert_eq!(
            ended(&both, &said(0, "test a ... ok <1.250s>\n", "")),
            Ok(timed)
        );
        assert_eq!(
            ended(&both, &said(101, "test a ... FAILED\n", "")),
            Err(Fault::Failed)
        );
        let refused = "error: unsupported operation: can't call foreign function `posix_spawn`\n";
        let why = ended(&both, &said(1, "", refused)).unwrap_err();
        assert!(
            matches!(&why, Fault::Refused(why) if why.contains("`posix_spawn`")),
            "{why:?}"
        );
        let hung = |reason| Err(stopped(exec::Stage::Hung, reason));
        assert_eq!(
            ended(&both, &hung(IN_B)),
            Err(Fault::Stopped("b".to_string()))
        );
        assert_eq!(
            ended(&both, &hung("no progress for 60s")),
            Err(Fault::Failed)
        );
    }

    #[test]
    fn nextest_runs_each_failed_group_again_less_the_test_chock_stopped_it_in() {
        let grouped = [
            group("lib", "a b"),
            group("cli", "c"),
            group("bin", "d e"),
            group("doc", "f"),
            group("xtask", "g"),
        ];
        let stopped = |test: &str| Some(Err(Fault::Stopped(test.to_string())));
        let mut ended = vec![
            Some(Ok(Vec::new())),
            stopped("c"),
            stopped("d"),
            Some(Err(Fault::Failed)),
            None,
        ];
        let again = vec![group("bin", "e"), group("doc", "f"), group("xtask", "g")];
        let named = vec!["cli c".to_string(), "bin d".to_string()];
        assert_eq!(left(&grouped, &ended), Ok((again, named)));
        ended[4] = Some(Err(Fault::Refused("miri cannot emulate it".to_string())));
        let why = left(&grouped, &ended).unwrap_err();
        assert_eq!(why, "miri cannot emulate it");
    }

    #[test]
    fn the_longest_group_on_record_starts_first_and_one_with_no_record_before_it() {
        let groups = [
            group("chock", "a"),
            group("chock", "b c"),
            group("chock", "d"),
            group("chock::cli", "a"),
        ];
        let recorded = [
            ("chock a", 5),
            ("chock b", 2),
            ("chock c", 4),
            ("chock::cli a", 1),
        ];
        let recorded = recorded.map(|(test, ms)| (test.to_string(), ms)).into();
        assert_eq!(slowest_first(&groups, &recorded), [[2], [1], [0], [3]]);
    }

    #[test]
    fn a_rerun_names_each_failed_test_in_its_binary_and_drops_the_partition() {
        let args = words("nextest run --partition=count:2/6 --features x");
        let (lib, cli) = (group("chock", "a b"), group("chock::cli", "c"));
        let picked =
            "(binary_id(=chock) & (test(=a) | test(=b))) | (binary_id(=chock::cli) & (test(=c)))";
        let mut kept = words("nextest run --features x -E");
        kept.push(picked.to_string());
        assert_eq!(rerun(&args, &[&lib, &cli]), kept);
    }

    #[test]
    fn a_filter_longer_than_a_command_holds_runs_the_whole_part_again() {
        let args = words("run --partition=count:2/6");
        let named = |long: usize| Group {
            tests: vec!["x".repeat(long)],
            ..group("b", "")
        };
        let spare = LONGEST - filter(&[&named(0)]).len();
        let fits = named(spare);
        let mut kept = words("run -E");
        kept.push(filter(&[&fits]));
        assert_eq!(
            rerun(&args, &[&fits]),
            kept,
            "a filter of `LONGEST` is used"
        );
        assert_eq!(
            rerun(&args, &[&named(spare + 1)]),
            args,
            "past it the part runs whole"
        );
    }
}
