//! Copies of the functions that plan chock's job counts. Verus has no spec for `map_or` or
//! `unwrap_or`, so a copy writes each as a `match`.

use vstd::prelude::*;

verus! {

// code: src/exec/budget.rs::jobs
// The copy reads the available memory as a number, where chock reads it only when nobody asked.
pub fn jobs(cpus: usize, per_job_mb: u64, available_mb: u64, asked: Option<usize>) -> (count: usize)
    requires
        per_job_mb >= 1,
    ensures
        count >= 1,
        asked matches Some(n) ==> count == if n == 0 { 1 } else { n },
        asked is None ==> count <= if cpus == 0 { 1 } else { cpus },
        asked is None && count > 1 ==> count * per_job_mb <= available_mb,
{
    if let Some(asked) = asked {
        return asked.max(1);
    }
    let by_memory = match usize::try_from(available_mb / per_job_mb) {
        Ok(by_memory) => by_memory,
        Err(_) => usize::MAX,
    };
    let count = cpus.min(by_memory).max(1);
    proof {
        let quotient = available_mb / per_job_mb;
        assert(quotient * per_job_mb <= available_mb) by (nonlinear_arith)
            requires
                per_job_mb >= 1,
                quotient == available_mb / per_job_mb,
        ;
        if count > 1 {
            assert(count * per_job_mb <= quotient * per_job_mb) by (nonlinear_arith)
                requires
                    count <= quotient,
        ;
        }
    }
    count
}

// code: src/exec/budget.rs::tightest
pub fn tightest(host: Option<u64>, container: Option<u64>) -> (tight: Option<u64>)
    ensures
        (host is None && container is None) <==> tight is None,
        tight matches Some(t) ==> (host matches Some(h) ==> t <= h),
        tight matches Some(t) ==> (container matches Some(c) ==> t <= c),
        tight matches Some(t) ==> (host == Some(t) || container == Some(t)),
{
    match (host, container) {
        (Some(host), Some(container)) => Some(host.min(container)),
        (found, None) | (None, found) => found,
    }
}

// code: src/exec/budget.rs::shared
// The copy takes the count of lanes that run at once, which chock reads from `RUNNING`.
pub fn shared(jobs: usize, running: usize) -> (share: usize)
    ensures
        share >= 1,
        share <= if jobs == 0 { 1 } else { jobs },
        share > 1 ==> share * running <= jobs,
{
    let lanes = running.max(1);
    let share = (jobs / lanes).max(1);
    proof {
        assert(jobs / lanes <= jobs) by (nonlinear_arith)
            requires
                lanes >= 1,
        ;
        assert((jobs / lanes) * lanes <= jobs) by (nonlinear_arith)
            requires
                lanes >= 1,
        ;
        if share > 1 {
            assert(share * running <= share * lanes) by (nonlinear_arith)
                requires
                    running <= lanes,
        ;
        }
    }
    share
}

// code: src/exec/budget.rs::within
// The copy takes the number that chock parses from the variable.
pub fn within(jobs: usize, set: Option<usize>) -> (count: usize)
    ensures
        count >= 1,
        count <= if jobs == 0 { 1 } else { jobs },
        set matches Some(n) ==> count <= if n == 0 { 1 } else { n },
{
    match set {
        Some(set) => set.min(jobs),
        None => jobs,
    }.max(1)
}

// code: src/gates/tools/proof.rs::jobs
pub fn kani_jobs(share: usize, asked: Option<u32>) -> (count: usize)
    ensures
        1 <= count <= 8,
        share >= 1 ==> count <= share,
        asked matches Some(n) ==> (n >= 1 ==> count <= n),
{
    let asked = match asked {
        Some(asked) => match usize::try_from(asked) {
            Ok(asked) => asked,
            Err(_) => usize::MAX,
        },
        None => usize::MAX,
    };
    share.clamp(1, 8).min(asked).max(1)
}

// code: src/gates/proofs/verus.rs::shares
pub fn shares(cap: usize, jobs: usize) -> (split: (usize, usize))
    ensures
        split.0 >= 1,
        split.1 >= 1,
        jobs >= 1 ==> split.0 <= jobs,
        split.0 * split.1 <= if cap == 0 { 1 } else { cap },
{
    let cap = cap.max(1);
    let workers = cap.min(jobs).max(1);
    let threads = (cap / workers).max(1);
    proof {
        assert(cap / workers >= 1) by (nonlinear_arith)
            requires
                1 <= workers <= cap,
        ;
        assert(workers * (cap / workers) <= cap) by (nonlinear_arith)
            requires
                workers >= 1,
        ;
    }
    (workers, threads)
}

} // verus!
