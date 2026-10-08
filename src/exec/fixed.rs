//! One command with a stdin of its own, the environment its caller chose, and a short limit.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::{DRAIN, ExecError, MAX_CAPTURE, Output, Process, Stage, captured, finish_capture};

/// Runs `command` to its end, or kills its whole process group once `limit` passes.
pub fn run(
    command: &mut Command,
    program: &str,
    stdin: &str,
    limit: Duration,
) -> Result<Output, ExecError> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = Process::spawn(command).map_err(|e| ExecError {
        program: program.to_string(),
        stage: Stage::Spawn,
        reason: e.to_string(),
    })?;
    if let Some(mut pipe) = process.child.stdin.take() {
        let text = stdin.to_string();
        // On its own thread: a child that never reads would block this one once the pipe fills.
        std::thread::spawn(move || {
            // outpost: ignore[discarded-result] a child that stops reading answers by its exit.
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let out = captured(process.child.stdout.take(), MAX_CAPTURE);
    let err = captured(process.child.stderr.take(), MAX_CAPTURE);
    finish_capture(&mut process, program, &out, &err, limit, DRAIN, &mut ())
}

#[cfg(test)]
#[cfg(unix)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_command_reads_the_stdin_it_was_given_and_reports_both_streams() {
        let out = run(
            &mut shell("cat; echo problem >&2; exit 3"),
            "sh",
            "typed\n",
            Duration::from_secs(20),
        )
        .unwrap();
        assert_eq!(out.stdout, "typed\n");
        assert_eq!(out.stderr, "problem\n");
        assert_eq!(out.code, Some(3));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_command_past_its_limit_is_killed_and_reported_as_hung() {
        let started = std::time::Instant::now();
        let err = run(
            &mut shell("sleep 600"),
            "sh",
            "",
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(err.hung(), "{err}");
        assert_eq!(err.program, "sh");
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_program_that_does_not_exist_fails_at_the_start_and_is_named() {
        let mut command = Command::new("chock-no-such-program");
        let err = run(&mut command, "the old build", "", Duration::from_secs(5)).unwrap_err();
        assert_eq!(err.stage, Stage::Spawn);
        assert_eq!(err.program, "the old build");
    }
}
