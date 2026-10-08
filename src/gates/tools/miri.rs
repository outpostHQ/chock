//! The test suite under miri, an interpreter that detects undefined behaviour. Most of this file
//! separates a setup failure from a finding and names the setting to change.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::exec;
use crate::run::report::Finding;
use crate::run::verdicts::Reads;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

mod groups;

pub const GATE: Gate = Gate {
    name: "miri",
    about: "the suite runs clean under an interpreter that detects undefined behaviour",
    group: Group::OptIn,
    builds: true,
    // Asked through the nightly that holds it, so a new nightly is a new key.
    reads: Some(Reads::tree_and(&["cargo +nightly miri", "cargo-nextest"])),
    kind: Kind::Binary(checked),
};

/// One part of the suite, from `--miri-partition=K/N`, so CI can run the parts at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    pub index: u32,
    pub of: u32,
}

impl Part {
    /// `K/N` with K from 1 to N, the form of nextest's `count:` partition.
    pub fn parse(text: &str) -> Result<Self, String> {
        let wrong = || format!("`--miri-partition` takes K/N with K from 1 to N, not `{text}`");
        let (index, of) = text.split_once('/').ok_or_else(wrong)?;
        let index: u32 = index.parse().map_err(|_| wrong())?;
        let of: u32 = of.parse().map_err(|_| wrong())?;
        if index == 0 || index > of {
            return Err(wrong());
        }
        Ok(Self { index, of })
    }
}

fn checked(ctx: &Ctx) -> Result<Outcome, String> {
    let prepared = asked(ctx).and_then(|asked| limited(ctx, &asked).map(|made| (asked, made)));
    prepared
        .and_then(|(asked, (_limit, args))| suite(ctx, &asked, &args, &|argv| interpret(ctx, argv)))
}

/// One cargo command under miri: its output, or why it stopped.
type Interpret<'a> = dyn Fn(&[String]) -> Result<exec::Output, String> + Sync + 'a;

/// Runs one command under miri; opt-in, since interpreting costs tens of times a normal run.
fn interpret(ctx: &Ctx, args: &[String]) -> Result<exec::Output, String> {
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let flags = miriflags(&ctx.miri);
    let env = [("MIRIFLAGS", flags.as_str())];
    exec::run_paced("cargo", &argv, &ctx.root, &env, moving)
        .map_err(|failed| advice(failed.hung(), &failed.to_string()))
}

/// The suite in groups, one interpreter each, as many at once as the lane's cores; a group that
/// fails runs again under nextest, which names each test. With no list to group, one nextest run.
fn suite(ctx: &Ctx, asked: &[String], args: &[String], run: &Interpret) -> Result<Outcome, String> {
    let listed = run(&groups::listing(args))
        .ok()
        .filter(exec::Output::success);
    let Some((listed, grouped)) =
        listed.and_then(|out| groups::grouped(&out.stdout).map(|grouped| (out, grouped)))
    else {
        return judge(&ctx.root, &run(args)?);
    };
    let recorded = crate::run::recorded_tests(&ctx.root, GATE.name);
    let lanes = groups::slowest_first(&grouped, &recorded);
    let timed = crate::run::workers::on_workers(&grouped, &lanes, exec::budget::cap(), &|group| {
        let out = run(&groups::command(group, asked))
            .ok()
            .filter(exec::Output::success)?;
        Some(groups::times(group, &out.stdout))
    });
    let failed: Vec<&groups::Group> = grouped
        .iter()
        .zip(&timed)
        .filter(|(_, timed)| !matches!(timed, Some(Some(_))))
        .map(|(group, _)| group)
        .collect();
    let tests_ms = timed.into_iter().flatten().flatten().flatten().collect();
    let outcome = match failed.is_empty() {
        true => super::verdict(&listed, &ctx.root),
        false => judge(&ctx.root, &run(&groups::rerun(args, &failed))?)?,
    };
    Ok(Outcome {
        tests_ms,
        ..outcome
    })
}

/// nextest's verdict, or why miri never tested the code.
fn judge(root: &Path, out: &exec::Output) -> Result<Outcome, String> {
    if let Some(why) = never_ran(&out.stderr) {
        return Err(why);
    }
    Ok(judged(out, root))
}

/// Progress under miri: any line but nextest's note that a test still runs, which a hung test
/// repeats each minute. So the per-test limit decides, not how many tests the run holds.
fn moving(line: &str) -> bool {
    let line = exec::strip_colour(line);
    let said = line.trim_start();
    !said.is_empty() && !said.starts_with("SLOW")
}

/// nextest's Miri profile only warns about a slow test, so one test could run unbounded and
/// unnamed. A project's own `default-miri` setting still wins over this one.
const PER_TEST: &str =
    "[profile.default-miri]\nslow-timeout = { period = \"60s\", terminate-after = 5 }\n";

/// The per-test limit, in a file of this run's own, removed when the run ends.
struct Limit(PathBuf);

impl Drop for Limit {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0) {
            let path = self.0.display();
            eprintln!("chock: cannot remove miri's per-test limit {path}: {error}");
        }
    }
}

/// The arguments, with the limit written where nextest reads a tool's own settings.
fn limited(ctx: &Ctx, asked: &[String]) -> Result<(Limit, Vec<String>), String> {
    let failed = |error: std::io::Error| format!("cannot write miri's per-test limit: {error}");
    let parent = ctx.root.join("target/test-scratch");
    crate::project::document::rooted(&parent).map_err(failed)?;
    std::fs::create_dir_all(&parent).map_err(failed)?;
    let path = parent.join(format!(
        "miri-limit-{}-{}.toml",
        std::process::id(),
        stamp()
    ));
    // A new file only, so a path already there, or a link planted there, is never written through.
    let mut file = std::fs::File::create_new(&path).map_err(failed)?;
    let limit = Limit(path);
    file.write_all(PER_TEST.as_bytes()).map_err(failed)?;
    let mut args = invocation(&ctx.miri, asked);
    args.extend(
        ctx.miri_part
            .map(|part| format!("--partition=count:{}/{}", part.index, part.of)),
    );
    args.push(format!("--tool-config-file=chock:{}", limit.0.display()));
    Ok((limit, args))
}

/// Nanoseconds since the epoch, so a crashed run's file under a reused pid is never in the way.
fn stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default()
}

/// The suite's verdict, naming each failed test: a failed assertion prints no span chock reads.
fn judged(out: &exec::Output, root: &Path) -> Outcome {
    let stopped = super::timed_out_tests(out);
    let mut outcome = super::verdict(out, root);
    outcome
        .findings
        .extend(super::failing_tests(out).iter().map(|test| {
            let why = if stopped.contains(test) {
                STOPPED
            } else {
                FAILED
            };
            Finding::at("", why).item(test)
        }));
    outcome
}

const FAILED: &str = "failed under miri; `cargo +nightly miri nextest run` with this test's name \
                      prints why";

const STOPPED: &str = "ran past its limit under miri, five minutes unless the project's \
                       `[profile.default-miri]` sets one, so it was stopped; give it a smaller \
                       input when `cfg(miri)` holds, or `#[cfg_attr(miri, ignore = \"too slow\")]`";

/// The feature and build flags to pass on; a cargo profile is refused, since miri cannot take one.
fn asked(ctx: &Ctx) -> Result<Vec<String>, String> {
    if ctx.build_flag("--profile").is_some() {
        return Err("miri interprets its own build and cannot be told a cargo profile".to_string());
    }
    Ok([ctx.features.as_slice(), ctx.build.as_slice()].concat())
}

/// `--no-fail-fast`, so the run lists every test the interpreter rejects and not only the first.
fn invocation(scope: &crate::project::config::Scope, features: &[String]) -> Vec<String> {
    let mut args = [
        "+nightly",
        "miri",
        "nextest",
        "run",
        "--no-tests=fail",
        "--no-fail-fast",
    ]
    .map(String::from)
    .to_vec();
    args.extend(scoped(scope));
    args.extend_from_slice(features);
    args
}

/// The remedy for a failed run: a per-test limit or longer deadline if it hung, else install miri.
fn advice(hung: bool, said: &str) -> String {
    if hung {
        format!(
            "{said}; no test finished in that time, so a build stalled or a test hangs with no \
             `terminate-after` in the project's `[profile.default-miri]`: set one there, or \
             raise {}",
            exec::TIMEOUT
        )
    } else {
        format!("{said} — this gate needs miri: `rustup +nightly component add miri`")
    }
}

/// The package arguments: named packages win over exclusions; with neither, the whole workspace.
fn scoped(scope: &crate::project::config::Scope) -> Vec<String> {
    let named = scope.packages.as_deref().unwrap_or_default();
    if !named.is_empty() {
        return named
            .iter()
            .flat_map(|package| ["-p".to_string(), package.clone()])
            .collect();
    }
    let mut args = vec!["--workspace".to_string()];
    for package in scope.exclude.as_deref().unwrap_or_default() {
        args.push("--exclude".to_string());
        args.push(package.clone());
    }
    args
}

/// The reason for a nightly with no Miri, or no nightly at all; `fixes::repair` reads it back.
pub const ABSENT: &str = "Miri is not installed on the `nightly` toolchain";

/// Whether cargo says it has no Miri to start: the component is missing, or the whole toolchain.
fn absent(stderr: &str) -> bool {
    stderr.contains("'cargo-miri' is not installed") || crate::gates::fixes::no_nightly(stderr)
}

/// Why miri never tested the code, if it did not: it is not installed, or it hit an operation it
/// cannot emulate.
fn never_ran(stderr: &str) -> Option<String> {
    if absent(stderr) {
        return Some(ABSENT.to_string());
    }
    let unsupported = stderr
        .lines()
        .find(|line| line.contains("unsupported operation:"))?;
    let what = unsupported.split("unsupported operation: ").nth(1)?;
    Some(format!(
        "miri cannot emulate something the suite does, so it stopped before finishing: {what}. \
         {}",
        instead(what)
    ))
}

/// The setting to change: the flags when the refusal names one, else the packages miri runs over.
fn instead(what: &str) -> String {
    match flag_named(what) {
        Some(flag) => format!(
            "{flag} is what refused, and the flags are the project's to set in .chock/config.json: \
             `\"miri\": {{\"flags\": [\"-Zmiri-disable-isolation\", \"-Zmiri-tree-borrows\"]}}`."
        ),
        None => "Name the crates it can run over in .chock/config.json: \
                 `\"miri\": {\"packages\": [\"a\", \"b\"]}`, or `\"exclude\"` the ones it cannot. \
                 For one test, write `#[cfg_attr(miri, ignore)]` above it."
            .to_string(),
    }
}

/// The `-Zmiri-` flag a refusal quotes, in backticks or bare; any other switch is ignored.
fn flag_named(what: &str) -> Option<&str> {
    let at = what.find("-Zmiri-")?;
    let rest = &what[at..];
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Default flags. Isolation is off so a suite may read files and the clock; strict provenance is
/// the rule being checked.
const MIRIFLAGS: &str = "-Zmiri-disable-isolation -Zmiri-strict-provenance";

/// The project's flags, replacing the defaults so a project can drop one; an empty list gets the
/// defaults.
fn miriflags(scope: &crate::project::config::Scope) -> String {
    match scope.flags.as_deref().unwrap_or_default() {
        [] => MIRIFLAGS.to_string(),
        flags => flags.join(" "),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn a_target_reaches_the_interpreter_and_a_profile_is_refused() {
        let mut ctx = Ctx::for_root(
            std::path::PathBuf::from("/w"),
            crate::run::baseline::Baseline::empty("0.1.0"),
        );
        ctx.features = vec!["--all-features".to_string()];
        ctx.build = ["--target", "x86_64-unknown-linux-gnu"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            asked(&ctx).unwrap(),
            ["--all-features", "--target", "x86_64-unknown-linux-gnu"]
        );
        ctx.build.extend(["--profile", "dist"].map(String::from));
        assert_eq!(
            asked(&ctx).unwrap_err(),
            "miri interprets its own build and cannot be told a cargo profile"
        );
        assert_eq!(
            checked(&ctx).unwrap_err(),
            "miri interprets its own build and cannot be told a cargo profile",
            "refused before the interpreter starts"
        );
    }

    #[test]
    fn the_interpreter_receives_scope_and_feature_flags_together() {
        let scope = crate::project::config::Scope {
            packages: Some(vec!["fixture".to_string()]),
            ..crate::project::config::Scope::default()
        };
        let flags = ["--no-default-features", "--features", "testkit"].map(String::from);
        assert_eq!(
            invocation(&scope, &flags),
            [
                "+nightly",
                "miri",
                "nextest",
                "run",
                "--no-tests=fail",
                "--no-fail-fast",
                "-p",
                "fixture",
                "--no-default-features",
                "--features",
                "testkit"
            ]
        );
    }

    #[test]
    fn a_test_that_ends_or_a_crate_that_builds_is_progress_and_a_slow_note_is_not() {
        assert!(moving(
            "        PASS [   2.104s] (3/9) chock vcs::tests::reads"
        ));
        assert!(moving("   Compiling chock v0.2.0"));
        assert!(moving(
            "     TIMEOUT [ 300.004s] (2/2) chock vcs::tests::slow"
        ));
        assert!(!moving("        SLOW [> 60.000s] chock vcs::tests::slow"));
        assert!(!moving(
            "\u{1b}[33m        SLOW\u{1b}[0m [>120.000s] chock vcs::tests::slow"
        ));
        assert!(!moving("   "));
        assert!(!moving(""));
    }

    #[test]
    fn a_deadline_reached_is_told_apart_from_a_component_that_is_not_installed() {
        let said = advice(true, "cargo gave up waiting");
        assert!(said.contains("terminate-after"), "{said}");
        assert!(said.contains(exec::TIMEOUT), "{said}");
        assert!(!said.contains("component add"), "{said}");

        let said = advice(false, "cargo could not start");
        assert!(said.contains("component add miri"), "{said}");
        assert!(!said.contains(exec::TIMEOUT), "{said}");
    }

    fn ctx_at(root: &Path) -> Ctx {
        Ctx::for_root(
            root.to_path_buf(),
            crate::run::baseline::Baseline::empty("0.1.0"),
        )
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_per_test_limit_is_a_file_of_the_runs_own_and_goes_when_the_run_ends() {
        let root = crate::testdir::make("miri-limit");
        let (limit, args) = limited(&ctx_at(&root), &["--all-features".to_string()]).unwrap();
        let named = args.last().unwrap();
        let path = named.strip_prefix("--tool-config-file=chock:").unwrap();
        assert_eq!(Path::new(path), limit.0);
        assert!(limit.0.starts_with(root.join("target/test-scratch")));
        assert_eq!(std::fs::read_to_string(&limit.0).unwrap(), PER_TEST);
        assert!(args.contains(&"--all-features".to_string()));
        let kept = limit.0.clone();
        drop(limit);
        assert!(!kept.exists(), "removed when the run ends");
        drop(Limit(kept.clone()));
        assert!(!kept.exists(), "a file already gone is only reported");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_part_of_the_suite_goes_to_nextest_as_its_partition() {
        let root = crate::testdir::make("miri-part");
        let mut ctx = ctx_at(&root);
        let (_whole_limit, whole) = limited(&ctx, &[]).unwrap();
        assert!(
            !whole.iter().any(|arg| arg.starts_with("--partition")),
            "{whole:?}"
        );
        ctx.miri_part = Some(Part { index: 2, of: 12 });
        let (_part_limit, part) = limited(&ctx, &[]).unwrap();
        assert!(
            part.contains(&"--partition=count:2/12".to_string()),
            "{part:?}"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_limit_that_cannot_be_written_stops_the_gate_before_the_interpreter() {
        let root = crate::testdir::tree("miri-limit-blocked", &[("target", "a file")]);
        let said = limited(&ctx_at(&root), &[]).err().unwrap();
        assert!(
            said.starts_with("cannot write miri's per-test limit: "),
            "{said}"
        );
        let said = limited(&ctx_at(Path::new("w")), &[]).err().unwrap();
        assert!(said.ends_with("is not under a project root"), "{said}");
    }

    #[test]
    fn a_test_the_limit_stopped_is_named_with_how_to_shorten_it() {
        let out = exec::Output {
            code: Some(100),
            stdout: String::new(),
            stderr: "     TIMEOUT [ 300.004s] (2/2) chock vcs::tests::slow\n".to_string(),
            truncated: false,
        };
        let outcome = judged(&out, Path::new("/w"));
        assert!(!outcome.passed);
        assert_eq!(
            outcome
                .findings
                .iter()
                .map(Finding::render)
                .collect::<Vec<_>>(),
            [format!("chock vcs::tests::slow: {STOPPED}")]
        );
    }

    /// As CI printed a failed assertion: the test is named, so the gate is not unable to run.
    #[test]
    fn a_test_that_failed_under_miri_is_named_though_it_printed_no_span() {
        let out = exec::Output {
            code: Some(100),
            stdout: String::new(),
            stderr: "        FAIL [  14.322s] (117/299) chock watch::tests::reused\n\
                     error: test run failed\n"
                .to_string(),
            truncated: false,
        };
        let outcome = judged(&out, Path::new("/w"));
        assert!(!outcome.passed);
        assert_eq!(
            outcome
                .findings
                .iter()
                .map(Finding::render)
                .collect::<Vec<_>>(),
            [format!("chock watch::tests::reused: {FAILED}")]
        );
    }

    fn said(code: i32, stdout: &str, stderr: &str) -> exec::Output {
        exec::Output {
            code: Some(code),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            truncated: false,
        }
    }

    const LISTED: &str = r#"{"rust-suites": {"chock": {"package-name": "chock",
        "binary-name": "chock", "kind": "lib", "testcases": {
        "a": {"ignored": false, "filter-match": {"status": "matches"}},
        "b": {"ignored": false, "filter-match": {"status": "matches"}}}}}}"#;

    /// `suite` against canned answers: the list, each group (`None` stops it), then nextest's run.
    fn suite_with(
        list: &exec::Output,
        group: Option<i32>,
        nextest: &exec::Output,
    ) -> (Result<Outcome, String>, Vec<Vec<String>>) {
        let asked = std::sync::Mutex::new(Vec::new());
        let run = |argv: &[String]| {
            asked.lock().unwrap().push(argv.to_vec());
            match (argv[2].as_str(), argv[3].as_str()) {
                ("nextest", "list") => Ok(list.clone()),
                ("test", _) => group
                    .map(|code| said(code, "", ""))
                    .ok_or("stopped".to_string()),
                _ => Ok(nextest.clone()),
            }
        };
        let args = invocation(&crate::project::config::Scope::default(), &[]);
        let ctx = ctx_at(Path::new("/w"));
        let outcome = suite(&ctx, &[], &args, &run);
        (outcome, asked.into_inner().unwrap())
    }

    fn verbs(asked: &[Vec<String>]) -> Vec<&str> {
        asked.iter().map(|argv| argv[3].as_str()).collect()
    }

    #[test]
    fn groups_that_pass_are_the_whole_run_and_nextest_never_starts() {
        let (outcome, asked) = suite_with(&said(0, LISTED, ""), Some(0), &said(1, "", ""));
        assert!(outcome.unwrap().passed);
        assert_eq!(verbs(&asked), ["list", "-p"], "one group for both tests");
        assert!(asked[1].ends_with(&["a".to_string(), "b".to_string()]));
    }

    #[test]
    fn a_group_that_fails_or_stops_runs_again_under_nextest_which_names_the_test() {
        let fail = "        FAIL [  1.000s] (1/2) chock a\n";
        for group in [Some(1), None] {
            let (outcome, asked) = suite_with(&said(0, LISTED, ""), group, &said(100, "", fail));
            let outcome = outcome.unwrap();
            assert!(!outcome.passed);
            assert_eq!(
                outcome
                    .findings
                    .iter()
                    .map(Finding::render)
                    .collect::<Vec<_>>(),
                [format!("chock a: {FAILED}")]
            );
            assert_eq!(verbs(&asked), ["list", "-p", "run"]);
            let filter = "(binary_id(=chock) & (test(=a) | test(=b)))";
            assert!(asked[2].ends_with(&["-E".to_string(), filter.to_string()]));
        }
    }

    #[test]
    fn a_list_that_fails_or_holds_no_group_leaves_the_one_nextest_run() {
        for list in [said(101, LISTED, ""), said(0, "{}", "")] {
            let (outcome, asked) = suite_with(&list, Some(0), &said(0, "", ""));
            assert!(outcome.unwrap().passed);
            assert_eq!(verbs(&asked), ["list", "run"]);
        }
        let absent = said(101, "", "error: 'cargo-miri' is not installed");
        let (outcome, _) = suite_with(&absent, Some(0), &absent);
        assert_eq!(outcome.unwrap_err(), ABSENT);
    }

    #[test]
    fn miri_runs_over_the_packages_a_project_named_and_the_workspace_when_it_named_none() {
        use crate::project::config::Scope;
        assert_eq!(scoped(&Scope::default()), vec!["--workspace".to_string()]);
        assert_eq!(
            scoped(&Scope {
                packages: Some(vec!["outpost-core".to_string(), "app-core".to_string()]),
                exclude: None,
                ..Scope::default()
            }),
            vec!["-p", "outpost-core", "-p", "app-core"]
        );
        assert_eq!(
            scoped(&Scope {
                packages: None,
                exclude: Some(vec!["outpost-tabular".to_string()]),
                ..Scope::default()
            }),
            vec!["--workspace", "--exclude", "outpost-tabular"]
        );
        // Both named: the package list wins.
        assert_eq!(
            scoped(&Scope {
                packages: Some(vec!["a".to_string()]),
                exclude: Some(vec!["b".to_string()]),
                ..Scope::default()
            }),
            vec!["-p", "a"]
        );
        // An empty list means nothing was named.
        assert_eq!(
            scoped(&Scope {
                packages: Some(Vec::new()),
                exclude: None,
                ..Scope::default()
            }),
            vec!["--workspace".to_string()]
        );
    }

    #[test]
    fn the_reason_miri_stopped_names_the_setting_that_scopes_it() {
        let stderr =
            "error: unsupported operation: can't call foreign function `mi_malloc_aligned`\n";
        let why = never_ran(stderr).unwrap();
        assert!(why.contains("mi_malloc_aligned"), "{why}");
        assert!(why.contains(r#""miri": {"packages": ["a", "b"]}"#), "{why}");
        assert!(
            why.ends_with("write `#[cfg_attr(miri, ignore)]` above it."),
            "{why}"
        );
    }

    #[test]
    fn a_refusal_that_names_a_flag_says_to_set_the_flags_and_not_to_drop_the_crate() {
        let stderr = "error: unsupported operation: integer-to-pointer casts and \
                      `ptr::with_exposed_provenance` are not supported with \
                      `-Zmiri-strict-provenance`\n";
        let why = never_ran(stderr).unwrap();
        assert!(
            why.contains("-Zmiri-strict-provenance is what refused"),
            "{why}"
        );
        assert!(why.contains(r#""miri": {"flags": ["#), "{why}");
        assert!(!why.contains("packages"), "{why}");
    }

    #[test]
    fn the_flag_a_refusal_quotes_is_read_out_of_whatever_punctuation_surrounds_it() {
        assert_eq!(
            flag_named("not supported with `-Zmiri-tree-borrows`"),
            Some("-Zmiri-tree-borrows")
        );
        assert_eq!(
            flag_named("-Zmiri-ignore-leaks, which"),
            Some("-Zmiri-ignore-leaks")
        );
        assert_eq!(flag_named("can't call foreign function `getppid`"), None);
        // Only a `-Zmiri-` switch counts.
        assert_eq!(flag_named("unsupported with -Zsanitizer=address"), None);
    }

    #[test]
    fn the_flags_a_project_names_are_what_miri_runs_with_and_an_empty_list_is_chocks_own() {
        use crate::project::config::Scope;
        assert_eq!(miriflags(&Scope::default()), MIRIFLAGS);
        assert_eq!(
            miriflags(&Scope {
                flags: Some(vec![
                    "-Zmiri-disable-isolation".to_string(),
                    "-Zmiri-tree-borrows".to_string(),
                ]),
                ..Scope::default()
            }),
            "-Zmiri-disable-isolation -Zmiri-tree-borrows"
        );
        // An empty list is not a run with no flags.
        assert_eq!(
            miriflags(&Scope {
                flags: Some(Vec::new()),
                ..Scope::default()
            }),
            MIRIFLAGS
        );
    }

    #[test]
    fn a_nightly_without_miri_and_a_machine_without_nightly_are_one_reason_with_one_repair() {
        let component = "error: 'cargo-miri' is not installed for the toolchain 'nightly'.\n";
        let toolchain = "error: toolchain 'nightly-x86_64-unknown-linux-gnu' is not installed\n";
        for said in [component, toolchain] {
            assert_eq!(never_ran(said).as_deref(), Some(ABSENT), "{said}");
        }
        assert_eq!(
            crate::gates::fixes::repair(ABSENT),
            Some(
                "run `chock init --global`: it installs each nightly toolchain a gate starts, and Miri"
            )
        );
    }

    #[test]
    fn undefined_behaviour_is_not_read_as_a_reason_miri_never_ran() {
        let said = "error: Undefined Behavior: attempting a read access\n";
        assert_eq!(never_ran(said), None);
        assert_eq!(never_ran(""), None);
    }

    /// The span miri prints points into `std`; read as a finding, it would blame the wrong file.
    #[test]
    fn a_syscall_miri_cannot_emulate_is_a_gate_that_could_not_run() {
        let said = "error: unsupported operation: socketpair: type 0x5 is unsupported\n   --> \
                    library/std/src/sys/net/connection/socket/unix.rs:135:25\n";
        let why = never_ran(said).unwrap();
        assert!(
            why.starts_with(
                "miri cannot emulate something the suite does, so it stopped before finishing: \
                 socketpair: type 0x5 is unsupported."
            ),
            "{why}"
        );
    }

    #[test]
    fn the_gate_names_how_to_rerun_itself_and_stays_out_of_the_default_set() {
        assert!(crate::run::rerun(GATE.name).contains(GATE.name));
        assert_eq!(GATE.group, Group::OptIn);
    }
}
