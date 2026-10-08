//! Putting chock into a project and keeping it there: what `init` installs and writes, the hooks
//! and agent files that run the gates where changes are made, and `doctor`, which checks both.

mod adoption;
pub mod agents;
pub mod doctor;
pub mod hooks;
pub mod init;
mod merge;
pub mod miri;
pub mod mutest;
pub mod network;
pub mod pins;
mod repin;
pub mod version;
