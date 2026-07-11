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

test:
    cargo test

lint:
    cargo clippy --all-targets -- -D warnings

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

# local mirror of CI
check: fmt-check lint test
