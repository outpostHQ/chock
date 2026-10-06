//! The `mutest` mutation-testing gate: running it, watching the run, reading its results and
//! ranking its survivors.

pub(super) mod mutest;
#[cfg(target_os = "linux")]
mod progress;
mod results;
mod scope;
pub mod survivors;
mod targets;
pub mod tool;
#[cfg(target_os = "linux")]
mod watch;
