# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),

## [0.3.0] - 2026-07-24

### Added

- **Intelligent Context Inference**: Implemented `CurrentEntityResolver::resolve_task` to auto-infer tasks based on current git worktree bindings or single active agent tasks. Removes the strict requirement for explicit `--task` flags in `session start`, `checkpoint`, `context`, and `handoff` commands.
- **Detailed JSON Status**: `carryctx status` in JSON format now outputs fully detailed arrays for `tasks`, `activeSessions`, `activeAgents`, and `worktrees` instead of just integer counts, greatly improving parsability for LLMs and external tools.

### Fixed

- **Task Timestamps**: Fixed an issue in `SqliteTaskRepository` where `started_at` and `completed_at` timestamps were not being correctly populated in the SQLite database during `in_progress` or `completed` state transitions.
- **Active Session Filtering**: Fixed a bug in `carryctx status` where the JSON output incorrectly counted *all* historical sessions as active. It now correctly filters by `SessionState::Active`.
- **Borrow Checker Conflicts**: Resolved complex memory lifetime and mutable borrow conflicts (E0502) related to `UnitOfWork` and transaction management in `checkpoint.rs` and `handoff.rs` by correctly scoping the transaction limits.

## [0.2.1] - 2026-07-23

### Added

- **Markdown output**: `carryctx status` now supports `--format markdown` for LLM-friendly output.
- **RUST_LOG tracing**: `RUST_LOG=carryctx=debug` now produces structured debug output.`

### Fixed

- **Empty repo init**: `carryctx init` no longer crashes on freshly initialized Git repos with no commits.
- **Event list agent clash**: The local `--agent` flag in `event list` no longer picks up the `CARRYCTX_AGENT` env var value and filters by raw agent name instead of ULID.
- **Event list task filter**: `event list --task` now correctly resolves display IDs to ULIDs before querying.
- **Progress list display ID**: `progress list --task ET-0001` now resolves the display ID instead of passing it raw to SQL.
- **Session resume state**: `session resume` now correctly transitions the session from Paused to Active (was using `touch_activity` which didn't change state).
- **Session fallback strings**: Pause/Resume/End/Abandon no longer use "unknown" or "default" placeholder strings.
- **Checkpoint fallback**: Checkpoint creation now properly validates that a task reference is provided.
- **Decision FK violation**: Decision domain struct now includes `task_id` instead of inserting an empty string.
- **Worktree list**: Main repository root no longer appears as an unregistered worktree with empty ID/dates.
- **Stats counting**: `total_sessions` and `total_seconds` now include active (ongoing) sessions.
- **Preset install**: Presets with names containing path separators (e.g. `workflows/bugfix`) now correctly create parent directories.
- **Config panic**: Renamed `--project` bool flag to `--cfg-project` in config commands to avoid clap name clash with the global `--project` flag.
- **Progress reorder**: SQL `CASE` expression now uses `WHERE id IN (...)` to avoid setting NULL positions on non-listed items.
- **Post-commit hook**: Now extracts task ID from context before creating checkpoints, preventing silent failures.
- **Dead code**: Removed 17 unused functions across 5 modules, eliminating ~1882 lines of dead code.
- **Empty files**: Cleaned up 4 empty stub files left after dead code removal.
- **nfpm version**: Packaging config version synced with Cargo.toml.

### Security

- **Supply chain**: All dependencies scanned via `cargo deny` and `cargo audit` — 0 vulnerabilities across 152 dependencies.
- **Code safety**: 100% safe Rust — zero `unsafe` blocks, zero `unwrap()`/`expect()` in production code.

## [0.2.0] - 2026-07-23

### Added

- **Project Prune**: New `carryctx project prune --older-than <days>` command clears old completed tasks to keep the database lightweight.
- **Remote Synchronization**: New `carryctx sync push/pull` commands to backup and retrieve state across environments.
- **Agent Analytics**: New `carryctx stats` command outputs tabular metrics and session durations for each active agent.

### Fixed

- **Windows Build**: Fixed a compilation error on Windows by properly gating UNIX-only filesystem permission logic in `hooks.rs`.
- **Dependencies**: Replaced deprecated `Ulid::new()` with `Ulid::generate()` following the `ulid` v3.0.0 crate update.

## [0.1.0] - 2026-07-23

### Added

- **Shell completions**: New `carryctx completions <shell>` command generates completion scripts for bash, zsh, fish, and PowerShell via `clap_complete`.
- **Git hooks integration**: New `carryctx hooks install/uninstall/status` commands install `post-commit` and `prepare-commit-msg` hooks that auto-checkpoint on commit and prepend task IDs to commit messages.
- **Enhanced Doctor**: `carryctx doctor` now detects orphaned tasks (owners deleted), reports active session count, shows git hook installation status with fix hints, and renders human-readable output by default.
- **Code modularisation**: All CLI command handlers extracted from `main.rs` into individual modules under `src/commands/` (e.g. `task.rs`, `session.rs`, `hooks.rs`, `completions.rs`), reducing `main.rs` from ~3100 lines to ~350 lines.

## [0.0.3] - 2026-07-23

### Added

- Extended multi-platform release packages (deb, rpm, apk, archlinux, macOS, Windows).
- Expanded CLI help and documentation for subcommands (`init`, `status`, `resume`, `context`, etc.).

### Removed

- Removed unused directories: `npm/`, `skills/`, `packaging/`, `.carryctx/`.

## [0.0.2] - 2026-07-23

### Fixed

- Resolved global agent name to ULID to prevent FK constraint errors.

### Added - 0.0.2

- Chinese `README.zh-CN.md` instructions.

## [0.0.1] - 2026-07-23

### Added - 0.0.1

- Initial release of CarryCtx CLI with SQLite state backend.
