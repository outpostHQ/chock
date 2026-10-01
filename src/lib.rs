//! chock — one-way quality gates for Rust projects. The rules live here rather than in the binary
//! so each one is unit-tested, and so a hook or a harness can call chock instead of reimplementing.

pub mod cli;
pub mod edited;
pub mod exec;
pub mod gates;
pub mod project;
pub mod run;
pub mod setup;
pub mod slop;
mod usage;

#[cfg(test)]
pub mod testdir;
