# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
