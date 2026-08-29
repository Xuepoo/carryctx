# CarryCtx

**Manage what your agent team intends, who does it, and what happened — not their chat logs.**

> AI 编码 Agent 的意图与管理层。

A chat transcript dies with its window, and it was never project state to begin with. The durable questions are: _what_ is this agent team trying to do, _who_ is doing it right now, and _what actually happened_. CarryCtx answers all three from a single local SQLite database and serves exactly the right slice of it to any agent, in any session, through CLI or MCP.

CarryCtx is the **intent & management layer for AI coding agents**:

- **WHAT** — structured, dependency-aware tasks with priorities and workflows. The lifecycle (`planned → ready → in_progress → completed`) is enforced by the store itself: strong dependencies gate both start and completion, so work can't be born done or closed over an open blocker.
- **WHO** — durable teams of commanders and role-specialized subagents, plus explicit handoffs between agents through an enforced state machine (`Open → Accepted/Rejected → Closed`). Concurrent claims are settled with compare-and-set updates: exactly one winner, ever.
- **WHAT HAPPENED** — an append-only, keyset-paginated event stream, Git-aware checkpoints, recorded decisions, and full-text search across tasks, progress, checkpoints, and decisions. Every mutation is auditable; history never silently rewrites itself.

CarryCtx records and serves state; it does not run your agents. There is no scheduler, no worker runtime, no prompt cache, and no cloud. Your harness spawns the processes — CarryCtx makes sure every one of them arrives knowing exactly what it needs to know.

[English](README.md) | [简体中文](README.zh-CN.md)

## Pillars

| Pillar                      | What you get                                                                                                                |
| --------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| Durable teams               | Commander + subagent roster that outlives every session; read-only status/context projections, sliced per agent or per task |
| Gated task lifecycle        | Dependencies, blockers, and transitions enforced by the database, not by convention                                         |
| Handoffs & safe concurrency | State-machine handoffs and CAS-guarded claims so parallel agents never double-take                                          |
| Auditable history           | Immutable event log, Git-aware checkpoints, decisions, FTS5 full-text search                                                |
| Worktree isolation          | One Git worktree per task, created and bound automatically                                                                  |
| Code-dependency graph       | AST-scanned graph of your codebase, exportable as Mermaid/DOT/ASCII/JSON                                                    |
| Offline-first storage       | One SQLite file at `<git-common-dir>/carryctx/state.sqlite` — shared by linked worktrees, no network stack in the binary    |
| Skills & presets            | Inject workflows, rules, and agent skills into whatever tool your team runs                                                 |
| Agent analytics             | Session length, throughput, and exportable Markdown/CSV reports                                                             |

## Installation

### Cargo (recommended)

```bash
cargo install carryctx
```

### npm

```bash
npm install -g carryctx
# or
bun add -g carryctx
```

### GitHub Releases

Download the prebuilt binary for your platform from the [releases page](https://github.com/Xuepoo/carryctx/releases).

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

```bash
yay/paru -S carryctx
yay/paru -S carryctx-bin
```

## Quick start

```bash
cd your-project
carryctx init                                          # creates .carryctx/ + shared Git state
carryctx agent register --name my-agent --provider claude-code
carryctx task create --title "Ship the CSV exporter"   # CTX-0001
carryctx task depend CTX-0002 --on CTX-0001            # gate work on prerequisites
carryctx task claim CTX-0001 --agent my-agent
carryctx resume --agent my-agent                       # pick up exactly where things stand
```

Every command speaks `--format json` with a stable envelope, so scripts and agents consume the same surface humans do.

## Teams: a commander and its subagents

One agent per repo was never the real shape of the work. A commander plans; subagents implement, review, and write docs. A CarryCtx team is a durable record of that structure — it lives in the same `state.sqlite` as your tasks, so it survives closed sessions, new windows, and linked worktrees without a re-introduction:

```bash
carryctx agent register --name commander-1 --provider claude-code --kind commander
carryctx agent register --name dev-1 --provider codex --kind subagent --role implementer
carryctx team create --name core --commander commander-1
carryctx team member add core --agent dev-1 --role implementer
carryctx task team set CTX-0042 --team core
```

Then ask what the team is actually doing — a read-only projection, rebuilt from durable records:

```bash
carryctx team status core            # roster, active sessions, open tasks, counts
carryctx team context core           # commander view: full coordination picture
carryctx team context core --agent-for dev-1   # just what that member needs
carryctx team context core --task CTX-0042     # just what that task needs
```

`team status` and `team context` open the database read-only and write nothing. A commander gets the whole graph; `--agent-for` and `--task` narrow every collection consistently, so a subagent receives its slice instead of the entire project. When work moves between agents, a handoff carries it through an enforced lifecycle instead of a hopeful ping:

```bash
carryctx handoff create --task CTX-0042 --target dev-1 --summary "Ready for review" --agent commander-1
carryctx handoff accept HO-0007     # Open → Accepted, atomically audited
```

**CarryCtx records the team; it does not run it.** Spawning processes, routing work, retries, and model selection stay with your harness.

## MCP: plug into any MCP client

CarryCtx ships six MCP tools over stdio — `carryctx_task_manager`, `carryctx_progress_tracker`, `carryctx_context_manager`, `carryctx_decision_logger`, `carryctx_graph_explorer`, and `carryctx_project_admin` — exposing the same durable state to Cursor, Claude Desktop, or any other MCP client:

```json
{
  "mcpServers": {
    "carryctx": {
      "command": "carryctx",
      "args": ["mcp"]
    }
  }
}
```

## What's inside

| Command                           | What it gives you                                                                                                                             |
| --------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `task`, `progress`                | Structured work units with dependency gating, blockers, and micro-progress logs — not a prose to-do list                                      |
| `checkpoint`, `resume`, `context` | Git-aware state snapshots and LLM-ready context dumps                                                                                         |
| `session`, `agent`, `handoff`     | Multi-agent, multi-window collaboration with state-machine ownership hand-off                                                                 |
| `team`                            | Persistent agent teams — roster, commander, task assignment, and read-only status/context projections                                         |
| `worktree`                        | Isolated parallel work per task, auto-bound to the right branch                                                                               |
| `graph`                           | AST-scanned code dependency graph, exportable as Mermaid/DOT/ASCII/JSON                                                                       |
| `search`                          | SQLite FTS5 search across tasks, progress, checkpoints, and decisions, with owning task, branch, and highlighted snippets                     |
| `mcp`                             | Six [Model Context Protocol](https://modelcontextprotocol.io) tools over stdio — plug straight into Cursor, Claude Desktop, and other clients |
| `event`                           | Keyset-paginated audit trail of every state change                                                                                            |
| `stats`                           | Agent performance analytics — session length, throughput, exportable as Markdown/CSV                                                          |
| `hooks`, `preset`, `skill`        | Auto-checkpointing on commit, plus workflow/rule/skill injection for your agents                                                              |
| `doctor`                          | Self-diagnosis for orphaned tasks, missing hooks, and DB drift — with schema migrations handled for you                                       |

## Full-Text Search

Find prior work by content without remembering which task or branch contained it:

```bash
carryctx search "markdown worker protocol"
carryctx search aria-owns --type decision --json
carryctx search "auth flow" --status in_progress --assignee my-agent
```

Results are ranked by relevance and resolve every hit back to its owning task, status, and best-known branch. Queries support exact phrases, uppercase `AND`/`OR`/`NOT`, and trailing `*` prefix matches. Bare hyphenated terms such as `aria-owns`, `pointer-events`, and `--deny-warnings` are treated as literal text.

## Shell Completions

Enable tab-completion for all commands and flags:

```bash
# Bash
carryctx completions bash >> ~/.bash_completion.d/carryctx

# Zsh (add to ~/.zshrc)
eval "$(carryctx completions zsh)"

# Fish
carryctx completions fish > ~/.config/fish/completions/carryctx.fish

# PowerShell
carryctx completions powershell | Out-String | Invoke-Expression
```

## Git Hooks

Install CarryCtx git hooks to auto-checkpoint on commit and prefix commit messages with the active task ID:

```bash
carryctx hooks install       # install post-commit + prepare-commit-msg hooks
carryctx hooks status        # check which hooks are active
carryctx hooks uninstall     # remove CarryCtx hooks (restores .bak if present)
```

## Diagnostics

```bash
carryctx doctor              # check project health (git, db, hooks, orphaned tasks)
carryctx doctor --json       # machine-readable output
```

## Agent Skill Setup

Give your coding agent first-class CarryCtx awareness with **use-carryctx** — the single entry-point skill shipped from [carryctx-skills](https://github.com/Xuepoo/carryctx-skills) via the [Vercel Labs Skills CLI](https://github.com/vercel-labs/skills). One install covers the full surface: commander doctrine plus focused references for tasks, teams, sessions/checkpoints, handoffs, presets/rules/personas, and troubleshooting.

List available skills:

```bash
npx skills add Xuepoo/carryctx-skills --list
```

Install use-carryctx for all detected agents:

```bash
npx skills add Xuepoo/carryctx-skills --all
```

Or install that one skill for specific agents only:

```bash
npx skills add Xuepoo/carryctx-skills \
  --skill use-carryctx \
  --agent codex \
  --agent claude-code \
  --agent cursor \
  --agent github-copilot
```

Use the skill without installing it:

```bash
npx skills use Xuepoo/carryctx-skills --skill use-carryctx
```

Load it **once per main session**, at the start of any multi-step engineering effort. The skill casts your main-session agent as the **commander**: plan the work as durable CarryCtx tasks, dispatch implementation to role-specialized subagents (each preferably isolated in its own Git worktree), then accept results by reading state back through `team status`, `team context`, and `task show` instead of trusting self-reports.

## Why not just Markdown notes or a `HANDOFF.md`?

|                                | Markdown hand-off doc                 | Chat history                    | CarryCtx                                    |
| ------------------------------ | ------------------------------------- | ------------------------------- | ------------------------------------------- |
| Survives a closed window       | Only if someone remembers to write it | No                              | Yes                                         |
| Machine-queryable              | No — free text                        | No                              | Yes — SQL + `--json`                        |
| Enforces workflow rules        | No                                    | No                              | Yes — lifecycle gates, CAS claims           |
| Tracks Git state automatically | No                                    | No                              | Yes (branch, HEAD, dirty files, diff stats) |
| Works across different agents  | Depends on convention                 | No — tied to one tool's context | Yes — agent-agnostic                        |
| Detects stale state            | No                                    | No                              | Yes (`carryctx doctor`)                     |
| Leaves your machine            | No                                    | Depends on provider             | Never — 100% local                          |

Git owns code history; CarryCtx owns intent — _why_ the code is the way it is, who owns it next, and what remains.

## Documentation

- Full docs & guides: [carryctx.xuepoo.xyz](https://carryctx.xuepoo.xyz)
- Agent skill source: [carryctx-skills](https://github.com/Xuepoo/carryctx-skills)

## Principles

- **Management layer, not chat memory.** CarryCtx holds intent, ownership, and history as queryable state — the thing chat windows were never going to be.
- **Local-first.** No network access at all — the binary ships no network stack. No account, no telemetry, no lock-in. State lives in `<git-common-dir>/carryctx/state.sqlite` and is shared by linked worktrees.
- **Agent-agnostic.** Claude Code, OpenCode, Copilot, Codex, or a human — everyone reads and writes the same structured state, over CLI or MCP.
- **Enforced by the store, not by convention.** Lifecycles, dependencies, handoffs, and claims are guarded in SQLite transactions — exactly-one-winner under concurrency.
- **Management, not orchestration.** CarryCtx persists teams, tasks, and context. Your harness runs the agents. It stays a tool, not a framework you have to adopt.

## License

MIT
