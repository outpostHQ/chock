//! The clippy code-shape ratchet: lints worth tracking but not safe to deny outright, counted one
//! number per lint.

use std::collections::BTreeSet;

use serde::Deserialize;

use crate::exec;
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

pub const GATE: Gate = Gate {
    name: "codeslop",
    about: "clippy's code-shape lints counted per lint — needless clones, pass-by-value, argument \
            counts, complexity — not comment length, which is `slop`",
    group: Group::Quality,
    builds: true,
    reads: None,
    kind: Kind::Ratchet {
        measure,
        // Keys are lint names, and a lint with no hit has no key: one the baseline lacks is a
        // first hit, which fails as any count the record lacks does.
        keys: Keys::Measures,
        unit: "findings",
    },
};

/// The lints this gate warns on. Not denied in `[lints.clippy]`, since the `lint` gate denies
/// warnings and every existing hit would then block a push.
pub const LINTS: [&str; 7] = [
    "clippy::too_many_arguments",
    "clippy::type_complexity",
    "clippy::large_enum_variant",
    "clippy::needless_pass_by_value",
    "clippy::redundant_clone",
    "clippy::cognitive_complexity",
    "clippy::unused_async",
];

/// How many of cargo's own lines a "could not run" carries back, newest last.
const TAIL: usize = 10;

/// The clippy command. JSON, since the human render caps each lint at ten warnings and names it
/// twice; features go before the `--`.
#[must_use]
pub fn invocation(features: &[String]) -> Vec<&str> {
    let run = ["clippy", "--workspace", "--all-targets", "--no-deps"];
    let asked = features.iter().map(String::as_str);
    // `--cap-lints=warn`, so `-D warnings` in RUSTFLAGS cannot stop the build before the count.
    let json = ["--message-format=json", "--", "--cap-lints=warn"];
    let warn = LINTS.iter().flat_map(|lint| ["-W", lint]);
    run.into_iter()
        .chain(asked)
        .chain(json)
        .chain(warn)
        .collect()
}

/// One diagnostic, reduced to what decides whether two of them are the same finding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hit {
    pub lint: String,
    /// Absent when the diagnostic has no primary span, which is how a crate-level lint arrives.
    pub file: Option<String>,
    pub line: Option<u32>,
}

/// The fields of a cargo JSON line that decide a count; serde drops the rest.
#[derive(Deserialize)]
struct CargoLine {
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    code: Option<Code>,
    #[serde(default)]
    spans: Vec<Span>,
}

#[derive(Deserialize)]
struct Code {
    code: String,
}

/// Both locators are optional, so a span chock cannot place still counts its lint.
#[derive(Deserialize)]
struct Span {
    file_name: Option<String>,
    line_start: Option<u32>,
    #[serde(default)]
    is_primary: bool,
}

/// Every distinct clippy finding in one run. Cargo repeats a diagnostic for each target, so a
/// repeat counts once.
#[must_use]
pub fn hits(stdout: &str) -> Vec<Hit> {
    let mut seen = BTreeSet::new();
    for line in stdout.lines() {
        // Other lines are bookkeeping, or the half line a killed run can leave.
        let Ok(parsed) = serde_json::from_str::<CargoLine>(line) else {
            continue;
        };
        let Some(message) = parsed.message else {
            continue;
        };
        let Some(code) = message.code else {
            continue;
        };
        if !code.code.starts_with("clippy::") {
            continue;
        }
        // Every clippy lint counts, not only `LINTS`: one the project's config enables is debt too.
        let primary = message.spans.iter().find(|span| span.is_primary);
        seen.insert(Hit {
            lint: code.code,
            file: primary.and_then(|span| span.file_name.clone()),
            line: primary.and_then(|span| span.line_start),
        });
    }
    seen.into_iter().collect()
}

/// One number per lint that fired; a lint that did not fire is absent, not zero.
#[must_use]
pub fn counts(hits: &[Hit]) -> Series {
    let mut series = Series::new();
    for hit in hits {
        series.set(&hit.lint, series.get(&hit.lint).unwrap_or(0) + 1);
    }
    series
}

/// What one clippy run measured, or why it measured nothing.
pub fn from_run(out: &exec::Output) -> Result<Series, String> {
    if !out.success() {
        return Err(did_not_finish(&out.stderr));
    }
    if out.truncated {
        return Err(format!(
            "clippy printed over {} bytes; a count from the prefix chock kept would be wrong",
            exec::MAX_CAPTURE
        ));
    }
    Ok(counts(&hits(&out.stdout)))
}

/// Why clippy did not finish, from cargo's last error lines; a failed build never counts as zero.
fn did_not_finish(stderr: &str) -> String {
    let named: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("error") || line.starts_with("warning: build failed"))
        .collect();
    let lines = if named.is_empty() {
        stderr.lines().collect()
    } else {
        named
    };
    let tail = &lines[lines.len().saturating_sub(TAIL)..];
    match tail.join("; ") {
        said if said.is_empty() => "cargo clippy did not finish and said nothing about why".into(),
        said => format!("cargo clippy did not finish, so nothing was measured: {said}"),
    }
}

fn measure(ctx: &Ctx) -> Result<Series, String> {
    let asked = [ctx.features.as_slice(), ctx.build.as_slice()].concat();
    from_run(&exec::run("cargo", &invocation(&asked), &ctx.root).map_err(|e| e.to_string())?)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    /// Captured from `cargo clippy --message-format=json` on a crate that clones a live binding.
    const REAL_CLONE: &str = r#"{"reason":"compiler-message","package_id":"path+file:///tmp/f#0.1.0","manifest_path":"/tmp/f/Cargo.toml","target":{"kind":["lib"],"crate_types":["lib"],"name":"f","src_path":"/tmp/f/src/lib.rs","edition":"2021","doc":true,"doctest":true,"test":true},"message":{"rendered":"warning: redundant clone\n --> src/lib.rs:3:21\n  |\n3 |     let copy = owned.clone();\n  |                     ^^^^^^^^ help: remove this\n  |\nnote: this value is dropped without further use\n --> src/lib.rs:3:16\n  |\n3 |     let copy = owned.clone();\n  |                ^^^^^\n  = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.98.0/index.html#redundant_clone\n  = note: requested on the command line with `-W clippy::redundant-clone`\n\n","$message_type":"diagnostic","children":[{"children":[],"code":null,"level":"note","message":"this value is dropped without further use","rendered":null,"spans":[{"byte_end":86,"byte_start":81,"column_end":21,"column_start":16,"expansion":null,"file_name":"src/lib.rs","is_primary":true,"label":null,"line_end":3,"line_start":3,"suggested_replacement":null,"suggestion_applicability":null,"text":[{"highlight_end":21,"highlight_start":16,"text":"    let copy = owned.clone();"}]}]},{"children":[],"code":null,"level":"help","message":"for further information visit https://rust-lang.github.io/rust-clippy/rust-1.98.0/index.html#redundant_clone","rendered":null,"spans":[]},{"children":[],"code":null,"level":"note","message":"requested on the command line with `-W clippy::redundant-clone`","rendered":null,"spans":[]},{"children":[],"code":null,"level":"help","message":"remove this","rendered":null,"spans":[{"byte_end":94,"byte_start":86,"column_end":29,"column_start":21,"expansion":null,"file_name":"src/lib.rs","is_primary":true,"label":null,"line_end":3,"line_start":3,"suggested_replacement":"","suggestion_applicability":"MachineApplicable","text":[{"highlight_end":29,"highlight_start":21,"text":"    let copy = owned.clone();"}]}]}],"level":"warning","message":"redundant clone","spans":[{"byte_end":94,"byte_start":86,"column_end":29,"column_start":21,"expansion":null,"file_name":"src/lib.rs","is_primary":true,"label":null,"line_end":3,"line_start":3,"suggested_replacement":null,"suggestion_applicability":null,"text":[{"highlight_end":29,"highlight_start":21,"text":"    let copy = owned.clone();"}]}],"code":{"code":"clippy::redundant_clone","explanation":null}}}"#;

    /// The same run's second diagnostic, whose first span is not the primary one.
    const REAL_LET_AND_RETURN: &str = r#"{"reason":"compiler-message","package_id":"path+file:///tmp/f#0.1.0","manifest_path":"/tmp/f/Cargo.toml","target":{"kind":["lib"],"crate_types":["lib"],"name":"f","src_path":"/tmp/f/src/lib.rs","edition":"2021","doc":true,"doctest":true,"test":true},"message":{"rendered":"warning: returning the result of a `let` binding from a block\n --> src/lib.rs:4:5\n  |\n3 |     let copy = owned.clone();\n  |     ------------------------- unnecessary `let` binding\n4 |     copy\n  |     ^^^^\n  |\n  = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.98.0/index.html#let_and_return\n  = note: `#[warn(clippy::let_and_return)]` on by default\nhelp: return the expression directly\n  |\n3 ~     \n4 ~     owned.clone()\n  |\n\n","$message_type":"diagnostic","children":[{"children":[],"code":null,"level":"help","message":"for further information visit https://rust-lang.github.io/rust-clippy/rust-1.98.0/index.html#let_and_return","rendered":null,"spans":[]},{"children":[],"code":null,"level":"note","message":"`#[warn(clippy::let_and_return)]` on by default","rendered":null,"spans":[]},{"children":[],"code":null,"level":"help","message":"return the expression directly","rendered":null,"spans":[{"byte_end":95,"byte_start":70,"column_end":30,"column_start":5,"expansion":null,"file_name":"src/lib.rs","is_primary":true,"label":null,"line_end":3,"line_start":3,"suggested_replacement":"","suggestion_applicability":"MachineApplicable","text":[{"highlight_end":30,"highlight_start":5,"text":"    let copy = owned.clone();"}]},{"byte_end":104,"byte_start":100,"column_end":9,"column_start":5,"expansion":null,"file_name":"src/lib.rs","is_primary":true,"label":null,"line_end":4,"line_start":4,"suggested_replacement":"owned.clone()","suggestion_applicability":"MachineApplicable","text":[{"highlight_end":9,"highlight_start":5,"text":"    copy"}]}]}],"level":"warning","message":"returning the result of a `let` binding from a block","spans":[{"byte_end":95,"byte_start":70,"column_end":30,"column_start":5,"expansion":null,"file_name":"src/lib.rs","is_primary":false,"label":"unnecessary `let` binding","line_end":3,"line_start":3,"suggested_replacement":null,"suggestion_applicability":null,"text":[{"highlight_end":30,"highlight_start":5,"text":"    let copy = owned.clone();"}]},{"byte_end":104,"byte_start":100,"column_end":9,"column_start":5,"expansion":null,"file_name":"src/lib.rs","is_primary":true,"label":null,"line_end":4,"line_start":4,"suggested_replacement":null,"suggestion_applicability":null,"text":[{"highlight_end":9,"highlight_start":5,"text":"    copy"}]}],"code":{"code":"clippy::let_and_return","explanation":null}}}"#;

    /// The bookkeeping every run ends with, captured alongside the two diagnostics above.
    const REAL_ARTIFACT: &str = r#"{"reason":"compiler-artifact","package_id":"path+file:///tmp/f#0.1.0","manifest_path":"/tmp/f/Cargo.toml","target":{"kind":["lib"],"crate_types":["lib"],"name":"f","src_path":"/tmp/f/src/lib.rs","edition":"2021","doc":true,"doctest":true,"test":true},"profile":{"opt_level":"0","debuginfo":2,"debug_assertions":true,"overflow_checks":true,"test":false},"features":[],"filenames":["/tmp/f/target/debug/deps/libf-b540e639842940d5.rmeta"],"executable":null,"fresh":false}"#;

    const REAL_FINISHED: &str = r#"{"reason":"build-finished","success":true}"#;

    #[test]
    fn a_hit_is_placed_at_the_primary_span_and_not_the_first_one() {
        let json = diagnostic(
            &lint("clippy::redundant_clone"),
            &[span("src/note.rs", 9, false), span("src/real.rs", 2, true)],
        );
        let found = hits(&json);
        assert_eq!(
            found
                .iter()
                .map(|h| (h.file.clone(), h.line))
                .collect::<Vec<_>>(),
            [(Some("src/real.rs".to_string()), Some(2))]
        );
    }

    /// A cargo line carrying one diagnostic, in the shape the captured ones above have.
    fn diagnostic(code: &str, spans: &[String]) -> String {
        format!(
            r#"{{"reason":"compiler-message","message":{{"code":{},"spans":[{}]}}}}"#,
            code,
            spans.join(",")
        )
    }

    fn lint(name: &str) -> String {
        format!(r#"{{"code":"{name}","explanation":null}}"#)
    }

    fn span(file: &str, line: u32, primary: bool) -> String {
        format!(r#"{{"file_name":"{file}","line_start":{line},"is_primary":{primary}}}"#)
    }

    fn at(lint: &str, file: &str, line: u32) -> Hit {
        Hit {
            lint: lint.to_string(),
            file: Some(file.to_string()),
            line: Some(line),
        }
    }

    fn ran(code: i32, stdout: &str, stderr: &str, truncated: bool) -> exec::Output {
        exec::Output {
            code: Some(code),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            truncated,
        }
    }

    fn real_run() -> String {
        format!("{REAL_CLONE}\n{REAL_LET_AND_RETURN}\n{REAL_ARTIFACT}\n{REAL_FINISHED}\n")
    }

    #[test]
    fn the_features_a_project_names_go_before_the_double_dash_where_cargo_reads_them() {
        let named = ["--features".to_string(), "testkit".to_string()];
        let asked = invocation(&named);
        let double_dash = asked.iter().position(|arg| *arg == "--").unwrap();
        let features = asked.iter().position(|arg| *arg == "--features").unwrap();
        assert!(features < double_dash, "{asked:?}");
        assert_eq!(asked[features + 1], "testkit");
    }

    #[test]
    fn the_invocation_asks_for_json_and_warns_every_lint_it_ratchets() {
        assert_eq!(
            invocation(&[]),
            vec![
                "clippy",
                "--workspace",
                "--all-targets",
                "--no-deps",
                "--message-format=json",
                "--",
                "--cap-lints=warn",
                "-W",
                "clippy::too_many_arguments",
                "-W",
                "clippy::type_complexity",
                "-W",
                "clippy::large_enum_variant",
                "-W",
                "clippy::needless_pass_by_value",
                "-W",
                "clippy::redundant_clone",
                "-W",
                "clippy::cognitive_complexity",
                "-W",
                "clippy::unused_async",
            ]
        );
    }

    #[test]
    fn a_captured_clippy_run_yields_its_diagnostics_and_none_of_its_bookkeeping() {
        assert_eq!(
            hits(&real_run()),
            vec![
                at("clippy::let_and_return", "src/lib.rs", 4),
                at("clippy::redundant_clone", "src/lib.rs", 3),
            ]
        );
    }

    #[test]
    fn the_primary_span_places_a_finding_even_when_it_is_not_the_first_span() {
        assert_eq!(
            hits(REAL_LET_AND_RETURN),
            vec![at("clippy::let_and_return", "src/lib.rs", 4)]
        );
    }

    #[test]
    fn a_line_that_is_not_json_is_skipped_rather_than_failing_the_run() {
        let stdout = format!("not json at all\n{REAL_CLONE}\n{{\"reason\":\"compiler-mess\n");
        assert_eq!(
            hits(&stdout),
            vec![at("clippy::redundant_clone", "src/lib.rs", 3)]
        );
    }

    #[test]
    fn a_cargo_line_that_carries_no_diagnostic_counts_nothing() {
        assert_eq!(hits(&format!("{REAL_ARTIFACT}\n{REAL_FINISHED}\n")), vec![]);
        assert_eq!(
            hits(r#"{"reason":"compiler-message","message":null}"#),
            vec![]
        );
    }

    #[test]
    fn the_same_lint_at_the_same_place_counts_once_however_many_targets_report_it() {
        let one = diagnostic(
            &lint("clippy::redundant_clone"),
            &[span("src/a.rs", 7, true)],
        );
        assert_eq!(
            hits(&format!("{one}\n{one}\n{one}\n")),
            vec![at("clippy::redundant_clone", "src/a.rs", 7)]
        );
    }

    #[test]
    fn the_same_lint_on_two_lines_of_one_file_counts_twice() {
        let first = diagnostic(
            &lint("clippy::redundant_clone"),
            &[span("src/a.rs", 7, true)],
        );
        let second = diagnostic(
            &lint("clippy::redundant_clone"),
            &[span("src/a.rs", 9, true)],
        );
        assert_eq!(
            hits(&format!("{first}\n{second}\n")),
            vec![
                at("clippy::redundant_clone", "src/a.rs", 7),
                at("clippy::redundant_clone", "src/a.rs", 9),
            ]
        );
    }

    #[test]
    fn the_same_lint_and_line_number_in_two_files_counts_twice() {
        let first = diagnostic(
            &lint("clippy::type_complexity"),
            &[span("src/a.rs", 7, true)],
        );
        let second = diagnostic(
            &lint("clippy::type_complexity"),
            &[span("src/b.rs", 7, true)],
        );
        assert_eq!(
            hits(&format!("{first}\n{second}\n")),
            vec![
                at("clippy::type_complexity", "src/a.rs", 7),
                at("clippy::type_complexity", "src/b.rs", 7),
            ]
        );
    }

    #[test]
    fn a_diagnostic_with_no_primary_span_counts_once_however_many_arrive() {
        let none = diagnostic(&lint("clippy::large_enum_variant"), &[]);
        let unplaced = diagnostic(
            &lint("clippy::large_enum_variant"),
            &[span("src/a.rs", 3, false)],
        );
        assert_eq!(
            hits(&format!("{none}\n{unplaced}\n{none}\n")),
            vec![Hit {
                lint: "clippy::large_enum_variant".to_string(),
                file: None,
                line: None,
            }]
        );
    }

    #[test]
    fn a_rustc_lint_and_a_diagnostic_with_no_code_are_both_left_out() {
        let rustc = diagnostic(&lint("dead_code"), &[span("src/a.rs", 1, true)]);
        let unused = diagnostic(&lint("unused_variables"), &[span("src/a.rs", 2, true)]);
        let uncoded = diagnostic("null", &[span("src/a.rs", 3, true)]);
        assert_eq!(hits(&format!("{rustc}\n{unused}\n{uncoded}\n")), vec![]);
    }

    #[test]
    fn a_line_with_nothing_to_count_leaves_the_lines_after_it_to_be_read() {
        let ahead = [
            ("a line carrying no diagnostic", REAL_ARTIFACT.to_string()),
            (
                "a diagnostic with no lint code",
                diagnostic("null", &[span("src/a.rs", 3, true)]),
            ),
            (
                "a lint of rustc's own",
                diagnostic(&lint("dead_code"), &[span("src/a.rs", 1, true)]),
            ),
            ("a line that is not json", "half a cargo li".to_string()),
        ];
        for (what, ignored) in ahead {
            assert_eq!(
                hits(&format!("{ignored}\n{REAL_CLONE}\n")),
                vec![at("clippy::redundant_clone", "src/lib.rs", 3)],
                "{what}"
            );
        }
    }

    #[test]
    fn a_clippy_lint_outside_the_warned_set_is_still_counted() {
        assert!(!LINTS.contains(&"clippy::let_and_return"));
        assert_eq!(
            counts(&hits(REAL_LET_AND_RETURN)).get("clippy::let_and_return"),
            Some(1)
        );
    }

    #[test]
    fn a_lint_that_never_fired_is_absent_from_the_series_rather_than_zero() {
        let series = counts(&hits(&real_run()));
        assert_eq!(series.get("clippy::redundant_clone"), Some(1));
        assert_eq!(series.get("clippy::too_many_arguments"), None);
    }

    #[test]
    fn a_run_that_finished_is_one_count_per_lint_that_fired() {
        let series = from_run(&ran(0, &real_run(), "", false)).unwrap();
        assert_eq!(series.get("clippy::let_and_return"), Some(1));
        assert_eq!(series.get("clippy::redundant_clone"), Some(1));
        assert_eq!(
            series.0.keys().cloned().collect::<Vec<_>>(),
            vec![
                "clippy::let_and_return".to_string(),
                "clippy::redundant_clone".to_string(),
            ]
        );
    }

    #[test]
    fn a_build_failure_is_could_not_run_rather_than_a_count_of_zero() {
        let stderr = "error[E0425]: cannot find value `x` in this scope\n \
                      --> src/lib.rs:2:5\n\
                      error: could not compile `f` (lib) due to 1 previous error\n";
        assert_eq!(
            from_run(&ran(101, "", stderr, false)),
            Err("cargo clippy did not finish, so nothing was measured: \
                 error[E0425]: cannot find value `x` in this scope; \
                 error: could not compile `f` (lib) due to 1 previous error"
                .to_string())
        );
    }

    #[test]
    fn a_build_failure_with_diagnostics_already_on_stdout_still_measures_nothing() {
        let out = ran(101, &real_run(), "error: could not compile `f`\n", false);
        assert_eq!(
            from_run(&out),
            Err("cargo clippy did not finish, so nothing was measured: \
                 error: could not compile `f`"
                .to_string())
        );
    }

    #[test]
    fn a_failure_cargo_did_not_label_falls_back_to_the_tail_of_what_it_said() {
        let stderr = "linking with `cc` failed\nld: cannot find -lfoo\n";
        assert_eq!(
            from_run(&ran(101, "", stderr, false)),
            Err("cargo clippy did not finish, so nothing was measured: \
                 linking with `cc` failed; ld: cannot find -lfoo"
                .to_string())
        );
    }

    #[test]
    fn a_failure_with_nothing_on_stderr_still_says_which_step_gave_up() {
        assert_eq!(
            from_run(&ran(101, "", "", false)),
            Err("cargo clippy did not finish and said nothing about why".to_string())
        );
    }

    #[test]
    fn a_build_failed_warning_is_carried_like_an_error_line() {
        let stderr = "note: some detail\nwarning: build failed, waiting for other jobs\n";
        assert_eq!(
            from_run(&ran(101, "", stderr, false)),
            Err("cargo clippy did not finish, so nothing was measured: \
                 warning: build failed, waiting for other jobs"
                .to_string())
        );
    }

    #[test]
    fn only_the_last_ten_complaints_are_carried_into_the_reason() {
        let stderr: String = (1..=12).map(|n| format!("error: e{n}\n")).collect();
        let carried: Vec<String> = (3..=12).map(|n| format!("error: e{n}")).collect();
        assert_eq!(
            from_run(&ran(101, "", &stderr, false)),
            Err(format!(
                "cargo clippy did not finish, so nothing was measured: {}",
                carried.join("; ")
            ))
        );
    }

    #[test]
    fn a_signal_that_killed_clippy_is_could_not_run_and_not_a_clean_tree() {
        let out = exec::Output {
            code: None,
            stdout: String::new(),
            stderr: "error: process didn't exit successfully\n".to_string(),
            truncated: false,
        };
        assert!(from_run(&out).is_err());
    }

    #[test]
    fn truncated_output_is_refused_rather_than_counted_from_its_prefix() {
        assert_eq!(
            from_run(&ran(0, &real_run(), "", true)),
            Err(
                "clippy printed over 16777216 bytes; a count from the prefix chock kept would be \
                 wrong"
                    .to_string()
            )
        );
    }

    #[test]
    fn the_first_hit_of_a_lint_the_baseline_never_saw_is_a_regression() {
        let now = counts(&hits(&real_run()));
        let was = Series::new();
        let new: Vec<String> = now
            .regressions(&was, Keys::Measures)
            .iter()
            .map(|change| change.key().to_string())
            .collect();
        assert_eq!(
            new,
            vec!["clippy::let_and_return", "clippy::redundant_clone"]
        );
        assert_eq!(now.new_at_zero(&was), Vec::<String>::new());
    }

    #[test]
    fn a_lint_that_fired_more_often_than_the_baseline_is_a_regression() {
        let mut was = Series::new();
        was.set("clippy::redundant_clone", 1);
        was.set("clippy::let_and_return", 1);
        let now = counts(&hits(&real_run()));
        assert_eq!(now.regressions(&was, Keys::Measures), vec![]);
        was.set("clippy::redundant_clone", 0);
        assert_eq!(
            now.regressions(&was, Keys::Measures),
            vec![crate::run::baseline::Change::Grew {
                key: "clippy::redundant_clone".to_string(),
                was: 0,
                now: 1,
            }]
        );
    }

    #[test]
    fn a_lint_in_the_baseline_that_no_longer_fires_is_not_a_regression() {
        let mut was = Series::new();
        was.set("clippy::needless_pass_by_value", 159);
        assert_eq!(Series::new().regressions(&was, Keys::Measures), vec![]);
    }

    #[test]
    fn the_gate_ratchets_lint_names_as_measures_rather_than_items() {
        assert_eq!(GATE.name, "codeslop");
        assert_eq!(crate::run::rerun(GATE.name), "chock run codeslop");
        assert_eq!(GATE.group, Group::Quality);
        match GATE.kind {
            Kind::Ratchet { keys, unit, .. } | Kind::AnnotatedRatchet { keys, unit, .. } => {
                assert_eq!(keys, Keys::Measures);
                assert_eq!(unit, "findings");
            }
            Kind::Binary(_) | Kind::Debt { .. } => {
                panic!("codeslop is a ratchet, not a pass/fail gate")
            }
        }
    }
}
