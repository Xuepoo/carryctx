# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),

## [0.6.0] - 2026-08-22

### Added

- **Persistent agent teams** (`carryctx team`): a project-scoped Team survives session boundaries, window closes, and linked worktrees, because it lives in the same shared `state.sqlite` as everything else. `team create --name <name> [--commander <agent>]`, `team member add/remove <team> --agent <agent> [--role <role>]`, and `team commander set <team> --agent <agent> | --clear` manage the roster. A commander must be a member of its own team, and removing the current commander is rejected until it is replaced or explicitly cleared. Tasks associate with a team through `task team set <task> --team <team|none>` and `task team unset <task>`; association is optional and never changes a task's lifecycle, ownership, dependencies, or scopes.
- **Read-only team projections**: `carryctx team status [<team>]` returns the roster with each member's role, kind, active session, non-terminal tasks, and aggregate counts (total, commanders, subagents, unassigned). `carryctx team context [<team>] [--agent-for <agent>] [--task <task>]` rebuilds a structured working context purely from durable records — members, tasks, dependencies, scopes, progress, scope conflicts, blockers, checkpoints, decisions, handoffs, and recent events. Both commands open the database read-only, run no migrations, and write nothing: no events, no claims, no sessions. A commander sees the whole coordination view; `--agent-for` and `--task` narrow every collection consistently, and dependency edges are closed over the returned task set.
- **Agent kind and task role metadata**: `agent register --kind commander|subagent` records an agent's role in a team, enforced by both the CLI and a schema `CHECK` constraint. `task create --required-role` and `task edit --required-role` record which role a task expects. Both fields are nullable and additive — existing agents and tasks are untouched.

### Changed

- **CarryCtx records team state; it does not run your agents.** Spawning, routing, retries, scheduling, and model selection stay with your harness. CarryCtx owns the durable answer to "who is on this team, what are they working on, and what does each one need to know" — and nothing beyond that. There is no scheduler, no worker runtime, no lease/heartbeat machinery, and no prompt cache in this release.
- **Multiple active tasks per agent are explicitly supported.** The legacy `task.single_active_task_per_agent` configuration key was documented as a limit but never enforced. It is now compatibility-only and non-enforcing: `claim`, `start`, and `assign` never fail on a task count. Capacity policy belongs to the commander or the harness.

### Fixed

- **Migrations create and verify a backup first**: any upgrade that has migrations to apply now writes a verified `VACUUM INTO` snapshot to `<state-dir>/backups/` and integrity-checks it through a separate read-only connection before touching the schema. Fresh initialization creates no backup. Backup failure aborts before any schema mutation.
- **Migration history is validated, not assumed**: applied versions must form a contiguous prefix, so a database missing an interior migration (for example `1..11` plus `13`) now returns `MIGRATION_REQUIRED` instead of reporting itself up to date. Newer-than-bundled schemas and checksum mismatches are rejected on `open`, `open_readonly`, `migrate`, and every public inspection method.
- **Concurrent migrations no longer fail the loser**: applied versions are re-checked inside the `BEGIN IMMEDIATE` transaction, so a process whose migration was already committed by another process skips it cleanly instead of failing on a unique-constraint violation.
- **Table-rebuild migrations restore the caller's `foreign_keys` setting**: the prior PRAGMA value is captured and restored on both the success and rollback paths, instead of being unconditionally forced back to `ON`.
- **Every state-changing command holds a project admission lock**: writable commands acquire the lock before opening the database and hold it until the connection is dropped. Read-only team projections skip it entirely. Lock release is ownership-aware — a lock is removed only when the on-disk metadata still matches the owning token, operation, host, and PID — and a lock held by another host is never reclaimed from local process state. Malformed lock metadata returns `STATE_CONFLICT`, and a failed metadata write cleans up the partial lock directory.
- **`project restore` is atomic and recoverable**: the requested backup is validated read-only before anything changes, a verified pre-restore backup is created, the replacement is staged and re-validated as a sibling candidate, the original is preserved via hard link, and the swap is a single `rename`. Restore no longer overwrites a live database through an open connection. An interrupted restore is recovered phase-aware on the next run: a validated candidate is promoted when the active database is missing, otherwise a valid original is restored; when neither can be recovered, the journal is preserved and `DATABASE_ERROR` is returned rather than deleting the only valid copy.
- **`sync pull` validates before it replaces**: the remote database must pass `PRAGMA integrity_check` and `PRAGMA foreign_key_check`, contain the required tables, columns, and exactly one well-formed project row, and be migration-compatible. Pulling a valid database that belongs to a **different project** is now rejected with `SYNC_PROJECT_MISMATCH` instead of silently replacing your project identity. A verified pre-pull backup is taken, and replacement uses the same staged-candidate/atomic-rename path as restore. `sync push` checkpoints the WAL and publishes a verified `VACUUM INTO` snapshot rather than copying a live database file. Both remain a plain local file copy — the binary still ships no network stack.
- **Recovery journals cannot be pointed outside the state directory**: journal operation IDs must be canonical ULIDs, journal paths are confined to trusted siblings under `<git-common-dir>/carryctx`, and the ID inside the journal must match its filename. A malformed journal returns `DATABASE_ERROR` before any cleanup runs, so a crafted journal can no longer direct recovery to delete arbitrary files.
- **`doctor` detects stale worktree registrations**: registrations whose directories no longer exist are reported. Detection is read-only; pruning requires the explicit `doctor --prune-stale-worktrees --yes`, supports `--dry-run`, detaches dependent session/checkpoint references, and records a `worktree.pruned` audit event in the same transaction as the deletion.
- **`session start` records a real successor**: the `session.ended` supersession payload now carries the ID of the session that actually replaced it, and predecessor update, supersession event, and successor creation happen in one immediate transaction. Previously `superseded_by` held a freshly generated ULID that matched no session.
- **Task transition event names are canonical and legacy-readable**: Transition writes now use the public event types `task.completed`, `task.released`, and `task.cancelled`. Event filters also match historical `task.completeed`, `task.releaseed`, and `task.canceled` rows without rewriting the append-only audit log.
- **`event list --until` no longer collides with `--limit`**: the two filters shared a SQL placeholder index, so combining them bound the wrong parameter.
- **`team create` with a duplicate name returns `STATE_CONFLICT`**: it previously surfaced the raw SQLite unique-constraint text as `DATABASE_ERROR`, unlike every other duplicate-resource path.

## [0.5.8] - 2026-08-17

### Fixed

- **CLI panic due to argument alias conflict**: Renamed the subcommand argument representing a task's owner (used in `task create`, `task list`, and `search`) from `--owner` to `--assignee`. This fixes a startup panic where `clap` detected a naming collision between the subcommand argument and the new global `--agent` alias `--owner` introduced in `0.5.7`.

## [0.5.7] - 2026-08-17

### Added

- **`--owner` alias for global `--agent` flag**: Callers can now use `--owner <name|ULID>` interchangeably with `--agent` across all commands.

### Fixed

- **Task priority values are discoverable and validated before write** ([#82](https://github.com/Xuepoo/carryctx/issues/82)): `task create --priority` and `task edit --priority` now use a typed enum with discoverable `--help` values (`[possible values: low, normal, high, urgent]`), support common aliases (`critical` → `urgent`, `medium` → `normal`, `backlog` → `low`), and reject invalid inputs during CLI argument parsing so `--dry-run` performs accurate validation.
- **`decision supersede` output renders target and superseding IDs** ([#83](https://github.com/Xuepoo/carryctx/issues/83)): Fixed compact text formatter which printed `Decision <empty> superseded by <empty>`; output now renders `Decision DEC-0001 superseded by DEC-0002` and handles both display IDs and ULIDs.
- **Concurrent CLI writes serialized with immediate transactions** ([#84](https://github.com/Xuepoo/carryctx/issues/84)): Upgraded SQLite write transactions across all subcommands to `TransactionBehavior::Immediate` (preventing SQLite shared-lock upgrade deadlocks) and raised `busy_timeout` to 10,000ms so concurrent agent processes cleanly wait and succeed without failing with `database is locked`.

## [0.5.6] - 2026-08-12

### Fixed

- **`handoff accept --claim-task` was parsed but ignored** ([#75](https://github.com/Xuepoo/carryctx/issues/75)): the flag was destructured away (`claim_task: _`), so accepting a handoff never claimed its task while the `--help` text promised "Automatically claim the associated task upon accepting the handoff". The accepting agent is now resolved and the handoff's task is claimed in the same transaction, mirroring `task claim` (task moves to `in_progress` and its ownership transfers to the accepting agent). If the task cannot be claimed (already owned, wrong status, incomplete dependencies), the whole accept fails with a standard error envelope instead of silently dropping the documented behavior. Regression test: accept as a second agent with `--claim-task` and assert the task is owned and `in_progress`.
- **`handoff show`/`accept`/`reject`/`close` and `progress show` returned a non-standard not-found error** ([#76](https://github.com/Xuepoo/carryctx/issues/76)): a missing ref short-circuited with a bare `ExitCode::ResourceNotFound`, printing nothing to stdout and exiting without the standard error envelope — machine consumers (`--json`, MCP) got no parseable error. These commands now route `RESOURCE_NOT_FOUND` through the standard error path like `task show`: `success:false` envelope on stderr, exit code 7. Regression tests: three new envelope/exit-code assertions.

## [0.5.5] - 2026-08-11

### Fixed

- **`decision list --task` was accepted and ignored** ([#71](https://github.com/Xuepoo/carryctx/issues/71)): the command had no `--task` flag of its own, so the global `--task` flag parsed and fell through — `decision list --task CTX-0320` returned every decision in the project, and a nonexistent ref returned the full dump with no error, unlike `progress list --task` which filters and validates. `decision list` now has a real `--task <ref>` flag: the ref is resolved and validated first (`RESOURCE_NOT_FOUND` for a bad ref), and the query narrows to that task's decisions. Regression test: filtered list must narrow the row count and every row must belong to the requested task.

## [0.5.4] - 2026-08-11

### Fixed

- **`task create --description` was parsed and then discarded** ([#70](https://github.com/Xuepoo/carryctx/issues/70)): the flag destructured the value and never forwarded it, so every task was created with `description = NULL` while reporting success, and no CLI path could fill the field afterwards (`task edit` exposed only `--title` and `--priority`). `--description` is now threaded through `create_task` into the insert (the `NewTask`/SQL already supported it), and `task edit` gained `--description` so existing tasks can be filled in or revised. The `task.created`/`task.edited` audit payloads now carry the description.
- **`task depend --kind <invalid>` exited 2 with no output at all** ([#69](https://github.com/Xuepoo/carryctx/issues/69)): the parse error was constructed and then thrown away by `.map_err(|e| e.exit_code)?`, so neither text mode nor `--json` mode rendered anything — and the `--kind` help text advertised `blocks`/`relates_to`, neither of which is accepted. Invalid kinds now render through the standard error path: text mode prints `Unknown dependency kind: blocks` (JSON envelope on stderr), `--json` mode emits the regular `INVALID_ARGUMENTS` envelope, and the help text documents the real values (`strong` or `informational`, default `strong`; the `info` alias still works). The same silent-discard pattern for `task create/list --status` and `task create/edit --priority` parse failures was fixed identically.
- **`task show`/`task list` compact text ignored `--fields` and `[output.fields]`** ([#68](https://github.com/Xuepoo/carryctx/issues/68)): `task_summary`/`tasks_summary` rendered a hardcoded template, so requesting more fields (`depends_on`, `blocks`) showed nothing new, and requesting fewer fields printed empty brackets/padded columns for the removed keys. Under an explicit projection the compact line now renders exactly the projected fields and appends any projected-but-unrendered ones as `label: value` fragments — dependency edges as their `display_id` list, e.g. `CTX-0320 [in_progress] fix(text): … — needs CTX-0321` — so `--fields display_id,status,title,depends_on,blocks` finally shows the edges, `--fields display_id` emits a clean `CTX-0320`, and default (unprojected) output is unchanged.

## [0.5.3] - 2026-08-10

### Fixed

- **Broken pipe panicked instead of exiting cleanly**: piping a multi-line compact output to a consumer that stops early (`carryctx checkpoint list | head -3`, `carryctx task list | head -1`) crashed with `thread 'main' panicked ... failed printing to stdout: Broken pipe (os error 32)` because `println!` aborts on EPIPE. The panic is now intercepted and converted into the conventional Unix exit code 141 (128 + SIGPIPE), so piped invocations terminate silently like every other CLI. Regression test: a closed stdout pipe must not produce a `panicked` message on stderr.

## [0.5.2] - 2026-08-10

### Fixed

- **`agent current --agent <name>` failed with `VALIDATION_FAILED` even when an explicit agent was passed** ([#64](https://github.com/Xuepoo/carryctx/issues/64)): the resolver always honored `--agent` (the failure only occurred without it), but the error gave no way to recover. The validation error now lists the available agents — `Multiple agents exist (antigravity, claude-code, kiro, omp, opencode); specify --agent <name>.` — and a regression test locks in that `agent current --agent opencode` resolves the named agent with five agents registered.
- **`task create` buried `display_id` deep inside the JSON output** ([#65](https://github.com/Xuepoo/carryctx/issues/65)): every entity command now prints a compact one-line summary in text mode — `Task created: CTX-0321` — so the next action (`task start`, `progress note --task`, `handoff create --task`) needs no JSON parsing or second query.

### Changed

- **Compact text output by default.** Text mode (`--format text`, the default) no longer dumps pretty-printed full records for entity commands (`task.*`, `agent.*`, `checkpoint.*`, `handoff.*`, `session.*`, `progress.*`, `decision.*`, `worktree.*`, `event.*`, `search`, `status`). It emits one short line per record with only high-attention fields (`display_id`, status, title/summary, short IDs), keeping agent context small on real projects — e.g. `status` on a 320-task project went from a multi-megabyte pretty JSON blob to a 6-line summary plus the active tasks. ULIDs are truncated to 8 chars, timestamps to `YYYY-MM-DD HH:MM`, free text to 80 chars.
- **Full detail on demand.** `--verbose` (global flag) or `[output] verbose = true` in `.carryctx/config.toml` restores the previous full pretty-printed text output. JSON output is unchanged and always carries the complete envelope.
- **Field projection.** The new global `--fields display_id,status,summary` flag and the per-command `[output.fields]` table (e.g. `"handoff.list" = ["display_id", "status", "summary"]`) trim entity records to an allowlist in both text and JSON output; the envelope structure is preserved. CLI flags override configuration.

## [0.5.1] - 2026-08-10

### Fixed

- **`handoff list` returned every record regardless of status** ([#62](https://github.com/Xuepoo/carryctx/pull/62)): measured on a real project the default listing returned 7 handoffs of which 1 was actionable and 6 were closed, making it useless as a session-start check. The default is now pending/open only. `--all` restores the unfiltered view; `--status <state>` selects a specific status (accepts both the persisted SQL spelling — `pending`, `declined` — and the domain name — `open`, `rejected`); `--for-agent <name|ulid|role>` filters by target agent.
- **`handoff create` display-id collisions on rapid inserts** ([#62](https://github.com/Xuepoo/carryctx/pull/62)): `handoff create` derived `display_id` by truncating a fresh ULID to 8 characters; two handoffs created inside the same millisecond share that prefix and collide on the unique index. The command now delegates to the application-layer `create_handoff()` and the `sequences`-backed allocator, producing sequential `HO-0001`, `HO-0002`, … ids that never collide. The application-layer function also carried the wrong prefix (`HF`); corrected to `HO` to match all existing records and documentation (the function had no callers before this release, so the wrong prefix never reached production data).

## [0.5.0] - 2026-08-10

### Fixed

- **`stats` billed every session to "now"** ([#60](https://github.com/Xuepoo/carryctx/issues/60)): `session end`, `session abandon`, and auto-end only updated `state`/`summary`/`updated_at` — `ended_at` had no write site anywhere in the codebase, so `stats` computed `Utc::now() - started_at` for every session, ended or not. An agent with 36 real sessions showed 4784h of "Time Spent". `update_state` now writes `ended_at` on terminal transitions (and `mark_overdue_stale` does too), `stats` falls back to `last_activity_at` instead of `Utc::now()` for sessions still open, and migration `0011` backfills `ended_at = last_activity_at` for pre-0.5.0 terminal sessions.
- **`stats` per-agent Checkpoints column was always 0** ([#60](https://github.com/Xuepoo/carryctx/issues/60)): the per-agent subquery joined `checkpoints.session_id = sessions.id`, but checkpoints are never created with a `session_id` (they carry `agent_id`), so the column read 0 while the overview totalled real checkpoints. The count now uses `checkpoints.agent_id`.
- **`handoff create --target <name-or-role>` always failed with `FOREIGN KEY constraint failed`** ([#60](https://github.com/Xuepoo/carryctx/issues/60)): the `--target` value was inserted verbatim as `to_agent_id`, so anything but a raw agent ULID violated the `agents(id)` FK — despite the help text promising "agent ULID or role name". The target now resolves by name, ULID, or role before insert, with a clear `Target agent '…' not found` error when unresolvable.
- **`task start` after `task claim` errored "Cannot transition from InProgress to start"** ([#60](https://github.com/Xuepoo/carryctx/issues/60)): `claim` already moves Ready → InProgress, so the documented claim-then-start workflow could never succeed. `start` on an InProgress task is now an idempotent no-op.
- **Text-mode warnings corrupted the JSON stream** ([#60](https://github.com/Xuepoo/carryctx/issues/60)): `render_json_with_warnings` appended `\nwarning: …` after the pretty-printed JSON document on stdout, breaking every `| jq` / `json.load` consumer (surfaced by the 0.4.6 `render_and_print_with_warnings` change). Warnings now go to stderr in text mode; `--json` mode keeps them in the envelope.

## [0.4.6] - 2026-08-07

### Fixed

- **`strict_completion` guard unreachable from the only states `Complete` allows** ([#57](https://github.com/Xuepoo/carryctx/issues/57)): `evaluate_transition`'s `(Ac::Complete, St::Review | St::InProgress) => true` arm matched unconditionally, before the arm that denies completion when `strict_completion` is on and the task has open progress items — so that guard could only ever be reached from a state (e.g. `Ready`) where `Complete` was already an invalid transition, reporting a misleading "open progress items" error when the real blocker was the state itself. From `InProgress` with `strict_completion = true` and an open item, `task complete` silently succeeded and the item stayed open. The guard now runs before the unconditional arm.
- **`task <transition>` commands always discarded their warnings** ([#58](https://github.com/Xuepoo/carryctx/issues/58)): `SuccessEnvelope.warnings` exists and `evaluate_transition` populates it (e.g. "Task has open progress items." when completing non-strictly), but all 8 transition call sites in `src/commands/task.rs` bound it as `_w` and dropped it before calling `render_and_print`, so the signal never reached JSON or text output. Added `render_and_print_with_warnings` and threaded it through `release`/`start`/`block`/`unblock`/`review`/`complete`/`cancel`/`reopen`.

## [0.4.5] - 2026-08-07

### Added

- **`decision add --rationale`** ([#55](https://github.com/Xuepoo/carryctx/issues/55)): `decisions.rationale` was `NOT NULL` and FTS-indexed but no CLI flag could ever set it, so every decision was stored with `rationale = ''` — the field most worth searching (the "why" behind a decision) was guaranteed to contain nothing. `decision add` now accepts `--rationale <RATIONALE>`, the column is nullable (migration `0010_decision_rationale`, distinguishing "not supplied" from "supplied as empty"), and `decision search` matches against it alongside title/context/decision/consequences.

### Fixed

- **`decision add` display ID collisions on rapid inserts** ([#54](https://github.com/Xuepoo/carryctx/issues/54)): `decision add` derived `display_id` by truncating the row's ULID to 8 characters, quantising it to a 1024ms bucket; every decision created within the same bucket produced an identical `display_id`, and since the column is `NOT NULL UNIQUE`, all but the first failed with a raw `DATABASE_ERROR: UNIQUE constraint failed`. `decision add` is now wired through the same `sequences`-backed allocator tasks and progress items already use, producing sequential, human-readable `DEC-0001`, `DEC-0002`, … ids that never collide. Decisions were the only entity affected; tasks and progress items were already unaffected.

## [0.4.4] - 2026-07-28

### Fixed

- **Hyphenated search queries** ([#47](https://github.com/Xuepoo/carryctx/issues/47)): `carryctx search aria-owns` no longer passes the bare hyphen directly to SQLite FTS5, where it was parsed as query syntax and failed with the misleading `DATABASE_ERROR: no such column: owns`. Bare terms containing FTS5-special characters are now quoted as literals while existing quoted phrases, uppercase `AND`/`OR`/`NOT`, and trailing `*` prefix searches remain supported.
- **npm packages contained no executable**: v0.4.3 corrected the platform package names, but the publish job looked for `carryctx`/`carryctx.exe` after the build job had renamed those artifacts to `carryctx-<target>`, so the four published platform tarballs contained only `package.json`; the initial Windows name `carryctx-cli-win32-x64` was additionally rejected by npm spam detection, and the root package was consequently skipped. Packaging now copies the actual renamed artifact, keeps the `.exe` suffix on Windows, fails before publication if the binary is missing or empty, verifies the platform tarball payload, uses the accepted `carryctx-cli-windows-x64` package name, lets every platform job finish independently, makes root publication rerunnable, and performs a clean `npm install --save-dev carryctx@<version>` plus `carryctx --version` smoke test against the registry before the release workflow can pass. The v0.4.4 Windows and root packages were recovered under the corrected name and verified from a clean project after the tag workflow failed.

## [0.4.3] - 2026-07-28

### Fixed

- **npm platform packages never actually published**: `carryctx` on npm has shipped since v0.4.0 as a thin launcher (`carryctx`) with five per-platform binary packages pulled in via `optionalDependencies`. Those platform packages were failing to publish on every release — CI reported green because the publish step piped its output through `|| echo "npm publish failed (may already exist)"`, silently swallowing the real error. Root cause was two-fold: (1) the platform packages were scoped (`@carryctx/cli-*`), and npm requires the `@carryctx` organization to exist before any scoped package can be published — it never did, so every publish 404'd with "Scope not found"; (2) the packaging matrix was missing `aarch64-unknown-linux-gnu` and used raw Rust target triples as package-name suffixes (e.g. `x86_64-unknown-linux-gnu`) instead of the npm platform-package convention (`linux-x64-gnu`) that the root launcher and `optionalDependencies` actually looked up, so even a successful publish would have been unresolvable at runtime. Net effect: every `npm install carryctx` since v0.4.0 installed a launcher with no binary behind it, and running `carryctx` failed with `no binary found`. Fixed by renaming the platform packages to unscoped `carryctx-cli-<platform>` (no org required), aligning the naming with the launcher's runtime resolution, adding the missing `linux-arm64-gnu` target to the publish matrix, and replacing the blanket `|| echo` with a check that only tolerates a version that's already published (any other failure now fails the job).

## [0.4.2] - 2026-07-28

### Added

- **`carryctx search`** ([#45](https://github.com/Xuepoo/carryctx/issues/45)): full-text search across tasks, progress items, checkpoints, and decisions, backed by SQLite FTS5 and ranked by `bm25()`. Every hit resolves back to the owning task's display ID, status, and (where known) the branch it was worked on — the branch name alone rarely carries what actually changed, which was the whole motivation for the feature. `--type` scopes to one entity kind, `--status`/`--owner` narrow by the owning task's status/owner agent, `--limit` caps result count, and `--format markdown`/`--json` match the rest of the CLI's output contract. `--owner` is deliberately not named `--agent` to avoid the same global-`--agent`/`CARRYCTX_AGENT` name-collision bug fixed in `event list` back in 0.2.1. New migration `0009_search.sql` adds one FTS5 virtual table per searchable entity, kept in sync by triggers on insert/update/delete, with a one-time backfill for rows that existed before the migration ran.

## [0.4.1] - 2026-07-26

### Fixed

- **Pending migrations were never backfilled onto pre-existing databases** ([#42](https://github.com/Xuepoo/carryctx/issues/42)): every command opened the project database via `ProjectDatabase::open`, which never applied pending migrations — only `carryctx init` (via `create_fresh`) and the explicit `carryctx project migrate` command did. A database created before a new migration shipped (e.g. `0008_jj_compat`) stayed stuck at its old schema version indefinitely; `carryctx checkpoint` then failed with `DATABASE_ERROR: no column named vcs_backend` while `carryctx doctor` incorrectly reported `"Schema version up to date"` (a hardcoded string, not an actual check). Every command now backfills pending migrations transparently on open, and `doctor`'s `database.schema` check now genuinely queries `pending_migrations()` and reports `error` with the specific pending migration names if any remain.

## [0.4.0] - 2026-07-25

### Added

- **jj colocation detection (Phase 1 of Jujutsu compatibility)**: `carryctx doctor` now reports an informational `vcs.jj_colocation` check when a `.jj/` directory sits alongside `.git/` (Jujutsu's colocated mode), so users get an explicit signal that CarryCtx sees the jj setup. See `carryctx-docs/plans/2026-07-25-jujutsu-compatibility.md`.
- **Checkpoint `vcs_backend`/`changed_files` (Phase 2 of Jujutsu compatibility)**: checkpoints now record `vcs_backend: "git" | "jj"` and a `changed_files` list that stays accurate under both backends. Under jj colocation, `staged_files`/`modified_files`/`untracked_files` are now reported as empty instead of a three-way split that jj's automatic working-copy snapshotting makes unreliable (read-only jj commands can write to the Git index as a side effect); `dirty` and diff stats remain accurate under both backends. New migration `0008_jj_compat.sql`.
- **`worktree create` refuses under jj colocation (Phase 3 of Jujutsu compatibility)**: `carryctx worktree create` now detects jj colocation and refuses with a clear error instead of creating a directory that neither `jj workspace list` recognizes nor carryctx's own state commands can read from inside (jj's secondary workspaces, created via `jj workspace add`, have no local `.git/` at all). The error points at `jj workspace add` directly, plus `carryctx worktree bind` from the primary colocated checkout if CarryCtx tracking is needed.
- **`hooks install` refuses under jj colocation (Phase 4 of Jujutsu compatibility)**: `carryctx hooks install` now detects jj colocation and refuses instead of silently installing `post-commit`/`prepare-commit-msg` hooks that `jj commit`/`jj describe` never trigger (jj writes commits via `jj git export`, bypassing Git's hook mechanism entirely). Points users at manual `carryctx checkpoint` as the interim workflow.

### Fixed

- **Checkpoint `head` on zero-commit repos**: `GitSnapshot::head` (used by `checkpoint create` and `worktree show`) is now `Option<String>` and reports `null` instead of an empty string `""` when `git rev-parse HEAD` fails (e.g. a brand-new repository with no commits yet, in Git or jj alike), matching how every other "absent" field in the same struct behaves.

## [0.3.2] - 2026-07-24

### Added

- **Task dependency visibility**: `carryctx task show` now returns `depends_on` (this task's prerequisites, each annotated with its current status) and `blocks` (tasks that depend on this one), alongside the existing task fields. Previously there was no way to see a task's dependency graph without manually replaying `task depend`/`undepend` history.

### Fixed

- **MCP server stale binary path**: `carryctx mcp` is a long-lived stdio process. Upgrading `carryctx` while it's running (cargo/npm/Homebrew/etc.) replaces the binary at the same path; the already-running server keeps its old file handle open and stays functional, but the next tool call that tries to spawn a subprocess via the process's own executable path failed with `No such file or directory`, since that path no longer resolves on disk. The server now detects this and falls back to resolving `carryctx` from `PATH` (finding whatever is actually installed), and gives an actionable error message if that also fails, instead of a bare OS error.
- **`graph edges <ID>` silently returned an empty list** when given a task, agent, or session ULID instead of an actual Context Graph node ID — those are separate ID spaces. It now checks whether the node exists first and returns a clear error pointing at `task show` for task dependencies instead.

## [0.3.1] - 2026-07-24

### Fixed

- **Progress task inference**: `progress todo`/`block`/`risk`/`note`/`list` no longer require an explicit `--task`. They now resolve the current task the same way `session start`, `checkpoint`, and `context` already do (`--task` → `CARRYCTX_TASK` → active session → current worktree → agent's single in-progress task).
- **`checkpoint list --task <DISPLAY_ID>`**: fixed a bug where passing a display ID (e.g. `CTX-0001`) silently returned an empty list. The filter now resolves the display ID to its underlying ULID before querying, and also falls back to `CARRYCTX_TASK` when `--task` is omitted.
- **Dependency auto-promotion**: completing a task now re-evaluates its dependents. Any task still sitting in `planned` whose last incomplete strong dependency was just completed is automatically promoted to `ready` (mirroring the existing behavior on `task undepend`), emitting a new `task.unblocked` event. Previously a task created as `planned` had no path back to `ready` once its blocking dependency actually finished.
- **`resume` fallback**: `carryctx resume` now falls back through the same task-resolution chain as the commands above instead of only checking `--task` or the current active session. Reopening a new window with no active session (the core "resume" scenario) now correctly finds the agent's single in-progress task instead of returning `currentTask: null`.
- **Stale README example**: `progress complete PX-0001 "<text>"` in `README.zh-CN.md` is not valid; `progress complete` takes a single positional argument. Corrected to `progress complete PX-0001`.
- **Homepage URL**: `carryctx.dev` is not registered yet. `Cargo.toml`'s `homepage` field and both READMEs' documentation links now point at `carryctx.xuepoo.xyz`, the site actually in production.

## [0.3.0] - 2026-07-24

### Added

- **Intelligent Context Inference**: Implemented `CurrentEntityResolver::resolve_task` to auto-infer tasks based on current git worktree bindings or single active agent tasks. Removes the strict requirement for explicit `--task` flags in `session start`, `checkpoint`, `context`, and `handoff` commands.
- **Detailed JSON Status**: `carryctx status` in JSON format now outputs fully detailed arrays for `tasks`, `activeSessions`, `activeAgents`, and `worktrees` instead of just integer counts, greatly improving parsability for LLMs and external tools.

### Fixed

- **Task Timestamps**: Fixed an issue in `SqliteTaskRepository` where `started_at` and `completed_at` timestamps were not being correctly populated in the SQLite database during `in_progress` or `completed` state transitions.
- **Active Session Filtering**: Fixed a bug in `carryctx status` where the JSON output incorrectly counted _all_ historical sessions as active. It now correctly filters by `SessionState::Active`.
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
