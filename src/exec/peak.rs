//! The most memory a gate's processes held at once, so a later run knows which slow gates fit side
//! by side. Read from `/proc` on Linux; elsewhere nothing is recorded.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How often a running process tree is read again.
const EVERY: Duration = Duration::from_secs(1);

/// What one gate's processes hold now and the most they held at once, in MB.
#[derive(Debug, Default)]
pub(crate) struct Peak {
    held: Mutex<(u64, u64)>,
}

impl Peak {
    /// The most the gate held at once, in MB; `None` when nothing was read.
    pub(crate) fn most(&self) -> Option<u64> {
        let (_, most) = *self.held.lock().unwrap_or_else(PoisonError::into_inner);
        (most > 0).then_some(most)
    }

    /// One tree now holds `to` MB where it held `from`.
    fn moved(&self, from: u64, to: u64) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        held.0 = held.0.saturating_sub(from) + to;
        held.1 = held.1.max(held.0);
    }
}

thread_local! {
    static GATE: std::cell::RefCell<Option<Arc<Peak>>> = const { std::cell::RefCell::new(None) };
}

/// The peak this thread counts the processes it starts toward, if any.
pub(crate) fn current() -> Option<Arc<Peak>> {
    GATE.with(|gate| gate.borrow().clone())
}

/// Counts what this thread starts toward `peak` until the guard drops.
pub(crate) fn enter(peak: Option<Arc<Peak>>) -> Entered {
    Entered(GATE.with(|gate| gate.replace(peak)))
}

/// The peak the thread counted toward before, put back on drop.
pub(crate) struct Entered(Option<Arc<Peak>>);

impl Drop for Entered {
    fn drop(&mut self) {
        let was = self.0.take();
        GATE.with(|gate| gate.replace(was));
    }
}

/// A watchdog that also reads one started tree's memory into the thread's peak, once a second.
pub(super) struct Sampled<'a> {
    inner: &'a mut dyn super::Watchdog,
    peak: Option<Arc<Peak>>,
    root: u32,
    read: fn(u32) -> u64,
    held: u64,
    read_at: Option<Instant>,
}

impl<'a> Sampled<'a> {
    pub(super) fn new(inner: &'a mut dyn super::Watchdog, root: u32) -> Self {
        Self::with(inner, root, tree_mb)
    }

    fn with(inner: &'a mut dyn super::Watchdog, root: u32, read: fn(u32) -> u64) -> Self {
        Self {
            inner,
            peak: current(),
            root,
            read,
            held: 0,
            read_at: None,
        }
    }

    fn sample(&mut self) {
        let Some(peak) = &self.peak else { return };
        if self.read_at.is_some_and(|at| at.elapsed() < EVERY) {
            return;
        }
        self.read_at = Some(Instant::now());
        let now = (self.read)(self.root);
        peak.moved(self.held, now);
        self.held = now;
    }
}

impl super::Watchdog for Sampled<'_> {
    fn poll(&mut self) -> Result<(), String> {
        self.sample();
        self.inner.poll()
    }
    fn complete(&mut self) -> Result<(), String> {
        self.inner.complete()
    }
}

impl Drop for Sampled<'_> {
    fn drop(&mut self) {
        if let Some(peak) = &self.peak {
            peak.moved(self.held, 0);
        }
    }
}

/// The resident memory of `root` and every process under it, in MB.
#[cfg(target_os = "linux")]
fn tree_mb(root: u32) -> u64 {
    mb(tree(root).into_iter().map(resident_kb).sum())
}

#[cfg(not(target_os = "linux"))]
fn tree_mb(_root: u32) -> u64 {
    0
}

#[cfg(target_os = "linux")]
fn mb(kb: u64) -> u64 {
    kb / 1024
}

/// `root` and every process under it.
#[cfg(target_os = "linux")]
fn tree(root: u32) -> Vec<u32> {
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

/// What `pid` holds in memory, in kB; 0 for a process that has gone.
#[cfg(target_os = "linux")]
fn resident_kb(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let line = status.lines().find_map(|line| line.strip_prefix("VmRSS:"));
    line.and_then(|kb| kb.split_whitespace().next()?.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::exec::Watchdog;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn the_peak_is_the_most_the_trees_held_at_once() {
        let peak = Peak::default();
        assert_eq!(peak.most(), None, "nothing read");
        peak.moved(0, 5);
        peak.moved(0, 3);
        peak.moved(5, 1);
        peak.moved(3, 0);
        peak.moved(1, 7);
        assert_eq!(peak.most(), Some(8));
    }

    #[test]
    fn a_thread_counts_toward_the_peak_it_entered_until_the_guard_drops() {
        let gate = Arc::new(Peak::default());
        let counted = enter(Some(Arc::clone(&gate)));
        let inner = enter(None);
        assert!(current().is_none());
        drop(inner);
        assert!(current().is_some_and(|now| Arc::ptr_eq(&now, &gate)));
        drop(counted);
        assert!(current().is_none(), "each guard puts back what it found");
    }

    /// Refuses on each call, so a test sees that the inner watchdog still decides.
    struct Refuses;

    impl Watchdog for Refuses {
        fn poll(&mut self) -> Result<(), String> {
            Err("poll".to_string())
        }
        fn complete(&mut self) -> Result<(), String> {
            Err("complete".to_string())
        }
    }

    static READS: AtomicU64 = AtomicU64::new(0);

    fn counted(root: u32) -> u64 {
        READS.fetch_add(1, Ordering::Relaxed);
        u64::from(root)
    }

    #[test]
    fn a_tree_is_read_at_most_once_a_second_and_its_share_goes_when_it_ends() {
        let (mut quiet, mut refuses) = ((), Refuses);
        Sampled::with(&mut quiet, 9, counted).poll().unwrap();
        assert_eq!(READS.load(Ordering::Relaxed), 0, "no gate, nothing read");
        let gate = Arc::new(Peak::default());
        let _counted = enter(Some(Arc::clone(&gate)));
        let mut first = Sampled::with(&mut refuses, 6, counted);
        assert_eq!(first.poll(), Err("poll".to_string()));
        assert_eq!(first.complete(), Err("complete".to_string()));
        first.poll().ok();
        assert_eq!(READS.load(Ordering::Relaxed), 1);
        first.read_at = Instant::now().checked_sub(EVERY);
        first.poll().ok();
        assert_eq!(READS.load(Ordering::Relaxed), 2);
        let mut second = Sampled::with(&mut quiet, 4, counted);
        second.poll().unwrap();
        drop((first, second));
        Sampled::with(&mut quiet, 3, counted).poll().unwrap();
        assert_eq!(gate.most(), Some(10), "6 and 4 at once; 3 after both ended");
    }

    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore = "Miri reads no process status")]
    fn a_running_process_holds_memory_and_one_that_has_gone_holds_none() {
        let mb = tree_mb(std::process::id());
        assert!((1..1024 * 1024).contains(&mb), "{mb}");
        assert_eq!(resident_kb(u32::MAX), 0);
        assert_eq!((super::mb(2047), super::mb(2048)), (1, 2));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn elsewhere_no_tree_is_read_so_none_holds_memory() {
        assert_eq!(tree_mb(std::process::id()), 0);
    }
}
