//! How much of the machine a spawned tool may use, bounded by memory as well as cores: a machine
//! without swap kills a build that runs out.

use std::sync::atomic::{AtomicUsize, Ordering};

/// What one build job may need at its peak, with headroom for workspaces larger than chock.
pub const PER_JOB_MB: u64 = 2048;

/// The environment variable that sets the job count outright.
pub const ASKED: &str = "CHOCK_JOBS";

/// Jobs to run: no more than the cores or memory allow, at least one, and `asked` wins outright.
#[must_use]
pub fn jobs(cpus: usize, available_mb: u64, asked: Option<usize>) -> usize {
    if let Some(asked) = asked {
        return asked.max(1);
    }
    let by_memory = usize::try_from(available_mb / PER_JOB_MB).unwrap_or(usize::MAX);
    cpus.min(by_memory).max(1)
}

/// Available memory in MB from `MemAvailable`, not `MemFree`, which leaves out reclaimable cache.
#[must_use]
pub fn available_from_meminfo(meminfo: &str) -> Option<u64> {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| kb / 1024)
}

/// A cgroup v2 container's headroom in MB: its limit less what is charged; `None` for `max`.
#[must_use]
pub fn available_from_cgroup(limit: &str, current: &str) -> Option<u64> {
    let limit: u64 = limit.trim().parse().ok()?;
    let current: u64 = current.trim().parse().ok().unwrap_or(0);
    Some(limit.saturating_sub(current) / (1024 * 1024))
}

/// Whichever reading is smaller, because a container sees both and only the tighter one binds.
#[must_use]
pub fn tightest(host: Option<u64>, container: Option<u64>) -> Option<u64> {
    match (host, container) {
        (Some(host), Some(container)) => Some(host.min(container)),
        (found, None) | (None, found) => found,
    }
}

/// The prefix that runs a spawn at the lowest best-effort I/O priority and nice 10. A prefix, since
/// lowering chock's own priority needs `unsafe`.
pub const COURTESY: [&str; 8] = ["ionice", "-c", "2", "-n", "7", "nice", "-n", "10"];

/// The tools heavy enough to run at lower priority; for anything else the extra execs cost more.
pub const HEAVY: [&str; 2] = ["cargo", "outpost"];

/// The prefix for a spawn of `program`: `COURTESY` for a heavy tool where it works, else nothing.
#[must_use]
pub fn courtesy(program: &str, can_lower: bool) -> &'static [&'static str] {
    if HEAVY.contains(&file_name(program)) && can_lower {
        &COURTESY
    } else {
        &[]
    }
}

/// The program's last non-empty path component, so a tool named by its path is still recognised.
#[must_use]
fn file_name(program: &str) -> &str {
    program
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(program)
}

/// Whether the `COURTESY` prefix works here, tried once: a container can have `ionice` and still
/// refuse the call.
fn can_lower_priority() -> bool {
    static WORKS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WORKS.get_or_init(|| {
        std::process::Command::new(COURTESY[0])
            .args(&COURTESY[1..])
            .arg("true")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|done| done.success())
    })
}

/// The prefix `exec` puts in front of a spawn of `program` on this machine.
#[must_use]
pub fn politely(program: &str) -> &'static [&'static str] {
    courtesy(program, can_lower_priority())
}

/// The job-cap variables cargo and nextest read. Set on each spawn, since setting them on chock's
/// own process needs `unsafe`.
pub const CARGO_JOBS: &str = "CARGO_BUILD_JOBS";
pub const NEXTEST_THREADS: &str = "NEXTEST_TEST_THREADS";

/// The job cap every spawn carries, computed on first use if `prime` was not called.
static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// Sets the job cap before any tool runs.
pub fn prime(jobs: usize) {
    // Already set means another entry point primed it first.
    let _ = CAP.set(jobs);
}

/// The job cap for this run, shared equally by the lanes of compiling gates that run at once.
#[must_use]
pub fn cap() -> usize {
    (machine() / RUNNING.load(Ordering::Relaxed).max(1)).max(1)
}

/// The job cap of the whole machine, before the lanes share it.
#[must_use]
pub fn machine() -> usize {
    *CAP.get_or_init(of_this_machine)
}

/// The jobs one lane needs to be worth running beside another.
pub const PER_LANE: usize = 8;

/// How many lanes of compiling gates may run at once with `jobs`: one below `2 * PER_LANE`, and
/// never more than two, the suite's and the one for gates that build apart.
#[must_use]
pub fn lanes(jobs: usize) -> usize {
    (jobs / PER_LANE).clamp(1, 2)
}

/// The lanes running now.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// One running lane, counted while it is held, so each spawn takes its share of the cap.
pub struct Lane;

impl Lane {
    #[must_use]
    pub fn enter() -> Self {
        RUNNING.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The cap variables to set on a spawn: the lane's share, never above a value already set.
#[must_use]
pub fn caps() -> Vec<(&'static str, String)> {
    [CARGO_JOBS, NEXTEST_THREADS]
        .into_iter()
        .map(|key| (key, within(cap(), std::env::var(key).ok().as_deref())))
        .map(|(key, jobs)| (key, jobs.to_string()))
        .collect()
}

/// `jobs`, or a smaller number someone `set`; a value that is no number leaves `jobs`.
#[must_use]
pub fn within(jobs: usize, set: Option<&str>) -> usize {
    set.and_then(|set| set.trim().parse::<usize>().ok())
        .map_or(jobs, |set| set.min(jobs))
        .max(1)
}

/// The job cap this machine allows. `available_parallelism` already honours CPU affinity and
/// cgroup quotas.
#[must_use]
pub fn of_this_machine() -> usize {
    // A cargo job cap set by the caller is this whole run's allowance, which the lanes then share.
    let cpus = within(
        std::thread::available_parallelism().map_or(1, Into::into),
        std::env::var(CARGO_JOBS).ok().as_deref(),
    );
    let host = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| available_from_meminfo(&text));
    let container = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        .ok()
        .and_then(|limit| {
            let current = std::fs::read_to_string("/sys/fs/cgroup/memory.current").ok()?;
            available_from_cgroup(&limit, &current)
        });
    let asked = std::env::var(ASKED).ok().and_then(|n| n.parse().ok());
    // Memory chock cannot read counts as one job's worth, not unlimited.
    jobs(cpus, tightest(host, container).unwrap_or(PER_JOB_MB), asked)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_with_memory_to_spare_is_held_to_its_cores() {
        assert_eq!(jobs(8, 64 * 1024, None), 8);
    }

    #[test]
    fn a_machine_with_more_cores_than_memory_is_held_to_its_memory() {
        assert_eq!(jobs(96, 16 * 1024, None), 8);
        assert_eq!(jobs(96, 4 * 1024, None), 2);
    }

    #[test]
    fn a_machine_with_almost_no_memory_still_runs_one_job() {
        assert_eq!(jobs(96, 0, None), 1);
        assert_eq!(jobs(0, 64 * 1024, None), 1);
    }

    #[test]
    fn a_number_somebody_asked_for_is_used_whatever_the_machine_looks_like() {
        assert_eq!(jobs(2, 1024, Some(32)), 32);
        assert_eq!(jobs(96, 999_999, Some(1)), 1);
    }

    #[test]
    fn a_request_for_no_jobs_at_all_still_runs_one() {
        assert_eq!(jobs(8, 64 * 1024, Some(0)), 1);
    }

    #[test]
    fn the_headroom_is_read_from_the_available_line_and_not_the_free_one() {
        let meminfo = "MemTotal:       131072000 kB\n\
                       MemFree:          1048576 kB\n\
                       MemAvailable:   104857600 kB\n";
        assert_eq!(available_from_meminfo(meminfo), Some(102_400));
    }

    #[test]
    fn a_meminfo_without_that_line_is_no_reading_rather_than_zero() {
        assert_eq!(available_from_meminfo("MemTotal: 1 kB\n"), None);
        assert_eq!(available_from_meminfo(""), None);
    }

    #[test]
    fn a_cgroup_limit_less_what_is_charged_is_the_headroom() {
        assert_eq!(
            available_from_cgroup("2147483648", "1073741824"),
            Some(1024)
        );
    }

    #[test]
    fn an_unlimited_cgroup_is_no_reading_rather_than_none_available() {
        assert_eq!(available_from_cgroup("max", "0"), None);
    }

    #[test]
    fn a_cgroup_charged_past_its_limit_leaves_nothing_rather_than_wrapping() {
        assert_eq!(available_from_cgroup("1048576", "2097152"), Some(0));
    }

    #[test]
    fn every_spawn_carries_a_cap_for_cargo_and_for_the_test_runner() {
        let carried: Vec<&str> = caps().into_iter().map(|(key, _)| key).collect();
        assert_eq!(carried, [CARGO_JOBS, NEXTEST_THREADS]);
    }

    #[test]
    fn a_job_cap_already_set_is_a_ceiling_the_share_never_raises() {
        let held: Vec<usize> = [
            Some("4"),
            Some("32"),
            None,
            Some("-1"),
            Some("0"),
            Some(" 2 "),
        ]
        .into_iter()
        .map(|set| within(8, set))
        .collect();
        assert_eq!(held, [4, 8, 8, 8, 1, 2]);
    }

    #[test]
    fn the_cap_this_machine_allows_is_at_least_one_job() {
        assert!(cap() >= 1);
    }

    #[test]
    fn a_second_lane_opens_at_twice_the_jobs_one_lane_needs_and_a_third_never() {
        let opened: Vec<usize> = [1, 8, 15, 16, 24, 96].into_iter().map(lanes).collect();
        assert_eq!(opened, [1, 1, 1, 2, 2, 2]);
    }

    #[test]
    fn a_lane_counts_while_it_is_held_and_its_share_never_falls_below_one_job() {
        let lane = Lane::enter();
        assert_eq!(Some(&machine()), CAP.get());
        assert!(RUNNING.load(Ordering::Relaxed) >= 1);
        assert!(cap() >= 1 && cap() <= *CAP.get_or_init(of_this_machine));
        drop(lane);
    }

    #[test]
    fn the_tighter_of_the_two_readings_is_the_one_that_binds() {
        assert_eq!(tightest(Some(100_000), Some(2_000)), Some(2_000));
        assert_eq!(tightest(Some(2_000), Some(100_000)), Some(2_000));
        assert_eq!(tightest(Some(500), None), Some(500));
        assert_eq!(tightest(None, Some(500)), Some(500));
        assert_eq!(tightest(None, None), None);
    }

    #[test]
    fn only_the_tools_that_move_gigabytes_are_asked_to_yield() {
        assert_eq!(courtesy("cargo", true), &COURTESY);
        assert_eq!(courtesy("outpost", true), &COURTESY);
        assert!(courtesy("git", true).is_empty());
        // Named by path, it is still the same tool.
        assert_eq!(courtesy("/usr/bin/cargo", true), &COURTESY);
        assert!(courtesy("/usr/bin/git", true).is_empty());
    }

    #[test]
    fn a_machine_that_cannot_lower_a_priority_still_runs_the_tool() {
        assert!(courtesy("cargo", false).is_empty());
        assert!(courtesy("git", false).is_empty());
    }

    #[test]
    fn the_courtesy_lowers_both_the_disk_and_the_cpu_priority() {
        assert_eq!(
            COURTESY.to_vec(),
            vec!["ionice", "-c", "2", "-n", "7", "nice", "-n", "10"]
        );
        for tool in ["ionice", "nice"] {
            assert!(COURTESY.contains(&tool), "{tool} is never invoked");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_courtesy_is_tried_before_it_is_relied_on() {
        // Either answer, as long as it is the same twice and the prefix follows it.
        let tried = can_lower_priority();
        assert_eq!(can_lower_priority(), tried);
        let nothing: &[&str] = &[];
        let want: &[&str] = if tried { &COURTESY } else { nothing };
        assert_eq!(courtesy("cargo", tried), want);
    }
}
