# Guide

How to use chock, from the first install to the daily routine. The other pages are reference:
[the gates](gates.md), [configuration](configuration.md), [hooks, stages and CI](ci-and-hooks.md)
and [the contract for agents](agents.md).

[Install](#1-install) · [Set up a project](#2-set-up-a-project) · [The daily loop](#3-the-daily-loop) ·
[Read a result](#4-read-a-result) · [When a gate does not pass](#5-when-a-gate-does-not-pass) ·
[See all the debt](#6-see-all-the-debt) · [Choose the gates](#7-choose-the-gates) ·
[JSON output](#8-json-output) · [Every command](#9-every-command) ·
[Cut code with proof](#10-cut-code-with-proof) · [Limits](#limits)

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
| `tool-versions.env` | the tool versions this project expects |
| `deny.toml` | `cargo deny` configuration |
| `.claude/settings.json` | the editor hook, merged into any settings already there |
| `.gitignore`, `.outpostignore` | the files the gates produce, such as `.chock/last-run.json`; lines are added, never removed |
| `AGENTS.md`, `CLAUDE.md` and the like | one line that points to `.chock/agents.md`, only in the files the project already has |
| `.chock/hooks/*` | one stub for each git hook, on git older than 2.54 only |
| `.outposthooks.toml` | the `pre-commit` hook, in an Outpost repository |

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
  `<name>.chock`. There are two exceptions. The first is `tool-versions.env`; see
  [Update chock](ci-and-hooks.md#update-chock). The second is `.chock/config.json`: init keeps
  each choice in it and measures no gate that builds. Where the config turns `wiring` on, init
  switches on each gate that is off, needs no compiler and passes, such as a gate newer than the
  config.

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

`run`, `gates`, `explain`, `doctor`, `survey`, `edited`, `slop`, `lean` and `oracle` take `--json`. A run's report
follows [`schema/run-v1.json`](../schema/run-v1.json). For the project above,
`chock run complexity --json` prints this:

```json
{
  "$schema": "https://raw.githubusercontent.com/outpostHQ/chock/main/schema/run-v1.json",
  "version": 1,
  "chock": "0.5.0",
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
| `places` | a finding | the other places the finding is about, such as the copy of a duplicated body |
| `fix` | a finding | the change that clears this finding, where the gate can name one |
| `grade` | a finding | `candidate` for a lead that the gate cannot settle from the source; it never trips the gate |
| `recalled` | a gate | `true` where chock reused an earlier verdict, because every input the gate reads is the same |
| `peak_mb` | a gate | the most memory its processes held at once; Linux only |
| `tests_ms` | a gate that times its tests (`miri`) | each test's time in ms, so the next run starts the slowest first |
| `ran_at` | a gate in `.chock/last-run.json` | when it ran, in seconds since the Unix epoch |

## 9. Every command

`chock --help` and `chock help` print this list. `chock init --help` shows the options of `init`.

| command | what it does |
|---|---|
| `chock init` | sets up this project, then installs the tools it pins |
| `chock init --global` | only the tools, once per machine |
| `chock init --local` | only this project: pins, config and hooks |
| `chock baseline [GATE...]` | accepts today's numbers as the record, also where they got worse |
| `chock baseline --lower GATE...` | records a gain on a gate that measures differently by machine |
| `chock doctor` | says whether this machine has the tools this project pins |
| `chock run [GATE...]` | runs the named gates; no name means every gate that is on |
| `chock run --fast` | runs only the gates staged for a commit; they need no compiler |
| `chock run --ci` | runs the gates a CI job runs: staged for commit, push or ci |
| `chock run --no-cache` | judges every gate again; no kept verdict answers |
| `chock run --miri-partition=K/N` | runs only part K of N of the Miri suite |
| `chock run --skip=GATE,...` | runs every gate but the ones named; CI runs those in other jobs |
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

Two commands read a tree or a program that chock does not own. They need no config and write no
record. [Cut code with proof](#10-cut-code-with-proof) explains them.

| command | what it does |
|---|---|
| `chock lean [DIR]` | lists every line the tree could lose, file by file, largest first |
| `chock lean --tests` | counts repeated test code too |
| `chock lean --min N` | lists only the files that could lose `N` lines or more |
| `chock oracle --old A --new B --corpus FILE` | runs two builds over the same scenarios and compares each answer |
| `chock sweep REV [PATH...]` | proves that only comments changed since `REV` |
| `chock moved REV PATH...` | proves that code only moved between the named files since `REV` |

Exit codes: `0` every gate passed, `1` a gate tripped, `2` a gate could not run.

## 10. Cut code with proof

The `lean` gate stops a file from gaining lines that could be written once. These two commands
work on the lines that are already there.

```text
chock lean --json ──▶ one change for each place ──▶ a person approves
                                                          │
chock run ◀── chock oracle: old build = new build ◀── apply, build again
```

chock calls no model and applies no change. An agent or a person does both.

### `chock lean`

`chock lean [DIR]` reads every `.rs` file under `DIR`, or under the current directory. It prints
the totals, the lines by kind, and the 30 files that could lose the most. `--json` prints every
file.

| field | where | what it holds |
|---|---|---|
| `files`, `lines` | report | the production files and their lines |
| `lines_counted` | report | which lines `lines` counts |
| `removable_lines` | report, row, place | the lines that could go |
| `by_kind` | report | `removable_lines` for each kind |
| `unread` | report | each file that did not parse; the command then exits `2` |
| `rows` | report | one row for each file, most `removable_lines` first |
| `path`, `lines`, `places` | row | the file, its lines, and each place in it |
| `line`, `end_line` | place | where the place starts and ends |
| `kind` | place | `forwarder`, `repeat`, `reexport_module`, `single_impl_trait` or `mirror_type` |
| `fix` | place | the change that removes the lines |
| `twin` | place | the other copy of a repeat or a mirror, as `file:line` |
| `evidence` | place | `exact` or `estimate` |

A place with `exact` evidence is a fact that the source settles: a private function that only
passes its parameters on. A place with `estimate` evidence is a shape that a person must judge. A
trait with one implementation can be a test seam, and a module of re-exports can be a public API.

A repeat counts once: each copy holds its share of the lines that one function would remove.

### `chock oracle`

`chock oracle` runs an old build and a new build of one program over the same scenarios. It
compares the exit code, stdout and stderr of each command, and the files that each build left.

```sh
chock oracle --old target/old/tool --new target/release/tool --corpus scenarios.jsonl \
    --fixture tests/fixture --probe "status --json" --normalize rules.jsonl --allow allowed.jsonl
```

| option | what it gives |
|---|---|
| `--old`, `--new` | the two builds |
| `--corpus FILE` | the scenarios, one JSON object on each line |
| `--fixture DIR` | a directory that each build gets its own copy of, as its working directory |
| `--probe "ARGS"` | a command that each build runs after the last step; repeat the option for more |
| `--normalize FILE` | rules for text that two honest runs print differently |
| `--allow FILE` | differences that a person accepted |
| `--timeout SECS` | the limit for one command; 60 without the option |

A scenario names its steps. Each step is the arguments of one command. `stdin` and `env` are
optional, and each step gets them.

```json
{"name": "commit then log", "steps": [["commit", "-m", "one"], ["log"]], "stdin": "", "env": {"PAGER": "cat"}}
```

Each build runs in its own directories, with its own `HOME` and temporary directory. chock clears
the environment and sets the same time zone, language, author and dates for both builds. It sets
the proxy variables to a closed local port. It does not block sockets, so a build that ignores
those variables can still reach the network.

A normalize rule replaces text with a token before the comparison. chock replaces each build's
own directory with `<DIR>` without a rule. The report counts what each token replaced.

```json
{"kind": "hex", "min": 12, "token": "<ID>"}
{"kind": "digits", "min": 10, "token": "<TIME>"}
{"kind": "text", "text": "tool 2.1.0", "token": "<VERSION>"}
```

| kind | what it replaces |
|---|---|
| `text` | each place the `text` stands |
| `hex` | each run of `min` or more hexadecimal digits |
| `digits` | each run of `min` or more decimal digits |

An allow line accepts one field of one scenario. The field is `exit`, `stdout`, `stderr`, `probe`
or `tree`. The difference stays in the report, and the scenario counts as `allowed`.

```json
{"scenario": "commit then log", "field": "stderr", "reason": "the new build drops a warning"}
```

The JSON report has `scenarios`, `equal`, `different`, `allowed` and `errors`, the counts in
`normalized`, and one row for each scenario. A row has `name`, `verdict`, `differences` and
`allowed`. A difference has `field`, `at` (the step or the probe), `old`, `new`, `line`,
`old_line` and `new_line`: both values, and the first line that is not the same. A `tree`
difference has the first path that the two builds hold differently.

| exit code | meaning |
|---|---|
| `0` | each scenario is `equal` or `allowed` |
| `1` | a scenario is `different` |
| `2` | a scenario could not be compared, or the run could not start |

Limits of the comparison:

- The two builds of a scenario run at the same time. Scenarios run one after another.
- A command that passes its limit is killed. A timeout never compares equal, also when both builds
  time out.
- The file comparison leaves out `.git` and `.outpost` directories. Use a probe to compare what a
  store holds.
- A file that holds a build's own directory differs between the builds. Add an allow line for it.
- A probe's arguments are split at spaces. A probe cannot hold an argument with a space in it.

### `chock sweep`

`chock sweep REV [PATH...]` proves that a change of comments changed no code. It reads each `.rs`
file that differs from the revision `REV`, or only the files below each `PATH`. It compares the
tokens of the file now with its tokens at `REV`. A comment is no token.

```sh
chock sweep main
chock sweep HEAD~3 src/gates
```

| verdict | meaning |
|---|---|
| `clean` | the same tokens: only comments and layout changed |
| `docs only` | only doc comments changed; the report counts them |
| `CODE` | a token changed; the report gives the first token that differs |

A file that `REV` does not hold is `skipped`: use `chock moved` for it. The command exits `0` when
no file has the verdict `CODE`, `1` when one has, and `2` when it could not read the revision.

### `chock moved`

`chock moved REV PATH...` proves that code only moved between files. Name each file that the code
left or reached. The command pools the tokens of those files now and at `REV`, and compares the
two pools.

```sh
chock moved main src/run/mod.rs src/run/report.rs
```

The report lists each token that the move removed and each token that it added. A split adds
`mod`, `use` and `pub` tokens, so read the added list. The command exits `0` when no token was
lost, `1` when one was, and `2` when it could not read the revision.

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
