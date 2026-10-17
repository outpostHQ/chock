//! Test scratch directories under the crate's `target/`, so even a mutated test's stray write
//! lands where cargo cleans up.
#![cfg(test)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A scratch directory that removes itself on drop, unless its test panicked.
pub struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A failed test's directory is its evidence, so it stays.
        if std::thread::panicking() {
            return;
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A `Ctx` that owns the scratch directory it points at, so the directory lives as long as it does.
pub struct Held {
    ctx: crate::run::Ctx,
    _dir: Scratch,
}

impl Held {
    #[must_use]
    pub fn new(dir: Scratch, ctx: crate::run::Ctx) -> Self {
        Self { ctx, _dir: dir }
    }

    /// `tree` under a `Ctx` whose baseline holds nothing.
    #[must_use]
    pub fn tree(name: &str, files: &[(&str, &str)]) -> Self {
        let dir = tree(name, files);
        let baseline = crate::run::baseline::Baseline::empty("0.1.0");
        let ctx = crate::run::Ctx::for_root(dir.to_path_buf(), baseline);
        Self::new(dir, ctx)
    }
}

impl std::ops::Deref for Held {
    type Target = crate::run::Ctx;

    fn deref(&self) -> &Self::Target {
        &self.ctx
    }
}

impl std::ops::DerefMut for Held {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ctx
    }
}

/// A crate whose one source the parser rejects. The manifest matters: only a file some crate
/// compiles stops a gate.
pub const UNPARSABLE: [(&str, &str); 2] = [("Cargo.toml", ""), ("src/lib.rs", "fn broken( {\n")];

/// A fresh empty scratch directory, named for the caller so a leftover says who left it.
#[must_use]
pub fn make(name: &str) -> Scratch {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-scratch")
        .join(format!("{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // No panic here: the test's own write fails and names the file it wanted.
    let _ = std::fs::create_dir_all(&dir);
    Scratch(dir)
}

/// A scratch directory that holds `files`; a path that names directories gets them made.
#[must_use]
#[allow(
    clippy::unwrap_used,
    reason = "a fixture that was not written is the test failing"
)]
pub fn tree(name: &str, files: &[(&str, &str)]) -> Scratch {
    let dir = make(name);
    for (path, text) in files {
        let at = dir.join(path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, text).unwrap();
    }
    dir
}

/// A git repository with one commit that holds `files`.
#[must_use]
#[allow(
    clippy::unwrap_used,
    reason = "a repository that git did not make is the test failing"
)]
pub fn repo(name: &str, files: &[(&str, &str)]) -> Scratch {
    let root = tree(name, files);
    let identity = ["-c", "user.email=t@example.com", "-c", "user.name=t"];
    let commit = [&identity[..], &["commit", "--quiet", "-m", "first"]].concat();
    for args in [&["init", "--quiet"][..], &["add", "."], &commit] {
        let out = crate::exec::run("git", args, &root).unwrap();
        assert!(out.success(), "git {args:?}: {}", out.stderr);
    }
    root
}

/// A proof gate's tools in `dir`, with `sh` running `script` in place of each program; the script
/// reads the program as `$1`, and its arguments after it.
#[must_use]
#[allow(
    clippy::unwrap_used,
    reason = "a fake that was not written is the test failing"
)]
pub fn faked(dir: &Path, script: &str) -> crate::gates::proofs::Tools {
    let fake = dir.join("fake.sh");
    std::fs::write(&fake, script).unwrap();
    crate::gates::proofs::Tools {
        home: Some(dir.join("tools")),
        launcher: vec!["sh".into(), fake.display().to_string()],
    }
}

/// The pin lines of a download that holds `file`, fetched and stamped in the tools of `dir`.
#[must_use]
#[allow(
    clippy::unwrap_used,
    reason = "a download that was not made is the test failing"
)]
pub fn fetched(dir: &Path, stem: &str, file: &str) -> String {
    let sha = "b781b53bba32fac14ac2e02b0332e1dc2edc64cb7b850c0608d8142cc7aa2ef4";
    let pin = format!("{stem}_VERSION=1\n{stem}_URL=https://x/{file}\n{stem}_SHA256={sha}\n");
    let at = crate::setup::downloads::parse(&pin).unwrap()[0].dir(&dir.join("tools"), sha);
    std::fs::create_dir_all(&at).unwrap();
    std::fs::write(at.join(file), "").unwrap();
    std::fs::write(at.join(crate::setup::downloads::STAMP), sha).unwrap();
    pin
}

#[allow(
    clippy::panic,
    clippy::unwrap_used,
    reason = "the panic is the subject: only a panicking thread reaches the branch under test"
)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_directory_whose_test_panicked_is_left_where_it_can_be_read() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let inside = std::sync::Arc::clone(&seen);
        let died = std::thread::spawn(move || {
            let dir = make("testdir-panicking");
            *inside.lock().unwrap() = Some(dir.to_path_buf());
            panic!("on purpose, so the guard drops while the thread is panicking");
        })
        .join();
        assert!(died.is_err(), "the thread did not panic");
        let left = seen.lock().unwrap().clone().unwrap();
        // Bound here: a message argument only a failure evaluates is a line never covered.
        let shown = left.display().to_string();
        assert!(left.is_dir(), "the evidence was deleted: {shown}");
        std::fs::remove_dir_all(&left).unwrap();
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_directory_whose_test_passed_is_removed_with_the_guard() {
        let path = {
            let dir = make("testdir-passing");
            std::fs::write(dir.join("f"), "x").unwrap();
            dir.to_path_buf()
        };
        let shown = path.display().to_string();
        assert!(!path.exists(), "it was left behind: {shown}");
    }
}
