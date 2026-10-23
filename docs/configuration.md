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
| `$schema` | the path or URL of `schema/config-v1.json`, so an editor checks the file as you type; chock does not read it |
| `version` | required; the version of the file's layout. chock refuses a higher number rather than half-read it |
| `enabled` | required; the gates the project has switched on, by the names `chock gates` prints. An unknown name fails the run |
| `runner` | the command that runs your tests, if not `cargo nextest run --workspace --no-tests=fail --no-fail-fast`; your command must also run every test after a failure, or the run lists only the first |
| `coverage` | the command that writes your coverage report; `{lcov}` marks where chock reads it |
| `message` | `subject`, `body` and `width`: the widest commit subject (72), the most non-blank body lines (12) and the widest body line (72) allowed |
| `stage` | gates that run at another step than their default; `chock stage` writes it |
| `local_only` | gates CI cannot run; `chock run --ci` leaves them out |
| `runner_tools` | tools your `runner` calls; with them named, `test` keeps its verdict until one of them changes |
| `coverage_tools` | tools your `coverage` command calls; with them named, `coverage` and `crap` keep their verdict until one of them changes |
| `test_wrapper` | a command your suite runs under, such as a script that starts a service; the suite, coverage and `mutest` commands follow it |
| `mutest_exhaustive` | `true` runs every test on each mutation, so `mutest` names each test that detects no mutation |
| `mutest_args` | flags `cargo mutest run` takes after chock's own, such as `--timings` |
| `not_shipped` | members nobody receives (fuzz harnesses, benchmarks), held to test-code rules |
| `miri` | `packages` or `exclude` names the packages Miri runs over; `flags` replaces chock's `-Zmiri-disable-isolation -Zmiri-strict-provenance` |
| `commands` | your own checks: a `name` and a `run` command; one that `counts` is ratcheted per item |
| `forbidden` | phrases source must not contain, each with the reason its finding gives; `in` limits a phrase to some paths |
| `seams` | Rust paths and crates only some files or packages may use; the `seams` gate holds them |
| `owners` | calls, such as `std::env::var`, whose value one file alone may read; the `owners` gate holds them |
| `calls` | calls that need another call in the same function, `after` it or `with` it; the `calls` gate holds them |
| `secrets` | the project's own credential shapes, by `name`, `prefix` and `tail`; `hygiene` and `history` find them |
| `proof_jobs` | the most Kani harnesses `proof` verifies at once; a lower number uses less memory |
| `proof` | how the `proof` gate runs Kani: `packages`, each passed as `-p` (the whole workspace), `timeout_s` of the verification run (1800, or `CHOCK_TIMEOUT`), `mutants` (`proofs/kani/mutants`), and `require_cover`, which makes a harness with no `kani::cover!` trip (`false`) |
| `tla` | how the `tla` gate checks TLA+ models: `dirs` (`["proofs/tla"]`), `workers` of one TLC or tlapm run (2), `timeout_s` of one cfg or of the proof of one module (300), `timeouts` by model or cfg, `inductive` (the models Apalache proves, each with its invariant; `{}` proves none) and `length` (the steps of a bounded Apalache run) |
| `verus` | how the `verus` gate runs Verus: `files` (`["proofs/verus/*.rs"]`), `mutants` (`proofs/verus/mutants`), `args` after the file (`["--crate-type=lib"]`) and `timeout_s` of one run (600) |
| `not_mutated` | Rust files `mutest` does not mutate, each a `path` from the root with the `why`; a path that is no source mutest mutates fails the run |
| `history` | `accepted`: credentials published on purpose, such as a test key, each named by `commit`, `path` and `rule` with the `reason` it is safe; `history` no longer reports them |
| `vcs` | `git` or `outpost`, for a tree both hold, when you want to name the one chock reads |
| `target`, `profile` | the target triple and cargo profile you ship, where the host and `release` are not; a tool that cannot be told refuses |
| `features`, `all_features`, `no_default_features` | the Cargo features chock's own compiler commands build with; your `runner` and `coverage` commands stay as you wrote them |
| `left_off` | default gates that are off, each with why; `init`, `enable` and `disable` keep it, and `doctor` names any gap |
| `strict` | ratchets held to zero rather than to the record |
| `clean_when_touched` | ratchets held to zero in each file the change touches |

## A service the tests need

When your tests need a service, such as a database, name a command that starts it in `test_wrapper`.
chock runs the suite, the coverage command and `cargo mutest` under it: the wrapper gets the command
as its arguments, runs it, and exits with its code.

```json
"test_wrapper": ["scripts/with-database", "--"]
```

## More from mutest

`"mutest_exhaustive": true` runs every test on each mutation. The run is slower. `mutest` then names
each test that detects no mutation, or only mutations that another test detects too, as a candidate.
A candidate never trips the gate.

`mutest_args` adds flags after chock's own, such as `["--timings"]`. Do not put `--exhaustive` there;
use `mutest_exhaustive`. After such a run the results stay in
`target/test-scratch/mutest-results-<process>-<time>/`. Delete the directory when you are done.

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

A phrase with `in` is refused only in those paths, such as a call that only one layer must not make:

```json
"forbidden": [
  { "text": "Command::new", "why": "Run tools through exec::run.", "in": ["src/gates/**"] }
]
```

## Declared rules

Four keys state a rule of your design, and a gate holds the rule at each commit. Each finding gives
your `why`.

```json
"seams": [
  { "path": "std::process::Command", "only": ["src/exec/**"], "why": "Start processes in exec." },
  { "crate": "aws-sdk-s3", "only": ["storage"], "why": "Only the storage crate talks to S3." }
],
"owners": [
  { "call": "std::env::var", "why": "Read each variable in one place, and pass the value on." }
],
"calls": [
  { "call": ".commit", "after": ".lock", "why": "Take the lock before the commit." }
],
"secrets": [
  { "name": "service token", "prefix": "svc_", "tail": 24 }
]
```

- `seams` reports a use of `path`, or of a path under it, outside the files in `only`. It reads
  each file's `use` aliases. A `crate` entry reports each package outside `only` whose normal
  dependency tree holds that crate. `chock edited` checks the path seams of each edited file.
- `owners` reports each read of a value, such as an environment variable name, that two or more
  files read.
- `calls` reports each function that makes `call` without the `after` call before it, or without
  the `with` call in it.
- `secrets` adds your credential shapes to `hygiene` and `history`.

`chock enable seams owners calls` switches the three gates on.

## Your own checks

`commands` runs a check of your own as part of a gate. Each entry has a `name` and a `run` command:
the whole command as a list, program first, started from the project root with no shell.

```json
"commands": [
  { "name": "shipped-config", "run": ["scripts/check-config"] },
  { "name": "doc-check", "run": ["scripts/doc-check"], "counts": true },
  { "name": "api-diff", "run": ["cargo", "semver-checks"], "builds": true },
  { "name": "schema", "run": ["scripts/gen-schema"], "outputs": ["schema/"] }
]
```

- An entry with neither flag passes or fails by its exit code.
- An entry with `counts` prints a JSON object of counts, such as `{"stale": 2}`, and chock holds
  each count to its record.
- An entry with `builds` compiles, so the `commands-build` gate runs it; the `commands` gate runs
  the others at each commit.
- An entry with `outputs` names the files or directories, from the root, that the command writes.
  The gate fails at each one whose bytes the command changed, so a committed generated file cannot
  go stale. The `test` gate does not count a change to them.

`chock enable commands commands-build` switches both gates on.
