//! The suite in groups of tests, one interpreter to a group: each interpreter compiles the crate
//! again before its first test, so one to a test spent most of the run compiling.

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

/// Each binary's tests that the filter and partition keep and that are not ignored, `PER_GROUP`
/// to a group; `None` for a list chock cannot read or that holds no test: one nextest run then.
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
        let target = picked(suite)?;
        groups.extend(tests.chunks(PER_GROUP).map(|tests| Group {
            binary: binary.clone(),
            target: target.clone(),
            tests: tests.to_vec(),
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

/// One group under libtest's own harness: each name matched whole, one test at a time.
pub(super) fn command(group: &Group, asked: &[String]) -> Vec<String> {
    let mut args = ["+nightly", "miri", "test"].map(String::from).to_vec();
    args.extend(group.target.iter().cloned());
    args.extend_from_slice(asked);
    args.extend(["--", "--exact", "--test-threads=1"].map(String::from));
    args.extend(group.tests.iter().cloned());
    args
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
    fn a_binary_with_more_tests_than_a_group_holds_is_split_in_order() {
        let names: Vec<String> = (0..=PER_GROUP).map(|at| format!("t{at:02}")).collect();
        let cases: Vec<String> = names.iter().map(|name| case(name)).collect();
        let json = format!(
            r#"{{"rust-suites": {{"b": {}}}}}"#,
            suite("lib", &cases.join(","))
        );
        let (first, rest) = names.split_at(PER_GROUP);
        let split = [group("b", &first.join(" ")), group("b", &rest.join(" "))];
        assert_eq!(grouped(&json).unwrap(), split);
    }

    #[test]
    fn a_group_runs_its_tests_by_exact_name_one_at_a_time_with_the_projects_flags() {
        let run = command(&group("chock", "a::b c"), &words("--features x"));
        let lib = "-p chock --lib --features x";
        assert_eq!(
            run,
            words(&format!(
                "+nightly miri test {lib} -- --exact --test-threads=1 a::b c"
            ))
        );
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
