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

#[allow(
    clippy::panic,
    clippy::unwrap_used,
    reason = "the panic is the subject: only a panicking thread reaches the branch under test"
)]
mod tests {
    use super::*;

    #[test]
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
