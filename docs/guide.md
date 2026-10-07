# Guide

How to use chock, from the first install to the daily routine. The other pages are reference:
[the gates](gates.md), [configuration](configuration.md), [hooks, stages and CI](ci-and-hooks.md)
and [the contract for agents](agents.md).

[Install](#1-install) · [Set up a project](#2-set-up-a-project) · [The daily loop](#3-the-daily-loop) ·
[Read a result](#4-read-a-result) · [When a gate does not pass](#5-when-a-gate-does-not-pass) ·
[See all the debt](#6-see-all-the-debt) · [Choose the gates](#7-choose-the-gates) ·
[JSON output](#8-json-output) · [Every command](#9-every-command) · [Limits](#limits)

## 1. Install

Do this once on each machine. You need Rust 1.99.0 or newer and a linker: `build-essential` on
Debian and Ubuntu, `xcode-select --install` on macOS, the MSVC Build Tools on Windows.

```sh
cargo install cargo-binstall   # skip this if you have it; `brew install cargo-binstall` also works
cargo binstall chock           # prebuilt for Linux, macOS and Windows; `cargo install chock` compiles it
chock init --global            # the tools chock runs, prebuilt, at pinned versions
```

`chock doctor` says which of the pinned tools this machine has.

## 2. Set up a project

Do this once in each repository. The project must be a git or
[Outpost](https://github.com/outpostHQ) repository.

```sh
cd your-project
chock init --local             # measures the tree and chooses the gates
chock run                      # judges the tree and writes each gate's first record
```

Then commit the files that chock wrote.

| file | holds |
|---|---|
| `.chock/config.json` | which gates are on, and your settings |
| `.chock/baseline.json` | the recorded numbers |
| `.chock/agents.md` | how an agent calls chock and reads its answer |
| `justfile` | one recipe per gate |
| `tool-versions.env` | the tool versions this project expects |
| `deny.toml` | `cargo deny` configuration |
| `.claude/settings.json` | the editor hook, merged into any settings already there |

Three facts about `chock init --local`:

- **It measures before it switches a gate on.** A ratchet is on with the numbers that the tree
  has today, which the first `chock run` writes as its record. The required gates (`test`, `lint`,
  `doc`, `modcheck`) are on even when they trip. Another pass/fail gate that trips is reported as
  `FOUND` and left off, with the reason under `left_off` in the config; `chock enable GATE` adopts
  it later. A gate that chock could not measure stays on, and the output
  names it. Add `--fast` to measure only the gates that need no compiler, which takes seconds.
- **On does not mean passed.** The last lines of the output name each gate that is on and needs
  attention.
- **It never overwrites a file you have.** Yours stays, and chock's lands beside it as
  `<name>.chock`. The one exception is `tool-versions.env`; see
  [Update chock](ci-and-hooks.md#update-chock).

`chock init` with no flag does both halves: the project first, then the tools.

Two facts about the first `chock run`:

- **It writes each gate's first record.** The run prints `wrote the first record for …` on stderr
  and leaves `.chock/baseline.json` for you to commit. A gate that you switch on later gets its
  first record in the same way, from the next run.
- **CI writes no record.** In `chock run --ci`, a ratchet whose record no change committed is
  `CANNOT RUN`, and the reason names the command to run outside CI.

## 3. The daily loop

```
   edit ──► chock run --fast ──► git commit ──► git push ──► CI
            seconds              pre-commit     pre-push     chock run --ci
                                 hook           hook         nobody can skip it
```

| when | command | what it runs |
|---|---|---|
| while you work | `chock run --fast` | the gates staged for a commit; they need no compiler |
| before a push | `chock run` | every gate that is on |
| after a gate tripped | `chock run complexity` | only the gates you name |
| in a CI job | `chock run --ci` | the gates staged for commit, push or ci |

The hooks run the same gates at each commit and each push, so the commands above only give you the
answer earlier. [Hooks, stages and CI](ci-and-hooks.md) says which gate runs at which step.

A slow gate is not judged twice on what did not change. chock keeps its verdict in the git-ignored
file `verdicts.json` in `.chock`, under a key of everything the gate reads:

```
   the files ──┐
   the tools ──┼─► key ─┬─ a kept verdict has it ──► the row says `recalled`
   the config ─┤        └─ none has it ────────────► the gate runs; its verdict is kept
   the record ─┘
```

- A commit hook, a push hook and a run by hand share the kept verdicts.
- `CANNOT RUN` is never kept, and a run that wrote a record judges that gate once more.
- Not every gate keeps a verdict. `deps` asks the network and `commands` runs your own program,
  so each run judges them again, as it does the gates that chock computes itself.
- A `runner` or `coverage` command of your own hides its tools from chock. Name them under
  `runner_tools` or `coverage_tools`, or the gates that use the command run every time.
- `chock run --no-cache` judges every gate again. `chock cache clear` removes the kept verdicts.

## 4. Read a result

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

- **The first block has one line for each gate.** A ratchet adds what it measured and what the
  record holds: complexity is 34 where the record holds 26.
- **The second block has the findings of each gate that did not pass.** Its heading ends with the
  command that runs that one gate again. Each finding names the file, the function or lint, and the
  two numbers.
- **`fix:` says how to repair that kind of finding.** A gate that could not run because its tool is
  missing has a `fix:` line too: the command that installs the tool.

| verdict | in the text | in the JSON | exit code | what to do |
|---|---|---|:-:|---|
| passed | `ok` | `passed` | `0` | nothing |
| tripped | `TRIPPED` | `tripped` | `1` | fix the findings |
| could not run | `CANNOT RUN` | `cannot_run` | `2` | read the reason and fix that first |

A gate that could not run measured nothing: a tool was missing, a file would not parse, or a
command died. It says nothing about the code, and chock never counts it as a pass. A run exits with
the highest code among its gates.

The report goes to stdout when the run ends. Before that, stderr gets a line as each gate that
compiles ends, and a gate that does not pass adds its reasons there at once.

## 5. When a gate does not pass

```
                    ┌─ fix the code ................... the normal answer
   TRIPPED ─────────┼─ chock baseline GATE ............ accept the debt: the record rises
                    └─ chock disable GATE --reason WHY  stop the gate: the reason goes into the config

   CANNOT RUN ─────── read the reason, do what `fix:` says, then run the gate again
```

In the example above the repair is to split `render`. `chock baseline complexity` accepts the debt
instead: it raises the record in `.chock/baseline.json`, a file that reviewers see in the diff.

The record goes down by itself. A run that measures less says `lowered the record for …` on stderr
and leaves `.chock/baseline.json` modified. Commit it with your change, or CI fails.

**Example.** Your project has 400 comment blocks that are too long. You switch on `slop` and run
`chock run slop`, which writes 400 as the first record for the files that hold them. From then on:

- a new file with a long comment block trips the gate;
- one more long block in a file that already has some trips the gate;
- fix 10 of them and the record drops to 390, so the count can never climb back to 400.

## 6. See all the debt

A run lists what got worse than the record. It does not list the debt the record already holds.
The two numbers on a gate's line count that debt, and `chock explain` lists it.

```
   chock run ───────────► the findings: what is new, or above its own record
   chock explain GATE ──► the findings of the last run of one gate, without a new run
   chock explain ───────► all the debt the record holds, largest first, with each fix
```

A gate can trip while its total is at or under the record. `897 against 928 in total, 12 findings`
says the tree holds less debt than the record, and 12 places are new or above their own number.

## 7. Choose the gates

```sh
chock gates                          # every gate: what it measures, its stage, and whether it is on
chock enable mutest                  # switch a gate on
chock disable crap --reason "WHY"    # switch a gate off; the reason goes into the config
chock stage binsize ci               # run a gate at another step: commit, push, ci or manual
chock survey                         # what every gate measures here; no config or record changes
```

To demand zero from the first day, list a ratchet under `strict` in `.chock/config.json`. To demand
zero only in each file a change touches, list it under `clean_when_touched`; the other files keep
their record. `chock init --local` lists there each ratchet that it switched on, where the ratchet
keeps a number for each file and measures the same on every machine. Remove a name to hold that
gate to its record only. [Configuration](configuration.md) has every key.

## 8. JSON output

`run`, `gates`, `explain`, `doctor`, `survey`, `edited` and `slop` take `--json`. A run's report
follows [`schema/run-v1.json`](../schema/run-v1.json). For the project above,
`chock run complexity --json` prints this:

```json
{
  "$schema": "https://raw.githubusercontent.com/outpostHQ/chock/main/schema/run-v1.json",
  "version": 1,
  "chock": "0.1.1",
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
| `verdict` | a gate | `passed`, `tripped` or `cannot_run` |
| `cannot_run_reason` | a gate | why nothing was measured; present only with `cannot_run` |
| `rerun` | a gate | the command that runs this one gate |
| `fix` | a gate that tripped, or that a missing tool stopped | how to repair this kind of finding, or the command that installs the tool; where chock has advice |
| `measured`, `baseline` | a gate, a finding | the number now and the number on record |
| `file`, `line` | a finding | where to go, from the project root; `line` where the gate has one |
| `item` | a finding | the function, lint or measure |
| `message` | a finding | the finding in words |

## 9. Every command

`chock --help` and `chock help` print this list. `chock init --help` shows the options of `init`.

| command | what it does |
|---|---|
| `chock init` | sets up this project, then installs the tools it pins |
| `chock init --global` | only the tools, once per machine |
| `chock init --local` | only this project: justfile, pins, config and hooks |
| `chock baseline [GATE...]` | accepts today's numbers as the record, also where they got worse |
| `chock baseline --lower GATE...` | records a gain on a gate that measures differently by machine |
| `chock doctor` | says whether this machine has the tools this project pins |
| `chock run [GATE...]` | runs the named gates; no name means every gate that is on |
| `chock run --fast` | runs only the gates staged for a commit; they need no compiler |
| `chock run --ci` | runs the gates a CI job runs: staged for commit, push or ci |
| `chock run --no-cache` | judges every gate again; no kept verdict answers |
| `chock run --miri-partition=K/N` | runs only part K of N of the Miri suite |
| `chock cache clear` | removes every kept verdict, so the next run judges every gate |
| `chock explain GATE` | shows the findings of the last run, without a new run |
| `chock explain` | lists all the debt in the record, largest first, with the fix for each kind |
| `chock gates` | lists every gate: what it measures, its stage, and whether it is on |
| `chock enable GATE...` | switches gates on |
| `chock disable GATE... --reason WHY` | switches gates off and records the reason |
| `chock stage GATE... STAGE` | sets when they run: `commit`, `push`, `ci` or `manual` |
| `chock survey` | measures what every gate finds here; it changes no config and no record |

The hooks call three more commands. You can run them by hand.

| command | what it does |
|---|---|
| `chock message FILE` | checks one commit message |
| `chock edited PATH...` | checks each file's own text, as the editor hook does |
| `chock slop [DIR]` | lists the comment blocks longer than the limit |

Exit codes: `0` every gate passed, `1` a gate tripped, `2` a gate could not run.

## Limits

- **Rust only.** chock reads `.rs` files and `Cargo.toml`.
- **The baseline is trusted.** Anyone can raise a record with `chock baseline`; chock makes that
  visible in the diff, and reviewers have to look.
- **A record is keyed by name.** Renaming a file or function drops its old record and holds the new
  name to zero, so moving debt counts as writing it again.
- **Fuzzing, property tests and benchmarks are yours to write.** chock runs property tests as
  tests, and has no benchmark gate, because one timing on a shared machine is noisier than the
  regression it would look for.

[The gates](gates.md) says what each gate leaves out on purpose.
