# Contributing

## What you need

- Rust 1.99.0, which `rust-toolchain.toml` selects if you use rustup.
- git and a C linker (`build-essential` on Debian or Ubuntu).
- `rustup component add llvm-tools-preview` for the coverage checks, and a nightly toolchain for
  `unused-deep`.

`chock init --global` installs the tools that are on crates.io, at the versions in
`tool-versions.env`. The [Requirements](README.md#requirements) table says what each check needs
beyond that; `mutest` needs mutest-rs built from its checkout.

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

A `v*` tag runs two workflows. `publish.yml` checks the tag against `Cargo.toml`, runs the fast
checks and publishes, using a thirty-minute crates.io token exchanged for GitHub's OIDC identity, so
no secret is stored; it skips a version crates.io already has. `release.yml` builds chock for Linux
(x86_64 and aarch64), macOS (Apple Silicon and Intel) and Windows, and attaches the archives to the
tag's GitHub release, where `cargo binstall chock` finds them.

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
