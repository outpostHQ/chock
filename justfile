# chock — one-way quality gates. `just` with no argument lists them.
# Exit codes: 0 the gate passed, 1 the gate tripped, 2 the gate could not run.

# Every recipe is an alias for `chock run`: the rule lives in the binary, where it has tests.
# `chock gates` says what each gate measures and which of them `chock run` enforces on its own.

default:
    @just --list

# Correctness. Everything that must pass before a change lands.
gates: test lint doc modcheck

# Shape. Ratchets against a committed baseline, and the supply chain.
quality: manifest placement profile features hygiene source deps unused typos msrv slop bigfiles splits lean complexity duplication codeslop crap

test:
    chock run test

lint:
    chock run lint

doc:
    chock run doc

modcheck:
    chock run modcheck

# Every dependency names an immutable version or revision.
manifest:
    chock run manifest

# Every dependency is declared where a clone can build it, for the builds that use it.
placement:
    chock run placement

# The release profile takes the free wins, and nothing silences a check for the whole build.
profile:
    chock run profile

# A declared feature nothing reaches, or a cfg naming a feature no manifest declares.
features:
    chock run features

# The repository tracks a secret-bearing file, a credential literal, or build output.
hygiene:
    chock run hygiene

# Suppressed lints and the source shapes that hide a failure.
source:
    chock run source

# Function bodies repeated across the tree, against a committed baseline.
duplication:
    chock run duplication

deps:
    chock run deps

sort:
    chock run sort

dupdeps:
    chock run dupdeps

supply:
    chock run supply

acl:
    chock run acl

padding:
    chock run padding

# Opt-in: every measure outpost reports, as one census, so no total drifts and none stops arriving.
measures:
    chock run measures

hazards:
    chock run hazards

unreferenced:
    chock run unreferenced

# Opt-in: the lens roster, so a lens outpost retires does not read as its hazards being fixed.
lenses:
    chock run lenses

# Opt-in: the regions outpost could not read. An advisory; it never fails.
unread:
    chock run unread

scan:
    chock run scan

boundaries:
    chock run boundaries

duplicates:
    chock run duplicates

unused:
    chock run unused

typos:
    chock run typos

msrv:
    chock run msrv

slop:
    chock run slop

bigfiles:
    chock run bigfiles

splits:
    chock run splits

lean:
    chock run lean

complexity:
    chock run complexity

codeslop:
    chock run codeslop

# The report both `coverage` and `crap` read. chock regenerates it when it is missing or stale, so
# this recipe is the explicit way to ask for it rather than the only one.
coverage:
    cargo llvm-cov nextest --workspace --all-targets --no-tests=pass --lcov --output-path lcov.info

crap: coverage
    chock run crap

# Opt-in: kani verifies the harnesses a project has written. Nothing to verify without them.
proof:
    chock run proof

# Opt-in: a second engine that builds every mutant into one binary. Needs a nightly and a local
# mutest-rs checkout, so it is asked for by name rather than assumed.
mutest:
    chock run mutest

# Opt-in: compiles the tree rather than scanning it, and needs a nightly toolchain.
unused-deep:
    chock run unused-deep

# An instrument, not a gate: it reports a number nobody has agreed to be held to.
bsize:
    chock run bsize

# Opt-in: the project's own checks, from `commands` in .chock/config.json. The first runs those
# that need no compiler, at every commit; the second those that compile, at push.
commands:
    chock run commands

commands-build:
    chock run commands-build

# Opt-in: the phrases `forbidden` in .chock/config.json lists.
phrases:
    chock run phrases

# Opt-in: rustfmt, alone, so a commit need not wait for clippy to learn a file is unformatted.
fmt:
    chock run fmt

assertions:
    chock run assertions

binsize:
    chock run binsize

citations:
    chock run citations

commits:
    chock run commits

dead:
    chock run dead

history:
    chock run history

idempotent:
    chock run idempotent

miri:
    chock run miri

nesting:
    chock run nesting

unsafety:
    chock run unsafety

wiring:
    chock run wiring

# Record where this tree is; the ratchets then hold it there.
baseline:
    chock baseline

doctor:
    chock doctor
