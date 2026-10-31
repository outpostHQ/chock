//! One chock run per tree at a time. Overlapping runs share `lcov.info` and the suite, so neither
//! verdict would hold. The system lets go of the lock when its run exits, a crash included.

use std::path::Path;

/// The lock file, in chock's per-tree directory. `DIR` is spelled out so making it needs no
/// `parent()` branch.
const DIR: &str = ".chock";
const FILE: &str = ".chock/run.lock";

/// The held run lock, released when it goes out of scope, error paths included.
#[derive(Debug)]
pub struct Held {
    file: std::fs::File,
}

impl Drop for Held {
    fn drop(&mut self) {
        // Closing the file lets go too; unlocking first frees the tree at once on every system.
        self.file.unlock().unwrap_or_default();
    }
}

/// Attempts and the rest between them, ten seconds in all: long enough for the fast gates to
/// finish, not for a push.
const TURNS: u32 = 50;
const BETWEEN: std::time::Duration = std::time::Duration::from_millis(200);

/// Takes the tree's run lock, waiting briefly for a running holder.
/// # Errors
/// The text says who holds the lock, or why the lock file could not be used.
pub fn take(root: &Path) -> Result<Held, String> {
    trying(root, TURNS, &std::thread::sleep)
}

/// Why an attempt failed. Only `Held` is worth waiting through; `Broken` will not change.
#[derive(Debug)]
enum Refused {
    Held(String),
    Broken(String),
}

impl Refused {
    fn why(self) -> String {
        match self {
            Self::Held(why) | Self::Broken(why) => why,
        }
    }
}

/// `take` with the attempts and the rest injected, so a test can run out of turns without sleeping.
fn trying(root: &Path, turns: u32, rest: &dyn Fn(std::time::Duration)) -> Result<Held, String> {
    let mut last = once(root);
    for _ in 1..turns {
        match last {
            Ok(held) => return Ok(held),
            Err(Refused::Broken(why)) => return Err(why),
            Err(Refused::Held(_)) => {
                rest(BETWEEN);
                last = once(root);
            }
        }
    }
    last.map_err(Refused::why)
}

/// One attempt to take the lock. The file is never removed, or two runs could lock two files,
/// and never cut here, or a run that finds the tree held would erase the holder's name.
fn once(root: &Path) -> Result<Held, Refused> {
    let dir = root.join(DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| Refused::Broken(format!("cannot make {}: {e}", dir.display())))?;
    let path = root.join(FILE);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| refused(&e, &path))?;
    file.try_lock()
        .map_err(std::io::Error::from)
        .map_err(|e| refused(&e, &path))?;
    named(file).map_err(|e| refused(&e, &path))
}

/// Only `WouldBlock` means another run holds the lock; any other error will not change by waiting.
fn refused(e: &std::io::Error, path: &Path) -> Refused {
    match e.kind() {
        std::io::ErrorKind::WouldBlock => Refused::Held(held_by(path)),
        _ => Refused::Broken(format!("cannot take {FILE}: {e}")),
    }
}

/// Writes this process's pid, then its command, into the lock it holds. The file can hold a
/// longer name from a run that ended, so it is emptied first.
fn named(mut file: std::fs::File) -> std::io::Result<Held> {
    use std::io::Write as _;
    file.set_len(0)?;
    write!(file, "{}\n{}", std::process::id(), command())?;
    Ok(Held { file })
}

/// This process's command on one line, so a run it holds back can say which run is in the way.
fn command() -> String {
    let words: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|word| word.to_string_lossy().replace('\n', " "))
        .collect();
    format!("chock {}", words.join(" ")).trim_end().to_string()
}

/// The pid and the command a lock names; a lock an older chock wrote names only the pid.
fn holder(text: &str) -> (&str, &str) {
    let (pid, command) = text.split_once('\n').unwrap_or((text, ""));
    (pid.trim(), command.trim())
}

/// How long ago the lock was written, or `None` if its time cannot be read.
fn age(path: &Path) -> Option<std::time::Duration> {
    let written = std::fs::metadata(path).and_then(|about| about.modified());
    written.ok()?.elapsed().ok()
}

/// Windows keeps every other handle from reading a locked file, so there the refusal names only
/// how long the holder has held the tree.
fn held_by(path: &Path) -> String {
    let who = std::fs::read_to_string(path).unwrap_or_default();
    let (pid, command) = holder(&who);
    told(pid, command, age(path))
}

/// The refusal. A held lock always has a live holder, so there is nothing to delete.
fn told(pid: &str, command: &str, age: Option<std::time::Duration>) -> String {
    let mut named: Vec<String> = Vec::new();
    named.extend((!pid.is_empty()).then(|| format!("pid {pid}")));
    named.extend((!command.is_empty()).then(|| format!("`{command}`")));
    named.extend(age.map(|age| match age.as_secs() {
        seconds @ 0..60 => format!("for {seconds} s"),
        seconds => format!("for {} min", seconds / 60),
    }));
    let named = if named.is_empty() {
        String::new()
    } else {
        format!(" ({})", named.join(", "))
    };
    format!("another chock run holds this tree{named} and is still running; run again when it ends")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_run_that_lets_go_while_we_wait_hands_the_tree_over_rather_than_refusing() {
        let dir = crate::testdir::make("lock-waited");
        let first = std::cell::RefCell::new(Some(once(&dir).unwrap()));
        let held = trying(&dir, 4, &|_| {
            first.borrow_mut().take();
        })
        .unwrap();
        drop(held);
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert_eq!(holder(&text).0, std::process::id().to_string());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_holder_that_never_lets_go_is_refused_once_the_turns_run_out() {
        let dir = crate::testdir::make("lock-impatient");
        let first = once(&dir).unwrap();
        let refused = trying(&dir, 3, &|_| {}).unwrap_err();
        assert!(
            refused.starts_with("another chock run holds this tree"),
            "{refused}"
        );
        drop(first);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_second_run_in_the_same_tree_is_refused_while_the_first_holds_it() {
        let dir = crate::testdir::make("lock-contended");
        let first = once(&dir).unwrap();
        let refused = once(&dir).unwrap_err().why();
        assert!(
            refused.starts_with("another chock run holds this tree"),
            "{refused}"
        );
        assert!(
            refused.contains("is still running") && !refused.contains("delete"),
            "{refused}"
        );
        // Windows keeps the second handle from reading the file the first one locked.
        #[cfg(not(windows))]
        assert!(
            refused.contains(&std::process::id().to_string()),
            "{refused}"
        );
        drop(first);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_tree_is_free_again_once_the_holder_is_dropped() {
        let dir = crate::testdir::make("lock-released");
        drop(take(&dir).unwrap());
        assert!(dir.join(FILE).exists(), "the lock file stays");
        drop(take(&dir).unwrap());
    }

    /// A killed run never reaches `Drop`, so its file stays, naming it.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_lock_file_left_by_a_run_that_ended_does_not_hold_the_tree() {
        let dir = crate::testdir::make("lock-left");
        let path = dir.join(FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("999999\nchock {}", "run ".repeat(1024))).unwrap();
        drop(once(&dir).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n{}", std::process::id(), command()),
            "the name of the run that ended is gone, all of it"
        );
    }

    #[test]
    fn a_refusal_names_the_holder_and_how_long_it_has_held_the_tree() {
        let twelve = Some(std::time::Duration::from_secs(12 * 60 + 5));
        assert_eq!(
            told("41", "chock pre-push", twelve),
            "another chock run holds this tree (pid 41, `chock pre-push`, for 12 min) and is \
             still running; run again when it ends"
        );
        assert_eq!(
            told("41", "", Some(std::time::Duration::from_secs(9))),
            "another chock run holds this tree (pid 41, for 9 s) and is still running; run again \
             when it ends"
        );
        assert_eq!(
            told("", "", Some(std::time::Duration::from_secs(60))),
            "another chock run holds this tree (for 1 min) and is still running; run again when it \
             ends"
        );
        assert_eq!(
            told("", "", None),
            "another chock run holds this tree and is still running; run again when it ends"
        );
        assert_eq!(holder(" 41 \nchock pre-push\n"), ("41", "chock pre-push"));
        assert_eq!(holder("41"), ("41", ""), "a lock an older chock wrote");
        assert!(command().starts_with("chock"), "{}", command());
    }

    /// Only `WouldBlock` means another run holds the lock. A directory at the lock's path cannot
    /// be opened for writing on any system, by any user.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_refusal_that_is_not_contention_is_reported_as_itself() {
        let dir = crate::testdir::make("lock-unwritable");
        std::fs::create_dir_all(dir.join(FILE)).unwrap();
        let refused = take(&dir).unwrap_err(); // broken, so it returns without waiting
        assert!(
            refused.starts_with(&format!("cannot take {FILE}")),
            "{refused}"
        );
        assert!(!refused.contains("another chock run"), "{refused}");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_with_no_room_for_the_lock_says_which_path_is_in_the_way() {
        let dir = crate::testdir::make("lock-no-room");
        std::fs::write(dir.join(".chock"), "not a directory").unwrap();
        let refused = take(&dir).unwrap_err(); // broken, so it returns without waiting
        assert!(refused.starts_with("cannot make "), "{refused}");
        assert!(refused.contains(".chock"), "{refused}");
    }
}
