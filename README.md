# CarryCtx

A local-first project state and continuity manager for coding agents.

CarryCtx is a CLI that preserves and restores project context across coding-agent sessions, windows, and Git worktrees. It provides structured task management, progress tracking, checkpoint-based state capture, and session lifecycle — all backed by a shared SQLite database.

## Installation

### Cargo (recommended)

```bash
cargo install carryctx
```

### npm

```bash
npm install -g carryctx
```

### GitHub Releases

Download the prebuilt binary for your platform from the [releases page](https://github.com/Xuepoo/carryctx/releases).

### Homebrew (future)

```bash
brew install carryctx/tap/carryctx
```

### AUR (future)

```bash
yay -S carryctx
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

## Agent Skill Setup

Load the CarryCtx skill to give your coding agent first-class CarryCtx awareness:

```bash
# Clone the skills repository
git clone https://github.com/anthropics/skills

# Load the CarryCtx skill in your agent
# See skills/carryctx/SKILL.md for usage
```

The skill teaches agents to manage sessions, tasks, progress, and checkpoints through CarryCtx — enabling persistent context across agent restarts and worktree switches.

## Documentation

- Requirements, CLI specification, and configuration: [carryctx-docs](https://github.com/Xuepoo/carryctx-docs)
- Agent skill: [carryctx-skills](https://github.com/Xuepoo/carryctx-skills)

## License

MIT
