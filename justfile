# Recipes for working on chock itself. A project that adopts chock needs none of them.

# Show the recipes.
default:
    @just --list

# `setup` installs the pinned Rust with llvm-tools, a nightly, this chock, every tool a gate runs at
# its pinned version, and the hooks. Run it from the root of a clone; running it again is safe.

# Set up this machine to build, test and gate chock.
setup:
    rustup toolchain install
    rustup component add llvm-tools-preview
    rustup toolchain install nightly --profile minimal
    command -v cargo-binstall >/dev/null || cargo install cargo-binstall --locked
    cargo install --path . --locked
    chock init --global
    chock init --local --fast
    chock doctor
