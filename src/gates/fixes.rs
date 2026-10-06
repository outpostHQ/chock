//! What to do about a gate that tripped or could not run, in one sentence a person or an agent can
//! act on.

use crate::gates::mutation::tool;
use crate::gates::tools::miri;

/// The fix for a tripped gate; `None` for a name no gate answers to.
#[must_use]
pub fn fix(gate: &str) -> Option<&'static str> {
    FIXES
        .iter()
        .find(|(name, _)| *name == gate)
        .map(|(_, hint)| *hint)
}

/// The reason a gate gives when a tool it starts is not on this machine.
#[must_use]
pub fn not_installed(tool: &str) -> String {
    format!("`{tool}`{NOTHING_TO_RUN}")
}

const NOTHING_TO_RUN: &str = " is not installed, so this gate has nothing to run";

/// The tool a `not_installed` reason names.
fn tool_named(reason: &str) -> Option<&str> {
    let quoted = reason.strip_suffix(NOTHING_TO_RUN)?;
    quoted.strip_prefix('`')?.strip_suffix('`')
}

/// rustup's words when `cargo +nightly…` finds no such toolchain on this machine.
const NO_NIGHTLY: [&str; 2] = ["toolchain 'nightly", "is not installed"];

#[must_use]
pub fn no_nightly(said: &str) -> bool {
    NO_NIGHTLY.iter().all(|part| said.contains(part))
}

/// What repairs a gate that could not run: an install, or a line the manifest lacks. `None` where
/// the reason is neither.
#[must_use]
pub fn repair(reason: &str) -> Option<&'static str> {
    match reason {
        crate::gates::tools::NO_MSRV => Some(DECLARE_MSRV),
        tool::FOREIGN => Some(tool::REPAIR),
        miri::ABSENT => Some(NIGHTLY),
        said if no_nightly(said) => Some(NIGHTLY),
        said => tool_named(said).map(installing),
    }
}

const DECLARE_MSRV: &str = "add `rust-version = \"1.NN\"` under `[package]` in Cargo.toml: the oldest Rust this crate promises to build on";

const NIGHTLY: &str =
    "run `chock init --global`: it installs each nightly toolchain a gate starts, and Miri";

const PINNED: &str = "run `chock init --global`: it installs each tool that `tool-versions.env` pins, or says why it cannot on this system";

fn installing(tool: &str) -> &'static str {
    match tool {
        "cargo-mutest" => tool::REPAIR,
        "rustfmt" => "run `rustup component add rustfmt`",
        "clippy-driver" => "run `rustup component add clippy`",
        "cargo" | "rustdoc" => "install Rust with rustup: https://rustup.rs",
        _ => PINNED,
    }
}

const FIXES: [(&str, &str); 53] = [
    (
        "test",
        "run the rerun command, read each failing test's output, and fix the code or the test; a crate with no tests needs one",
    ),
    (
        "lint",
        "run `cargo fmt`, then fix each clippy warning at its line; allow a lint only at the one site that needs it, with a reason",
    ),
    (
        "doc",
        "fix each rustdoc warning at its line: a broken intra-doc link, an unclosed code block, a missing item",
    ),
    (
        "modcheck",
        "add the missing `mod` declaration, delete the file nothing declares, or point `#[path]` at the right file",
    ),
    (
        "assertions",
        "assert the values the test produced, such as `assert_eq!(found, [..])`, rather than how many there are",
    ),
    (
        "citations",
        "point the doc comment at a path that exists, or stop backticking a path that is only an example",
    ),
    (
        "commits",
        "reword the commit before pushing: a subject within the width, and a body that says why rather than repeating the diff",
    ),
    (
        "manifest",
        "pin the dependency to a version or a `rev`, not a branch",
    ),
    (
        "placement",
        "move a dependency only tests use to `[dev-dependencies]`, and replace a path outside the repository with a published version",
    ),
    (
        "profile",
        "set what the finding names in `[profile.release]`, or remove the override that silences a check for the whole build",
    ),
    (
        "features",
        "delete the feature nothing reaches, or declare the feature the `cfg` names",
    ),
    (
        "hygiene",
        "stop tracking the file, rotate any credential it held, and add it to the ignore file",
    ),
    (
        "source",
        "allow the lint at the one site that needs it, with `reason = \"…\"`, rather than for the whole crate",
    ),
    (
        "duplication",
        "move the shared body into one function and call it from both places",
    ),
    (
        "duplicates",
        "merge the two implementations into one and call it from both places",
    ),
    (
        "dead",
        "delete the function or call it; mark one reached only through a macro `#[expect(dead_code)]`",
    ),
    (
        "history",
        "rotate the credential: it stays in history even after the file is deleted",
    ),
    (
        "boundaries",
        "move the definition next to the code that calls it, or split the directory along its concerns",
    ),
    (
        "padding",
        "shorten the code the finding names: drop repetition and fold boilerplate into helpers",
    ),
    (
        "measures",
        "read the measure the finding names in `outpost check` and bring it back down",
    ),
    (
        "hazards",
        "fix the shape at the line: quote a spliced value, compare two different things, stop re-reading in the loop",
    ),
    (
        "unreferenced",
        "delete the code nothing reaches, or reach it from shipped code",
    ),
    (
        "lenses",
        "fix the hazard at each site the finding names; a lens that stopped reporting is a tool change, not a fix: read why before re-recording",
    ),
    (
        "unread",
        "nothing to fix: it names regions outpost could not parse",
    ),
    (
        "scan",
        "fix the pattern at the line; validate untrusted input before it reaches the sink",
    ),
    (
        "proof",
        "run the failing harness with `cargo kani --harness <name>` and fix the code or the proof",
    ),
    (
        "deps",
        "upgrade the crate the advisory names, add its licence to `deny.toml` deliberately, or replace the unknown source",
    ),
    ("sort", "run `cargo sort --workspace`"),
    (
        "dupdeps",
        "align the two versions with `cargo update -p <crate>`, or accept the duplicate with `chock baseline dupdeps`",
    ),
    (
        "acl",
        "grant the API in `cackle.toml` if the dependency needs it, or replace the dependency; \
         remove a grant cackle calls unused",
    ),
    (
        "supply",
        "read the new build script or proc-macro, then accept it with `chock baseline supply`",
    ),
    (
        "unused",
        "remove the dependency from `Cargo.toml`, or use it",
    ),
    (
        "typos",
        "fix the spelling, or add a real word to `typos.toml`",
    ),
    (
        "msrv",
        "raise `rust-version` to what the code needs, or stop using the newer API",
    ),
    (
        "slop",
        "cut the comment to two lines; the rest of the reasoning belongs in the commit message",
    ),
    (
        "bigfiles",
        "split the file along a concern into modules of under 1000 production lines",
    ),
    (
        "binsize",
        "find what grew with `chock run bsize`, and drop the dependency or feature that brought it",
    ),
    (
        "complexity",
        "split the function: move each branch's body into a named helper",
    ),
    (
        "nesting",
        "return early and move inner blocks into helpers until it nests four levels or fewer",
    ),
    (
        "codeslop",
        "fix each clippy lint the finding names, at its line",
    ),
    (
        "unsafety",
        "replace the `unsafe` with a safe API, or move it into one audited module",
    ),
    (
        "coverage",
        "write a test that executes the lines the finding names",
    ),
    (
        "crap",
        "add tests for the function, or split it so each part is simpler; `chock explain` lists every function the record holds over CRAP 30",
    ),
    (
        "idempotent",
        "make the tests clean up what they write, or write under a fresh directory each run",
    ),
    (
        "miri",
        "fix the undefined behaviour Miri reports at the line it names",
    ),
    (
        "mutest",
        "add an assertion that fails when the decision at the named line is changed",
    ),
    (
        "unused-deep",
        "remove the dependency the compiler shows nothing uses",
    ),
    (
        "bsize",
        "nothing to fix: it reports where a binary's bytes go",
    ),
    ("fmt", "run `cargo fmt`"),
    (
        "commands",
        "fix what your own check reports; its output says what",
    ),
    (
        "commands-build",
        "fix what your own check reports; its output says what",
    ),
    (
        "phrases",
        "replace the phrase; the finding carries the reason your config gives for refusing it",
    ),
    ("wiring", "switch the gate on with `chock enable GATE`"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gate_has_a_fix_and_no_fix_names_a_gate_that_is_not_there() {
        let gates: Vec<&str> = crate::gates::registry().iter().map(|g| g.name).collect();
        let missing: Vec<&&str> = gates.iter().filter(|name| fix(name).is_none()).collect();
        assert_eq!(missing, Vec::<&&str>::new());
        let stray: Vec<&str> = FIXES
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| !gates.contains(name))
            .collect();
        assert_eq!(stray, Vec::<&str>::new());
        assert_eq!(fix("sort"), Some("run `cargo sort --workspace`"));
    }

    #[test]
    fn a_gate_that_a_missing_tool_stopped_is_told_the_install_for_that_tool() {
        let told = |tool: &str| repair(&not_installed(tool));
        assert_eq!(
            not_installed("kani"),
            "`kani` is not installed, so this gate has nothing to run"
        );
        assert_eq!(told("cargo-mutest"), Some(tool::REPAIR));
        assert_eq!(told("rustfmt"), Some("run `rustup component add rustfmt`"));
        assert_eq!(
            told("clippy-driver"),
            Some("run `rustup component add clippy`")
        );
        let rust = Some("install Rust with rustup: https://rustup.rs");
        assert_eq!([told("cargo"), told("rustdoc")], [rust, rust]);
        assert_eq!(
            [told("kani"), told("cargo-acl")],
            [Some(PINNED), Some(PINNED)]
        );
    }

    #[test]
    fn a_reason_no_install_answers_gets_no_repair() {
        assert_eq!(repair("the suite does not compile"), None);
        // The sentence without a quoted tool names nothing to install.
        assert_eq!(repair(NOTHING_TO_RUN), None);
        assert_eq!(repair(&format!("kani{NOTHING_TO_RUN}")), None);
        assert_eq!(repair(&format!("`kani{NOTHING_TO_RUN}")), None);
        assert_eq!(repair(&format!("{} today", not_installed("kani"))), None);
    }

    #[test]
    fn a_foreign_mutest_a_missing_miri_and_a_missing_nightly_each_name_chock_init() {
        assert_eq!(repair(tool::FOREIGN), Some(tool::REPAIR));
        assert_eq!(repair(miri::ABSENT), Some(NIGHTLY));
        assert_eq!(repair(crate::gates::tools::NO_MSRV), Some(DECLARE_MSRV));
        let rustup = "cargo udeps could not build: error: toolchain 'nightly-x86_64-unknown-linux-gnu' is not installed";
        assert_eq!(repair(rustup), Some(NIGHTLY));
        assert!(no_nightly(rustup));
        // Each half alone is some other failure.
        assert!(!no_nightly(
            "error: toolchain 'nightly-x86_64-unknown-linux-gnu' is broken"
        ));
        assert!(!no_nightly(
            "error: toolchain 'stable-x86_64-unknown-linux-gnu' is not installed"
        ));
        assert_eq!(repair("error: 'cargo-fuzz' is not installed"), None);
    }
}
