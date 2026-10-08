# Hooks, stages and CI

chock runs at four steps. Each later step runs the gates of the earlier steps again.

```
   while you write     at commit             at push               in CI
  ┌───────────────┐   ┌───────────────┐   ┌───────────────┐   ┌───────────────┐
  │ editor hook   │ ► │ pre-commit    │ ► │ pre-push      │ ► │ chock run --ci│
  │ the file just │   │ stage commit: │   │ stage commit  │   │ every stage   │
  │ written       │   │ gates that    │   │ and push:     │   │ but manual    │
  │               │   │ need no       │   │ + the gates   │   │               │
  │               │   │ compiler      │   │ that compile  │   │               │
  │               │   │ + commit-msg  │   │               │   │               │
  └───────────────┘   └───────────────┘   └───────────────┘   └───────────────┘
     on every edit        seconds             minutes          cannot be skipped
```

`chock init --local` installs the editor hook and the git hooks. CI is yours to add; see
[CI](#ci).

## Stages

Each gate has a stage. A gate that needs no compiler starts at `commit`, and a gate that compiles
starts at `push`. `chock gates` shows the stage of each gate.

| stage | runs at |
|---|---|
| `commit` | the pre-commit hook, the pre-push hook and CI |
| `push` | the pre-push hook and CI |
| `ci` | CI only |
| `manual` | only a `chock run` that somebody starts; `chock doctor` names the gate, because nothing enforces it |

`chock stage` moves gates when the default does not suit the project:

```sh
chock stage binsize ci     # a full release build, too slow for the push on this workspace
chock stage binsize push   # back to the default: chock removes the entry
```

## The editor hook

`chock edited --hook` runs after every file an agent writes. It reports only what the commit would
refuse for that file. These are silent: a gate that is off, a directory the gate never reads, and
debt the baseline already accepts. A gate under `clean_when_touched` reports the accepted debt too,
because the edit touches the file.

The hook answers on stderr with exit 2, which is how Claude Code hands a hook's answer back to the
model. Other editors can call `chock edited <path>`, or pipe a path or the editor's JSON to
`chock edited --hook`. Only `--hook` reads stdin.

## The git hooks

On git 2.54 and later the hooks are declared in `.git/config`, two keys for each of `pre-commit`,
`commit-msg` and `pre-push`:

```ini
[hook "chock-pre-commit"]
    command = chock hook pre-commit
    event = pre-commit
```

They run beside any hooks you keep in `.git/hooks`. The setting is per clone, so each clone runs
`chock init --local` once.

Older git gets `core.hooksPath` pointed at `.chock/hooks`, which holds one stub file for each hook.
If `.git/hooks` already holds hooks of your own, or `core.hooksPath` points somewhere else, chock
changes nothing: it prints a note, and its hooks do not run until you wire them. After an upgrade
to git 2.54, `chock init --local` declares the hooks, deletes its stubs and unsets the path it set.

A project in a subfolder of a repository, such as `crates/tova`, declares its hooks at the top of
the repository under names of its own: `hook.chock-pre-commit@crates/tova.command = chock hook
pre-commit --project crates/tova`. `--project` moves the hook into that folder before it runs.
Each project in the repository keeps its own hooks. This needs git 2.54; older git gets a note and
no stubs, because a stub directory would replace the hooks of every other project.

In an Outpost repository, chock declares one `[[pre-commit]]` entry named `chock` in
`.outposthooks.toml`, and trusts that file when chock wrote all of it. Outpost runs only
`pre-commit` hooks, so there is no message check and no push gate there. A project in a subfolder
gets its own entry, named `chock@<path>`.

`git commit --no-verify` and `git push --no-verify` skip both git hooks. CI is the step that nobody
can skip.

## CI

Install the pinned tools, then run every gate but the `local_only` ones:

```yaml
- uses: actions/checkout@v6
  with:
    fetch-depth: 2           # `clean_when_touched` compares HEAD with its parent
- uses: cargo-bins/cargo-binstall@main
- run: cargo binstall -y chock
- run: chock init --global   # the tools, prebuilt, at the versions in tool-versions.env
- run: chock run --ci
```

`chock run --fast --ci` runs only the gates of the commit set, with CI's rules. chock's own CI runs
it as a quick first job on Linux.

Two environment variables change how a run uses the machine:

| Variable | Default | What it sets |
|---|---|---|
| `CHOCK_TIMEOUT` | 1800 (30 min) | The seconds one tool may run before chock stops it. |
| `CHOCK_JOBS` | the cores, as memory allows | The build jobs at once. chock then does not read the memory. |

`chock init --global` installs [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) first,
then every other tool from its own prebuilt release, in minutes; a tool with no release for the
platform is compiled. [`SECURITY.md`](../SECURITY.md) says what that trades.

A run that measures less than the record lowers `.chock/baseline.json`. CI fails a change that does
not commit the lower number, so the record in the repository is always the true one.

CI writes no record. A ratchet whose first record no change committed is `CANNOT RUN` there:
run `chock run GATE` outside CI and commit `.chock/baseline.json`. A gate that keeps a record for
each system, such as `coverage@macos`, needs that run on that system.

Two branches that both lower the record can conflict in `.chock/baseline.json`. Keep the lower
number for each key.

### A long Miri suite

`miri` interprets each test, tens of times slower than a normal run. chock builds the tests once,
lists them, and runs them in groups of up to 32 tests, each group in one Miri interpreter. Each
group takes every n-th test of the list, so the slow tests that sit side by side in one module go
to different groups. Groups run at once, one for each core, and one for each 512 MB of free memory.
When the last run recorded each test's time, the slowest groups start first.

A group with no progress for 6 minutes is stopped. Only `CHOCK_TIMEOUT` limits the whole run. At the end, chock prints one line: the build time, the number
of groups, how many ran at once, and the sum of the test times.

chock's own [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs the whole Miri suite as
one job on each system, `chock run --ci miri`. Where the suite does not fit one CI job, split it
into parts: `--miri-partition=K/N` runs part K of N, as nextest's `count:K/N` partition splits the
tests. `--skip` leaves a slow gate out of the other job, so all jobs run at once:

```yaml
- run: chock run --ci --skip=miri                                # every gate but miri
- run: chock run --ci --miri-partition=${{ matrix.part }}/4 miri # parts 1 to 4, one job each
```

Each part keeps its verdict under a key of its own.

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
