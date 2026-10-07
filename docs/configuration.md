# Configuration

`.chock/config.json` holds what no measurement can answer. `chock init --local` writes it, and
`chock enable`, `chock disable` and `chock stage` change it. The full shape is
[`schema/config-v1.json`](../schema/config-v1.json).

## Hold a ratchet to zero

A ratchet holds each file and function to its record. Two keys make it stricter.

| key | what it does |
|---|---|
| `strict` | holds the ratchets it lists to zero: any finding trips, and no baseline is needed |
| `clean_when_touched` | holds the ratchets it lists to zero in each file the change touches; the other files keep their record |

`chock init --local` writes `clean_when_touched` with each ratchet that it switched on, where the
ratchet keeps a number for each file and measures the same on every machine. Remove a name to hold
that gate to its record only.

`clean_when_touched` holds only a gate that keeps a number for each file or function. chock
refuses a config that lists another gate there, such as `binsize`, whose record is for the whole
project.

In CI, `clean_when_touched` compares `HEAD` with its parent, so the checkout needs `fetch-depth: 2`.
Without the parent, the gate cannot run.

## Every key

| key | what it says |
|---|---|
| `runner` | the command that runs your tests, if not `cargo nextest run --workspace --no-tests=fail --no-fail-fast`; your command must also run every test after a failure, or the run lists only the first |
| `coverage` | the command that writes your coverage report; `{lcov}` marks where chock reads it |
| `message` | the widest commit subject and the longest body allowed |
| `stage` | gates that run at another step than their default; `chock stage` writes it |
| `local_only` | gates CI cannot run; `chock run --ci` leaves them out |
| `runner_tools` | tools your `runner` calls; with them named, `test` keeps its verdict until one of them changes |
| `coverage_tools` | tools your `coverage` command calls; with them named, `coverage` and `crap` keep their verdict until one of them changes |
| `not_shipped` | members nobody receives (fuzz harnesses, benchmarks), held to test-code rules |
| `miri` | which packages Miri runs over, and its flags |
| `commands` | your own checks: a `name` and a `run` command; one that `counts` is ratcheted per item |
| `forbidden` | phrases source must not contain, each with the reason its finding gives |
| `history` | `accepted`: credentials published on purpose, such as a test key, each named by `commit`, `path` and `rule` with the `reason` it is safe; `history` no longer reports them |
| `vcs` | `git` or `outpost`, for a tree both hold, when you want to name the one chock reads |
| `target`, `profile` | the target triple and cargo profile you ship, where the host and `release` are not; a tool that cannot be told refuses |
| `features`, `all_features`, `no_default_features` | the Cargo features chock's own compiler commands build with; your `runner` and `coverage` commands stay as you wrote them |
| `left_off` | default gates that are off, each with why; `init`, `enable` and `disable` keep it, and `doctor` names any gap |
| `strict` | ratchets held to zero rather than to the record |
| `clean_when_touched` | ratchets held to zero in each file the change touches |

## Forbidden phrases

The `phrases` gate reads every file `slop` reads, ignores case, and counts each phrase per file
against the record. `word` matches only a whole word; `except` lists paths where the phrase is
allowed:

```json
"forbidden": [
  { "text": "utilize", "why": "Write \"use\".", "word": true },
  { "text": "dbg!", "why": "Remove debug output.", "except": ["examples/**"] }
]
```

## Your own checks

`commands` runs a check of your own as part of a gate. Each entry has a `name` and a `run` command:
the whole command as a list, program first, started from the project root with no shell.

```json
"commands": [
  { "name": "shipped-config", "run": ["scripts/check-config"] },
  { "name": "doc-check", "run": ["scripts/doc-check"], "counts": true },
  { "name": "api-diff", "run": ["cargo", "semver-checks"], "builds": true }
]
```

- An entry with neither flag passes or fails by its exit code.
- An entry with `counts` prints a JSON object of counts, such as `{"stale": 2}`, and chock holds
  each count to its record.
- An entry with `builds` compiles, so the `commands-build` gate runs it; the `commands` gate runs
  the others at each commit.

`chock enable commands commands-build` switches both gates on.
