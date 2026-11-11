//! Where a tool chock downloads for itself finds its command, and the scratch directories chock
//! makes and clears for its own use.

use std::path::{Path, PathBuf};

use crate::exec;

/// Where a proof gate finds its tools: chock's tools directory, which holds each download pin's
/// file, and the command each tool starts behind, which is empty but in a test that fakes them.
#[derive(Debug, Clone, Default)]
pub struct Tools {
    /// chock's tools directory, which holds each download pin's file; `None` when it has none.
    pub home: Option<PathBuf>,
    /// The command each tool starts behind; empty but in a test that fakes the tools.
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
        let mut words: Vec<String> = all.map(str::to_string).collect();
        let first = words.remove(0);
        (first, words)
    }

    /// `program` run once in `dir`, as `exec::run` runs it.
    /// # Errors
    /// A program that could not run, with the step at which it stopped.
    pub fn run(
        &self,
        program: &str,
        args: &[&str],
        dir: &Path,
    ) -> Result<exec::Output, exec::ExecError> {
        let (first, rest) = self.command(program, args);
        exec::run(&first, &strs(&rest), dir)
    }
}

fn strs(argv: &[String]) -> Vec<&str> {
    argv.iter().map(String::as_str).collect()
}

/// Makes a scratch directory of chock's own. A relative path would put it in the directory that
/// chock runs in, so chock refuses one.
pub(crate) fn made(scratch: &Path) -> std::io::Result<()> {
    crate::project::document::rooted(scratch).and_then(|()| std::fs::create_dir_all(scratch))
}

/// Removes a scratch directory of chock's own; one that is not there is already clear. A relative
/// path is refused as `made` refuses it: it names what the directory chock runs in holds.
pub(crate) fn cleared(scratch: &Path) -> Result<(), String> {
    match crate::project::document::rooted(scratch).and_then(|()| std::fs::remove_dir_all(scratch))
    {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("cannot remove {}: {e}", scratch.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_tool_runs_behind_its_launcher_and_a_bare_one_runs_alone() {
        let dir = crate::testdir::tree("proofs-ran", &[]);
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

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_scratch_is_removed_only_at_an_absolute_path() {
        let dir = crate::testdir::tree("proofs-cleared", &[("held/file", "x")]);
        cleared(&dir.join("held")).unwrap();
        assert!(!dir.join("held").exists());
        assert_eq!(
            cleared(&dir.join("held")),
            Ok(()),
            "a missing scratch is clear"
        );
        // An emptied root makes this path, and cargo runs a test in the real project's root.
        let why = cleared(Path::new("target/test-scratch/chock-cleared")).unwrap_err();
        assert_eq!(
            why,
            "cannot remove target/test-scratch/chock-cleared: target/test-scratch/chock-cleared \
             is not under a project root"
        );
    }
}
