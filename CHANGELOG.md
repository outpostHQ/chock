# Changelog

Notable changes to chock. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-10-06

First release.

chock runs checks over a Rust project. Counted checks compare every file and function with the
number committed for it in `.chock/baseline.json`: code the record has never seen must be clean, a
recorded count may not grow, and a count that shrinks is locked in, so existing debt only goes down.

- **Records.** A run lowers the record where it measured less; CI fails a change that does not
  commit the lower number. `strict` holds a check to zero, and `clean_when_touched` holds it to
  zero in each file a change touches. `chock baseline` raises a record deliberately, and
  `chock baseline --lower` records a gain on a check that varies by machine, `crap` included.
  macOS and Windows keep their own records of `binsize`, `bsize`, `coverage`, `crap` and `mutest`.
  A run lists what got worse; `chock explain` lists all the debt on record, largest first.
  A count that the record does not hold fails when it is above zero, in every counted check.
- **Nothing held back.** `test` and `miri` run every test after a failure. `lint` runs clippy when
  formatting differs. `doc` builds every package after one fails. `deps` names each `cargo deny`
  check that has no section in `deny.toml`. `crap` holds two functions with one name in one file
  each to its own score on record, in file order, and a pass says how many functions the record
  lacks. `mutest` says how many mutations timed out, and a local run mutates the whole crate where the
  touched files alone pass the 2% limit on timeouts. `coverage` names every line no test ran in a
  file that is past its record. A list or a failure read from a tool output that was cut says so.
- **Contract.** Exit codes `0` passed, `1` failed, `2` could not run. A check that measured nothing
  never reports a pass. `run`, `gates`, `explain`, `doctor`, `survey`, `edited` and `slop` take
  `--json`, and `schema/` holds the shape of a run's report, the config and the baseline. Every
  check in a report carries `verdict` and `rerun`; every finding carries `file` and `message`, and
  `line`, `item`, `measured` and `baseline` where the check has them.
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
  the checks the tree already passes. After you install a newer chock, `chock init` moves the
  project's pins to it and keeps each key chock does not set.
