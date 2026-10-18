#!/usr/bin/env bash
# The single quality gate. A slice is not done until this passes.
# Runs as the git pre-commit hook (scripts/pre-commit.sh) and in CI (.github/workflows/ci.yml).
set -euo pipefail

# The repository is the folder above this script. Not `git rev-parse
# --show-toplevel`: in a pre-commit hook of a linked worktree git sets
# GIT_DIR, and that answers with the current folder instead.
cd "$(dirname "$(realpath "$0")")/.."

step() {
    printf '\n\033[1m==> %s\033[0m\n' "$*"
}

step "cargo fmt --check"
cargo fmt --all --check

step "cargo clippy (warnings are errors)"
cargo clippy --workspace --all-targets --locked -- -D warnings

step "cargo test"
cargo test --workspace --locked

step "cargo deny check"
# -D: if the pinned hash of ratex-katex-fonts' OFL.txt stops matching, cargo-deny
# silently falls back to the crate's "MIT" label and only warns that the OFL-1.1
# exception was not used. That must fail the gate (ADR 0021, point 8).
cargo deny --locked check -D license-exception-not-encountered --hide-inclusion-graph

printf '\n\033[1;32mAll checks passed.\033[0m\n'
