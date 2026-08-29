# CarryCtx development commands

registry := "https://github.com/Xuepoo/carryctx"

setup:
    cargo fetch
    lefthook install

dev *args:
    cargo run -- {{args}}

build:
    cargo build --release

check-fast:
    cargo fmt --check
    cargo clippy --workspace -- -D warnings
    cargo check
    cargo test --lib

check:
    cargo fmt --check
    cargo clippy --workspace -- -D warnings
    cargo check
    cargo test
    just markdownlint
    cargo deny check
    cargo audit

ci:
    just fmt-check
    just lint
    just typecheck
    just test
    just markdownlint
    just actionlint
    just package-smoke

typecheck:
    cargo check

lint:
    cargo clippy --workspace -- -D warnings

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

test:
    cargo test

test-unit:
    cargo test --lib

test-integration:
    cargo test --test '*'

markdownlint:
    markdownlint-cli2 "**/*.md" "#target" "#node_modules" "#.worktrees"

actionlint:
    actionlint -color -shellcheck=

audit:
    cargo audit

deny:
    cargo deny check

machete:
    cargo machete

coverage:
    cargo llvm-cov --all-features --html

package-smoke:
    @tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT; \
    cargo package --locked; \
    cargo install --locked --force --root "$tmp/root" --path .; \
    "$tmp/root/bin/carryctx" --version | grep -F "carryctx 0.8.0"; \
    test -x "$tmp/root/bin/carryctx"

release-check:
    just fmt-check
    just lint
    just typecheck
    just test
    just markdownlint
    just actionlint
    just package-smoke
    @cargo metadata --no-deps --format-version 1 | jq -e '.packages[0].version == "0.8.0"' >/dev/null
    @test -n "$$(awk '/^## \[0\.8\.0\]/{found=1} END{print found}' CHANGELOG.md)"

act:
    act pull_request

clean:
    cargo clean
