# Changelog

Notable changes to chock. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-10-07

No command or flag is removed or renamed. The JSON reports add fields and remove none.

- **`splits`, a new gate.** It names a part of a file, 100 lines or more, that only one private
  item uses. That part can be a module of its own. The finding gives the part's lines and the item
  that leads into it. A `pub` item is never such a lead, because a move needs a re-export.
- **`lean`, a new gate.** It counts the private functions that only pass their parameters on to
  another call, unchanged and in order. The caller can make that call itself. The finding names
  each such function and the call to make in its place.
- **Leads that never trip a gate.** Where the source alone cannot settle a shape, `lean` reports
  a finding with `grade` set to `candidate`: a function of up to five lines with one statement
  and one caller, two arms next to each other with one body, a private trait with one `impl`.
- **Findings say where and what to do.** A finding in the JSON report can carry `places`, the
  other places it is about, and `fix`, the change that clears it. `duplication`, `complexity`,
  `splits` and `lean` fill them in. `schema/run-v1.json` now names both fields and `grade`, and
  a test holds the schema to the report.
- **Both new gates are on for a new project.** A project set up before 0.2.0 keeps its list:
  `chock enable splits lean` switches them on, and the next run outside CI writes their records.
- **More gates keep their verdict.** `coverage`, `crap`, `miri` and `bsize` now keep a verdict
  under a key of everything they read, as `test`, `mutest` and the other slow gates did. A later
  run or a hook recalls it, and the row says `recalled`. `chock run --no-cache` judges every
  gate again, and `chock cache clear` removes the kept verdicts. A project with its own coverage
  command names the tools that command calls under `coverage_tools`.
- **`chock run` writes the first record.** A gate with no record takes it from its first run
  outside CI, and the run prints `wrote the first record for …`. `chock baseline` is no step of
  the setup now; it stays for a record that you raise on purpose. CI writes no record, so a
  ratchet with none is `CANNOT RUN` there.
- **Touched files are held to zero from the first day.** `chock init --local` lists under
  `clean_when_touched` each ratchet that it switched on, where the ratchet keeps a number for
  each file and measures the same on every machine. A project that has a config keeps it.
- **A `clean_when_touched` entry that can hold nothing is refused.** A gate whose record is for
  the whole project, such as `binsize`, passed that list in silence before.
- **`chock init --global` waits out a dropped link.** A rustup or git download that the network
  fails is tried again after 10 s and after 30 s. A third failure names the command, the error and
  what to check. The cargo-mutest build prints each step, so a long step does not look like a hang.
- **Miri runs chock's own suite.** Each test that Miri cannot run, or did not end in 15 minutes,
  is marked with the reason.
- **A compiler crash leaves no file in the project.** rustc wrote `rustc-ice-*.txt` into the
  tree, and `typos` read it on the next run. Now the crash goes to the gate's output only, unless
  `RUSTC_ICE` names a place.
- **`miri` names a stuck or failed test.** Each test has five minutes under Miri. A test past
  that stops, and the finding names it. Before, one stuck test used the whole run's limit and was
  not named. A failed assertion is named too; before, the gate said that it could not run. A
  `default-miri` profile in the project's own nextest settings still wins.
- **`mutest` joins its targets.** A mutation is a survivor only when every target that ran it
  missed it. Before, a miss in one target counted even when another target detected it.
- **`mutest` names each mutation that timed out.** Each one is a finding at its file and line,
  with `grade` set to `candidate`. Before, one line gave only a count.
- **`chock init --global` needs no project.** Outside a project, or in one with no
  `tool-versions.env`, it installs the pins this chock ships. Before, it stopped with "cannot
  read", so the first step that the README gives each machine failed.
- **A release checks the runs on `main`, and does not repeat them.** A `v*` tag publishes only
  when the `ci.yml` run on `main` for the tagged commit passed on the three systems and its
  `corpus.yml` run passed on the nine projects. Each push to `main` starts both.
- **`miri` runs one part of its suite.** `chock run --miri-partition=K/N` runs part K of N, as
  nextest's `count:K/N` partition splits the tests. CI can run the N parts as jobs at once. Each
  part keeps its verdict under a key of its own.
- **`crap` leaves out `build.rs`.** No test can reach a build script, so its score says nothing
  about the tests.
- **The run lock names its holder.** A second run gives the first run's pid, command and how long
  it has held the lock. It advises a delete of the lock only when that run may have exited.
- **chock runs on code that its authors did not write.** The `corpus` workflow, started by hand,
  runs every gate twice on nine public Rust projects at pinned commits. It fails on a crash, a
  gate with no row, or a gate that passed and then tripped on the same tree.

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
