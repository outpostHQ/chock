# Security

## Reporting a vulnerability

Report privately through GitHub's security advisory page for this repository, not as a public
issue. Include what you did, what happened, and the version (`chock --version`).

## What chock does on your machine

Worth knowing before you run it in CI or hand it to an agent:

- **It runs other programs.** Every gate that is not native Rust shells out to a pinned tool. The
  list is `tool-versions.env`, and `chock doctor` reports what is installed.
- **It reads your source and your manifests.** The `hygiene` gate reads files git tracks, looking
  for committed credentials. **A finding never quotes the value it matched** — only the file, the
  line and the name of the pattern.
- **It writes only under `.chock/` and files you asked for.** `init --local` never overwrites: a
  file that differs is left alone and chock's version lands beside it as `<name>.chock`.
- **chock makes one network request of its own, and only in an Outpost repository.** `commits`
  asks `outpost branch -r origin` what has been published, because Outpost keeps its refs in a
  database rather than a file git could read locally. It is skipped when no remote is configured.
- **The tools chock runs make their own.** `cargo deny` fetches the advisory database,
  `cargo install` fetches crates, `msrv` may ask rustup for a toolchain, and the first
  `cargo kani` downloads a solver bundle and the nightly it runs on.

## Supply chain

`cargo install <crate> --version X --locked` is how chock installs every tool that is on
crates.io, because a version there is an immutable coordinate: `--version 0.1.0 --locked` resolves
to the same bytes forever.

`chock init --global` compiles `cargo-binstall` first, at the version `tool-versions.env` pins, and
fetches every other tool with it, because compiling twelve tools takes tens of minutes. It takes
only the prebuilt binary the tool's own repository publishes for that version
(`--disable-strategies quick-install` refuses third-party build caches), and compiles when there
is none. A release asset can be replaced after it is published, where a crates.io version cannot.
`cargo binstall chock` fetches chock the same way, from this repository's own releases.

Two tools are not on crates.io — `cargo-mutest` and `outpost`. They are pinned at `0.0.0` in
`tool-versions.env`, which means *build it from its checkout*: `chock init --global` does not try
to fetch them, and `chock doctor` reports the version it found without comparing it to anything.
Every gate that needs one of them is opt-in and says so when it is absent. Judge those two the way
you would judge any binary you build yourself.

`chock doctor` fails on drift for everything else, and never passes a tool whose version it could
not read.

## What runs code while you build

A build script and a proc-macro execute on the machine doing the build, before any test does. Two
gates cover that, and neither is what `cargo deny` checks.

`supply` counts them, keyed by crate, so a *new* one fails until somebody approves it.

`acl` runs [cackle](https://github.com/cackle-rs/cackle), which holds each one to the APIs its entry
in `cackle.toml` grants and fails when one reaches past them — the network, a process, a path.
cackle analyses a build script by running it, so it runs it inside **bubblewrap**; without the
sandbox that analysis is the unsandboxed execution it exists to prevent, and the gate reports that
it could not run rather than passing. Ubuntu 23.10 and newer also need
`sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`.

`cargo acl --no-ui --auto-accept-fixes` writes the config from what the tree already does. Read it
before committing it: it is the list of permissions you are agreeing to.

The `deps` gate runs `cargo deny` over this project's own dependencies.

## chock's own dependencies

Five crates: `proc-macro2`, `serde`, `serde_json`, `syn`, and on Linux and macOS `rustix`, which
chock uses to watch and clean up the process groups it starts without any `unsafe` of its own. The
build scripts of `rustix` 1.1.5 and `libc` 0.2.189 were read before they were admitted: they probe
the compiler and print cargo directives, and neither writes a file or reaches the network.
`cackle.toml` grants `rustix` the process API at run time and nothing else.
