# Changelog

Notable changes to chock. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-10-02

First release.

chock runs checks over a Rust project. Counted checks compare every file and function with the
number committed for it in `.chock/baseline.json`: code the record has never seen must be clean, a
recorded count may not grow, and a count that shrinks is locked in, so existing debt only goes down.

- **Records.** A run lowers the record where it measured less; CI fails a change that does not
  commit the lower number. `strict` holds a check to zero, and `clean_when_touched` holds it to zero in each
  file a change touches. `chock baseline` raises a record
  deliberately and `chock baseline --lower` records a gain on a check that varies by machine.
  macOS and Windows keep their own records of `binsize`, `bsize`, `coverage`, `crap` and `mutest`.
- **Contract.** Exit codes `0` passed, `1` failed, `2` could not run. A check that measured nothing
  never reports a pass. `--json` on every command, against the schemas in `schema/`. Every finding
  carries `file`, `line`, `item`, `measured`, `baseline` and `rerun`.
- **Checks.** 53, listed by `chock gates`: native analysis over `syn` (complexity, duplication,
  nesting, dead code, `unsafe`, comments, manifests), pinned external tools (tests, coverage,
  mutation testing, advisories, unused dependencies, typos, binary size), Kani proofs, and Outpost's
  measures.
- **Where it runs.** An editor hook on each file written, `pre-commit` for the checks that need no
  compiler, `pre-push` for the rest, and CI for everything but `local_only`.
- **Version control.** git and Outpost. A crate below its repository's root is read from that
  repository. Only `src/project/vcs.rs` runs a version-control command.
- **Tooling.** `tool-versions.env` pins every external tool. `chock doctor` reports drift;
  `chock init --global` installs the tools, and `chock init --local` writes the hooks and enables
  the checks the tree already passes.
