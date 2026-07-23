# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
