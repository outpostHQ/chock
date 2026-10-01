//! Records chock's verdict against the commit it judged, so a landing gate can ask for it later.

use std::path::Path;

use crate::exec;

/// The directory that marks an Outpost repository, the only kind of tree that records a verdict.
const REPOSITORY: &str = ".outpost";

const RECORDER: &str = "outpost";

const TOOL: &str = "chock";
const KIND: &str = "gate";

/// Records a run's exit code through `outpost`. Best effort: the verdict is already reported, so a
/// missing or refusing recorder changes nothing.
pub fn record(root: &Path, command: &str, code: u8) {
    let Some(argv) = argv(root.join(REPOSITORY).is_dir(), command, code) else {
        return;
    };
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let _ = exec::run(RECORDER, &args, root);
}

/// The `outpost evidence record` arguments for a run, or `None` outside an Outpost repository.
#[must_use]
pub fn argv(in_repository: bool, command: &str, code: u8) -> Option<Vec<String>> {
    in_repository.then(|| {
        vec![
            "evidence".to_string(),
            "record".to_string(),
            "--kind".to_string(),
            KIND.to_string(),
            "--tool".to_string(),
            TOOL.to_string(),
            "--command".to_string(),
            command.to_string(),
            "--exit-code".to_string(),
            code.to_string(),
        ]
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_that_keeps_no_commits_records_nothing() {
        assert_eq!(argv(false, "chock run", 0), None);
    }

    #[test]
    fn a_passing_run_is_recorded_with_the_command_that_produced_it() {
        assert_eq!(
            argv(true, "chock run slop", 0).unwrap(),
            [
                "evidence",
                "record",
                "--kind",
                "gate",
                "--tool",
                "chock",
                "--command",
                "chock run slop",
                "--exit-code",
                "0"
            ]
        );
    }

    /// The recorder reads any non-zero code as a failure.
    #[test]
    fn a_run_that_could_not_gate_is_recorded_as_a_failure_not_an_absence() {
        let argv = argv(true, "chock run", 2).unwrap();
        assert_eq!(argv.last().map(String::as_str), Some("2"));
    }
}
