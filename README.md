# CarryCtx

**Your coding agent forgets everything the moment its window closes. CarryCtx doesn't.**

You close Claude Code. Tomorrow's session — or a teammate's, or a different agent entirely — has no idea what you were doing, what's done, what's blocked, or what branch you were on. Chat history isn't project state. Commit messages don't explain intent. Markdown notes go stale the moment you stop updating them by hand.

CarryCtx is a local-first CLI that gives coding agents a real memory: structured tasks, progress, decisions, and Git-aware checkpoints, all persisted in a single SQLite file inside your repo. Any agent, in any window, on any worktree, runs one command and picks up exactly where the last one left off.

```bash
carryctx resume
```

```text
Task CTX-0014 — Add streaming CSV export
Owner: claude-core · Status: in_progress

Last checkpoint (12m ago):
  Done:      Implemented CSV writer, added unit tests
  Remaining: Add streaming support for >1M rows
  Blocker:   None

Git: branch feature/csv-export, HEAD 32ac891, 2 files dirty
Next: Wire the writer into the streaming pipeline
```

No re-reading chat logs. No "catch me up" prompts. No stale hand-off doc.

English | [简体中文](README.zh-CN.md)

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
carryctx init
carryctx agent register --name my-agent --provider claude-code
carryctx task create --title "My first task"
carryctx task claim CTX-0001
carryctx session start
carryctx resume
```

## Why not just Markdown notes or a `HANDOFF.md`?

| | Markdown hand-off doc | Chat history | CarryCtx |
| --- | --- | --- | --- |
| Survives a closed window | Only if someone remembers to write it | No | Yes |
| Machine-queryable | No — free text | No | Yes — SQL + `--json` |
| Tracks Git state automatically | No | No | Yes (branch, HEAD, dirty files, diff stats) |
| Works across different agents | Depends on convention | No — tied to one tool's context | Yes — agent-agnostic |
| Detects stale state | No | No | Yes (`carryctx doctor`) |
| Leaves your machine | No | Depends on provider | Never — 100% local |

CarryCtx doesn't replace Git and it doesn't run your agent. It's the layer in between: Git owns code history, CarryCtx owns *why* the code is the way it is right now.

## What's inside

| Command | What it gives you |
| --- | --- |
| `task`, `progress`, `depend` | Structured work units with dependencies, blockers, and micro-progress logs — not a prose to-do list |
| `checkpoint`, `resume`, `context` | Git-aware state snapshots and LLM-ready context dumps |
| `session`, `agent`, `handoff` | Multi-agent, multi-window collaboration with explicit ownership hand-off |
| `worktree` | Isolated parallel work per task, auto-bound to the right branch |
| `graph` | AST-scanned code dependency graph, exportable as Mermaid/DOT/ASCII |
| `mcp` | A stdio [Model Context Protocol](https://modelcontextprotocol.io) server — plug straight into Cursor, Claude Desktop, and other MCP clients |
| `stats` | Agent performance analytics — session length, throughput, exportable as Markdown/CSV |
| `hooks` | Git `post-commit` auto-checkpointing, task-ID-prefixed commit messages |
| `doctor` | Self-diagnosis for orphaned tasks, missing hooks, and DB drift |
| `sync` | Push/pull state across machines when you need it — network access stays opt-in, never default |

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

Load the CarryCtx skill to give your coding agent first-class CarryCtx awareness:

```bash
npx skills add https://github.com/Xuepoo/carryctx-skills --skill carryctx
```

The skill teaches agents to manage sessions, tasks, progress, and checkpoints through CarryCtx — enabling persistent context across agent restarts and worktree switches.

## Documentation

- Full docs & guides: [carryctx.dev](https://carryctx.dev)
- Agent skill source: [carryctx-skills](https://github.com/Xuepoo/carryctx-skills)

## Principles

- **Local-first.** No network access by default, no account, no telemetry. State lives in `.git/carryctx/state.sqlite`.
- **Agent-agnostic.** Claude Code, OpenCode, Copilot, Codex, or a human — everyone reads and writes the same structured state.
- **Git is the source of truth for code; CarryCtx is the source of truth for intent.** It never rewrites history or resolves merge conflicts for you.

## License

MIT
