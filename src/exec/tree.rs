//! The processes under a started command, read from `/proc` on Linux: for the memory they hold,
//! and to stop each one, also one in a process group of its own.

use std::collections::BTreeSet;

/// `root` and every process under it.
#[cfg(target_os = "linux")]
pub(super) fn tree(root: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let mut todo = vec![root];
    while let Some(pid) = todo.pop() {
        found.push(pid);
        todo.extend(children(pid));
    }
    found
}

/// The processes each thread of `pid` started.
#[cfg(target_os = "linux")]
fn children(pid: u32) -> Vec<u32> {
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task"))
        .into_iter()
        .flatten()
        .flatten();
    let lists = tasks.filter_map(|task| std::fs::read_to_string(task.path().join("children")).ok());
    let text = lists.collect::<Vec<_>>().join(" ");
    text.split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect()
}

/// Stops `root` and each process under it, and gives the IDs it held. Each pass reads again what
/// every stopped process started, since one may have been starting a process as its stop came.
#[cfg(target_os = "linux")]
pub(super) fn stopped(root: u32) -> BTreeSet<u32> {
    let (mut held, mut seen) = (BTreeSet::new(), BTreeSet::new());
    let mut found = vec![root];
    while !found.is_empty() {
        seen.extend(found.iter().copied());
        // One that has gone, or that this user may not signal, is not held.
        held.extend(
            found
                .into_iter()
                .filter(|pid| signal(*pid, rustix::process::Signal::STOP).is_ok()),
        );
        let started = held.iter().flat_map(|pid| children(*pid));
        found = started.filter(|pid| !seen.contains(pid)).collect();
    }
    held
}

/// Elsewhere no tree is read, so the group kill alone stops a command.
#[cfg(target_os = "macos")]
pub(super) fn stopped(_root: u32) -> BTreeSet<u32> {
    BTreeSet::new()
}

/// Kills each process `stopped` held, and gives the last refusal.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn killed(held: &BTreeSet<u32>) -> Result<(), rustix::io::Errno> {
    let mut refused = Ok(());
    for pid in held {
        if let Err(error) = signal(*pid, rustix::process::Signal::KILL) {
            refused = Err(error);
        }
    }
    refused
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn signal(pid: u32, signal: rustix::process::Signal) -> Result<(), rustix::io::Errno> {
    let pid = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw);
    rustix::process::kill_process(pid.ok_or(rustix::io::Errno::SRCH)?, signal)
}

#[cfg(all(test, target_os = "linux"))]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_tree_holds_its_root_and_every_process_under_it() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let tree = tree(std::process::id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(tree.first(), Some(&std::process::id()));
        assert!(tree.contains(&child.id()), "{tree:?}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_stopped_process_is_held_until_it_is_killed_even_after_a_refusal() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let mut held = stopped(child.id());
        assert_eq!(held, BTreeSet::from([child.id()]));
        assert_eq!(killed(&BTreeSet::new()), Ok(()));
        // No process has ID 0, so its kill is refused before the child's.
        held.insert(0);
        assert_eq!(killed(&held), Err(rustix::io::Errno::SRCH));
        // A child the kill missed runs on and ends by itself, so the test fails and does not hang.
        signal(child.id(), rustix::process::Signal::CONT).unwrap();
        let status = child.wait().unwrap();
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(9)
        );
    }
}
