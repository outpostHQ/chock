//! Gates over the proofs that stand beside the code: TLA+ models, which TLC and Apalache check,
//! Verus proofs and their mutants, and the links from a model back to the code it describes.

pub mod laws;
pub mod links;
pub(crate) mod patch;
pub mod tla;
pub mod verus;

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::exec;

/// Where a proof gate finds its tools: chock's tools directory, which holds each download pin's
/// file, and the command each tool starts behind, which is empty but in a test that fakes them.
#[derive(Debug, Clone, Default)]
pub struct Tools {
    pub home: Option<PathBuf>,
    pub launcher: Vec<String>,
}

impl Tools {
    /// The tools of this machine.
    #[must_use]
    pub fn here() -> Self {
        Self {
            home: crate::setup::downloads::home(),
            launcher: Vec::new(),
        }
    }

    /// The program that starts for `program` with `args`, and the arguments it starts with.
    fn command(&self, program: &str, args: &[&str]) -> (String, Vec<String>) {
        let launcher = self.launcher.iter().map(String::as_str);
        let all = launcher
            .chain(std::iter::once(program))
            .chain(args.iter().copied());
        let mut argv: Vec<String> = all.map(str::to_string).collect();
        let first = argv.remove(0);
        (first, argv)
    }

    /// `program` run once in `dir`, as `exec::run` runs it.
    pub fn run(
        &self,
        program: &str,
        args: &[&str],
        dir: &Path,
    ) -> Result<exec::Output, exec::ExecError> {
        let (first, argv) = self.command(program, args);
        exec::run(&first, &strs(&argv), dir)
    }
}

fn strs(argv: &[String]) -> Vec<&str> {
    argv.iter().map(String::as_str).collect()
}

/// One run of a proof tool: the directory it runs in, its arguments, and the seconds it may take.
pub(crate) struct Run {
    pub dir: PathBuf,
    pub argv: Vec<String>,
    pub limit: u64,
}

/// Each run of `program`, as many at once as `workers`; a run past its limit is stopped and comes
/// back hung. The results are in the order of `runs`.
pub(crate) fn run_all(
    tools: &Tools,
    program: &str,
    runs: &[Run],
    workers: usize,
) -> Vec<Result<exec::Output, exec::ExecError>> {
    let lanes: Vec<Vec<usize>> = runs.iter().enumerate().map(|(at, _)| vec![at]).collect();
    let one = |run: &Run| {
        let (first, argv) = tools.command(program, &strs(&run.argv));
        let pace: exec::Pace = (|_| false, Duration::from_secs(run.limit));
        exec::run_paced(&first, &strs(&argv), &run.dir, &[], pace)
    };
    let outs = crate::run::workers::on_workers(runs, &lanes, workers.max(1), &one);
    outs.into_iter().flatten().collect()
}

/// Makes a scratch directory of chock's own. A relative path would put it in the directory that
/// chock runs in, so chock refuses one.
pub(crate) fn made(scratch: &Path) -> std::io::Result<()> {
    crate::project::document::rooted(scratch).and_then(|()| std::fs::create_dir_all(scratch))
}

/// Removes a scratch directory of chock's own; one that is not there is already clear.
pub(crate) fn cleared(scratch: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(scratch) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("cannot remove {}: {e}", scratch.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;

    fn run(dir: &Path, script: &str, limit: u64) -> Run {
        let argv = ["-c", script].map(str::to_string).to_vec();
        Run {
            dir: dir.to_path_buf(),
            argv,
            limit,
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn runs_come_back_in_their_order_behind_the_launcher_and_one_past_its_limit_hangs() {
        let dir = crate::testdir::tree("proofs-ran", &[("a/keep", "")]);
        let runs = [
            run(&dir.join("a"), "ls", 30),
            run(&dir, "echo two; exit 3", 30),
            run(&dir, "sleep 5", 1),
        ];
        let outs = run_all(&Tools::default(), "sh", &runs, 0);
        assert_eq!(outs[0].as_ref().unwrap().stdout.trim(), "keep");
        let second = outs[1].as_ref().unwrap();
        assert_eq!((second.stdout.trim(), second.code), ("two", Some(3)));
        assert!(outs[2].as_ref().unwrap_err().hung());
        let behind = Tools {
            home: None,
            launcher: vec!["sh".into(), "-c".into(), "echo \"$0 $*\"".into()],
        };
        let said = behind.run("java", &["-version"], &dir).unwrap();
        assert_eq!(said.stdout.trim(), "java -version");
        let alone = Tools::here()
            .run("sh", &["-c", "echo alone"], &dir)
            .unwrap();
        assert_eq!(alone.stdout.trim(), "alone");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_missing_scratch_is_clear_and_a_present_one_is_removed() {
        let dir = crate::testdir::tree("proofs-cleared", &[("scratch/a/b", "x")]);
        let scratch = dir.join("scratch");
        assert_eq!(cleared(&scratch), Ok(()));
        assert!(!scratch.exists());
        assert_eq!(cleared(&scratch), Ok(()));
        let file = dir.join("file");
        std::fs::write(&file, "").unwrap();
        assert!(cleared(&file).unwrap_err().starts_with("cannot remove "));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_scratch_is_made_only_at_an_absolute_path() {
        let dir = crate::testdir::tree("proofs-made", &[]);
        let at = dir.join("a").join("b");
        made(&at).unwrap();
        assert!(at.is_dir());
        // The relative path is inside `target/`, so a check that fails writes nowhere else.
        let relative = Path::new("target/test-scratch/chock-made");
        let why = made(relative).unwrap_err().to_string();
        assert_eq!(
            why,
            "target/test-scratch/chock-made is not under a project root"
        );
        assert!(!relative.exists());
    }
}
