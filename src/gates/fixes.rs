//! What to do about a tripped gate, in one sentence a person or an agent can act on.

/// The fix for a tripped gate; `None` for a name no gate answers to.
#[must_use]
pub fn fix(gate: &str) -> Option<&'static str> {
    FIXES
        .iter()
        .find(|(name, _)| *name == gate)
        .map(|(_, hint)| *hint)
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
    ("wiring", "switch the check on with `chock enable <check>`"),
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
}
