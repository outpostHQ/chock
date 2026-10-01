//! The test suite under miri, an interpreter that detects undefined behaviour. Most of this file
//! separates a setup failure from a finding and names the setting to change.

use crate::exec;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

pub const GATE: Gate = Gate {
    name: "miri",
    about: "the suite runs clean under an interpreter that detects undefined behaviour",
    group: Group::OptIn,
    builds: true,
    reads: None,
    kind: Kind::Binary(checked),
};

fn checked(ctx: &Ctx) -> Result<Outcome, String> {
    asked(ctx).and_then(|asked| miri(ctx, &asked))
}

/// Runs the suite under miri; opt-in, since interpreting costs tens of times a normal run.
fn miri(ctx: &Ctx, asked: &[String]) -> Result<Outcome, String> {
    let args = invocation(&ctx.miri, asked);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let flags = miriflags(&ctx.miri);
    let out = exec::run_env("cargo", &argv, &ctx.root, &[("MIRIFLAGS", flags.as_str())])
        .map_err(|failed| advice(failed.hung(), &failed.to_string()))?;
    if let Some(why) = never_ran(&out.stderr) {
        return Err(why);
    }
    Ok(super::verdict(&out, &ctx.root))
}

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

/// The remedy for a failed run: a narrower scope or longer deadline if it hung, else install miri.
fn advice(hung: bool, said: &str) -> String {
    if hung {
        format!(
            "{said}; interpreting costs tens of times what running does, so name the packages \
             worth it in `miri.packages`, or raise {}",
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

/// Why miri never tested the code, if it did not: it is not installed, or it hit an operation it
/// cannot emulate.
fn never_ran(stderr: &str) -> Option<String> {
    if stderr.contains("'cargo-miri' is not installed") {
        return Some(
            "miri is not installed on the nightly toolchain — \
             `rustup +nightly component add miri`"
                .to_string(),
        );
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
                 `\"miri\": {\"packages\": [\"a\", \"b\"]}`, or `\"exclude\"` the ones it cannot."
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
    fn a_deadline_reached_is_told_apart_from_a_component_that_is_not_installed() {
        let said = advice(true, "cargo gave up waiting");
        assert!(said.contains("miri.packages"), "{said}");
        assert!(said.contains(exec::TIMEOUT), "{said}");
        assert!(!said.contains("component add"), "{said}");

        let said = advice(false, "cargo could not start");
        assert!(said.contains("component add miri"), "{said}");
        assert!(!said.contains(exec::TIMEOUT), "{said}");
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
    fn a_missing_miri_component_names_the_command_that_installs_it() {
        let said = "error: 'cargo-miri' is not installed for the toolchain 'nightly'.\n";
        assert_eq!(
            never_ran(said),
            Some(
                "miri is not installed on the nightly toolchain — \
                 `rustup +nightly component add miri`"
                    .to_string()
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
