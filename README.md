# ▰ chock

[![crates.io](https://img.shields.io/crates/v/chock.svg)](https://crates.io/crates/chock)
[![ci](https://github.com/outpostHQ/chock/actions/workflows/ci.yml/badge.svg)](https://github.com/outpostHQ/chock/actions/workflows/ci.yml)
[![licence](https://img.shields.io/crates/l/chock.svg)](LICENSE)

**Quality gates for Rust: new code must be clean, and old debt has a limit that each fix lowers.**

chock checks a Rust project with 70 gates and gives one answer with one exit code. The gates cover
your tests, clippy, rustfmt, coverage, mutation testing, the supply chain, complexity, duplication,
and formal proofs: TLA+ models (TLC, Apalache, TLAPS), Kani and Verus.

Most projects hold debt on the day they adopt chock. You do not have to fix it first. chock writes
today's numbers into a file that you commit, and from then on:

- **New code must be clean.** A new file or function with any debt trips its gate.
- **Old debt cannot grow.** A count above its record trips the gate.
- **Each fix stays fixed.** A lower count lowers the record at the next commit, so the debt cannot
  come back.

```
$ chock run test lint modcheck complexity
  test       ok
  lint       ok
  modcheck   ok          0 against 0
  complexity TRIPPED     34 against 26

complexity — chock run complexity
  src/parse.rs: render: 34 cognitive, over the recorded 26
  fix: split the function: move each branch's body into a named helper

1 of 4 gates tripped (complexity).
```

[Quick start](#quick-start) · [Words](#words-chock-uses) · [Daily work](#daily-work) ·
[When a gate does not pass](#when-a-gate-does-not-pass) · [Old debt](#how-old-debt-goes-down) ·
[Commands](#commands) · [For agents](#for-agents) · [The gates](#the-gates) ·
[Documentation](#documentation)

## Quick start

You need Rust 1.99.0 or newer, and a git or [Outpost](https://github.com/outpostHQ) repository.

```sh
# once on each machine
cargo install cargo-binstall   # skip this if you have it
cargo binstall chock           # prebuilt for Linux, macOS and Windows; `cargo install chock` compiles it

# once in each repository
cd your-project
chock init                     # chooses the gates, installs the hooks, then installs the tools
chock run                      # judges the tree and writes the first record of each gate
```

Then commit the files that chock wrote; `git status` lists them. From now on the git hooks run
chock at each commit and each push, and CI runs it on each change.

Each gate gives one of three answers:

| you see | it means | do this |
|---|---|---|
| `ok` | the gate passed | nothing |
| `TRIPPED` | new code has debt, or old debt grew | fix each finding; its `fix:` line says how |
| `CANNOT RUN` | chock measured nothing: a tool is missing, or a file did not parse | do what the `fix:` line says, then run the gate again |

`chock help` lists every command, and `chock help run` (or `chock run --help`) shows one. When you
mistype a command or a gate, chock names the closest one. The [guide](docs/guide.md) explains each
step and each file.

## Words chock uses

| word | what it means |
|---|---|
| gate | one check, such as `lint` or `coverage`. `chock gates` lists all of them. |
| pass/fail gate | a gate that trips on any problem, such as `test` or `lint` |
| ratchet | a gate that counts debt for each file or function and compares it with the record |
| record | the counts in `.chock/baseline.json` that a ratchet holds the code to; also called the baseline |
| trip | a gate trips when it finds a problem. The run then exits `1`. |
| cannot run | the gate measured nothing. It is never a pass. The run then exits `2`. |
| stage | when a gate runs: at each `commit`, at each `push`, only in `ci`, or only when you name it (`manual`) |
| fast set | the gates of the `commit` stage. None of them compiles, so `chock run --fast` takes seconds. |
| kept verdict | the last answer of a slow gate. A run uses it again while nothing the gate reads changed, and its row says `recalled`. |

## Daily work

```
   edit ──► chock run --fast ──► git commit ──► git push ──► CI
            seconds              pre-commit     pre-push     chock run --ci
                                 hook           hook         nobody can skip it
```

| you want to | run |
|---|---|
| get an answer in seconds while you work | `chock run --fast` |
| run every gate that is on | `chock run` |
| run one gate again after a fix | `chock run complexity` |
| run every gate but one | `chock run --skip mutest` |
| read the last findings of a gate again | `chock explain complexity` |
| see all the debt on record, largest first | `chock explain` |
| see every gate, and whether it is on | `chock gates` |
| know whether this machine has the tools | `chock doctor` |

The hooks run the same gates, so these commands only give you the answer earlier.

## When a gate does not pass

```
                    ┌─ fix the code ................... the normal answer
   TRIPPED ─────────┼─ chock baseline GATE ............ accept the debt: the record rises
                    └─ chock disable GATE --reason WHY  stop the gate: the reason goes into the config

   CANNOT RUN ─────── read the reason, do what `fix:` says, then run the gate again
```

`chock baseline` raises a record, and a reviewer sees the higher number in the diff. Use it only
when the team accepts the debt. The guide lists the [common problems](docs/guide.md#11-common-problems)
and their fixes.

## How old debt goes down

When `chock init` switches a ratchet on, the first `chock run` records the counts that the tree has
today. After that, a record moves in one direction only:

```
   adoption              you fix one function           the debt comes back
   record 6    ───────►  commit: 3 against 6  ───────►  run: 6 against 3
                         the record becomes 3           TRIPPED, exit 1
```

```
$ git commit -m "Split the parser"
45 gates ok.
chock: lowered the record for nesting (6 to 3); .chock/baseline.json is in this commit
```

chock does not make you repair a file that no change touches. Two keys in `.chock/config.json` set
how much each change must repair, for the ratchets that you list in them:

| key | a change passes when | use it |
|---|---|---|
| `clean_when_touched` | each file it touches has no debt left | `chock init` writes it, so the debt leaves each file that the team changes |
| neither | it adds no debt | for a gate whose old files are too large to clean in one change |
| `strict` | the tree has no debt | for a gate that must be at zero now |

chock measures and refuses. It does not change your code.

## Commands

The commands for every day:

| command | what it does |
|---|---|
| `chock run [GATE...]` | runs the named gates; no name means every gate that is on |
| `chock run --fast` | runs only the gates of a commit: quick, and none of them compiles |
| `chock run --skip GATE` | runs every gate that is on but the named ones |
| `chock explain [GATE]` | shows the last findings of one gate; with no name, all the debt on record |
| `chock gates` | lists every gate: what it measures, its stage, and whether it is on |
| `chock enable GATE...` | switches gates on |
| `chock disable GATE... --reason WHY` | switches gates off and records the reason |
| `chock doctor` | says whether this machine has the tools that this project pins |
| `chock baseline GATE` | accepts today's counts as the record, also where they got worse |

`--json` gives machine-readable output, for an agent or a CI job. The guide lists
[every command](docs/guide.md#9-every-command), with the options for CI, for a part of the suite,
and for the special tools.

## For agents

`chock init` writes `.chock/agents.md`, the contract for an agent; it is
[`docs/agents.md`](docs/agents.md) in this repository.

<!-- doc-check: foreign -->
It also adds one line that points to that file in each of `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`,
`.github/copilot-instructions.md` and `.cursor/rules/chock.mdc` that the project already has.
<!-- doc-check: ours -->

1. Run `chock run --json`, or `chock run --fast --json` for an answer in seconds.
2. Read `verdict` in each entry of `gates`.
3. For `tripped`, go to the `file` of each finding, fix the code, and run the command in `rerun`.
4. For `cannot_run`, read `cannot_run_reason`, run `fix` where the entry has one, and run it again.
5. Repeat until the run exits `0`.

Three rules:

- **`cannot_run` is not a pass.** Nothing was measured, so exit code `2` says nothing about the
  code.
- **Never run `chock baseline` to make a gate pass.** It raises the record, which accepts the debt.
  That is a person's decision, made in a commit that a reviewer sees.
- **Let a commit lower a record.** The pre-commit hook writes each lower count into
  `.chock/baseline.json` and adds the file to the commit. Without the hook, run
  `chock baseline --lower GATE` and commit the file, or CI fails.

Read the list of gates from `chock gates --json`; never hard-code it. A run's report follows
[`schema/run-v1.json`](schema/run-v1.json), and the [guide](docs/guide.md#8-json-output) shows one
with each field explained.

## The gates

70 gates: 40 are on by default and 30 are opt-in. `chock gates` prints the list from the binary,
and [docs/gates.md](docs/gates.md) says when each one trips.

| topic | on by default | opt-in |
|---|---|---|
| Tests | `test`, `coverage`, `crap`, `assertions`, `testlint`, `admits` | `mutest`, `idempotent`, `miri` |
| Proofs and models | `links` | `tla`, `proof`, `verus`, `laws` |
| Lints and format | `lint`, `codeslop`, `source`, `typos` | `fmt`, `shellcheck` |
| Docs and comments | `doc`, `slop`, `citations`, `claims`, `pinned` | `phrases` |
| Code shape | `complexity`, `nesting`, `bigfiles`, `splits`, `lean`, `duplication`, `unsafety`, `modcheck` | `duplicates`, `dead`, `lines` |
| Dependencies | `deps`, `manifest`, `placement`, `features`, `sort`, `dupdeps`, `unused`, `supply` | `unused-deep`, `acl` |
| Build and release | `msrv`, `profile`, `toolchain`, `binsize` | `bsize` |
| Repository | `commits`, `hygiene`, `wiring`, `workflows` | `history` |
| Your own checks | | `commands`, `commands-build`, `seams`, `owners`, `calls` |
| [Outpost](https://github.com/outpostHQ) | | `padding`, `boundaries`, `scan`, `hazards`, `unreferenced`, `lenses`, `measures`, `unread` |

Five gates do [formal verification](docs/gates.md#proofs-and-models). `tla` checks each TLA+ model
with TLC, Apalache and TLAPS, `proof` each Kani harness, and `verus` each Verus proof; a planted
fault must make each one fail. `links` holds each `code:` comment in a model or a proof to the item
that it names. `laws` pins the text of each theorem, contract and harness, so a weaker law cannot
pass as the same one. `admits` counts each proof step that nothing checks. chock checks its own run
lock, baseline writes and job counts this way.

## Documentation

| page | read it for |
|---|---|
| [Guide](docs/guide.md) | the first install, the daily loop, how to read a result, every command, common problems |
| [The gates](docs/gates.md) | when each gate trips, what it needs, and what it leaves out |
| [Configuration](docs/configuration.md) | every key of `.chock/config.json`, `strict`, your own checks |
| [Hooks, stages and CI](docs/ci-and-hooks.md) | which gate runs at which step, the CI job, how to update chock |
| [For agents](docs/agents.md) | the contract that `chock init` puts into each project |
| [Changelog](CHANGELOG.md) | what changed in each version |

## Limits

- **Rust only.** chock reads `.rs` files, `Cargo.toml`, and the TLA+ and Verus files next to them.
- **The baseline is trusted.** Anyone can raise a record with `chock baseline`; chock makes that
  visible in the diff, and reviewers have to look.
- **A record is keyed by name.** Renaming a function, or splitting a file, drops the old record and
  holds the new name to zero, so moving debt counts as writing it again. A renamed file keeps its
  record with `chock baseline --moved OLD=NEW`.

`chock run --ci` runs every gate that is on but the `local_only` ones, on Linux, macOS and Windows.
Where a gate's tool does not run on a system, the run leaves that gate out and says so.

## Work on chock itself

You need rustup and [just](https://github.com/casey/just). From the root of a clone, `just setup`
installs the pinned Rust, builds this chock, installs every tool that a gate runs at its pinned
version, and wires the hooks. chock's own CI also runs the commit set first, on Linux only.

chock turns on every gate for itself but `shellcheck`, because it has no shell script. `--ci`
leaves out the 10 gates that read Outpost's graph; `chock run` without it runs them too.

## Licence

Apache-2.0 — Copyright 2026 Outpost Innovations, Inc. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
