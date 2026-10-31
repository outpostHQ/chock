//! One started command that chock owns: the leader of its own process group, ended as a whole.

use std::fmt::Write as _;
use std::process::Command;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use std::process::Stdio;

use super::{CLEANUP, reap_until};

/// What a group of only zombies answers the final signal: Darwin says EPERM where Linux says ESRCH.
/// The group is chock's own, so a live member would have taken it.
#[cfg(target_os = "macos")]
const SETTLED: [rustix::io::Errno; 2] = [rustix::io::Errno::SRCH, rustix::io::Errno::PERM];
#[cfg(target_os = "linux")]
const SETTLED: [rustix::io::Errno; 1] = [rustix::io::Errno::SRCH];

/// Whether the final signal leaves nothing running; the leader is kept unreaped on purpose.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn nothing_left(signalled: Result<(), rustix::io::Errno>) -> Result<(), String> {
    match signalled {
        Err(error) if !SETTLED.contains(&error) => {
            Err(format!("cannot signal owned process group: {error}"))
        }
        _ => Ok(()),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn observed_child_exit(
    result: Result<bool, rustix::io::Errno>,
) -> std::io::Result<bool> {
    match result {
        Err(rustix::io::Errno::INTR) => Ok(false),
        result => result.map_err(Into::into),
    }
}

pub(super) struct Process {
    pub(super) child: std::process::Child,
    pub(super) owned: bool,
    pub(super) finalized: bool,
}

impl Process {
    pub(super) fn spawn(command: &mut Command) -> std::io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        command.spawn().map(|child| Self {
            child,
            owned: true,
            finalized: false,
        })
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn exited(&mut self) -> std::io::Result<bool> {
        use rustix::process::{WaitId, WaitIdOptions, waitid};
        let pid = rustix::process::Pid::from_child(&self.child);
        // Keeping the leader unreaped pins its group ID until the final signal.
        let flags = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        let observed = waitid(WaitId::Pid(pid), flags).map(|status| status.is_some());
        if observed == Err(rustix::io::Errno::CHILD) {
            self.owned = false;
        }
        observed_child_exit(observed)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn exited(&mut self) -> std::io::Result<bool> {
        self.child.try_wait().map(|status| status.is_some())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn terminate(&mut self) -> Result<(), String> {
        use rustix::process::{Pid, Signal, kill_process_group};
        let pid = Pid::from_child(&self.child);
        if pid.is_init() {
            return Err("refusing to signal process group 1".to_string());
        }
        self.exited().map_err(|error| error.to_string())?;
        // A group kill alone misses a process in a group of its own, as mutest starts each test.
        let held = super::tree::stopped(self.child.id());
        let group = nothing_left(kill_process_group(pid, Signal::KILL));
        let tree = nothing_left(super::tree::killed(&held));
        group.and(tree)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn terminate(&mut self) -> Result<(), String> {
        if self.exited().map_err(|error| error.to_string())? {
            return Ok(());
        }
        // `/T` takes the children too, which would hold the pipes open; the PID cannot be reused
        // while `child` holds its handle. Best effort: the kill below still ends the child itself.
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &self.child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        self.child.kill().map_err(|error| error.to_string())
    }

    pub(super) fn finish(&mut self) -> Result<std::process::ExitStatus, String> {
        if self.finalized || !self.owned {
            self.finalized = true;
            return Err("child ownership is no longer available for cleanup".to_string());
        }
        self.finalized = true;
        let mut cleanup = self.terminate();
        if !self.owned {
            return Err(
                "child was reaped elsewhere; refusing a stale process identity".to_string(),
            );
        }
        if let Err(reason) = &mut cleanup
            && let Err(error) = self.child.kill()
        {
            let _ = write!(reason, "; direct-child kill failed: {error}");
        }
        let reaped = reap_until(&mut self.child, CLEANUP);
        match (cleanup, reaped) {
            (Ok(()), status) => status,
            (Err(error), Ok(_)) => Err(error),
            (Err(error), Err(reap)) => Err(format!("{error}; {reap}")),
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if !self.finalized {
            // A drop has no caller to give the error to, and `finish` kills what it can either way.
            let _ = self.finish();
        }
    }
}
