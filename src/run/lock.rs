//! One chock run per tree at a time. Overlapping runs share `lcov.info` and the suite, so neither
//! verdict would hold.

use std::path::{Path, PathBuf};

/// The lock file, in chock's per-tree directory. `DIR` is spelled out so making it needs no
/// `parent()` branch.
const DIR: &str = ".chock";
const FILE: &str = ".chock/run.lock";

/// The held run lock, released when it goes out of scope, error paths included.
#[derive(Debug)]
pub struct Held {
    path: PathBuf,
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Attempts and the rest between them, ten seconds in all: long enough for the fast gates to
/// finish, not for a push.
const TURNS: u32 = 50;
const BETWEEN: std::time::Duration = std::time::Duration::from_millis(200);

/// Takes the tree's run lock, waiting briefly for a running holder; `Err` says who holds it.
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

/// One attempt to take the lock, taking over a stale one.
fn once(root: &Path) -> Result<Held, Refused> {
    let dir = root.join(DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| Refused::Broken(format!("cannot make {}: {e}", dir.display())))?;
    let path = root.join(FILE);
    match write_new(&path) {
        Ok(()) => Ok(Held { path }),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match stale(&path) {
            true => {
                // Ignored: a lock that cannot be removed makes the write below fail, which says so.
                let _ = std::fs::remove_file(&path);
                write_new(&path)
                    .map(|()| Held { path })
                    .map_err(|e| Refused::Broken(format!("cannot take {FILE}: {e}")))
            }
            false => Err(Refused::Held(held_by(&path))),
        },
        Err(e) => Err(Refused::Broken(format!("cannot take {FILE}: {e}"))),
    }
}

/// Creates the lock holding this process's pid. `create_new` means two racing processes cannot
/// both succeed.
fn write_new(path: &Path) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    write!(file, "{}", std::process::id())
}

/// Whether no live run holds the lock: its writer has exited, or it outlived a run's deadline.
fn stale(path: &Path) -> bool {
    let who = std::fs::read_to_string(path).unwrap_or_default();
    if gone(who.trim()) == Some(true) {
        return true;
    }
    let Ok(held) = std::fs::metadata(path).and_then(|about| about.modified()) else {
        return false;
    };
    held.elapsed()
        .is_ok_and(|age| age > crate::exec::deadline())
}

/// Whether the lock's writer has exited, or `None` if that cannot be known. Only ESRCH counts.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn gone(pid: &str) -> Option<bool> {
    let raw = i32::try_from(pid.parse::<u32>().ok()?).ok()?;
    let pid = rustix::process::Pid::from_raw(raw)?;
    Some(rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH))
}

/// Windows has no `kill(pid, 0)` chock can reach without `unsafe`, so only the lock's age counts.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn gone(_pid: &str) -> Option<bool> {
    None
}

fn held_by(path: &Path) -> String {
    let who = std::fs::read_to_string(path).unwrap_or_default();
    let who = who.trim();
    let named = if who.is_empty() {
        String::new()
    } else {
        format!(" (pid {who})")
    };
    format!(
        "another chock run holds this tree{named}; wait for it, or delete {FILE} if nothing is running"
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn a_run_that_lets_go_while_we_wait_hands_the_tree_over_rather_than_refusing() {
        let dir = crate::testdir::make("lock-waited");
        let first = std::cell::RefCell::new(Some(once(&dir).unwrap()));
        let held = trying(&dir, 4, &|_| {
            first.borrow_mut().take();
        })
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE)).unwrap().trim(),
            std::process::id().to_string()
        );
        drop(held);
    }

    #[test]
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
    fn a_second_run_in_the_same_tree_is_refused_while_the_first_holds_it() {
        let dir = crate::testdir::make("lock-contended");
        let first = once(&dir).unwrap();
        let refused = once(&dir).unwrap_err().why();
        assert!(
            refused.starts_with("another chock run holds this tree"),
            "{refused}"
        );
        assert!(
            refused.contains(&std::process::id().to_string()),
            "{refused}"
        );
        drop(first);
    }

    #[test]
    fn the_tree_is_free_again_once_the_holder_is_dropped() {
        let dir = crate::testdir::make("lock-released");
        drop(take(&dir).unwrap());
        assert!(!dir.join(FILE).exists());
        drop(take(&dir).unwrap());
    }

    #[test]
    fn a_lock_older_than_a_run_could_live_is_taken_over() {
        let dir = crate::testdir::make("lock-stale");
        let path = dir.join(FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "999999").unwrap();
        let long_ago = std::time::SystemTime::now() - crate::exec::deadline() * 2;
        set_modified(&path, long_ago);
        assert!(stale(&path), "the fixture is not old enough to be stale");
        let taken = take(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::process::id().to_string()
        );
        drop(taken);
    }

    #[test]
    fn a_lock_written_just_now_is_not_stale() {
        let dir = crate::testdir::make("lock-fresh");
        let held = take(&dir).unwrap();
        assert!(!stale(&dir.join(FILE)));
        drop(held);
    }

    #[test]
    fn a_lock_naming_no_pid_still_refuses_the_tree() {
        let dir = crate::testdir::make("lock-anonymous");
        let path = dir.join(FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        let refused = once(&dir).unwrap_err().why();
        assert_eq!(
            refused,
            format!(
                "another chock run holds this tree; wait for it, or delete {FILE} if nothing is running"
            )
        );
    }

    /// Only `AlreadyExists` means another run holds the lock.
    #[cfg(unix)]
    #[test]
    fn a_refusal_that_is_not_contention_is_reported_as_itself() {
        let dir = crate::testdir::make("lock-unwritable");
        let inside = dir.join(".chock");
        std::fs::create_dir_all(&inside).unwrap();
        read_only(&inside, true);
        let refused = take(&dir).unwrap_err(); // broken, so it returns without waiting
        read_only(&inside, false);
        assert!(
            refused.starts_with(&format!("cannot take {FILE}")),
            "{refused}"
        );
        assert!(!refused.contains("another chock run"), "{refused}");
    }

    #[test]
    fn a_tree_with_no_room_for_the_lock_says_which_path_is_in_the_way() {
        let dir = crate::testdir::make("lock-no-room");
        std::fs::write(dir.join(".chock"), "not a directory").unwrap();
        let refused = take(&dir).unwrap_err(); // broken, so it returns without waiting
        assert!(refused.starts_with("cannot make "), "{refused}");
        assert!(refused.contains(".chock"), "{refused}");
    }

    /// A missing file has no age, and guessing "old" would hand one run's lock to another.
    #[test]
    fn a_lock_that_is_not_there_is_not_stale() {
        let dir = crate::testdir::make("lock-absent");
        assert!(!stale(&dir.join(FILE)));
    }

    /// A killed run never reaches `Drop`, so its lock outlives it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_lock_whose_holder_has_exited_is_taken_over_without_waiting() {
        let dir = crate::testdir::make("lock-dead-holder");
        let path = dir.join(FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, exited_pid()).unwrap();
        assert!(stale(&path), "a pid nothing is running is not held");
        let taken = take(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::process::id().to_string()
        );
        drop(taken);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_lock_whose_holder_is_this_very_process_is_not_stale() {
        assert_eq!(gone(&std::process::id().to_string()), Some(false));
    }

    #[test]
    fn a_pid_that_is_not_a_number_tells_us_nothing_either_way() {
        assert_eq!(gone(""), None);
        assert_eq!(gone("not-a-pid"), None);
    }

    /// A pid no process can have: the kernel's maximum plus one.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn exited_pid() -> String {
        let max = std::fs::read_to_string("/proc/sys/kernel/pid_max").unwrap_or_default();
        let max: u64 = max.trim().parse().unwrap_or(4_194_304);
        (max + 1).to_string()
    }

    #[cfg(unix)]
    fn read_only(dir: &Path, yes: bool) {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = if yes { 0o555 } else { 0o755 };
        let mut perms = std::fs::metadata(dir).unwrap().permissions();
        perms.set_mode(mode);
        std::fs::set_permissions(dir, perms).unwrap();
    }

    fn set_modified(path: &Path, to: std::time::SystemTime) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(to).unwrap();
    }
}
