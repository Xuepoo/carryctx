# CarryCtx

**The local-first project lifecycle control layer for coding agents and human collaborators.**

CarryCtx keeps a project coherent from initialization to release. It turns the project contract, plan, ownership, execution state, decisions, handoffs, Git evidence, and audit history into durable, queryable local state. Agents and people can change tools, sessions, windows, and worktrees without losing the project picture.

CarryCtx is a persistence and control layer, not an agent runtime. Your external harness still launches agent processes, schedules work, routes prompts, retries failures, and selects models. CarryCtx does not ship Completion Gates or a generic Automation Engine. It records and enforces the project state that those external systems and human collaborators rely on.

[English](README.md) | [简体中文](README.zh-CN.md)

<p align="center">
  <img src="https://cdn.xuepoo.xyz/carryctx/carryctx-promo.gif" alt="CarryCtx CLI walkthrough: init, agent register, task create, claim, session start, progress note, checkpoint, resume" width="100%">
</p>

## The Project Lifecycle

CarryCtx follows the chain a real project needs:

1. **Initialize and define the contract.** `carryctx init` establishes project identity, task prefixes, branch defaults, configuration, agent guidance, and the shared state database.
2. **Plan and express dependencies.** Tasks, priorities, scopes, blockers, and dependency edges make planned work and the ready queue explicit. Strong dependencies gate starting and completing work.
3. **Assign teams and roles.** Durable commanders, subagents, human collaborators, teams, roles, task ownership, and scopes make responsibility visible without coupling it to a chat provider.
4. **Execute in worktrees and sessions.** Bind tasks to isolated Git worktrees, register agents, start sessions, and use CLI from any harness. The harness owns process scheduling and model selection.
5. **Track progress and checkpoints.** Record notes, todos, blockers, Git-aware checkpoints, context, and decisions as work changes. Resume can reconstruct the next useful slice for a person or agent.
6. **Handoff and review.** Transfer ownership through an audited handoff state machine, preserve review context, and use task transitions and checkpoint corrections when the record needs an authorized terminal-state correction.
7. **Clean up and reconcile.** Complete or cancel work, inspect stale registrations, apply cleanup policies, and run durable cleanup requests. Dirty worktrees, active sessions, current directories, locks, missing metadata, and jj-colocated layouts fail closed instead of being removed unexpectedly.
8. **Audit and analyze.** The append-only event log, full-text search, checkpoints, decisions, session history, and `stats` reports explain what happened and how the team worked.
9. **Produce release evidence.** Backups, migrations, project status, Git snapshots, audit records, analytics, and verification output provide evidence for release decisions. CarryCtx records evidence; it does not declare a release complete for you.

## Boundaries

- **Local-first and offline.** CarryCtx uses SQLite and local Git/filesystem integration. The authoritative project state is `<git-common-dir>/carryctx/state.sqlite`, shared by linked worktrees. `.carryctx/` contains project configuration and versioned guidance; it is not a universal state location.
- **Workspace 4+1.** The CLI is split into `crates/{core,sqlite,vcs,pack,cli}` plus a thin root `src/` facade (`adapter`/`application`/`domain`/`repository`/`error`/`output` re-export `crates/carryctx-cli`); only `main.rs` + `commands/` remain in `src/` until the follow-on migration moves them.
- **No unshipped promises.** Completion Gates and a generic Automation Engine are not part of v0.9. CarryCtx has no cloud service, telemetry, prompt cache, or required hosted account.
- **Agent-agnostic.** Claude Code, OpenCode, Copilot, Codex, another CLI harness, or a human can use the same CLI surface.

## Installation

### Cargo (recommended)

```bash
cargo install carryctx
```

### npm (optional wrapper/distribution channel)

```bash
npm install -g carryctx
# or
bun add -g carryctx
```

npm is an optional thin wrapper and platform-binary distribution channel. The native binary remains the primary CarryCtx artifact.

### GitHub Releases

Download a prebuilt binary from the [releases page](https://github.com/Xuepoo/carryctx/releases).

### Homebrew

```bash
brew tap Xuepoo/tap https://github.com/Xuepoo/homebrew-tap.git
brew install carryctx
```

### Scoop (Windows)

```powershell
scoop bucket add Xuepoo https://github.com/Xuepoo/scoop-bucket.git
scoop install carryctx
```

### AUR (Arch Linux)

AUR publication is currently disabled because of an upstream AUR outage. Use Cargo or the [GitHub Releases](https://github.com/Xuepoo/carryctx/releases) binaries until publication resumes. The `carryctx` and `carryctx-bin` AUR packages are unavailable until then.

## Quick Start

```bash
cd your-project
carryctx init --name billing --task-prefix BILL
carryctx agent register --name commander --provider claude-code --kind commander
carryctx task create --title "Ship the CSV exporter"       # BILL-0001
carryctx task create --title "Document the CSV exporter"   # BILL-0002
carryctx task depend BILL-0002 --on BILL-0001              # plan the dependency
carryctx task claim BILL-0001 --agent commander
carryctx session start --agent commander
carryctx progress note --task BILL-0001 "Implementation started"
carryctx checkpoint --agent commander --done "Implementation started"
carryctx resume --agent commander
```

Every command supports the stable `--format json` envelope for scripts and agents. Human-readable text and markdown formats remain available.

## Teams, Worktrees, and Handoffs

```bash
carryctx agent register --name dev-1 --provider codex --kind subagent --role implementer
carryctx team create --name core --commander commander
carryctx team member add core --agent dev-1 --role implementer
carryctx task team set BILL-0001 --team core
carryctx worktree create BILL-0001
carryctx handoff create --task BILL-0001 --target dev-1 --summary "Ready for implementation" --agent commander
carryctx handoff accept HO-0001 --claim-task --agent dev-1
```

`team status` and `team context` are read-only projections rebuilt from durable records. They can return the complete coordination view or a slice for one agent or task. By default (`--format text`) they print readable compact summaries, and `--format markdown` renders GFM tables; use `--json` for the stable machine-readable envelope. CarryCtx records teams and handoffs; the harness decides when and where to launch each participant.

## Agent Skill: `use-carryctx`

For AI coding agents (Claude Code, Cursor, OpenCode, Codex, Antigravity, etc.), CarryCtx provides an official standalone skill: [`use-carryctx`](https://github.com/Xuepoo/carryctx/tree/main/carryctx-skills/skills/use-carryctx). It equips your primary AI agent to act as an engineering **Commander**, orchestrating Subagents while keeping context lean and state durable.

### Installing the Skill

Install via standard agent skill managers or copy directly into your project's skill directory:

```bash
# Via skills CLI / npx
npx skills add Xuepoo/carryctx/carryctx-skills/skills/use-carryctx

# Or copy directly into your project repository
mkdir -p .agents/skills/use-carryctx
curl -sSL https://raw.githubusercontent.com/Xuepoo/carryctx/main/carryctx-skills/skills/use-carryctx/SKILL.md -o .agents/skills/use-carryctx/SKILL.md
```

### Usage Example: The Commander Workflow

Invoke the skill at the start of a multi-step engineering session:

```text
/use-carryctx Use carryctx to manage the <project-name> project. You act as Commander: decompose the goal into tasks, define dependency DAGs between them, assign tasks to respective Subagents working in isolated Git worktrees, and verify the deliverables yourself before completing tasks.
```

### The 4-Step Commander Loop

1. **Plan**: Decompose the goal into explicit tasks with prerequisite DAGs (`carryctx task create --title "..."`, `carryctx task depend <child> --on <parent>`).
2. **Assign & Isolate**: Assign tasks to teams and spin up isolated worktrees (`carryctx worktree create <TASK_REF>`) so Subagents never collide on git branches or workspace files.
3. **Dispatch**: Hand off scoped task context (`carryctx team context <team> --agent-for <sub>`) to the Subagent inside its `.worktrees/<task-id>` directory. The Subagent records runtime notes (`carryctx progress note`) and checkpoints (`carryctx checkpoint`).
4. **Accept & Verify**: Never trust Subagent self-reports. As Commander, inspect durable state (`carryctx team status`, `carryctx task show`), run tests and diffs independently, and mark completion (`carryctx task complete <TASK_REF>`), which automatically drains idle worktree cleanup.

## v0.8 Operational Safety

- **Cleanup policies and CLI:** configure safe `keep` or `when_idle` behavior and inspect, show, dry-run, or run durable `worktree cleanup` requests. `when_idle` requests are now drained automatically once their blockers clear — `task complete`/`cancel` and `session end` retry every eligible request for the project (best-effort; a failure is a warning and never fails the primary command), and `session end --no-cleanup` opts out. `delete_branch = "when_removed"` deletes the task branch after a successful removal only when it is merged (default remains `never`; roll out per repository).
- **Terminal correction:** authorized `--force` corrections to terminal task state are explicit and audited; ordinary lifecycle transitions remain guarded.
- **jj guard:** worktree creation and cleanup fail closed for unsupported live jj-colocated Git layouts; use jj-native workspace operations and bind when appropriate.

## Command Surface

| Area                         | Commands                                                         |
| ---------------------------- | ---------------------------------------------------------------- |
| Contract and state           | `init`, `project`, `config`, `doctor`                            |
| Plan and execution           | `task`, `progress`, `checkpoint`, `resume`, `context`, `session` |
| Collaboration                | `agent`, `team`, `handoff`, `decision`                           |
| Isolation and reconciliation | `worktree`, `worktree cleanup`, `hooks`                          |
| Evidence and analysis        | `event`, `search`, `stats`, `graph`                              |
| Agent integration            | `preset`, `skill`, `completions`                                 |

`sync` is only a local file-copy mechanism for explicit snapshots. `--remote` is required (no `/tmp` default). It is not cloud sync and does not add networking to the binary.

## Inspiration & Acknowledgments

CarryCtx is built with deep appreciation for and inspiration from:

- **[Git](https://github.com/git/git)** — for content-addressable storage, plumbing/porcelain separation, DAG-based merge semantics, and the unix philosophy of local durability.
- **[SQLite](https://github.com/sqlite/sqlite)** — for embedded ACID reliability, zero-configuration local persistence, and self-contained transactional state without server daemons.

## Documentation

- Full docs and guides: [carryctx.xuepoo.xyz](https://carryctx.xuepoo.xyz)
- Agent skill source: [carryctx-skills](https://github.com/Xuepoo/carryctx-skills)
- License: [MIT](LICENSE)
