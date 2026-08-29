# CarryCtx

**管理 Agent 团队的意图、分工与历史——而不是他们的聊天记录。**

> The intent & management layer for AI coding agents.

聊天窗口一关，记录就蒸发——但那从来就不是项目状态。真正难以持久回答的问题是：这个 Agent 团队*打算做什么*、_现在是谁在做_、_实际发生了什么_。CarryCtx 把这三个答案存进一个本地 SQLite 数据库，并通过 CLI 或 MCP，在任何会话里把恰好需要的那一部分交给任何 Agent。

CarryCtx 是 **AI 编码 Agent 的意图与管理层**：

- **做什么（WHAT）** —— 结构化、带依赖的任务与优先级、工作流。生命周期（`planned → ready → in_progress → completed`）由存储层本身强制执行：强依赖同时门控开始与完成，工作不会"出生即完成"，也不会带着未解决的阻塞收尾。
- **谁来做（WHO）** —— 指挥官与角色特化的子 Agent 组成的持久团队；Agent 之间的交接走强制的状态机（`Open → Accepted/Rejected → Closed`）。并发认领由 compare-and-set 更新裁决：永远只有一个赢家。
- **发生了什么（WHAT HAPPENED）** —— 只追加、按 keyset 分页的事件流、Git 感知的 Checkpoint、被记录的决策，以及跨任务、进度、Checkpoint 与决策的全文检索。每一次变更都可审计，历史不会被悄悄改写。

CarryCtx 记录并分发状态；它不运行你的 Agent。没有调度器、没有 worker 运行时、没有 prompt 缓存、也没有云端。拉起进程的是你的 harness——CarryCtx 保证它们每一个到场时都确切知道自己该知道的事。

[English](README.md) | 简体中文

## 核心支柱

| 支柱                 | 你得到的东西                                                                                          |
| -------------------- | ----------------------------------------------------------------------------------------------------- |
| 持久团队             | 比任何会话都活得久的指挥官 + 子 Agent 名册；只读 status/context 投影，可按成员或按任务切片            |
| 受门控的任务生命周期 | 依赖、阻塞与状态转换由数据库而非约定来强制                                                            |
| 交接与并发安全       | 状态机交接与 CAS 守卫的认领，并行 Agent 永不重复领取                                                  |
| 可审计的历史         | 不可变事件日志、Git 感知 Checkpoint、决策记录、FTS5 全文检索                                          |
| Worktree 隔离        | 每个任务一个 Git worktree，自动创建并绑定                                                             |
| 代码依赖图谱         | 基于 AST 扫描的代码库依赖图谱，可导出 Mermaid/DOT/ASCII/JSON                                          |
| 离线优先存储         | 单个 SQLite 文件位于 `<git-common-dir>/carryctx/state.sqlite`——linked worktree 共享，二进制内无网络栈 |
| 技能与预设注入       | 向团队使用的任意工具注入工作流、规则与 Agent Skill                                                    |
| Agent 效能分析       | 会话时长、产出统计，可导出 Markdown/CSV                                                               |

## 🚀 安装指南

### Cargo (推荐)

```bash
cargo install carryctx
```

### npm

```bash
npm install -g carryctx
# 或
bun add -g carryctx
```

### Homebrew (macOS / Linux)

```bash
brew tap Xuepoo/tap https://github.com/Xuepoo/homebrew-tap.git
brew install carryctx
```

### Scoop (Windows)

```bash
scoop bucket add Xuepoo https://github.com/Xuepoo/scoop-bucket.git
scoop install carryctx
```

### AUR (Arch Linux)

```bash
yay/paru -S carryctx
yay/paru -S carryctx-bin
```

### GitHub Releases

直接从 [Releases 页面](https://github.com/Xuepoo/carryctx/releases) 下载适配您平台的预编译二进制文件。

## ⚡ 快速开始

```bash
cd your-project
carryctx init                                          # 创建 .carryctx/ 与共享的 Git 状态
carryctx agent register --name my-agent --provider claude-code
carryctx task create --title "实现 CSV 导出器"          # CTX-0001
carryctx task depend CTX-0002 --on CTX-0001            # 用前置依赖门控工作
carryctx task claim CTX-0001 --agent my-agent
carryctx resume --agent my-agent                       # 从当前进展处精确接续
```

每条命令都支持 `--format json` 的稳定信封结构，脚本和 Agent 与人使用同一个命令面。

## 👥 团队：一名指挥官与其子 Agent

一个仓库只有一个 Agent，从来都不是真实的工作形态。指挥官负责规划；子 Agent 负责实现、评审、写文档。CarryCtx 的 Team 就是这套结构的持久化记录——它和任务存在同一个 `state.sqlite` 里，因此关掉会话、开新窗口、切换 linked worktree 之后都无需重新自我介绍：

```bash
carryctx agent register --name commander-1 --provider claude-code --kind commander
carryctx agent register --name dev-1 --provider codex --kind subagent --role implementer
carryctx team create --name core --commander commander-1
carryctx team member add core --agent dev-1 --role implementer
carryctx task team set CTX-0042 --team core
```

然后直接查询团队此刻在做什么——一个由持久化记录重建出来的只读投影：

```bash
carryctx team status core            # 名册、活跃会话、未完成任务、聚合计数
carryctx team context core           # 指挥官视图：完整协调画面
carryctx team context core --agent-for dev-1   # 只给该成员它需要的部分
carryctx team context core --task CTX-0042     # 只给该任务它需要的部分
```

`team status` 与 `team context` 以只读方式打开数据库，不写入任何内容。指挥官拿到完整图谱；`--agent-for` 与 `--task` 会一致地收窄所有集合，子 Agent 拿到的是属于自己的切片，而不是整个项目。当工作在 Agent 之间流转时，一次 handoff 会带着它走过受强制的生命周期，而不是一声碰运气的呼喊：

```bash
carryctx handoff create --task CTX-0042 --target dev-1 --summary "可以评审了" --agent commander-1
carryctx handoff accept HO-0007      # Open → Accepted，原子化写入审计
```

**CarryCtx 记录团队，但不运行团队。** 拉起进程、分派工作、重试与模型选择，都属于你的 harness。

## MCP：接入任何 MCP 客户端

CarryCtx 通过 stdio 提供六个 MCP 工具——`carryctx_task_manager`、`carryctx_progress_tracker`、`carryctx_context_manager`、`carryctx_decision_logger`、`carryctx_graph_explorer` 与 `carryctx_project_admin`——把同一份持久状态开放给 Cursor、Claude Desktop 或任何其他 MCP 客户端：

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

## 📦 功能一览

| 命令                              | 提供什么                                                                                                                   |
| --------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `task`、`progress`                | 结构化的工作单元，带依赖门控、阻塞与微进度日志——不是一段自然语言的待办清单                                                 |
| `checkpoint`、`resume`、`context` | 带 Git 感知的状态快照，以及可直接喂给 LLM 的上下文导出                                                                     |
| `session`、`agent`、`handoff`     | 多 Agent、多窗口协作，所有权交接由状态机强制                                                                               |
| `team`                            | 持久化的 Agent 团队——成员名册、指挥官、任务归属，以及只读 status/context 投影                                              |
| `worktree`                        | 按任务隔离的并行工作区，自动绑定到正确的分支                                                                               |
| `graph`                           | 基于 AST 扫描的代码依赖图谱，可导出 Mermaid/DOT/ASCII/JSON                                                                 |
| `search`                          | 基于 SQLite FTS5 跨任务、进度、Checkpoint 与决策全文检索，并返回所属任务、分支和高亮片段                                   |
| `mcp`                             | 六个 [Model Context Protocol](https://modelcontextprotocol.io) 工具经 stdio 提供——直接接入 Cursor、Claude Desktop 等客户端 |
| `event`                           | 按 keyset 分页的审计轨迹，记录每一次状态变更                                                                               |
| `stats`                           | Agent 效能分析——会话时长、产出统计，可导出 Markdown/CSV                                                                    |
| `hooks`、`preset`、`skill`        | Commit 时自动创建快照，外加面向你的 Agent 的工作流/规则/Skill 注入                                                         |
| `doctor`                          | 自诊断孤立任务、缺失 Hook 与数据库漂移——schema 迁移替你自动处理                                                            |

## 全文搜索

无需记住内容属于哪个任务或分支，直接按文本查找历史工作：

```bash
carryctx search "markdown worker protocol"
carryctx search aria-owns --type decision --json
carryctx search "auth flow" --status in_progress --assignee my-agent
```

结果按相关度排序，每条命中都会解析回所属任务、状态和当前已知的最佳分支。Query 支持精确短语、大写 `AND`/`OR`/`NOT`，以及末尾 `*` 前缀匹配；`aria-owns`、`pointer-events`、`--deny-warnings` 等带连字符的裸词会按普通文本处理。

## 💡 Agent Skill 配置

CarryCtx 现在只提供一份入口技能 **use-carryctx**，来自 [carryctx-skills](https://github.com/Xuepoo/carryctx-skills) 仓库，通过官方 [Vercel Labs skills CLI](https://github.com/vercel-labs/skills) 安装。一次安装即可覆盖完整能力面：指挥官准则（commander doctrine），外加任务、团队、Session 与 Checkpoint、Handoff、预设/规则/人格以及故障排查的专题参考。

列出仓库中所有可用 Skill：

```bash
npx skills add Xuepoo/carryctx-skills --list
```

为所有检测到的 Agent 安装 use-carryctx：

```bash
npx skills add Xuepoo/carryctx-skills --all
```

只为指定 Agent 安装这一份技能：

```bash
npx skills add Xuepoo/carryctx-skills \
  --skill use-carryctx \
  --agent codex \
  --agent claude-code \
  --agent cursor \
  --agent github-copilot
```

不安装、直接使用：

```bash
npx skills use Xuepoo/carryctx-skills --skill use-carryctx
```

每个主会话加载**一次**即可，在任何多步骤工程任务开始时生效。该技能会把你的主会话 Agent 变成**指挥官**：把工作拆解为持久的 CarryCtx 任务，把实现派发给角色化的子 Agent（最好各自隔离在独立的 Git Worktree 中），再通过 `team status`、`team context`、`task show` 读回状态验收结果，而不是轻信自述。

## 🤔 为什么不直接写 Markdown 交接文档？

|                   | 手写交接文档     | 聊天记录                 | CarryCtx                            |
| ----------------- | ---------------- | ------------------------ | ----------------------------------- |
| 关窗口后还能留存  | 只有你记得写才行 | 否                       | 是                                  |
| 可被程序查询      | 否——自由文本     | 否                       | 是——SQL + `--json`                  |
| 强制工作流规则    | 否               | 否                       | 是——生命周期门控、CAS 认领          |
| 自动采集 Git 状态 | 否               | 否                       | 是（分支、HEAD、脏文件、Diff 统计） |
| 跨不同 Agent 通用 | 靠约定           | 否——绑定单一工具的上下文 | 是——Agent 无关                      |
| 能检测状态过期    | 否               | 否                       | 是（`carryctx doctor`）             |
| 会离开你的机器    | 否               | 视 Provider 而定         | 从不——100% 本地                     |

Git 负责代码历史；CarryCtx 负责意图——代码为什么会是现在这个样子、接下来归谁、还剩什么没做。

## 📚 详细文档

- 完整文档与指南：[carryctx.xuepoo.xyz](https://carryctx.xuepoo.xyz)
- Agent Skill 源码与规范：[carryctx-skills](https://github.com/Xuepoo/carryctx-skills)

## 🧭 设计原则

- **管理层，不是聊天记忆。** CarryCtx 把意图、所有权与历史保存为可查询的状态——那正是聊天窗口从来做不到的事。
- **本地优先。** 完全不联网——二进制内不含任何网络栈；不需要账号、不上报任何遥测数据、没有锁定。所有状态存储在 `<git-common-dir>/carryctx/state.sqlite` 中，并由 linked worktree 共享。
- **Agent 无关。** Claude Code、OpenCode、Copilot、Codex，或是人类开发者——通过 CLI 或 MCP，共享同一份结构化状态。
- **由存储强制，而非靠约定。** 生命周期、依赖、交接与认领都在 SQLite 事务中守卫——并发下永远只有一个赢家。
- **它是管理工具，不是编排框架。** CarryCtx 负责持久化团队、任务与上下文，运行 Agent 的是你的 harness。它始终是一个工具，而不是一套你必须整体采纳的框架。

---

## 📄 开源协议

[MIT License](LICENSE)
