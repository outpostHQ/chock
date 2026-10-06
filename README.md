# ▰ chock

[![crates.io](https://img.shields.io/crates/v/chock.svg)](https://crates.io/crates/chock)

**Quality gates for Rust: new code must be clean, and existing debt can only go down.**

chock has 53 checks for a Rust project. It runs the ones that are on and gives one result and one
exit code. The checks include your tests, clippy, rustfmt, coverage, mutation testing, dependency
and supply-chain checks, complexity and duplication.

**Start here:** [Quick start](#quick-start) · [How chock decides](#how-chock-decides) ·
[Read a result](#read-a-result) · [Commands](#commands) · [For agents](#for-agents)

**Reference:** [Where it runs](#where-it-runs) · [Checks](#checks) ·
[Configuration](#configuration) · [CI](#ci) · [Update chock](#update-chock) ·
[Requirements](#requirements) · [Limits](#limits)

## Quick start

You need Rust 1.99.0 or newer and a git or [Outpost](https://github.com/outpostHQ) repository.

**1. Install chock and its tools.** Do this once on each machine.

```sh
cargo install cargo-binstall   # skip this if you have it; `brew install cargo-binstall` also works
cargo binstall chock           # prebuilt for Linux, macOS and Windows; `cargo install chock` compiles it
chock init --global            # the tools chock runs, prebuilt, at pinned versions
```

**2. Set up the project.** Do this once in each repository.

```sh
cd your-project
chock init --local             # measures the tree and switches on the checks that pass
chock baseline                 # records today's numbers in .chock/baseline.json
```

**3. Commit the files that chock wrote.**

| file | holds |
|---|---|
| `.chock/config.json` | which checks are on, and your settings |
| `.chock/baseline.json` | the recorded numbers |
| `.chock/agents.md` | how an agent calls chock and reads its answer |
| `justfile` | one recipe per check |
| `tool-versions.env` | the tool versions this project expects |
| `deny.toml` | `cargo deny` configuration |
| `.claude/settings.json` | the editor hook, merged into any settings already there |

**4. Run the checks.**

```sh
chock run          # every check that is on
chock run --fast   # only the checks that need no compiler, in seconds
```

After step 2, chock also runs at each commit and each push. [Where it runs](#where-it-runs) says
which checks run at which step.

Two facts about `chock init --local`:

- **It measures before it switches a check on.** The core checks (`test`, `lint`, `doc`,
  `modcheck`) stay on even when they fail. A quality check that fails is reported and left off;
  adopt it later with `chock enable <check>`. Add `--fast` to measure only the checks that need no
  compiler, which takes seconds instead of a full build.
- **It never overwrites a file you have.** Yours stays, and chock's lands beside it as
  `<name>.chock`. The one exception is `tool-versions.env`; see [Update chock](#update-chock).

## How chock decides

Each check is one of two kinds.

- A **pass/fail check** fails on any problem. Tests, lints, docs, advisories, MSRV and committed
  secrets are of this kind.
- A **counted check** keeps a number for each file or function in `.chock/baseline.json`. Comment
  length, complexity and coverage are of this kind. New code must be clean, a number may not go up,
  and when one goes down chock lowers the record. Debt only ever goes down. The tables under
  [Checks](#checks) mark a counted check as a ratchet.

```
   your tree ──► measure every file and function ──► compare with .chock/baseline.json

   new code with any debt ..................... TRIPPED     exit 1
   a recorded count grew ...................... TRIPPED     exit 1
   a recorded count stayed the same ........... ok          exit 0
   a recorded count went down ................. ok          exit 0, and the record is lowered;
                                                            CI fails until the change commits it
   a tool was missing or its output unreadable  CANNOT RUN  exit 2, never counted as a pass
```

**Example.** Your project has 400 comment blocks that are too long. You switch on `slop` and run
`chock baseline`, which records 400 for the files that hold them. From then on:

- a new file with a long comment block fails;
- one more long block in a file that already has some fails;
- fix 10 of them and the record drops to 390, so the count can never climb back to 400.

To demand zero from day one instead, list the check under `strict` in `.chock/config.json`. To
demand zero only in each file a change touches, list it under `clean_when_touched`; the other files
keep their record.

## Read a result

```
$ chock run test lint modcheck complexity
  test       ok
  lint       ok
  modcheck   ok          0 against 0
  complexity TRIPPED     34 against 26

complexity — chock run complexity
  src/parse.rs: render: 34 cognitive, over the recorded 26
  fix: split the function: move each branch's body into a named helper

1 of 4 check(s) tripped.
```

- **The first block has one line for each check.** A counted check adds what it measured and what
  the record holds: complexity is 34 where the record holds 26.
- **The second block has the findings of each check that did not pass.** Its heading ends with the
  command that runs that one check again. Each finding names the file, the function or lint, and
  the two numbers.
- **`fix:` says how to repair that kind of finding.**

Here the repair is to split `render`. Accepting the debt instead is `chock baseline complexity`,
which raises the record in a file reviewers see in the diff.

| result | in the text | in the JSON | exit code | what to do |
|---|---|---|:-:|---|
| passed | `ok` | `passed` | `0` | nothing |
| failed | `TRIPPED` | `tripped` | `1` | fix the findings |
| could not run | `CANNOT RUN` | `cannot_run` | `2` | read the reason and fix that first |

A check that could not run measured nothing: a tool was missing, a file would not parse, or a
command died. It says nothing about the code, and chock never counts it as a pass. A run exits with
the highest code among its checks.

The report goes to stdout when the run ends. Before that, stderr gets a line as each check that
compiles ends, and a check that does not pass adds its reasons there at once.

### What a run lists, and where the rest is

A run lists what got worse than the record. It does not list the debt the record already holds.
The two numbers on a check's line count that debt, and `chock explain` lists it.

```
   chock run ───────► the findings: what is new, or above its own record
   chock explain ───► all the debt the record holds, largest first, with each fix
```

A check can trip while its total is at or under the record. `897 against 928 in total, 12
finding(s)` says the tree holds less debt than the record, and 12 places are new or above their own
number.

`crap` counts the functions that score over 30. Its findings are each function whose score went up
and each new function over 30. A pass says how many functions the record lacks, because a rise in
one of them passes until it is over 30; `chock baseline crap` records them, and
`chock baseline --lower crap` also records each score that went down.

`chock` holds a function to the record by its file and its name. Where one file has two functions
with one name, such as two `impl From` blocks, `chock` pairs them in file order. Where their number
changed since the record, `chock` compares the scores from the worst down, so a pass means that no
score level holds more of them than the record does.

`cargo crap` with no coverage file scores every function as if no test ran it, so it shows far more
functions than chock does. Give it the coverage file chock wrote to see the same scores:

```sh
cargo crap --lcov lcov.info --workspace
```

### What a check leaves out

A check leaves these out on purpose. Each one has a place that shows it.

| check | what it leaves out | where you see it |
|---|---|---|
| `hazards`, `lenses` | an Outpost finding with the severity `note` | `outpost check` |
| `history` | a leak that `history.accepted` names in the config | the config entry, with its reason |
| `source` | a line that has `<tool>: ignore[<rule>] <why>` on it or on the line above | the comment, with its reason |
| `deps` | a `cargo deny` check that has no section in `deny.toml` | the run names each absent section |
| `unused` | a dependency that a source file of the crate names as a whole word, also in a comment or a string | `cargo machete`; `unused-deep` compiles and finds it |
| `supply` | a new version of a crate that the record already holds | the diff of `Cargo.lock` |
| `features` | a `.rs` file that does not parse and is outside `src/` or read by `include!` | the compiler, where a build uses the file |
| `placement` | a path dependency outside the tree, where the tree is in no repository | the `path` in `Cargo.toml` |
| `mutest` | on your machine, the files that the change did not touch. Where more than 2% of the mutations in the touched files time out, the run mutates the whole crate | the run says which it did; CI mutates the whole crate |
| `mutest` | a mutation that times out counts as detected, for up to 2% of the mutations | the run says how many timed out |
| `modcheck` | the directory of a file that holds the text `chock:modcheck-exempt` | the comment in that file |
| `modcheck` | a `mod` name that a macro builds and no file has; a `.rs` file that does not parse and is outside `src/` or read by `include!` | the compiler, where a build uses it |

A tool can print more than the 16 MiB that `chock` keeps. A list that `chock` reads from such an
output ends with a finding that says the list is not complete. A count is not read from such an
output: the check reports that it cannot run, and its reason says that the output was cut.

## Commands

| command | what it does |
|---|---|
| `chock run` | runs every check that is on |
| `chock run lint complexity` | runs only the checks you name |
| `chock run --fast` | runs only the checks that need no compiler |
| `chock gates` | lists every check: what it measures, its stage, and whether it is on |
| `chock explain complexity` | shows the last run's findings for one check, without running it again |
| `chock explain` | lists all the debt on record, largest first, with how to fix each kind |
| `chock enable <check>` | switches a check on |
| `chock disable <check> --reason "<why>"` | switches a check off and records the reason |
| `chock stage <check> <stage>` | moves a check to another [stage](#where-it-runs) |
| `chock baseline [<check>]` | accepts today's numbers as the record, raising it where they grew |
| `chock baseline --lower <check>` | records a gain on a check that measures differently by machine |
| `chock doctor` | says whether this machine has the pinned tool versions |
| `chock survey` | measures what every check finds here; needs no config and writes nothing |
| `chock init` | sets up the machine and the project; `--global` and `--local` do one half each |

`run`, `gates`, `explain`, `doctor`, `survey`, `edited` and `slop` take `--json`. `chock --help`
lists every command.

## For agents

`chock init --local` writes `.chock/agents.md`, the contract for an agent, which is
[`docs/agents.md`](docs/agents.md) in this repository. It also adds one line that points to that
file in each of `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `.github/copilot-instructions.md` and
`.cursor/rules/chock.mdc` that the project already has.

1. Run `chock run --json`, or `chock run --fast --json` for an answer in seconds.
2. Read `verdict` in each entry of `gates`.
3. For `tripped`, go to the `file` of each finding, fix the code, and run the command in `rerun`.
4. For `cannot_run`, read `cannot_run_reason` and fix that first.
5. Repeat until the run exits `0`.

Three rules:

- **`cannot_run` is not a pass.** Nothing was measured, so exit code `2` says nothing about the
  code.
- **Never run `chock baseline` to turn a check green.** It raises the record, which accepts the
  debt. That is a person's decision, made in a commit a reviewer sees.
- **Commit a lowered record.** A run that measures less says `lowered the record for …` on stderr
  and leaves `.chock/baseline.json` modified. Commit it with your change, or CI fails.

Read the list of checks from `chock gates --json`; never hard-code it. A run's report follows
[`schema/run-v1.json`](schema/run-v1.json). For the project above, `chock run complexity --json`
prints this:

```json
{
  "$schema": "https://raw.githubusercontent.com/outpostHQ/chock/main/schema/run-v1.json",
  "version": 1,
  "chock": "0.1.0",
  "gates": [
    {
      "gate": "complexity",
      "verdict": "tripped",
      "exit_code": 1,
      "measured": 34,
      "baseline": 26,
      "unit": "cognitive",
      "findings": [
        {
          "file": "src/parse.rs",
          "item": "render",
          "measured": 34,
          "baseline": 26,
          "message": "34 cognitive, over the recorded 26"
        }
      ],
      "rerun": "chock run complexity",
      "duration_ms": 0,
      "fix": "split the function: move each branch's body into a named helper"
    }
  ]
}
```

| field | on | says |
|---|---|---|
| `verdict` | a check | `passed`, `tripped` or `cannot_run` |
| `cannot_run_reason` | a check | why nothing was measured; present only with `cannot_run` |
| `rerun` | a check | the command that runs this one check |
| `fix` | a check that tripped | how to repair this kind of finding, where chock has advice |
| `measured`, `baseline` | a check, a finding | the number now and the number on record |
| `file`, `line` | a finding | where to go, from the project root; `line` where the check has one |
| `item` | a finding | the function, lint or measure |
| `message` | a finding | the finding in words |

## Where it runs

```
   while you write     at commit             at push               in CI
  ┌───────────────┐   ┌───────────────┐   ┌───────────────┐   ┌───────────────┐
  │ editor hook   │ ► │ pre-commit    │ ► │ pre-push      │ ► │ chock run --ci│
  │ the file just │   │ stage commit: │   │ stage commit  │   │ every stage   │
  │ written       │   │ checks that   │   │ and push:     │   │ but manual    │
  │               │   │ need no       │   │ + the checks  │   │               │
  │               │   │ compiler      │   │ that compile  │   │               │
  │               │   │ + commit-msg  │   │               │   │               │
  └───────────────┘   └───────────────┘   └───────────────┘   └───────────────┘
     on every edit        seconds             minutes          cannot be skipped
```

- **Editor hook.** `chock edited --hook` runs after every file an agent writes and reports only what
  the commit would refuse for that file: a check that is off, a directory the check never reads, and
  debt the baseline already accepts are all silent. A check under `clean_when_touched` reports the
  accepted debt too, because the edit touches the file. It answers on stderr with exit 2, which is how
  Claude Code hands a hook's answer back to the model. Other editors can pipe a path to it.
- **Git hooks.** On git 2.54 and later they are declared in `.git/config`
  (`hook.chock-pre-commit.command = chock hook pre-commit`) and run beside any hooks you keep in
  `.git/hooks`. Older git gets `core.hooksPath` pointed at `.chock/hooks`. The setting is per clone,
  so each clone runs `chock init --local` once.
- **CI.** `--no-verify` skips both git hooks; CI is the step that cannot be bypassed.

Each check has a stage, and each later step runs it again. A check that needs no compiler starts at
`commit`, and a check that compiles starts at `push`. `chock stage` moves checks when the default
does not suit the project, and `chock gates` shows the stage of each check:

```sh
chock stage binsize ci     # a full release build, too slow for the push on this workspace
chock stage binsize push   # back to the default: chock removes the entry
```

| Stage | Runs at |
|---|---|
| `commit` | the pre-commit hook, the pre-push hook and CI |
| `push` | the pre-push hook and CI |
| `ci` | CI only |
| `manual` | only a `chock run` that somebody starts; `chock doctor` names the check, because nothing enforces it |

## Checks

`chock gates` prints the list from the binary: 53 checks, 31 on by default and 22 you opt into. A
**ratchet** holds new code to zero, fails when a recorded count grows, and locks in a count that
shrinks; the other checks pass or fail outright. A count that the record does not hold fails when
it is above zero. This is true for every ratchet: a `codeslop` lint, a `measures` row and a `lenses`
row included. Coverage, mutation testing, binary size, clippy's
shape lints and the Outpost checks measure differently from machine to machine, so a run reports
their gains and `chock baseline --lower <check>` records them. Coverage, mutation testing, binary
size, `bsize` and `crap` also measure differently on each system, so macOS and Windows keep their
own records, as `coverage@macos` and `coverage@windows`; Linux keeps `coverage`. `crap` keeps its
own file the same way, as `.chock/crap-baseline@macos.json` beside `.chock/crap-baseline.json`.

### On by default

| check | ratchet | fails when |
|---|:-:|---|
| `test` | | a test fails, or the crate has none; the run lists every failing test |
| `lint` | | rustfmt would change a file, or clippy warns; both run, and the run lists both |
| `doc` | | the documentation of a package builds with a warning; the run lists every such package |
| `deps` | | `cargo deny` finds an advisory, a disallowed licence or an unapproved source |
| `msrv` | | the crate stops building on the oldest Rust it declares |
| `crap` | | a function's complexity-times-uncoverage score goes up, or a new function scores over 30 |
| `commits` | | an unpushed commit's subject is too wide, its body repeats the diff, or a tool is credited as author |
| `profile` | | the release profile skips a free win or silences a compiler check |
| `hygiene` | | the repository tracks a secret, a credential or build output |
| `wiring` | | a check passes on this tree but is switched off |
| `modcheck` | ✓ | a `mod` names no file, or more files are reached by no `mod` |
| `manifest` | ✓ | more dependency declarations are unpinned |
| `placement` | ✓ | a dependency is declared outside the repository, or for every build when only tests use it |
| `features` | ✓ | more features are unused or undeclared |
| `sort` | ✓ | dependency tables fall further out of order |
| `dupdeps` | ✓ | one more crate is compiled twice, at two versions or feature sets |
| `unused` | ✓ | one more declared dependency is referred to nowhere |
| `supply` | ✓ | one more dependency runs code at build time |
| `typos` | ✓ | one more misspelling in code, comments or docs |
| `source` | ✓ | one more lint is allowed crate-wide or suppressed without a reason |
| `slop` | ✓ | one more comment block runs past two lines |
| `bigfiles` | ✓ | a file over 1000 production lines grows, or another one crosses 1000 |
| `complexity` | ✓ | a function's cognitive complexity rises |
| `nesting` | ✓ | a function nests deeper, past four levels |
| `codeslop` | ✓ | one of clippy's code-shape lints fires more often, or fires for the first time |
| `duplication` | ✓ | one more function body is a copy, or close enough to merge |
| `coverage` | ✓ | a file gains lines no test executes; the run names every line of that file no test ran |
| `binsize` | ✓ | a release binary grows by more than 1% or 256 KiB, whichever is larger |
| `unsafety` | ✓ | a file gains an `unsafe` block, function, trait, impl or `extern` |
| `citations` | ✓ | a doc comment names one more path the repository does not have |
| `assertions` | ✓ | a test gains an assertion about *how many* instead of *what* |

### Opt-in

Switch one on with `chock enable <check>`. `chock disable <check> --reason "<why>"` switches a check
off and records the reason in `left_off`.

| check | ratchet | fails when |
|---|:-:|---|
| `mutest` | ✓ | a mutation changes a decision and no test notices, one more time per file and operator |
| `duplicates` | ✓ | the same job is written twice in different shapes, once more |
| `commands` | ✓ | a check you declared fails, or counts more than it held |
| `commands-build` | ✓ | the same, for your declared checks that need a compiler |
| `phrases` | ✓ | source gains a phrase your config forbids |
| `dead` | | a private function nothing in the crate names |
| `acl` | | a dependency reaches an API its `cackle.toml` entry does not grant |
| `idempotent` | | the suite passes once and fails the second time |
| `proof` | | a Kani proof that verified no longer does |
| `miri` | | a test fails under Miri; the run lists every failing test |
| `unused-deep` | | a real (nightly) compile shows an unused dependency |
| `fmt` | | rustfmt would change a file; needs no compiler, so a commit can run it |
| `history` | | any commit holds a credential, not only the current tree |
| `unread` | | never; reports regions Outpost could not parse |
| `bsize` | | never; reports where a binary's bytes went |

The Outpost checks, also opt-in, read one `outpost check`, which resolves references across the
whole tree.

| check | ratchet | fails when one more… |
|---|:-:|---|
| `padding` | ✓ | stretch of shipped code is longer than the tree's own density explains |
| `boundaries` | ✓ | file sits outside its directory's concern, or group spreads across directories |
| `scan` | ✓ | known defect pattern appears, or untrusted input reaches a sink |
| `hazards` | ✓ | spot of syntax is about to be wrong, such as a value spliced into a command |
| `unreferenced` | ✓ | piece of shipped code is reached by nothing, or only by tests |
| `lenses` | ✓ | hazard lens stops reporting, or lens with no record reports a hazard |
| `measures` | ✓ | Outpost measure grows, stops arriving, or arrives above zero with no record |

Mutation testing is the slowest check, about four minutes on chock itself. Every test of every
mutation runs in its own process, so one mutation that aborts cannot end the run.

- **Names that sound alike.** `slop` measures comment length; `codeslop` counts clippy's code-shape
  lints. `unused` and `unused-deep` each find dependencies the other misses.
- **When `bigfiles` and `boundaries` disagree.** Splitting a file for `bigfiles` can leave the new
  file calling helpers that stayed behind, and `boundaries` counts that edge. Split along a concern,
  and move what both halves need to where both can reach it.
- **`acl` asks for more grants than you might expect.** cackle pre-approves only the one-colon
  `cargo:` build-script instructions, so a crate printing `cargo::` needs its own grant, and it asks
  for proc-macro grants across the whole dependency graph.

## Configuration

`.chock/config.json` holds what no measurement can answer. The full shape is
[`schema/config-v1.json`](schema/config-v1.json).

| key | what it says |
|---|---|
| `runner` | the command that runs your tests, if not `cargo nextest run --workspace --no-tests=fail --no-fail-fast`; your command must also run every test after a failure, or the run lists only the first |
| `coverage` | the command that writes your coverage report; `{lcov}` marks where chock reads it |
| `message` | the widest commit subject and the longest body allowed |
| `stage` | checks that run at another step than their default; `chock stage` writes it |
| `local_only` | checks CI cannot run; `chock run --ci` leaves them out |
| `runner_tools` | tools your `runner` calls, so a cached verdict knows to re-run when they change |
| `not_shipped` | members nobody receives (fuzz harnesses, benchmarks), held to test-code rules |
| `miri` | which packages Miri runs over, and its flags |
| `commands` | your own checks: a `name` and a `run` command; one that `counts` is ratcheted per item |
| `forbidden` | phrases source must not contain, each with the reason its finding gives |
| `history` | `accepted`: credentials published on purpose, such as a test key, each named by `commit`, `path` and `rule` with the `reason` it is safe; `history` no longer reports them |
| `vcs` | `git` or `outpost`, for a tree both hold, when you want to name the one chock reads |
| `target`, `profile` | the target triple and cargo profile you ship, where the host and `release` are not; a tool that cannot be told refuses |
| `features`, `all_features`, `no_default_features` | the Cargo features chock's own compiler commands build with; your `runner` and `coverage` commands stay as you wrote them |
| `left_off` | default checks that are off, each with why; `init`, `enable` and `disable` keep it, and `doctor` names any gap |
| `strict` | ratchets held to zero rather than to the record: any finding fails, and no baseline is needed |
| `clean_when_touched` | ratchets held to zero in each file the change touches; the other files keep their record |

The `phrases` check reads every file `slop` reads, ignores case, and counts each phrase per file
against the record. `word` matches only a whole word; `except` lists paths where the phrase is allowed:

```json
"forbidden": [
  { "text": "utilize", "why": "Write \"use\".", "word": true },
  { "text": "dbg!", "why": "Remove debug output.", "except": ["examples/**"] }
]
```

## CI

Install the pinned tools, then run every check but the `local_only` ones:

```yaml
- uses: cargo-bins/cargo-binstall@main
- run: cargo binstall -y chock
- run: chock init --global   # the tools, prebuilt, at the versions in tool-versions.env
- run: chock run --ci
```

`chock init --global` installs [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) first,
then every other tool from its own prebuilt release, in minutes; a tool with no release for the
platform is compiled. [`SECURITY.md`](SECURITY.md) says what that trades.

## Update chock

```sh
cargo binstall chock   # or `cargo install chock`
cd your-project
chock init             # moves the pins in tool-versions.env to this chock, and installs the tools
chock run              # a newer tool can count differently
```

Commit `tool-versions.env` with any baseline that changes. `chock init` sets each pin that chock
sets to the new chock's version, and keeps each key chock does not set. chock does not move a file
that names a newer chock than itself, and `chock init --global` does not replace a newer chock with
an older pin.

## Requirements

- **Rust 1.99.0 or newer**, and a linker (`build-essential` on Debian and Ubuntu, `xcode-select
  --install` on macOS, the MSVC Build Tools on Windows). CI runs every check on all three.
  On a system a check's tool does not run on (`acl` off Linux, `proof` on Windows), `chock run`
  leaves that check out and says so; a Linux run enforces it.
- **A git or Outpost repository.** `commits`, `hygiene` and `history` read it; outside one they
  report that they could not run. A crate below its repository's root is read as its own part of
  that repository.
- `chock init --global` installs the other tools at the versions in `tool-versions.env`. These
  checks need more:

| check | also needs |
|---|---|
| `coverage`, `crap` | `rustup component add llvm-tools-preview` |
| `unused-deep`, `miri` | a nightly toolchain; `miri` also `rustup +nightly component add miri` |
| `mutest` | the nightly in `MUTEST_NIGHTLY` with `rustc-dev` and `llvm-tools`, and [mutest-rs](https://github.com/outpostHQ/mutest-rs) at `392a078` or later, both binaries installed with `cargo install --path` |
| `proof` | `#[kani::proof]` functions; Linux or macOS |
| `acl` | a `cackle.toml`, and bubblewrap; Linux only |
| Outpost checks | [Outpost](https://github.com/outpostHQ) on `PATH`, with `check --grouping=report` |

## Limits

- **Rust only.** chock reads `.rs` files and `Cargo.toml`.
- **The baseline is trusted.** Anyone can raise a record with `chock baseline`; chock makes that
  visible in the diff, and reviewers have to look.
- **A record is keyed by name.** Renaming a file or function drops its old record and holds the new
  name to zero, so moving debt counts as writing it again.
- **`clean_when_touched` in CI reads one commit.** It compares `HEAD` with its parent, so the
  checkout needs `fetch-depth: 2`. Without the parent, the check cannot run.
- **Two branches that both lower the record can conflict** in `.chock/baseline.json`. Keep the lower
  number for each key.
- **`unused` and `dead` give leads.** Neither sees a dependency used only in a doc example, or a
  function reached only through a name a macro builds; mark such a function `#[expect(dead_code)]`.
- **Fuzzing, property tests and benchmarks are yours to write.** chock runs property tests as
  tests, and has no benchmark gate, because one timing on a shared machine is noisier than the
  regression it would look for.

## Licence

Apache-2.0 — Copyright 2026 Outpost Innovations, Inc. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
