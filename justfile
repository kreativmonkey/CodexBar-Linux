# show available recipes
default:
    @just --list

# fetch dependencies
setup:
    cargo fetch

# debug build
build:
    cargo build

# release build
build-release:
    cargo build --release

# run the tray app (debug)
run:
    cargo run

# run the tray app explicitly
run-gui:
    cargo run -- gui

# query usage from the CLI
usage *args:
    cargo run -- {{args}}

test:
    cargo test

lint:
    cargo clippy --all-targets -- -D warnings

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

# Debian/Ubuntu build dependencies (release-style builds outside Nix)
install-deps:
    bash contrib/ci-install-deps.sh

# local mirror of CI
check: fmt-check lint test
