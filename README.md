# CarryCtx

A local-first project state and continuity manager for coding agents.

CarryCtx is a CLI that preserves and restores project context across coding-agent sessions, windows, and Git worktrees. It provides structured task management, progress tracking, checkpoint-based state capture, and session lifecycle — all backed by a shared SQLite database.

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

- Agent skill: [carryctx-skills](https://github.com/Xuepoo/carryctx-skills)

## License

MIT
