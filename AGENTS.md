# CarryCtx CLI Repository Instructions

## Scope

This repository owns the `carryctx` Rust CLI published as a native binary.

## Architecture

- Keep command parsing in `crates/carryctx-cli/src/commands/` (and transitionally `src/commands/` until facade `src/` is removed) and orchestration in `crates/carryctx-core/src/application/` (and transitionally `src/application/`). See `carryctx-docs/engineering-standards.md` §5.2 for the 4+1 crate workspace.
- Keep the domain layer pure (`crates/carryctx-core`): it must not import `rusqlite`, Git/`VcsBackend`, `clap`, terminal, filesystem, or network APIs (P1 transition: `clap` retained in `core` for `TaskPriority` `ValueEnum`, to be extracted in P5). Facade `src/` mirrors `crates/carryctx-cli/src/` and will be deleted once `build.rs`/`migrations/` anchoring is verified.
- Define persistence contracts under `crates/carryctx-core/src/repository/` (traits); implement them under `crates/carryctx-sqlite` / `crates/carryctx-vcs` / `crates/carryctx-pack`. Pending deletion, `src/adapter/` remains a thin re-export to `crates/carryctx-cli/src/adapter/`.
- Do not execute SQL or Git subprocesses from command handlers.
- Centralize output envelopes, error mapping, and exit codes in `crates/carryctx-cli/src/output.rs` (`src/output.rs` is a byte-identical re-export) and `src/error.rs`.
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
- Use the project-local `ctxctl` configuration at `.ctxctl/config.toml` to reduce analysis context: start with `ctxctl outline <file>`, inspect targeted implementations with `ctxctl symbol <file> --name <symbol>` or `ctxctl read <file> --lines <ranges>`, inspect imports with `ctxctl deps <file>`, and compress command output with `ctxctl exec <command>`.
- Prefer `ctxctl --json` when passing structured analysis to another agent. Use `ctxctl --output <path>` for intentionally large payloads. `ctxctl` is stateless and read-only; it does not replace CarryCtx task, progress, session, or checkpoint recording.
- Use Conventional Commits.
- Before completion, run `just ci` and verify that all linters and tests pass.

## Public compatibility

Treat command names, JSON schemas, error codes, exit codes, stdout/stderr separation, configuration keys, and persisted migrations as public interfaces. Any intentional incompatibility requires a matching documentation update and migration or compatibility note.
