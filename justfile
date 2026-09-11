# CarryCtx development commands

registry := "https://github.com/Xuepoo/carryctx"

# Install development dependencies
setup:
    cargo fetch
    bun install
    lefthook install

# Run the CLI with arguments
dev *args:
    cargo run -- {{args}}

# Build release binary
build:
    cargo build --release

# Fast check: format + lint + check + test
check-fast:
    cargo fmt --check
    cargo clippy --workspace -- -D warnings
    cargo check
    cargo test --lib

# Full check: all quality gates
check:
    cargo fmt --check
    cargo clippy --workspace -- -D warnings
    cargo check
    cargo test
    just markdownlint
    cargo deny check
    cargo audit

# CI pipeline (runs in CI)
ci:
    just fmt-check
    just lint
    just typecheck
    just test
    just markdownlint
    just actionlint
    just package-smoke

# Type-check (alias for cargo check)
typecheck:
    cargo check

# Lint with clippy
lint:
    cargo clippy --workspace -- -D warnings

# Format code
fmt:
    cargo fmt

# Check formatting
fmt-check:
    cargo fmt --check

# Run tests
test:
    cargo test

test-unit:
    cargo test --lib

test-integration:
    cargo test --test '*'

# Markdown linting
markdownlint:
    markdownlint-cli2 "**/*.md" "#target" "#node_modules" "#.worktrees"

# GitHub Actions workflow linting (matches CI: shellcheck integration disabled)
actionlint:
    actionlint -color -shellcheck=

# Security audit
audit:
    cargo audit

deny:
    cargo deny check

machete:
    cargo machete

# Run the repository's available unused-dependency checker, if supported.
dependency-audit:
    @set -eu; \
    if command -v cargo-machete >/dev/null 2>&1; then \
        cargo machete; \
    else \
        echo 'SKIP: cargo-machete unavailable; this Rust repository has no supported Knip-equivalent installed.'; \
    fi

# Coverage
coverage:
    cargo llvm-cov --all-features --html

# Package smoke test (offline build check — the workspace path deps are not
# published to crates.io, so `cargo package` resolution is verified by CI
# publish instead; here we verify the release binary builds and reports)
package-smoke:
    @set -eu; \
    tmp=`mktemp -d`; trap 'rm -rf "$tmp"' EXIT; \
    cargo build --release --locked; \
    binary="target/release/carryctx"; \
    test -x "$binary"; \
    version=`"$binary" --version`; \
    test "$version" = "carryctx 0.11.2"

# Release verification
release-check:
    just release-worktree-clean
    just fmt-check
    just lint
    just typecheck
    just test
    just markdownlint
    just actionlint
    just dependency-audit
    just package-smoke
    @cargo metadata --no-deps --format-version 1 | jq -e '.packages[0].version == "0.11.2"' >/dev/null
    @test -n "$$(awk '/^## \[0\.11\.2\]/{found=1} END{print found}' CHANGELOG.md)"

# Require a clean Git worktree before release verification.
release-worktree-clean:
    @set -eu; \
    if git diff-index --quiet HEAD -- && test -z "`git ls-files --others --exclude-standard`"; then \
        exit 0; \
    else \
        echo 'ERROR: release-check requires a clean Git worktree.' >&2; \
        git status --short >&2; \
        exit 1; \
    fi

# GitHub Actions local test
act:
    act pull_request

# Clean build artifacts
clean:
    cargo clean
