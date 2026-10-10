# Changelog

Notable changes to chock. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.1] - 2026-10-10

Two gates of 0.5.0 gave wrong findings. No command, flag or JSON field changes. A record of
`testlint` or `claims` can only go down.

- **`testlint` reads only the files that a cargo target compiles.** 0.5.0 read each `.rs` file,
  so a UI fixture with `#[test] fn test()` counted as a test of the project, and each new fixture
  tripped the gate. A file that no target reaches by `mod`, `#[path]` or `include!` is data now.
- **`ambiguous-test-name` counts the tests of one name.** The finding lists at most 3 of them.
  0.5.0 listed each one, which gave 22861 lines for one tree.
- **`claims` reads a path that starts at `src/` from a root.** 0.5.0 matched each cited path by
  its end, so a note about another repository that cited a line of a `src/` file was checked
  against a file of that name in a crate of this tree. A path that starts at `src/`, `tests/`,
  `examples/` or `benches/` now names a file from the root of the tree, or from a directory above
  the document. Any other path still names the one file that ends in it.

## [0.5.0] - 2026-10-10

No command or flag is removed or renamed. The JSON report of `chock run` does not change. Two
gates are new: `testlint` and `claims`. Two commands are new: `chock sweep` and `chock moved`.

- **`testlint`, a new gate for tests that protect nothing.** It reads each `#[test]` function of
  the tree and reports, by file and rule: a test that asserts nothing, an assertion that is always
  true, `#[ignore]` with no reason, `#[should_panic]` with no `expected` text, a placeholder name,
  a name of one word, a name that two tests have, a test that reaches the shared temp directory
  or the home directory, and a shell script in `bin/` or `scripts/` that pipes with no `pipefail`.
  A test asserts through a helper of the test code too, also when it gives the helper to a runner
  by name. `// test-lint: allow(<rule>) — <reason>` in a test leaves one rule out for that test.
- **`claims`, a new gate for documents that the tree contradicts.** It reads each `.md` file and
  reports a code span that cites a line past the end of its file. A comment `absent:` lists the
  symbols and paths that the tree does not hold, and the gate reports each one that the tree does
  hold. `absent-by-design:` needs a reason. `doc-check: foreign` marks text about another tree.
  A fenced block claims nothing.
- **Both new gates are ratchets and are on for a new project.** `chock init` switches them on in
  a project that an older chock set up. The first run writes their records, so the debt that a
  tree already holds does not trip; new debt does.
- **`chock sweep REV [PATH...]` proves that a change touched only comments.** It compares the
  tokens of each `.rs` file with its tokens at `REV`. Each file is `clean`, `docs only` or `CODE`,
  and the report gives the first token that differs. It exits 1 when code changed.
- **`chock moved REV PATH...` proves that code only moved between files.** It pools the tokens of
  the named files now and at `REV`, and lists each token that the move lost or added. It exits 1
  when a token was lost.
- **chock is smaller.** 24 private functions that only passed their parameters on are gone; each
  caller now holds the one call. No output changes.

## [0.4.0] - 2026-10-09

No command or flag is removed or renamed. The JSON report of `chock run` does not change. Two
commands are new: `chock lean` and `chock oracle`.

- **`chock lean [DIR]` lists every line a tree could lose.** The `lean` gate reports only what a
  change adds. The command reads the whole tree, with no config and no record, and ranks the
  files by the lines that could go. It finds five kinds of place: a function that only passes its
  parameters on, code repeated in one shape, a module that only re-exports, a trait with one
  implementation, and a struct that mirrors another one through a `From`. Each place has a fix
  and says if it is a fact (`exact`) or a shape that a person must judge (`estimate`). `--tests`
  counts repeated test code, `--min N` leaves out the small files, and `--json` prints each file.
- **`chock oracle` compares two builds of one program.** It runs the old build and the new build
  over the same scenarios, each in its own copy of a fixture with a fixed environment. It compares
  the exit code, stdout and stderr of each step, the output of each probe, and the files that
  each build left. Rules replace text that two honest runs print differently, and an allow file
  holds each difference that a person accepted. It exits 0 when each scenario is equal or allowed,
  1 for a difference, and 2 when a scenario could not be compared.
- **A sequence for an agent that cuts code.** `docs/agents.md`, which `chock init` installs as
  `.chock/agents.md`, gives the steps: `chock lean --json`, one change for each place, a person
  approves, `chock oracle`, `chock run`. chock calls no model and applies no change.
- **`chock init` merges into your `deny.toml`.** Before, a project with its own `deny.toml` got a
  `deny.toml.chock` beside it on each run. Now chock adds each key and table that it ships and
  the project does not set, and keeps each line of the project. A `deny.toml` that already has
  them is `unchanged`.
- **`chock init` updates `.chock/agents.md` in place.** Before, a contract from an older chock
  got an `agents.md.chock` beside it. Now chock replaces a file that it wrote, and keeps a file
  that the project wrote under that name.
- **`lean` reads past a file that does not parse.** Before, the first such file stopped the gate
  with no findings. Now the gate shows what every other file holds and names each file it could
  not read. It still exits 2 and records nothing, and `chock baseline lean` still refuses.
- **`miri` does not stop a slow test on a slow machine.** Each test had five minutes under Miri,
  and a group stopped after 6 minutes with no test ended. A test that took 90 s on a 32-core
  machine took more than five minutes on a Windows CI runner, so the gate tripped there on correct
  code. Now one test may run for `CHOCK_TIMEOUT`, 30 minutes by default. A test that runs past
  it is stopped and named after that one wait: chock reads the name from the output of the group
  it stopped, and nextest runs only the other tests of that group again. Under nextest the limit
  is one minute less, and a `default-miri` limit in the project's own nextest settings wins there.
- **The release binary is built with `opt-level = "z"`.** It was `"s"`. The two commands add
  code, and with `"s"` the Linux binary is 3.85 MB, which is over the `binsize` limit. With `"z"`
  it is 3.61 MB, so no record changes. `chock lean` on chock's own tree takes 0.03 s longer.
- **One compiler process for every gate: decided against.** `docs/gates.md` gives the reasons.
  Each compiler tool needs a different build of the same source, and most of a run is code that
  executes.

## [0.3.0] - 2026-10-08

No command or flag is removed or renamed. The JSON reports do not change. The `lean` record
changes its unit, so a project that recorded `lean` runs `chock baseline lean` once.

- **`lean` finds code that could be written once.** Before, it counted only the private functions
  that pass their parameters on. Now it also reads each statement and match arm as a shape, where
  names and literals are values. A run of 3 lines or more that repeats, with at most 4 values that
  differ, is a group. The finding names the merge: one function with a parameter for each value,
  one table for copies side by side, or a call to a function whose whole body is one copy. It gives
  the lines that the merge removes, less the lines that the function and its calls add. Each copy
  is a place, `copy i of k` or `row i of k`. A group counts when it removes 6 lines or more.
- **`lean` reads tests.** Production code, the tests in each `src` directory and the tests in each
  `tests` directory are apart: no group mixes them. Two more merges are named: one function that
  takes the one statement that differs as a closure, and one test that loops over a table of
  cases where whole test bodies repeat.
- **`lean` counts what rustfmt writes.** A function merges at most 6 values that differ, and a
  table at most 12, one column each. A call or a row wider than 88 columns costs a line for each
  value and 2 more, because rustfmt breaks it over lines. A copy can bind at most 3 names that
  later code reads, and the merged function returns them.
- **`lean` counts removable lines.** Each file holds its share of each group, and the lines of each
  function that only passes its parameters on. The unit is now `removable line(s)`. A record in
  the old unit, `forwarders`, makes the gate report that it cannot run, and names the command.
- **`lean` leaves out what a merge cannot take.** Copies that differ in a string that a macro
  reads do not merge, except format strings with the same placeholders: the function passes the
  text that differs to a placeholder of its own. Nor do copies where later code reads more than 3
  names that they bind. A file that a tool wrote is left out: a part of its path is `generated`, or
  one of its first 5 lines says so.
- **Not yet in `lean`.** A production function that only tests call, and copies of the fields of
  one struct that a `derive` would write.
- **A project in a subfolder of a repository gets its hooks.** Before, `chock init --local` in a
  folder such as `crates/tova` wired no hook, because it found no `.git` there. Now it declares
  each hook at the top of the repository under a name of its own,
  `hook.chock-pre-commit@crates/tova`, which runs `chock hook pre-commit --project crates/tova`.
  `--project` moves the hook into that folder before it runs. Each project in one repository keeps
  its own hooks, and `chock doctor` checks only its own. This needs git 2.54. An Outpost repository
  gets an entry `chock@<path>` in `.outposthooks.toml`.
- **`chock init` moves its pins in `tool-versions.env` in place.** Before, a pin file that the
  project had changed was rewritten in chock's order. Now each line that chock pins takes chock's
  value where it stands, with its comment. The project's own lines stay as they are, and a pin
  that the file did not hold is added at the end.
- **`chock init` switches on a passing gate that is newer than the config.** Before, a project
  whose config an older chock wrote kept new gates such as `splits` and `lean` off. Its
  `wiring` gate then refused the next commit. Now, where the config turns `wiring` on, init tries
  each gate that is off and needs no compiler, and switches on each one that passes. It names them.
  Where `wiring` is off, init does not change the config.
- **Miri groups take every n-th test.** Before, each group held 32 neighbouring tests, so the slow
  tests of one module shared a group. On chock's own suite with 32 cores, one group took 203 s,
  and the others waited for it. Now each group takes every n-th test of the list. From the measured
  test times, the groups take about 100 s on 32 cores. On 3 or 4 cores the time does not change.
- **`miri` prints one summary line.** It names the build time, the number of groups, how many ran
  at once, and the sum of the test times.
- **The documents match the code.** The CI and hooks guide describes the declared git keys, older
  git, Outpost hooks, Miri groups, `--fast --ci`, `CHOCK_TIMEOUT` and `CHOCK_JOBS`. The guide lists
  every report field and every file `init` writes. A check found 23 points; the others are
  corrected in the README, the gate list, the configuration keys and the agent skill.
- **Tests that start git stay out of Miri.** Miri cannot start a process, so these tests are
  ignored under Miri, as the other tests that start a process are.
- **`chock init --local` writes no `justfile`, and `just` is not pinned.** chock is the runner:
  `chock run <gate>` runs each gate. A project can delete the `justfile` that init wrote before.
- **`fmt` leaves out files outside the project.** `cargo fmt --all` also checks path dependencies,
  and one in another repository is not this project's to format. If the output was cut before it
  named a file in the project, the gate cannot run.
- **CI tries each nightly install 3 times.** A download from static.rust-lang.org failed once on
  a runner's network.

The tests and flags of the other gates do not change.

## [0.2.0] - 2026-10-07

No command or flag is removed or renamed. The JSON reports add fields and remove none.

- **`splits`, a new gate.** It names a part of a file, 100 lines or more, that only one private
  item uses. It reads files of 200 to 1000 production lines; `bigfiles` names the parts of a
  larger file. That part can be a module of its own. The finding gives the part's lines and the item
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
- **`miri` compiles the crate once for each group of tests, not once for each test.** Under
  nextest, each test started its own Miri process, and each process compiled the crate again. Now
  one Miri process runs up to 32 tests of one binary, and the groups run at the same time on the
  lane's cores. The tests and flags are the same. A group that fails or stops runs again under
  nextest, one test to a process, so the finding names each test as before. On 53 of chock's own
  tests, the work went from 169 s to 74 s.
- **`miri` counts the memory of one interpreter, not one build job.** The limit was one group for
  each 2 GB free, so a 7 GB Mac with 3 cores could not use all its cores. Now the limit is one group
  for each 512 MB free. Chock's own suite peaked at 5968 MB in all on 32 cores.
- **`miri` stops a group that waits on one test past its limit.** A group waited up to 30 minutes
  on a test that ran too long. Now it stops after 6 minutes with no progress, and nextest runs its
  tests under their own 5-minute limit. On smoltcp, Miri took 2312 s.
- **`miri` stops when a group shows that Miri cannot emulate the code.** Before, nextest ran the
  group's tests again and met the same refusal. The result is `cannot_run` in both cases. On
  progenitor, the gate took 740 s to give that answer.
- **`miri` has no limit on the whole run.** The run stops only when no test ends and no crate
  builds for `CHOCK_TIMEOUT` (30 minutes). Before, a suite or a CI part with many tests failed at
  30 minutes in all, even when each test kept to its own limit.
- **`mutest` has no limit on the whole run.** The run stops only when mutest prints no line for
  `CHOCK_TIMEOUT` (30 minutes). Before, chock's own mutest on macOS stopped at 30 minutes in all,
  while it still confirmed its timeouts one by one.
- **`mutest` and `miri` run beside the other gates when they fit in memory.** Each gate's report
  now holds `peak_mb`, the most memory its processes held at once (Linux only). `mutest` and
  `miri` build in directories of their own. On the next run, each takes a lane of its own when
  the recorded peaks of all that would run at once fit in three quarters of the free memory. A
  gate with no record runs in turn, as before. The `miri` report also holds `tests_ms`, each
  test's time, so the next run starts the slowest tests first.
- **chock reads the free memory on macOS and Windows.** Before, it read it only on Linux, so a run
  on macOS or Windows ran one build job at a time. Now `vm_stat` on macOS and a performance counter
  on Windows give it, and the job count follows the cores and the memory, as on Linux. With
  `CHOCK_JOBS` set, chock does not read the memory.
- **chock's own CI runs the whole Miri suite as one job on each system.** That job takes the time a
  user's `chock run miri` takes on one machine. One push now starts 10 jobs. Before, it started 55,
  and 45 of them were parts of the Miri suite.
- **A run longer than the deadline keeps its lock.** The holder refreshes the lock while it runs.
  Before, a second run took over the lock of a live run after 30 minutes.
- **`mutest` trusts a timeout that it confirmed.** A mutest that re-runs each timed-out mutation
  alone with a longer limit prints `timeouts confirmed:`. Each timeout left is then a hang, and
  the finding says so. The 2% limit on timeouts applies only to a mutest without this line.
- **`chock run --skip=GATE,...` leaves gates out of a run.** CI can then run a slow gate in a
  job of its own, at the same time as the job with every other gate. chock's own CI runs `miri`
  and `mutest` this way. A name that is not a gate is refused.
- **`mutest` joins its targets.** A mutation is a survivor only when every target that ran it
  missed it. Before, a miss in one target counted even when another target detected it.
- **`mutest` names each mutation that timed out.** Each one is a finding at its file and line,
  with `grade` set to `candidate`. Before, one line gave only a count.
- **`chock init --global` needs no project.** Outside a project, or in one with no
  `tool-versions.env`, it installs the pins this chock ships. Before, it stopped with "cannot
  read", so the first step that the README gives each machine failed.
- **A release checks the runs on `main`, and does not repeat them.** A `v*` tag publishes only
  when the `ci.yml` run on `main` for the tagged commit passed on the three systems and its
  `corpus.yml` run passed on the nine projects. Each push to `main` starts `ci.yml`, and a
  release starts `corpus.yml` by hand on the same commit.
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
