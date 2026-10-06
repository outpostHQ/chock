# Changelog

Notable changes to chock. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-10-06

No command or flag is removed or renamed, and the JSON reports keep their shape. The text that
chock prints changed: a program that reads it, and not `--json`, must be checked.

- **A missing tool names its install.** A gate that cannot run because its tool is missing, or is
  another build of it, adds a `fix` line: the command that installs the tool. The line is in the
  text and in the JSON report, for `chock run` and for `chock baseline`.
- **mutest.** `chock init --global` builds Outpost's fork of mutest-rs from the newest commit of
  its `main`, with the nightly that commit names. Upstream mutest-rs prints the same version and
  refuses the fork's flags, so a mutation run first asks `cargo-mutest` whether it is the fork.
  `chock doctor` shows the installed commit against `main`, and fails where the fork is missing,
  is another build, or is behind.
- **Miri.** `chock init --global` installs the `nightly` toolchain with its `miri` and `rust-src`
  components. `chock doctor` has a row for Miri where the project runs the `miri` gate.
- **`crap` findings say why.** A finding shows the function's complexity and coverage now and on
  record, says which of the two got worse, and what to do about it.
- **`deps` names an unlicensed crate.** `cargo deny` reports a crate with no licence without a
  place in a file. That was `CANNOT RUN` with the tool's raw output; it is now a finding, and it
  says to add a `license` field where the crate is yours.
- **`msrv` says how to start.** A `Cargo.toml` with no `rust-version` gets a `fix` line.
- **Help.** `chock --help` groups the commands by task: set up, every day, choose the gates,
  called by the hooks. `chock help` and `chock help init` work.
- **One word.** The output says "gate" for a gate; "check" is only a row of `chock doctor`.
  Counts read `1 gate` and `3 gates` in place of `gate(s)`. `chock init` puts the opt-in gates on
  one line, and says that on does not mean passed.
- **`chock stage` and `chock slop`.** `chock stage` answers `runs at ci`. `chock slop FILE` says
  that `chock edited FILE` reads one file. `chock slop` and the edit hook use one wording for a
  long comment block.
- **`chock explain`.** A reason for `CANNOT RUN` prints once. A name that is no gate gets the
  list of gates, as `chock run` gives it. `chock enable` reads right for one gate.
- **Docs.** The README is a short front page. The guide, the gate reference, the configuration
  and the hooks and CI pages are under `docs/`.
- **Release.** One workflow runs on a tag. It publishes the crate only after every gate passed on
  Linux, macOS and Windows and each prebuilt binary started from its archive.

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
