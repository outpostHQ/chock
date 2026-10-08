# The gates

`chock gates` prints this list from the binary: 55 gates, 33 on by default and 22 opt-in.
`chock gates --json` gives the same list to a program.

A gate is one of two kinds.

- A **pass/fail gate** trips on any problem.
- A **ratchet** keeps a number for each file or function in `.chock/baseline.json`. It holds new
  code to zero, trips when a recorded count grows, and lowers the record when a count shrinks. A
  count that the record does not hold trips when it is above zero.

[On by default](#on-by-default) · [Opt-in](#opt-in) · [Outpost gates](#outpost-gates) ·
[What a gate needs](#what-a-gate-needs) · [Records by machine and system](#records-by-machine-and-system) ·
[How `crap` counts](#how-crap-counts) · [What a gate leaves out](#what-a-gate-leaves-out)

## On by default

| gate | ratchet | trips when |
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
| `wiring` | | a gate passes on this tree but is switched off, and is not opt-in |
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
| `splits` | ✓ | a file gains a part of 100 lines or more that only one private item uses |
| `lean` | ✓ | a file gains a private function that only passes its parameters on to another call |
| `complexity` | ✓ | a function's cognitive complexity rises |
| `nesting` | ✓ | a function nests deeper, past four levels |
| `codeslop` | ✓ | one of clippy's code-shape lints fires more often, or fires for the first time |
| `duplication` | ✓ | one more function body is a copy, or close enough to merge |
| `coverage` | ✓ | a file gains lines no test executes; the run names every line of that file no test ran |
| `binsize` | ✓ | a release binary grows by more than 1% or 256 KiB, whichever is larger |
| `unsafety` | ✓ | a file gains an `unsafe` block, function, trait, impl or `extern` |
| `citations` | ✓ | a doc comment names one more path the repository does not have |
| `assertions` | ✓ | a test gains an assertion about *how many* instead of *what* |

## Opt-in

`chock enable GATE` switches one on. `chock disable GATE --reason "WHY"` switches a gate off and
records the reason under `left_off` in the config.

| gate | ratchet | trips when |
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

Mutation testing is the slowest gate, about six minutes on chock itself. Every test of every
mutation runs in its own process, so one mutation that aborts cannot end the run.

## Outpost gates

These gates are opt-in too. They read one `outpost check`, which resolves references across the
whole tree.

| gate | ratchet | trips when one more… |
|---|:-:|---|
| `padding` | ✓ | stretch of shipped code is longer than the tree's own density explains |
| `boundaries` | ✓ | definition has callers spread past its directory, or directory holds more than one concern |
| `scan` | ✓ | known defect pattern appears, or untrusted input reaches a sink |
| `hazards` | ✓ | spot of syntax is about to be wrong, such as a value spliced into a command |
| `unreferenced` | ✓ | piece of shipped code is reached by nothing, or only by tests |
| `lenses` | ✓ | hazard lens stops reporting, or lens with no record reports a hazard |
| `measures` | ✓ | Outpost measure grows, stops arriving, or arrives above zero with no record |

## Notes on some gates

- **Names that sound alike.** `slop` measures comment length; `codeslop` counts clippy's code-shape
  lints. `unused` and `unused-deep` each find dependencies the other misses.
- **When `bigfiles` and `boundaries` disagree.** Splitting a file for `bigfiles` can leave the new
  file calling helpers that stayed behind, and `boundaries` counts that edge. Split along a concern,
  and move what both halves need to where both can reach it.
- **`acl` asks for more grants than you might expect.** cackle pre-approves only the one-colon
  `cargo:` build-script instructions, so a crate printing `cargo::` needs its own grant, and it asks
  for proc-macro grants across the whole dependency graph.
- **`unused` and `dead` give leads.** Neither sees a dependency used only in a doc example, or a
  function reached only through a name a macro builds; mark such a function `#[expect(dead_code)]`.

## What a gate needs

`chock init --global` installs the tools at the versions in `tool-versions.env`. `chock doctor`
says which of them this machine has. These gates need more:

| gate | also needs |
|---|---|
| `coverage`, `crap` | `rustup component add llvm-tools-preview` |
| `unused-deep`, `miri` | the `nightly` toolchain, and for `miri` its `miri` and `rust-src` components; `chock init --global` installs all three |
| `mutest` | Outpost's fork of [mutest-rs](https://github.com/outpostHQ/mutest-rs) and the nightly it names; `chock init --global` builds the newest commit of the fork's `main`, which needs `git` |
| `proof` | `#[kani::proof]` functions; Linux or macOS |
| `acl` | a `cackle.toml`, and bubblewrap; Linux only |
| `commits`, `hygiene`, `history` | a git or Outpost repository; outside one they report that they could not run |
| Outpost gates, `history`, `duplicates`, `unread` | [Outpost](https://github.com/outpostHQ) on `PATH`, with `check --grouping=report` |

On a system a gate's tool does not run on (`acl` off Linux, `proof` on Windows), `chock run` leaves
that gate out and says so; a Linux run enforces it.

## Records by machine and system

Coverage, mutation testing, binary size, clippy's shape lints and the Outpost gates measure
differently from machine to machine. A run reports their gains, and `chock baseline --lower GATE`
records them.

Coverage, mutation testing, binary size, `bsize` and `crap` also measure differently on each
system. macOS and Windows keep their own records, as `coverage@macos` and `coverage@windows`; Linux
keeps `coverage`. `crap` keeps its own file the same way, as `.chock/crap-baseline@macos.json`
beside `.chock/crap-baseline.json`.

## How `crap` counts

`crap` counts the functions that score over 30. Its findings are each function whose score went up
and each new function over 30. A finding shows the function's complexity and coverage now and on
record, and says which of the two got worse. A pass says how many functions the record lacks,
because a rise in one of them passes until it is over 30; `chock baseline crap` records them, and
`chock baseline --lower crap` also records each score that went down.

chock holds a function to the record by its file and its name. Where one file has two functions
with one name, such as two `impl From` blocks, chock pairs them in file order. Where their number
changed since the record, chock compares the scores from the worst down, so a pass means that no
score level holds more of them than the record does.

`cargo crap` with no coverage file scores every function as if no test ran it, so it shows far more
functions than chock does. Give it the coverage file chock wrote to see the same scores:

```sh
cargo crap --lcov lcov.info --workspace
```

## What a gate leaves out

A gate leaves these out on purpose. Each one has a place that shows it.

| gate | what it leaves out | where you see it |
|---|---|---|
| `hazards`, `lenses` | an Outpost finding with the severity `note` | `outpost check` |
| `history` | a leak that `history.accepted` names in the config | the config entry, with its reason |
| `source` | a line that has `<tool>: ignore[<rule>] <why>` on it or on the line above | the comment, with its reason |
| `deps` | a `cargo deny` check that has no section in `deny.toml` | the run names each absent section |
| `unused` | a dependency that a source file of the crate names as a whole word, also in a comment or a string | `cargo machete`; `unused-deep` compiles and finds it |
| `supply` | a new version of a crate that the record already holds | the diff of `Cargo.lock` |
| `features` | a `.rs` file that does not parse and is outside `src/` or read by `include!` | the compiler, where a build uses the file |
| `placement` | a path dependency outside the tree, where the tree is in no repository | the `path` in `Cargo.toml` |
| `mutest` | on your machine, the files that the change did not touch. Where more than 2% of the mutations in the touched files time out and mutest did not confirm them by a re-run alone, the run mutates the whole crate | the run says which it did; CI mutates the whole crate |
| `mutest` | a mutation that times out counts as detected, for up to 2% of the mutations, or for any number once mutest re-ran each alone (`timeouts confirmed:`) | each timed-out mutation is a `candidate` finding at its file and line |
| `modcheck` | the directory of a file that holds the text `chock:modcheck-exempt` | the comment in that file |
| `modcheck` | a `mod` name that a macro builds and no file has; a `.rs` file that does not parse and is outside `src/` or read by `include!` | the compiler, where a build uses it |

A tool can print more than the 16 MiB that chock keeps. A list that chock reads from such an output
ends with a finding that says the list is not complete. A count is not read from such an output:
the gate reports that it cannot run, and its reason says that the output was cut.
