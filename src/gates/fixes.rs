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

/// Why a gate stops when the download it needs is pinned but not fetched to this machine.
#[must_use]
pub fn not_fetched(download: &str) -> String {
    format!("`{download}`{NOT_FETCHED}")
}

const NOT_FETCHED: &str = " is not fetched, so this gate has nothing to run";

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
        said => ending(said).or_else(|| tool_named(said).map(installing)),
    }
}

/// The repair of a reason that ends with the words of a download pin's problem.
fn ending(said: &str) -> Option<&'static str> {
    let endings = [
        (crate::setup::downloads::NOT_PINNED, PIN_DOWNLOADS),
        (NOT_FETCHED, FETCH),
    ];
    let found = endings.into_iter().find(|(end, _)| said.ends_with(end));
    found.map(|(_, repair)| repair)
}

const DECLARE_MSRV: &str = "add `rust-version = \"1.NN\"` under `[package]` in Cargo.toml: the oldest Rust this crate promises to build on";

const NIGHTLY: &str =
    "run `chock init --global`: it installs each nightly toolchain a gate starts, and Miri";

const PINNED: &str = "run `chock init --global`: it installs each tool that `tool-versions.env` pins, or says why it cannot on this system";

const RUSTUP: &str = "install Rust with rustup: https://rustup.rs";

const PIN_DOWNLOADS: &str = "run `chock init --local`: it adds the download pins of this chock to `tool-versions.env`; then run `chock init --global`";

const FETCH: &str = "run `chock init --downloads`: it fetches the downloads of each gate that is on, so for a gate that is off, run `chock enable GATE` first";

/// The repair for each tool that `chock init --global` does not install from `tool-versions.env`.
const INSTALLS: [(&str, &str); 7] = [
    (
        "java",
        "install a Java runtime, 21 or newer, such as Temurin 21, or set JAVA_HOME to one",
    ),
    ("cargo-mutest", tool::REPAIR),
    ("rustfmt", "run `rustup component add rustfmt`"),
    ("clippy-driver", "run `rustup component add clippy`"),
    ("cargo", RUSTUP),
    ("rustdoc", RUSTUP),
    (
        "shellcheck",
        "install shellcheck with your package manager, such as apt or brew",
    ),
];

fn installing(tool: &str) -> &'static str {
    let found = INSTALLS.iter().find(|(name, _)| *name == tool);
    found.map_or(PINNED, |(_, repair)| repair)
}

const FIXES: [(&str, &str); 72] = [
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
        "testlint",
        "give the test an assertion on what the code returned, a reason on its `#[ignore]`, the \
         expected text on its `#[should_panic]`, a name of its own that states the claim, or a \
         directory of its own in place of the shared one; a rule that is wrong for one test is \
         waived in it with `// test-lint: allow(<rule>) — reason`.",
    ),
    (
        "citations",
        "point the doc comment at a path that exists, or stop backticking a path that is only an example",
    ),
    (
        "claims",
        "cite a line the file still has, or the path with no line; drop from the `<!-- absent: … -->` list what the tree now holds, and give each `absent-by-design` its reason after a dash",
    ),
    (
        "pinned",
        "restore the pinned test, or the pin that left the documents; to drop a pin on purpose, take it out of the documents and run `chock baseline pinned`",
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
        "set what the finding names in `[profile.release]`, add the lint or the `clippy.toml` key a floor finding names, or remove the override that silences a check for the whole build",
    ),
    (
        "toolchain",
        "set `channel` in `rust-toolchain.toml` to a release such as `1.98.1`, or to a dated nightly such as `nightly-2026-09-17`, and name no `path`; a release before `1.96.0` has a Cargo with a known security fault",
    ),
    (
        "features",
        "delete the feature nothing reaches, or declare the feature the `cfg` names",
    ),
    (
        "hygiene",
        "stop tracking the file, rotate any credential it held, and add it to the ignore file; delete a hidden character, a `.gitmodules` value that begins with `-` or holds a control character, and a tracked link into `.git`",
    ),
    (
        "workflows",
        "pin each action to the full commit SHA that its tag names now, with the tag in a comment, and each image by its sha256 digest",
    ),
    (
        "source",
        "allow the lint at the one site that needs it, rather than for the whole crate, and give the reason: `reason = \"…\"` from Rust 1.81, a `//` comment on the line above where `rust-version` is older",
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
        "delete the function or call it; mark one reached only through a macro `#[expect(dead_code)]`, or `#[allow(dead_code)]` with a `//` reason where `rust-version` is older than 1.81",
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
        "wrappers",
        "make the call at the one caller and delete the function that only passes it on",
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
        "tla",
        "run TLC on the named cfg and read its trace: fix the model, or the cfg's `\\* expect:` line",
    ),
    (
        "verus",
        "run `verus <file> --crate-type=lib` and fix the proof it names; for a mutant that verifies, make the proof state what the mutant breaks; for a recorded function the run no longer verifies, restore it, or, when the change is meant, ask a person to run `chock baseline verus`",
    ),
    (
        "links",
        "write each `code:` link as `file.rs::Type::fn` or `Type::fn` that names one item; for a drifted item, read the model against the code, then commit the model",
    ),
    (
        "laws",
        "restore the text of the law, or, when the new law is meant, ask a person to run `chock baseline laws` so a reviewer sees it",
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
        "shellcheck",
        "fix the script at the line, or put `# shellcheck disable=SCnnnn` above it with the reason",
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
        "echoes",
        "delete the comment, or make it say why the line is there rather than what it does",
    ),
    (
        "bigfiles",
        "split the file along a concern into modules of under 1000 production lines",
    ),
    (
        "splits",
        "move the part the finding names into a child module; only its owner, with the fields and \
         methods the file uses, needs `pub(super)`",
    ),
    (
        "lean",
        "call what the forwarder calls where the forwarder is called, then remove the forwarder",
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
        "admits",
        "prove the step, make it a hypothesis of the theorem that needs it, or list its line in \
         `.chock/escapes.json` with the reason it stays",
    ),
    (
        "lines",
        "delete what the tree no longer needs, or say the same thing in fewer lines",
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
        "fix the undefined behaviour Miri reports at the line it names; a test of what Miri does \
         not model, such as a file in `/proc`, takes `#[cfg_attr(miri, ignore = \"reason\")]`",
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
    (
        "seams",
        "move the use into a file or package the entry allows, or call the code that owns it",
    ),
    (
        "owners",
        "read the value in one file, and pass it on to the code that needs it",
    ),
    (
        "calls",
        "make the call the rule names in the same function; the finding carries your reason",
    ),
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
        let fetch = not_fetched("tla-tools");
        assert_eq!(
            fetch,
            "`tla-tools` is not fetched, so this gate has nothing to run"
        );
        assert_eq!(repair(&fetch), Some(FETCH));
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
