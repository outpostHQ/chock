//! Which `cargo-mutest` this machine has. Outpost's fork and upstream both print version `0.0.0`,
//! so the test is a flag that only the fork's `run` takes.

use crate::exec;
use std::path::Path;

/// Where `chock init --global` gets the tool: the newest commit of `main`.
pub const FORK: &str = "https://github.com/outpostHQ/mutest-rs";

/// The flag chock passes on Linux; upstream's `run` and an old build of the fork refuse it.
const MARK: &str = "--require-progress";

/// The reason for a tool that is some other build; `fixes::repair` reads it back.
pub const FOREIGN: &str = "this machine's `cargo-mutest` is not a current build of Outpost's fork: its `run` does not take `--require-progress`";

/// What repairs a missing or foreign tool.
pub const REPAIR: &str = "run `chock init --global`: it builds `cargo-mutest` from the newest commit of `main` at https://github.com/outpostHQ/mutest-rs";

/// What answers `cargo mutest` here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// A build of the fork that takes every flag chock passes.
    Usable,
    /// Upstream, or a build of the fork from before the flag.
    Foreign,
    Absent,
}

/// Asks the tool for the help of its `run`. An error is a cargo that could not answer.
pub fn found(root: &Path) -> Result<Tool, String> {
    asked(&exec::run_env, root)
}

/// `found` through `start`, so an install can ask before and after it builds.
pub fn asked(start: exec::Start, root: &Path) -> Result<Tool, String> {
    let help =
        start("cargo", &["mutest", "run", "--help"], root, &[]).map_err(|e| e.to_string())?;
    told(&help)
}

fn told(help: &exec::Output) -> Result<Tool, String> {
    match (help.success(), help.stdout.contains(MARK)) {
        (true, true) => Ok(Tool::Usable),
        (true, false) => Ok(Tool::Foreign),
        (false, _) if help.stderr.contains("no such command") => Ok(Tool::Absent),
        (false, _) => Err(format!(
            "`cargo mutest run --help` failed: {}",
            help.why_it_failed()
        )),
    }
}

/// `Ok` for a tool the gate can drive, else the reason it cannot run.
pub fn usable(root: &Path) -> Result<(), String> {
    found(root).and_then(refusal)
}

/// Why the gate cannot drive this tool; `Ok` for one it can.
pub fn refusal(tool: Tool) -> Result<(), String> {
    match tool {
        Tool::Usable => Ok(()),
        Tool::Foreign => Err(FOREIGN.to_string()),
        Tool::Absent => Err(crate::gates::fixes::not_installed("cargo-mutest")),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn said(code: i32, stdout: &str, stderr: &str) -> exec::Output {
        exec::Output::of(Some(code), stdout, stderr)
    }

    #[test]
    fn the_flag_in_the_help_of_run_tells_the_fork_from_any_other_build() {
        let fork = "Options:\n      --require-progress  Fail when no progress record is written\n";
        assert_eq!(told(&said(0, fork, "")), Ok(Tool::Usable));
        let upstream = "Options:\n      --isolate <MODE>\n";
        assert_eq!(told(&said(0, upstream, "")), Ok(Tool::Foreign));
        // The flag in an error is not the tool saying it takes it.
        let refused = "error: unexpected argument '--require-progress' found";
        assert_eq!(told(&said(0, "", refused)), Ok(Tool::Foreign));
    }

    #[test]
    fn a_cargo_with_no_such_command_is_an_absent_tool_and_any_other_failure_is_an_error() {
        let absent = "error: no such command: `mutest`\n\nhelp: view all installed commands with `cargo --list`\n";
        assert_eq!(told(&said(101, "", absent)), Ok(Tool::Absent));
        assert_eq!(
            told(&said(101, MARK, "error: the driver crashed\n")),
            Err("`cargo mutest run --help` failed: error: the driver crashed".to_string())
        );
    }

    #[test]
    fn only_a_usable_tool_lets_the_gate_run_and_each_refusal_names_its_repair() {
        assert_eq!(refusal(Tool::Usable), Ok(()));
        assert_eq!(refusal(Tool::Foreign), Err(FOREIGN.to_string()));
        assert_eq!(
            refusal(Tool::Absent),
            Err("`cargo-mutest` is not installed, so this gate has nothing to run".to_string())
        );
        for reason in [refusal(Tool::Foreign), refusal(Tool::Absent)] {
            let reason = reason.unwrap_err();
            assert_eq!(crate::gates::fixes::repair(&reason), Some(REPAIR));
        }
        assert!(
            REPAIR.contains(FORK),
            "the repair names where the tool comes from"
        );
    }

    #[test]
    fn the_tool_is_asked_for_the_help_of_its_run_in_the_project() {
        let start: exec::Start = &|program, args, cwd, env| {
            assert_eq!((program, args), ("cargo", &["mutest", "run", "--help"][..]));
            assert_eq!((cwd, env.len()), (Path::new("/project"), 0));
            Ok(said(0, MARK, ""))
        };
        assert_eq!(asked(start, Path::new("/project")), Ok(Tool::Usable));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_cargo_that_cannot_start_is_an_error_and_not_an_absent_tool() {
        // A file as the root, so the spawn itself fails.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let error = usable(&root).unwrap_err();
        assert!(error.starts_with("could not start cargo: "), "{error}");
    }
}
