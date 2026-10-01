# ▰ chock

**Quality gates for Rust: new code must be clean, and existing debt can only go down.**

chock has 53 checks — your tests, clippy, rustfmt, coverage, mutation testing, dependency and
supply-chain checks, complexity, duplication — and gives one result and one exit code.

- **Pass/fail checks** — tests, lints, docs, advisories, MSRV, committed secrets — fail on any
  problem.
- **Counted checks** — comment length, complexity, coverage and the rest — keep a number for each
  file or function in `.chock/baseline.json`. New code must be clean, a number may not go up, and
  when one goes down chock lowers the record. Debt only ever goes down.

**Example.** Your project has 400 comment blocks that are too long. You switch on `slop` and run
`chock baseline`, which records 400 for the files that hold them. From then on:

- a new file with a long comment block fails;
- one more long block in a file that already has some fails;
- fix 10 of them and the record drops to 390, so the count can never climb back to 400.

To demand zero from day one instead, list the check under `strict` in `.chock/config.json`. To
demand zero only in each file a change touches, list it under `clean_when_touched`; the other files
keep their record.

```
   your tree ──► measure every file and function ──► compare with .chock/baseline.json

   new code with any debt ..................... TRIPPED     exit 1
   a recorded count grew ...................... TRIPPED     exit 1
   a recorded count stayed the same ........... PASS        exit 0
   a recorded count went down ................. PASS        exit 0, and the record is lowered;
                                                            CI fails until the change commits it
   a tool was missing or its output unreadable  CANNOT RUN  exit 2, never counted as a pass
```

```
$ chock run
  test       ok
  lint       ok
  modcheck   ok
  complexity TRIPPED     47 against 41

complexity — chock run complexity
  src/parse.rs: render: 23 cognitive, over the recorded 17
  fix: split the function: move each branch's body into a named helper

1 of 4 check(s) tripped.
```

The project's complexity total is 47 where the record holds 41, and one function accounts for it.
Fix `render`. Accepting the debt instead is `chock baseline complexity`, which raises the record in
a file reviewers see in the diff.

**[Install](#install)** · **[Where it runs](#where-it-runs)** · **[Checks](#checks)** ·
**[Configuration](#configuration)** · **[Agents and CI](#agents-and-ci)** ·
**[Requirements](#requirements)** · **[Limits](#limits)**

## Install

You need Rust 1.99.0 or newer and a git or [Outpost](https://github.com/outpostHQ) repository.

```sh
cargo install cargo-binstall   # once, unless you have it; `brew install cargo-binstall` also works
cargo binstall chock           # prebuilt for Linux, macOS and Windows; `cargo install chock` compiles it
chock init --global            # once per machine: the tools chock runs, prebuilt, at pinned versions

cd your-project
chock init --local         # once per repository: measures the tree and switches on what passes
chock baseline             # records where you stand today
```

`init --local` measures before it switches anything on. The core checks (`test`, `lint`, `doc`,
`modcheck`) stay on even when they fail; a quality check that fails is reported and left off for you
to adopt with `chock enable <check>`. `--fast` measures only the checks that need no compiler, which
takes seconds instead of a full build.

It writes these files, and never overwrites one you have: yours stays, and chock's lands beside it
as `<name>.chock`. The one exception is `tool-versions.env`, below.

| file | holds |
|---|---|
| `.chock/config.json` | which checks are on, and your settings |
| `.chock/baseline.json` | the recorded numbers |
| `justfile` | one recipe per check |
| `tool-versions.env` | the tool versions this project expects |
| `deny.toml` | `cargo deny` configuration |
| `.claude/settings.json` | the editor hook, merged into any settings already there |

Commit all of them.

To move a project to a newer chock, install it and run `chock init` in the project. It sets each
pin in `tool-versions.env` that chock sets to the new chock's version, keeps each key chock does not
set, and installs the tools. A newer tool can count differently, so run `chock run` and commit the
pins with any baseline that changes. chock does not move a file that names a newer chock than
itself, and `chock init --global` does not replace a newer chock with an older pin.

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
shrinks; the other checks pass or fail outright. Coverage, mutation testing, binary size, clippy's
shape lints and the Outpost checks measure differently from machine to machine, so a run reports
their gains and `chock baseline --lower <check>` records them. Coverage, mutation testing, binary
size, `bsize` and `crap` also measure differently on each system, so macOS and Windows keep their
own records, as `coverage@macos` and `coverage@windows`; Linux keeps `coverage`. `crap` keeps its
own file the same way, as `.chock/crap-baseline@macos.json` beside `.chock/crap-baseline.json`.

### On by default

| check | ratchet | fails when |
|---|:-:|---|
| `test` | | a test fails, or the crate has none |
| `lint` | | rustfmt would change something, or clippy warns |
| `doc` | | the documentation builds with a warning |
| `deps` | | `cargo deny` finds an advisory, a disallowed licence or an unapproved source |
| `msrv` | | the crate stops building on the oldest Rust it declares |
| `crap` | | a function's complexity-times-uncoverage score gets worse |
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
| `codeslop` | ✓ | clippy's code-shape lints fire more often |
| `duplication` | ✓ | one more function body is a copy, or close enough to merge |
| `coverage` | ✓ | a file gains lines no test executes |
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
| `miri` | | the tests fail under Miri |
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
| `lenses` | ✓ | hazard lens stops reporting |
| `measures` | ✓ | Outpost measure grows or stops arriving |

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
| `runner` | the command that runs your tests, if not `cargo nextest run --workspace` |
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

## Agents and CI

Exit codes are `0` passed, `1` failed, `2` could not run. `run`, `gates`, `explain`, `doctor` and
`edited` take `--json`; a run's report follows [`schema/run-v1.json`](schema/run-v1.json).

```sh
chock run                    # every check that is on
chock run lint complexity    # only these
chock run --fast             # only the checks that need no compiler
chock gates --json           # what exists and what is on; read it, never hard-code a check
chock explain complexity     # the last run's findings, without running it again
chock explain                # all the debt on record, largest first, with how to fix each kind
chock baseline               # accept today's numbers, raising the record; reviewers see it
chock baseline --lower coverage  # lock in a gain on a check that varies by machine
chock doctor                 # does this machine have the pinned tool versions?
```

Each finding carries `file`, `line`, the function or lint, the number measured, the number recorded,
and `rerun`, the command for that one check; a tripped check also says how to fix it, as `fix` in
the JSON and a `fix:` line in the text. An agent goes to `file:line`, fixes it, runs `rerun`.
`cannot_run` means nothing was checked; `cannot_run_reason` says why. The contract for agents is in
[`docs/agents.md`](docs/agents.md).

The report goes to stdout when the run ends. Before that, stderr gets a line as each check that
compiles ends, and a check that does not pass adds its reasons there at once.

In CI, install the pinned tools, then run every check but the `local_only` ones:

```yaml
- uses: cargo-bins/cargo-binstall@main
- run: cargo binstall -y chock
- run: chock init --global   # the tools, prebuilt, at the versions in tool-versions.env
- run: chock run --ci
```

`chock init --global` installs [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) first,
then every other tool from its own prebuilt release, in minutes; a tool with no release for the
platform is compiled. [`SECURITY.md`](SECURITY.md) says what that trades.

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
| `proof` | `cargo install --locked kani-verifier`, and `#[kani::proof]` functions; Linux or macOS |
| `acl` | `cargo install cargo-acl`, a `cackle.toml`, and bubblewrap; Linux only |
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
