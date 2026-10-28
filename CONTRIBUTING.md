# Contributing

## What you need

- rustup, git, a C linker (`build-essential` on Debian or Ubuntu) and
  [just](https://github.com/casey/just).

`just setup`, from the root of a clone, does the rest: the Rust 1.99.1 that `rust-toolchain.toml`
pins with `llvm-tools-preview` for the coverage checks, a nightly for `unused-deep`, this chock,
every tool a gate runs at its version in `tool-versions.env`, and the hooks. The table
[What a gate needs](docs/gates.md#what-a-gate-needs) says what each gate needs.

## Before you open a pull request

```sh
chock doctor          # does this machine hold the pinned versions?
chock run             # every check this project has on
```

chock checks itself. CI runs the same checks on Linux, macOS and Windows, with
`RUSTFLAGS=-D warnings` and without Outpost, so run `RUSTFLAGS='-D warnings' chock run --ci` to
see what it will see. `mutest` is the slow one here (about four minutes), then `proof` (five Kani
harnesses, about a minute). `history` stays off because this is a git repository, not an Outpost
one.

The rules for changing the code, and for adding a check, are in [`AGENTS.md`](AGENTS.md).

## Baselines

`.chock/baseline.json` goes down by itself: a run that measures less lowers it, and the change that
caused the drop commits it. CI fails a change that leaves a gain out. Raising a record is
`chock baseline`, and a pull request that does says which debt it accepts and why.

## Tools chock runs

chock runs pinned tools rather than reimplementing them. `tool-versions.env` is the one source of
every version: adding a tool means pinning it there, and `chock doctor` reads the pin file as data.

## Releasing

A `v*` tag runs `publish.yml`. It stops unless the `ci.yml` and `corpus.yml` runs on `main` for the
tagged commit passed, so tag a commit only after both are green. It builds chock for Linux (x86_64 and
aarch64), macOS (Apple Silicon and Intel) and Windows, checks that each archived binary names the
tag, and attaches the archives to the tag's GitHub release, where `cargo binstall chock` finds them.
Then it publishes, using a thirty-minute crates.io token exchanged for GitHub's OIDC identity, so no
secret is stored; it skips a version crates.io already has.

The first release is published by hand, because crates.io only offers trusted publishing for a crate
that already exists:

```sh
cargo login                  # a token from crates.io → Account Settings → API Tokens
cargo publish --locked
```

Then, on the crate's page, add a trusted publisher under **Settings → Trusted Publishing**: repository
`outpostHQ/chock`, workflow `publish.yml`, no environment. Revoke the token, and push the tag
(`git tag v0.1.0 && git push --tags`) so the prebuilt binaries are built. Every later release is
the tag alone.

## Licence

By contributing you agree that your work is licensed under Apache-2.0, as section 5 of the licence
provides.
