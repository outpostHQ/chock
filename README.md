# ▰ chock

[![crates.io](https://img.shields.io/crates/v/chock.svg)](https://crates.io/crates/chock)
[![ci](https://github.com/outpostHQ/chock/actions/workflows/ci.yml/badge.svg)](https://github.com/outpostHQ/chock/actions/workflows/ci.yml)
[![licence](https://img.shields.io/crates/l/chock.svg)](LICENSE)

**Quality gates for Rust: new code must be clean, and old debt has a limit that each fix lowers.**

chock runs 57 gates over a Rust project and gives one result and one exit code. The gates cover
your tests, clippy, rustfmt, coverage, mutation testing, the supply chain, complexity and
duplication.

Most projects hold debt on the day they adopt chock. chock records that debt as a limit for each
file and function, in a file that you commit. The limit is where the project starts. It is not the
goal:

- New code starts at zero. A new file or function with any debt trips its gate.
- Old debt cannot grow, and debt that you removed cannot come back.
- Each fix lowers the limit in the same run. No run raises it.
- `chock explain` lists all the debt that is left, largest first, with the fix for each kind.
- You decide how fast the rest must go: [in each file that a change touches, or in the whole tree
  at once](#how-old-debt-goes-down).

```
$ chock run test lint modcheck complexity
  test       ok
  lint       ok
  modcheck   ok          0 against 0
  complexity TRIPPED     34 against 26

complexity — chock run complexity
  src/parse.rs: render: 34 cognitive, over the recorded 26
  fix: split the function: move each branch's body into a named helper

1 of 4 gates tripped.
```

[Install](#install) · [Use it](#use-it) · [How chock decides](#how-chock-decides) ·
[Old debt](#how-old-debt-goes-down) · [Commands](#commands) · [For agents](#for-agents) ·
[The gates](#the-gates) · [Documentation](#documentation)

## Install

You need Rust 1.99.0 or newer and a git or [Outpost](https://github.com/outpostHQ) repository.

```sh
# once on each machine
cargo install cargo-binstall   # skip this if you have it
cargo binstall chock           # prebuilt for Linux, macOS and Windows; `cargo install chock` compiles it
chock init --global            # the tools chock runs, prebuilt, at pinned versions

# once in each repository
cd your-project
chock init --local             # measures the tree, chooses the gates, installs the hooks
chock run                      # judges the tree and writes each gate's first record
```

Commit the files that chock wrote. After that, chock runs at each commit and each push. The
[guide](docs/guide.md) explains each step and each file.

## Use it

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
| read the last findings of a gate again | `chock explain complexity` |
| see all the debt on record, largest first | `chock explain` |
| see every gate, and whether it is on | `chock gates` |
| know whether this machine has the tools | `chock doctor` |

chock keeps the verdict of each slow gate: tests, coverage, mutation testing, Miri, binary size
and more. The next run recalls it when nothing that the gate reads has changed: the files, the
tools, the config and the record. The row then says `recalled`.
`chock run --no-cache` judges every gate again, and `chock cache clear` removes the kept verdicts.

When a gate does not pass, the result says what to do:

```
                    ┌─ fix the code ................... the normal answer
   TRIPPED ─────────┼─ chock baseline GATE ............ accept the debt: the record rises
                    └─ chock disable GATE --reason WHY  stop the gate: the reason goes into the config

   CANNOT RUN ─────── read the reason, do what `fix:` says, then run the gate again
```

## How chock decides

A gate is one of two kinds.

- A **pass/fail gate** trips on any problem. Tests, lints, docs, advisories, MSRV and committed
  secrets are of this kind.
- A **ratchet** keeps a number for each file or function in `.chock/baseline.json`. Comment length,
  complexity and coverage are of this kind. New code must be clean, and a recorded number cannot
  go up. When a number goes down, chock lowers the record, so the gain stays.

```
   your tree ──► measure every file and function ──► compare with .chock/baseline.json

   new code with any debt ..................... TRIPPED     exit 1
   a recorded count grew ...................... TRIPPED     exit 1
   a recorded count stayed the same ........... ok          exit 0
   a recorded count went down ................. ok          exit 0, and the record is lowered;
                                                            CI fails until the change commits it
   a tool was missing or its output unreadable  CANNOT RUN  exit 2, never counted as a pass
```

A run exits with the highest code among its gates: `0` every gate passed, `1` a gate tripped, `2` a
gate could not run.

## How old debt goes down

When `chock init --local` switches a ratchet on, the ratchet starts with the numbers that the tree
has today. The first `chock run` writes them to `.chock/baseline.json` as the first record. After
that, the record moves in one direction:

```
   adoption              you fix one function           the debt comes back
   record 6    ───────►  run: 3 against 6     ───────►  run: 6 against 3
                         the record becomes 3           TRIPPED, exit 1
```

```
$ chock run nesting
  nesting    ok          3 against 6
chock: lowered the record for nesting; commit .chock/baseline.json with this change
```

Only `chock baseline GATE` raises a record. A person runs it, and a reviewer sees the number rise
in the diff.

chock does not make you repair a file that no change touches. Two keys in `.chock/config.json`
say how much a change must repair, for the ratchets that you list in them:

| key | a change passes when | use it |
|---|---|---|
| `clean_when_touched` | each file it touches has no debt left | `chock init --local` writes it, so the debt leaves each file that the team changes |
| neither | it adds no debt | remove a gate from `clean_when_touched` when its old files are too large to clean in one change |
| `strict` | the tree has no debt | for a gate that must be at zero now |

```json
"clean_when_touched": ["complexity", "nesting"],
"strict": ["unsafety"]
```

`clean_when_touched` holds only a gate that keeps a number for each file. chock refuses a config
that lists another gate there, such as `binsize`, because the key would do nothing for that gate.

`chock explain` lists the debt that is left, with the fix for each kind, so a person or an agent
can work through it. chock measures and refuses. It does not rewrite your code.

## Commands

| command | what it does |
|---|---|
| `chock init` | sets up this project, then installs the tools it pins; `--global` and `--local` do one half each |
| `chock baseline [GATE...]` | accepts today's numbers as the record, also where they got worse |
| `chock baseline --lower GATE...` | records a gain on a gate that measures differently by machine |
| `chock doctor` | says whether this machine has the tools this project pins |
| `chock run [GATE...]` | runs the named gates; no name means every gate that is on |
| `chock run --fast` | runs only the gates staged for a commit; they need no compiler |
| `chock run --ci` | runs the gates a CI job runs; `--fast --ci` runs only the commit set |
| `chock run --skip=GATE` | runs every gate that is on but the named ones |
| `chock run --miri-partition=K/N miri` | runs part K of N of the Miri suite, for a CI job each |
| `chock run --no-cache` | judges every gate again; no kept verdict answers |
| `chock cache clear` | removes every kept verdict, so the next run judges every gate |
| `chock explain [GATE]` | shows the findings of the last run of one gate; with no name, all the debt on record |
| `chock gates` | lists every gate: what it measures, its stage, and whether it is on |
| `chock enable GATE...` | switches gates on |
| `chock disable GATE... --reason WHY` | switches gates off and records the reason |
| `chock stage GATE... STAGE` | sets when they run: `commit`, `push`, `ci` or `manual` |
| `chock survey` | measures what every gate finds here; it changes no config and no record |
| `chock edited PATH...` | checks each file's own text, as the editor hook does; `--hook` reads stdin |
| `chock message FILE` | checks a commit message against the `message` limits |
| `chock slop [DIR]` | lists comment blocks longer than the limit, in any tree |
| `chock lean [DIR]` | lists every line a tree could lose, file by file, largest first; it reads no record and writes none |
| `chock oracle --old A --new B --corpus FILE` | runs two builds of one program over the same scenarios and compares each answer |
| `chock sweep REV [PATH...]` | proves that only comments changed since `REV`, file by file |
| `chock moved REV PATH...` | proves that code only moved between the named files since `REV` |

`chock --help` lists every command, and `--json` gives machine-readable output. The
[guide](docs/guide.md#9-every-command) has the full list.

## For agents

`chock init --local` writes `.chock/agents.md`, the contract for an agent; it is
[`docs/agents.md`](docs/agents.md) in this repository. It also adds one line that points to that
file in each of `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `.github/copilot-instructions.md` and
`.cursor/rules/chock.mdc` that the project already has.

1. Run `chock run --json`, or `chock run --fast --json` for an answer in seconds.
2. Read `verdict` in each entry of `gates`.
3. For `tripped`, go to the `file` of each finding, fix the code, and run the command in `rerun`.
4. For `cannot_run`, read `cannot_run_reason`, run `fix` where the entry has one, and run it again.
5. Repeat until the run exits `0`.

Three rules:

- **`cannot_run` is not a pass.** Nothing was measured, so exit code `2` says nothing about the
  code.
- **Never run `chock baseline` to make a gate pass.** It raises the record, which accepts the debt.
  That is a person's decision, made in a commit a reviewer sees.
- **Commit a lowered record.** A run that measures less says `lowered the record for …` on stderr
  and leaves `.chock/baseline.json` modified. Commit it with your change, or CI fails.

Read the list of gates from `chock gates --json`; never hard-code it. A run's report follows
[`schema/run-v1.json`](schema/run-v1.json), and the [guide](docs/guide.md#8-json-output) shows one
with each field explained.

## The gates

57 gates: 35 are on by default and 22 are opt-in. `chock gates` prints the list from the binary,
and [docs/gates.md](docs/gates.md) says when each one trips.

| topic | on by default | opt-in |
|---|---|---|
| Tests | `test`, `coverage`, `crap`, `assertions`, `testlint` | `mutest`, `idempotent`, `miri`, `proof` |
| Lints and format | `lint`, `codeslop`, `source`, `typos` | `fmt` |
| Docs and comments | `doc`, `slop`, `citations`, `claims` | `phrases` |
| Code shape | `complexity`, `nesting`, `bigfiles`, `splits`, `lean`, `duplication`, `unsafety`, `modcheck` | `duplicates`, `dead` |
| Dependencies | `deps`, `manifest`, `placement`, `features`, `sort`, `dupdeps`, `unused`, `supply` | `unused-deep`, `acl` |
| Build and release | `msrv`, `profile`, `binsize` | `bsize` |
| Repository | `commits`, `hygiene`, `wiring` | `history` |
| Your own checks | | `commands`, `commands-build` |
| [Outpost](https://github.com/outpostHQ) | | `padding`, `boundaries`, `scan`, `hazards`, `unreferenced`, `lenses`, `measures`, `unread` |

## Documentation

| page | read it for |
|---|---|
| [Guide](docs/guide.md) | the first install, the daily loop, how to read a result, every command |
| [The gates](docs/gates.md) | when each gate trips, what it needs, and what it leaves out |
| [Configuration](docs/configuration.md) | every key of `.chock/config.json`, `strict`, your own checks |
| [Hooks, stages and CI](docs/ci-and-hooks.md) | which gate runs at which step, the CI job, how to update chock |
| [For agents](docs/agents.md) | the contract that `chock init` puts into each project |
| [Changelog](CHANGELOG.md) | what changed in each version |

## Limits

- **Rust only.** chock reads `.rs` files and `Cargo.toml`.
- **The baseline is trusted.** Anyone can raise a record with `chock baseline`; chock makes that
  visible in the diff, and reviewers have to look.
- **A record is keyed by name.** Renaming a file or function drops its old record and holds the new
  name to zero, so moving debt counts as writing it again.

`chock run --ci` runs every gate that is on but the `local_only` ones, on Linux, macOS and Windows.
chock's own CI also runs the commit set first, on Linux only. Where a gate's tool does not run on a
system, the run leaves that gate out and says so.

## Licence

Apache-2.0 — Copyright 2026 Outpost Innovations, Inc. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
