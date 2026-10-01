//! Gates on what cargo builds: manifests and dependencies, the release profile, features, and
//! the module tree rustc compiles.

pub mod features;
pub mod manifest;
pub mod modcheck;
pub mod placement;
pub mod profile;
pub mod supply;
