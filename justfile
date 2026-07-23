# CarryCtx development commands

registry := "https://github.com/Xuepoo/carryctx"

# Install development dependencies
setup:
    cargo fetch
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

# Security audit
audit:
    cargo audit

deny:
    cargo deny check

machete:
    cargo machete

# Coverage
coverage:
    cargo llvm-cov --all-features --html

# Package smoke test
package-smoke:
    cargo build --release
    @echo "Package smoke: binary available at target/release/carryctx"
    ./target/release/carryctx --version

# GitHub Actions local test
act:
    act pull_request

# Clean build artifacts
clean:
    cargo clean
