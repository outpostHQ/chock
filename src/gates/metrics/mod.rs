//! Counts over production code, parsed rather than matched: size, complexity, nesting, copies,
//! `unsafe`, functions nothing calls, and how tests assert. `prodlines` says what production is.

pub mod assertions;
pub mod bigfiles;
pub mod complexity;
pub mod dead;
pub mod duplication;
pub mod lean;
pub mod nesting;
pub mod prodlines;
pub mod splits;
pub mod unsafety;
