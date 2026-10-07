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
model. Other editors can pipe a path to `chock edited`.

## The git hooks

On git 2.54 and later the hooks are declared in `.git/config`
(`hook.chock-pre-commit.command = chock hook pre-commit`) and run beside any hooks you keep in
`.git/hooks`. Older git gets `core.hooksPath` pointed at `.chock/hooks`. The setting is per clone,
so each clone runs `chock init --local` once.

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

`miri` interprets each test, tens of times slower than a normal run. Where the whole suite does not
fit one CI job, split it into parts: `--miri-partition=K/N` runs part K of N, as nextest's
`count:K/N` partition splits the tests. One job runs every gate with part 1, and one job each runs
only `miri` for the other parts:

```yaml
- run: chock run --ci --miri-partition=1/4                       # every gate, and part 1 of 4
- run: chock run --ci --miri-partition=${{ matrix.part }}/4 miri # parts 2 to 4, one job each
```

Each part keeps its verdict under a key of its own. chock's own
[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs its suite this way.

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
