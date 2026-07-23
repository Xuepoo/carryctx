# Contributing

## Prerequisites

- Rust 1.96+
- `just` — command runner
- `lefthook` — Git hooks
- `cargo-nextest` — test runner (optional, `cargo test` works too)
- `cargo-deny` — license/advisory checks
- `cargo-audit` — vulnerability scanning
- `markdownlint-cli2` — Markdown linting

## Setup

```bash
just setup
```

## Development loop

```bash
just check              # fast: format + clippy + check + test
just ci                 # full pipeline
cargo test              # run tests
cargo nextest run       # faster parallel tests (if installed)
```

## Committing

Use Conventional Commits:

```text
feat(task): add atomic task claiming
fix(config): resolve project override precedence
docs(cli): document resume JSON schema
```
