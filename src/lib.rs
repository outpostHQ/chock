//! chock: quality gates for Rust, with a limit on old debt that each fix lowers. The rules live
//! here, not in the binary, so each one is unit-tested and a hook or a harness can call them.

pub mod cli;
pub mod edited;
pub mod exec;
pub mod gates;
pub mod oracle;
pub mod project;
pub mod run;
pub mod setup;
pub mod slop;
mod usage;

#[cfg(test)]
pub mod testdir;
