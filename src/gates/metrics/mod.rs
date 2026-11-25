//! Counts over production code, parsed rather than matched: size, complexity, nesting, copies,
//! `unsafe`, functions nothing calls, and how tests assert. `prodlines` says what production is.

pub mod admits;
pub mod assertions;
pub mod bigfiles;
pub mod blocking;
pub mod clones;
pub mod complexity;
pub mod dead;
pub mod duplication;
pub mod lean;
pub mod lines;
pub mod nesting;
pub mod prodlines;
pub mod splits;
pub mod testlint;
pub mod unsafety;

/// The lines where a rule's `hits` found `pattern`; the tests of `blocking` and `clones` read it.
#[cfg(test)]
fn lines_of(hits: Result<Vec<(&'static str, u32)>, String>, pattern: &str) -> Vec<u32> {
    let hits = hits.unwrap();
    hits.into_iter()
        .filter(|(found, _)| *found == pattern)
        .map(|(_, line)| line)
        .collect()
}
