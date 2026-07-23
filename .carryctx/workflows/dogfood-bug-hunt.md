# Dogfooding & Edge-Case Bug Hunting SOP

This workflow details the systematic execution plan for testing CarryCtx CLI subcommands and discovering potential bugs.

## Suite 1: Database & Initialization Edge Cases
- Test `carryctx init` in a non-git directory (expect clean error exit code).
- Test `carryctx init` twice in the same git repository (idempotency test).
- Test database auto-migrations with legacy schema files.

## Suite 2: Code Graph Subsystem (`carryctx graph`)
- Test `carryctx graph scan` on repositories with complex module trees and circular imports.
- Test `carryctx graph export --type mermaid/dot/ascii/json` with `--focus` pointing to a non-existent module name.
- Test `carryctx graph export` with invalid `--depth 0` or negative values.
- Test `carryctx graph export --compact` module-level aggregation on 100+ file codebases.

## Suite 3: MCP Stdio JSON-RPC Server (`carryctx mcp`)
- Test JSON-RPC `initialize`, `tools/list`, and `tools/call` with invalid arguments.
- Test MCP tool execution when SQLite database is locked or read-only.

## Suite 4: Stats & Report Export (`carryctx stats`)
- Test `carryctx stats --markdown` on projects with 0 sessions/tasks.
- Test `carryctx stats --output` writing to a non-existent or read-only directory.

## Suite 5: Worktrees & Concurrent State Management
- Test creating `carryctx worktree` with non-existent task IDs.
- Test `carryctx project prune --older-than 0` edge case.
