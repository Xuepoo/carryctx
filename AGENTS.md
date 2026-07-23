# CarryCtx CLI Repository Instructions

## Scope

This repository owns the `carryctx` Rust CLI published as a native binary. Complete and verify the CLI before beginning implementation in sibling product repositories.

## Sources of truth

The sibling `../carryctx-docs/` repository defines product behavior:

- `requirements.md` — domain, scope, and acceptance criteria.
- `cli-specification.md` — commands, output envelopes, streams, and exit codes.
- `configuration.md` — TOML configuration, XDG paths, and state locations.
- `engineering-standards.md` — toolchain, architecture, testing, packaging, and Definition of Done.

When implementation exposes a documentation conflict, update the relevant document in `../carryctx-docs/` alongside the code change.

## Architecture

- Keep command parsing in `src/cli.rs` and orchestration in `src/application/`.
- Keep the domain layer pure: it must not import rusqlite, Git, terminal, or filesystem APIs.
- Define persistence contracts under `src/repository/` (traits); implement them under `src/adapter/`.
- Do not execute SQL or Git subprocesses from command handlers.
- Centralize output envelopes, error mapping, and exit codes in `src/output.rs` and `src/error.rs`.
- Store project state in `<git-common-dir>/carryctx/state.sqlite`, shared by linked worktrees.

## Data safety

- Validate all external input at the CLI/application boundary.
- Bind every SQL value as a parameter; never interpolate user input into SQL.
- Whitelist any dynamic SQL identifier.
- Use transactions for multi-step writes and append the audit event in the same transaction.
- Enable foreign keys, WAL, `busy_timeout`, and `synchronous = NORMAL` on database connections.
- Create and verify a backup before migrations or destructive repairs.
- Do not expose SQL details, secrets, or complete environment dumps in errors or logs.

## Development workflow

- Follow test-driven development for behavior: write a failing test, verify the failure, implement the minimum, and rerun the relevant and full suites.
- Use `cargo test` (or `cargo nextest`); integration tests must create disposable Git repositories under a temporary directory.
- Keep generated probes, package tarballs, and reports under `../tmp/`.
- Use Conventional Commits.
- Before completion, run `just ci` and the package smoke test, then record results in `../carryctx-docs/reports/`.

## Public compatibility

Treat command names, JSON schemas, error codes, exit codes, stdout/stderr separation, configuration keys, and persisted migrations as public interfaces. Any intentional incompatibility requires a matching documentation update and migration or compatibility note.
