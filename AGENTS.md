# AGENTS.md

How to call chock, and the rules for changing this repository. The contract an adopting project's
agents read is [`docs/agents.md`](docs/agents.md).

## Calling chock

```sh
chock run --json            # every check this project has on
chock run <name>... --json  # only the ones named
chock run --fast --json     # only the checks that need no compiler
chock gates --json          # what exists; read it, never hard-code a list
chock explain <name>        # the last run's findings, without running again
chock enable <name>...      # switch checks on; chock disable switches them off
chock edited <path>...      # what the commit would refuse in these files; --hook reads stdin
```

| verdict | exit | meaning | what to do |
|---|:-:|---|---|
| `passed` | 0 | measured, no worse than the baseline | nothing |
| `tripped` | 1 | measured, worse than the baseline | fix the findings |
| `cannot_run` | 2 | nothing was measured | read `cannot_run_reason` and fix that first |

`cannot_run` says nothing about the code. Reading it as a pass is the one mistake that makes a gate
worthless. A failed outcome with no findings is turned into `cannot_run`, because that is what a
parser produces when its tool changes output format; never add a placeholder finding to avoid it.

Each finding has `file` and `message`, and where the check has them `line`, `item` (the function,
lint or measure), `measured` and `baseline`. Each check has `rerun`, and `fix` when it tripped and
chock has advice. Go to the file, fix it, run `rerun`. The schemas are in [`schema/`](schema/).

## Enforcement

```
   editor hook  ──►  pre-commit  ──►  commit-msg  ──►  pre-push  ──►  CI (chock run --ci)
   the file just     checks that      the message      checks that     everything but
   written           need no                           compile         local_only
                     compiler
```

- The git hooks are declared in `.git/config` (`hook.chock-pre-commit.command = chock hook
  pre-commit`), so upgrading chock upgrades every repository's hooks. Git older than 2.54 gets a
  delegating script in `.chock/hooks/`.
- CI runs `chock run --ci`, which leaves out the `local_only` checks (their tools cannot be installed
  on a runner). `--no-verify` skips the git hooks, never CI.
- In a repository only Outpost holds there is one hook moment, `pre-commit`, declared in
  `.outposthooks.toml` and effective only after `outpost hooks trust`. Commits Outpost records for
  agent turns run no hook; the editor hook and CI cover them.
- This repository is held by git and mirrored by Outpost; `vcs` in `.chock/config.json` says git.
  Bring history into the mirror with `outpost git import`, never `outpost git sync`: `sync` rewrites
  turn commits into new ids and pushes them into git.
- Run `chock doctor` after anything rewrites the tree; it names stale hooks and missing pins.

## Baselines

- **Never run `chock baseline` to turn a red check green.** Raising a record is a deliberate commit
  that says which debt was accepted and why.
- **Commit the record a run lowers.** A run prints `lowered the record for …` and leaves
  `.chock/baseline.json` modified; CI fails a change that leaves that gain out.
- **Record the worst case across the machines that run a check.** A ratchet fails above the record,
  so the record is the maximum. An optional tool moves coverage without a code change (with
  `outpost` installed the Outpost arm of each dispatch in `src/project/vcs.rs` runs; without it
  another line runs), and which machine comes out higher has to be measured, not guessed.
- **Record with no `CHOCK_JOBS` set.** It takes an early return in `budget::jobs` and leaves the
  line below it unrun, so a record taken with it is one line short.

## Changing this repository

Run `chock run` before calling a change finished. chock gates itself.

- **Comments say why, never what**, and a comment block is at most two lines. A decision needing
  more belongs in the commit message that makes it. `slop` enforces the length; only a reader can
  enforce the rest.
- **A comment naming a file must name one that exists.** `citations` counts those that do not.
- **No `unwrap`, `expect` or `panic!` outside `#[cfg(test)]`.** A test module allows them with
  `#[allow(clippy::unwrap_used, clippy::panic, reason = "…")]`; `source` checks for the reason.
- **Assert content, never length.** `found.len() == 1` holds against every mutation that corrupts
  the result and keeps the count. `assertions` counts these.
- **Test names are sentences**: `a_gate_that_could_not_measure_never_reports_a_pass`.
- **Separate the rule from the world.** A check takes its input as a value — captured output, a
  manifest string, a file listing — so the rule is tested without a process or a filesystem
  (`project::find_with`, `manifest::faults`). Split at `exec::Output`, not after it: a reader that
  gets only `stdout` cannot tell an empty answer from a failed command.
- **Only `src/project/vcs.rs` runs a version-control command.** A gate calling `git` directly is
  wrong in an Outpost repository, where `outpost setup` puts a `git` shim first on `PATH`.
- **No seams that exist only for tests.** A check reads what it needs from `Ctx`, filled once at the
  edge; a function reading the environment mid-stack has an input no test can set.
- **Never let a test invoke a check that runs the test suite.** `src/exec/mod.rs` counts spawn depth
  and `run_one` refuses past two, as a backstop.

## Adding a check

A check supplies numbers or a verdict. The baseline comparison, exit code, findings and JSON are
written once, in `src/run/`. Put the check in the family under `src/gates/` that matches what it
reads, and register it in `src/gates/mod.rs`.

- A **ratchet** returns a map of key to number, lower being better, and declares its key kind:
  `Items` (a new key is new debt and fails), `Measures` (a new key is a new metric and is reported),
  `Census` (every key in the domain has a row, zeros included, and a row that stops arriving fails),
  or `Sizes` (items that may grow by a tolerance).
- A **pass/fail** check returns an `Outcome`. `Err` always means "could not run", never "found
  something".
- **Prefer a ratchet when the thing counted already exists.** A check demanding zero of something a
  real codebase has cannot be adopted; `source`, `mutest` and `citations` all started pass/fail and
  had to change.
- **Key by something that does not move**: file plus function, or file plus rule. A line number
  moves on every edit above it.
- **Set `builds` honestly.** It decides whether the check runs at commit or waits for the push.
- **Measure before you enable.** Run a candidate over real repositories and report its false-positive
  rate; a noisy rule gets the whole check switched off.
- **Do not judge code cargo does not build.** A crate under another crate's `tests/`, `examples/` or
  `benches/` is a fixture; use `project::fixture_crates` and `prodlines::for_each_source`.

## Anything expensive

- **Cap parallelism explicitly** (`cargo build -j 8`, `cargo test -- --test-threads=8`), and read
  I/O wait as well as load: load counts processes blocked on disk too.
- **chock must never make a machine unusable.** `budget` starts every tool in `budget::HEAVY` under
  `ionice -c 2 -n 7 nice -n 10`. `ionice` has no effect under the `none` I/O scheduler most NVMe
  disks use, so there the remedy is one heavy job at a time.
- **Coverage and mutation testing are measurements taken once, when the work is done**, never
  progress checks.
- **A slow commit is fixed with `at_push`, a slow push with `at_ci`** — they move when a check runs,
  never whether. A project naming `at_ci` checks needs CI, or nothing runs them.

## Working with the model

Sessions here run on Opus 5.5, with Sonnet 5 for mechanical subagent work. Effort is `medium` by
default, `low` for mechanical work, `high` or `xhigh` for a check's verdict logic. Never write
"think carefully" or "step by step" into a prompt or a rule.

- Give the whole task up front with its finish line: what done looks like, how it is checked, when to
  stop.
- Keep going when a step needs no input from the owner. Ask first before anything destructive:
  deleting data, force-pushing, or changing anything outside this repository.
- Treat an answered question as settled unless asked again.
- Check a subagent's evidence before accepting its report. Mark anything unconfirmed, with where you
  looked.
- A review lists only the problems you would block the merge for.

Quality gates: read .chock/agents.md before running or reading chock.
