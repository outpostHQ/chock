//! The commands `chock init --global` runs that download: rustup and git, tried again when the
//! network rather than the request fails them.

use crate::exec::{ExecError, Output, Stage, Start, Starter};
use std::time::Duration;

/// The waits before each new try: a link that drops for a moment is back within them.
pub const PAUSES: [Duration; 2] = [Duration::from_secs(10), Duration::from_secs(30)];

/// What rustup, git and cargo print, in lower case, when a connection failed.
const OFFLINE: [&str; 6] = [
    "timed out",
    "error sending request",
    "could not resolve",
    "failed to connect",
    "connection reset",
    "network is unreachable",
];

/// `start`, but a run the network failed is tried again after each of `PAUSES`; one the network
/// fails each time is an `Offline` error that says what to check.
pub fn patiently<'a>(start: Start<'a>, wait: &'a dyn Fn(Duration)) -> Box<Starter<'a>> {
    Box::new(move |program, args, cwd, env| {
        let mut out = start(program, args, cwd, env)?;
        for (tried, pause) in (2..).zip(PAUSES) {
            if !offline(&out) {
                return Ok(out);
            }
            println!(
                "  waiting   `{program}` could not reach the network; try {tried} of {} in {} s",
                PAUSES.len() + 1,
                pause.as_secs()
            );
            wait(pause);
            out = start(program, args, cwd, env)?;
        }
        match offline(&out) {
            false => Ok(out),
            true => Err(ExecError {
                program: program.to_string(),
                stage: Stage::Offline,
                reason: format!(
                    "`{program} {}` failed {} times: {}; check the connection, a VPN or a proxy, \
                     then run `chock init --global` again, which keeps what is installed",
                    args.join(" "),
                    PAUSES.len() + 1,
                    out.why_it_failed()
                ),
            }),
        }
    })
}

/// Whether a run failed and says that a connection failed.
fn offline(out: &Output) -> bool {
    let said = out.stderr.to_ascii_lowercase();
    !out.success() && OFFLINE.iter().any(|text| said.contains(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::Path;

    const TIMED_OUT: &str = "error: could not download file from 'https://static.rust-lang.org/\
        dist/2026-10-03/channel-rust-nightly.toml': error sending request: connection error: \
        timed out\n";

    fn output(code: i32, stderr: &str) -> Output {
        Output {
            code: Some(code),
            stdout: String::new(),
            stderr: stderr.to_string(),
            truncated: false,
        }
    }

    /// Runs `patiently` against answers given in turn, and returns its result, the pauses it
    /// waited and how many runs it started.
    fn tried(
        answers: &[Result<Output, ExecError>],
    ) -> (Result<Output, ExecError>, Vec<u64>, usize) {
        let runs = RefCell::new(0);
        let waited = RefCell::new(Vec::new());
        let start = |_: &str, _: &[&str], _: &Path, _: &[(&str, &str)]| {
            let at = runs.replace_with(|n| *n + 1);
            answers[at].clone()
        };
        let wait = |pause: Duration| waited.borrow_mut().push(pause.as_secs());
        let args = ["toolchain", "install", "nightly-2026-10-03"];
        let done = patiently(&start, &wait)("rustup", &args, Path::new("."), &[]);
        (done, waited.into_inner(), runs.into_inner())
    }

    #[test]
    fn a_run_that_passes_or_fails_for_another_reason_is_started_once() {
        let passed = output(0, "");
        assert_eq!(tried(&[Ok(passed.clone())]), (Ok(passed), vec![], 1));
        let refused = output(
            1,
            "error: toolchain 'nightly-1900-01-01' is not installable\n",
        );
        assert_eq!(tried(&[Ok(refused.clone())]), (Ok(refused), vec![], 1));
    }

    #[test]
    fn a_run_the_network_fails_is_tried_again_after_each_pause_until_it_passes() {
        let passed = output(0, "");
        let answers = [
            Ok(output(1, TIMED_OUT)),
            Ok(output(1, TIMED_OUT)),
            Ok(passed.clone()),
        ];
        assert_eq!(tried(&answers), (Ok(passed), vec![10, 30], 3));
    }

    #[test]
    fn a_run_the_network_fails_each_time_says_so_and_what_to_check() {
        let answers = [
            Ok(output(1, TIMED_OUT)),
            Ok(output(
                1,
                "fatal: unable to access: Could not resolve host: github.com\n",
            )),
            Ok(output(128, TIMED_OUT)),
        ];
        let reason = format!(
            "`rustup toolchain install nightly-2026-10-03` failed 3 times: {}; check the \
             connection, a VPN or a proxy, then run `chock init --global` again, which keeps what \
             is installed",
            TIMED_OUT.trim_end()
        );
        let failed = ExecError {
            program: "rustup".to_string(),
            stage: Stage::Offline,
            reason,
        };
        assert_eq!(tried(&answers), (Err(failed.clone()), vec![10, 30], 3));
        assert_eq!(
            failed.to_string(),
            format!("could not reach the network for rustup: {}", failed.reason)
        );
    }

    #[test]
    fn a_run_that_cannot_start_is_not_tried_again() {
        let missing = ExecError {
            program: "rustup".to_string(),
            stage: Stage::Spawn,
            reason: "not found".to_string(),
        };
        assert_eq!(
            tried(&[Err(missing.clone())]),
            (Err(missing.clone()), vec![], 1)
        );
        let answers = [Ok(output(1, TIMED_OUT)), Err(missing.clone())];
        assert_eq!(tried(&answers), (Err(missing), vec![10], 2));
    }
}
