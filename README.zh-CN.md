# CarryCtx

**你的 Coding Agent 一关窗口就失忆。CarryCtx 不会。**

你关掉了 Claude Code 的窗口。第二天打开一个新 Session——或者换了个同事、换了个 Agent——完全不知道昨天做到哪了、哪些完成了、被什么卡住、当前在哪个分支上。聊天记录不是项目状态，Commit Message 不会解释"为什么"，手写的交接文档写完当天就开始过期。

CarryCtx 是一个本地优先（Local-first）的命令行工具，给 Coding Agent 补上一个真正的记忆：结构化的任务、进度、决策，以及带 Git 感知的状态快照，全部存在你仓库里的一个 SQLite 文件中。任何 Agent，在任何窗口、任何 Worktree 里，执行一条命令就能精确接续上一次的进度。

```bash
carryctx resume
```

```text
任务 CTX-0014 — 实现 CSV 流式导出
负责人：claude-core · 状态：进行中

最近一次快照（12 分钟前）：
  已完成：实现了 CSV 写入器，补充了单元测试
  剩余：  为超过 100 万行的数据增加流式写入支持
  阻塞：  无

Git：分支 feature/csv-export，HEAD 32ac891，2 个文件未提交
下一步：将写入器接入流式处理管道
```

不用翻聊天记录，不用写"快帮我梳理一下之前做了什么"的 Prompt，也不再需要一份写完就过期的交接文档。

[English](README.md) | 简体中文

---

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
brew tap Xuepoo/tap
brew install carryctx
```

### Scoop (Windows)

```bash
scoop bucket add Xuepoo https://github.com/Xuepoo/scoop-bucket
scoop install carryctx
```

### AUR (Arch Linux)

```bash
yay/paru -S carryctx
yay/paru -S carryctx-bin
```

### GitHub Releases

直接从 [Releases 页面](https://github.com/Xuepoo/carryctx/releases) 下载适配您平台的预编译二进制文件。

---

## ⚡ 快速开始

```bash
# 进入项目目录并初始化 CarryCtx
cd your-project
carryctx init

# 注册并指定当前 Agent 身份
carryctx agent register --name my-agent --provider claude-code
carryctx agent list # 或 carryctx agent current

# 创建并领取任务
carryctx task create --title "实现用户登录页面"
carryctx task claim CTX-0001
carryctx task start CTX-0001

# 开启会话并恢复上下文
carryctx session start
carryctx resume
```

## 🤔 为什么不直接写 Markdown 交接文档？

|                   | 手写交接文档     | 聊天记录                 | CarryCtx                            |
| ----------------- | ---------------- | ------------------------ | ----------------------------------- |
| 关窗口后还能留存  | 只有你记得写才行 | 否                       | 是                                  |
| 可被程序查询      | 否——自由文本     | 否                       | 是——SQL + `--json`                  |
| 自动采集 Git 状态 | 否               | 否                       | 是（分支、HEAD、脏文件、Diff 统计） |
| 跨不同 Agent 通用 | 靠约定           | 否——绑定单一工具的上下文 | 是——Agent 无关                      |
| 能检测状态过期    | 否               | 否                       | 是（`carryctx doctor`）             |
| 会离开你的机器    | 否               | 视 Provider 而定         | 从不——100% 本地                     |

CarryCtx 不取代 Git，也不控制你的 Agent。它是夹在中间的一层：Git 负责代码历史，CarryCtx 负责记录"代码为什么会是现在这个样子"。

---

## 📦 功能一览

| 命令                              | 提供什么                                                                                                              |
| --------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `task`、`progress`、`depend`      | 结构化的工作单元，带依赖、阻塞与微日志——不是一段自然语言的待办清单                                                    |
| `checkpoint`、`resume`、`context` | 带 Git 感知的状态快照，以及可直接喂给 LLM 的上下文导出                                                                |
| `session`、`agent`、`handoff`     | 多 Agent、多窗口协作，带显式的所有权交接                                                                              |
| `team`                            | 持久化的 Agent 团队——成员名册、指挥官、任务归属，以及跨会话存活的只读 status/context 投影                             |
| `worktree`                        | 按任务隔离的并行工作区，自动绑定到正确的分支                                                                          |
| `graph`                           | 基于 AST 扫描的代码依赖图谱，可导出 Mermaid/DOT/ASCII                                                                 |
| `search`                          | 基于 SQLite FTS5 跨任务、进度、Checkpoint 与决策全文检索，并返回所属任务、分支和高亮片段                              |
| `mcp`                             | 一个 stdio [Model Context Protocol](https://modelcontextprotocol.io) 服务器——直接接入 Cursor、Claude Desktop 等客户端 |
| `stats`                           | Agent 效能分析——会话时长、产出统计，可导出 Markdown/CSV                                                               |
| `hooks`                           | Git `post-commit` 自动创建快照，Commit Message 自动带上任务编号前缀                                                   |
| `doctor`                          | 自诊断孤立任务、缺失 Hook 与数据库漂移                                                                                |
| `sync`                            | 在本地 `--remote` 路径之间复制状态数据库——纯文件拷贝，二进制内不含任何网络栈                                          |

## 全文搜索

无需记住内容属于哪个任务或分支，直接按文本查找历史工作：

```bash
carryctx search "markdown worker protocol"
carryctx search aria-owns --type decision --json
carryctx search "auth flow" --status in_progress --assignee claude-code
```

结果按相关度排序，每条命中都会解析回所属任务、状态和当前已知的最佳分支。Query 支持精确短语、大写 `AND`/`OR`/`NOT`，以及末尾 `*` 前缀匹配；`aria-owns`、`pointer-events`、`--deny-warnings` 等带连字符的裸词会按普通文本处理。

---

## 👥 Agent 团队

一个仓库只有一个 Agent，从来都不是真实的工作形态。指挥官负责规划，子 Agent 负责实现、评审、写文档——而它们的记忆都会在窗口关闭的那一刻消失。

CarryCtx 的 Team 是一份比它们都活得更久的名册。它和任务存在同一个 `state.sqlite` 里，因此关掉会话、开新窗口、切换 linked worktree 之后都无需重新自我介绍：

```bash
carryctx team create --name core --commander commander-1
carryctx team member add core --agent dev-1 --role implementer
carryctx team member add core --agent reviewer-1 --role reviewer
carryctx task team set CTX-0042 --team core
```

然后直接查询团队此刻在做什么——一个由持久化记录重建出来的只读投影：

```bash
carryctx team status core            # 名册、活跃会话、未完成任务、聚合计数
carryctx team context core           # 指挥官视图：完整协调画面
carryctx team context core --agent-for dev-1   # 只给该成员它需要的部分
carryctx team context core --task CTX-0042     # 只给该任务它需要的部分
```

`team status` 与 `team context` 以只读方式打开数据库，不写入任何内容——不产生事件、不认领任务、不创建会话。指挥官拿到完整图谱；`--agent-for` 与 `--task` 会一致地收窄所有集合，因此子 Agent 拿到的是属于自己的切片，而不是整个项目。

**CarryCtx 记录团队，但不运行团队。** 拉起进程、分派工作、重试、并发上限与模型选择，都属于你的 harness。CarryCtx 只负责持久化地回答*这个团队有谁、他们在做什么、每个人需要知道什么*——并且刻意不越过这条线：没有调度器、没有 worker 运行时、没有 prompt 缓存。

---

## 💡 Agent Skill 配置

使用官方 `skills` CLI 工具可以将 CarryCtx Skill 直接从 [carryctx-skills](https://github.com/Xuepoo/carryctx-skills) 下载并安装到本地 Agent 环境中，使 AI Coding Agent 拥有首类上下文管理能力：

列出仓库中所有可用 Skill：

```bash
npx skills add Xuepoo/carryctx-skills --list
```

为所有检测到的 Agent 安装全部 Skill：

```bash
npx skills add Xuepoo/carryctx-skills --all
```

为指定 Agent 安装选中的 Skill：

```bash
npx skills add Xuepoo/carryctx-skills \
  --skill carryctx-core \
  --skill carryctx-rules \
  --skill carryctx-workflows \
  --skill carryctx-personas \
  --skill carryctx-handoff \
  --agent codex \
  --agent claude-code \
  --agent cursor \
  --agent github-copilot
```

不安装、直接使用单个 Skill：

```bash
npx skills use Xuepoo/carryctx-skills --skill carryctx-core
```

安装完成后，Agent 会自动学习如何通过 CarryCtx 进行 Session 管理、Task 追踪、状态 Checkpoint 和 Context 恢复，实现跨重启与跨 Worktree 的连续协作。

---

## 📋 常用工作流

### 1. 任务管理 (Task Management)

```bash
carryctx task create --title "设计数据库 Schema"
carryctx task create --title "编写 API 接口" --depends-on CTX-0001
carryctx task claim CTX-0001
carryctx task start CTX-0001
carryctx task complete CTX-0001
```

### 2. 结构化进度追踪 (Progress Tracking)

```bash
carryctx progress todo "编写单元测试"
carryctx progress complete PX-0001
carryctx progress block "等待第三方 API 密钥"
carryctx progress risk "上游依赖版本可能更新"
carryctx progress note "建议使用 Redis 缓存热点数据"
```

### 3. 保存状态快照 (Checkpoints)

```bash
carryctx checkpoint \
  --done "实现了登录页面前端" \
  --remaining "待实现密码重置逻辑" \
  --blocker "等待 API 审核"
```

### 4. 会话与上下文恢复 (Session & Context)

```bash
carryctx session start
carryctx status            # 查看项目整体状态与进度
carryctx resume            # 获取当前 Task 的完整上下文与下一步行动
carryctx session end       # 结束当前 Session（提示创建 Checkpoint）
```

---

## 📚 详细文档

- 完整文档与指南：[carryctx.xuepoo.xyz](https://carryctx.xuepoo.xyz)
- Agent Skill 源码与规范：[carryctx-skills](https://github.com/Xuepoo/carryctx-skills)

---

## 🧭 设计原则

- **本地优先。** 完全不联网——二进制内不含任何网络栈；不需要账号、不上报任何遥测数据。所有状态存储在 `.git/carryctx/state.sqlite` 中。
- **Agent 无关。** Claude Code、OpenCode、Copilot、Codex，或是人类开发者——共享同一份结构化状态。
- **Git 是代码的事实来源，CarryCtx 是意图的事实来源。** 它不会替你改写历史，也不会替你解决 Merge Conflict。
- **它是管理工具，不是编排框架。** CarryCtx 负责持久化团队、任务与上下文，运行 Agent 的是你的 harness。它始终是一个工具，而不是一套你必须整体采纳的框架。

---

## 📄 开源协议

[MIT License](LICENSE)
