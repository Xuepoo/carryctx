# CarryCtx

**面向 Coding Agent 与人类协作者的本地优先项目全生命周期控制层。**

CarryCtx 贯穿项目从初始化到发布的全过程。它把项目契约、计划、依赖、责任、执行状态、决策、交接、Git 证据和审计历史保存为持久、可查询的本地状态。Agent 或人可以更换工具、Session、窗口和 Worktree，而不会丢失项目全貌。

CarryCtx 是持久化与控制层，不是 Agent 运行时。外部 harness 仍负责拉起 Agent 进程、调度工作、路由 Prompt、重试失败和选择模型。CarryCtx 不提供 Completion Gates，也不提供通用 Automation Engine；它记录并约束外部系统和人类协作者所依赖的项目状态。

[English](README.md) | 简体中文

## 项目生命周期

CarryCtx 覆盖真实项目所需的完整链条：

1. **初始化并定义契约。** `carryctx init` 建立项目身份、任务前缀、分支默认值、配置、Agent 指引和共享状态数据库。
2. **规划并表达依赖。** 用任务、优先级、Scope、Blocker 和依赖边明确计划与 Ready 队列；强依赖会门控开始和完成。
3. **分配团队与角色。** 持久化 Commander、子 Agent、人类协作者、Team、Role、任务所有权和 Scope，让责任不再绑定于某个聊天 Provider。
4. **在 Worktree 与 Session 中执行。** 将任务绑定到隔离的 Git Worktree，注册 Agent，启动 Session，并从任意 harness 使用 CLI 或 MCP。进程调度和模型选择仍由 harness 负责。
5. **记录进度与 Checkpoint。** 随工作变化记录 Note、Todo、Blocker、Git 感知 Checkpoint、Context 和 Decision；`resume` 可为人或 Agent 重建下一步所需的切片。
6. **交接与评审。** 通过审计的 Handoff 状态机转移所有权，保留评审上下文；需要更正终态记录时使用经过授权的终态修正路径。
7. **清理并对账。** 完成或取消工作，检查过期注册，应用清理策略并运行持久化清理请求。Dirty Worktree、活跃 Session、当前目录、锁、缺失元数据和 jj-colocated 布局会安全拒绝，而不是意外删除。
8. **审计与分析。** 追加式事件日志、全文检索、Checkpoint、Decision、Session 历史和 `stats` 报告解释发生了什么以及团队如何工作。
9. **形成发布证据。** Backup、Migration、项目状态、Git 快照、审计记录、统计和验证输出为发布决策提供证据。CarryCtx 记录证据，但不会替你宣布发布完成。

## 边界

- **本地优先、离线运行。** CarryCtx 使用 SQLite 以及本地 Git/文件系统集成。权威项目状态位于 `<git-common-dir>/carryctx/state.sqlite`，由 linked worktree 共享。`.carryctx/` 存放项目配置和版本化指引，不是通用的状态目录。
- **控制而非编排。** CarryCtx 持久化并校验生命周期状态；外部 harness 负责拉起进程、调度、重试、Prompt 路由以及模型/Provider 选择。
- **不承诺未发布能力。** v0.8 不包含 Completion Gates 或通用 Automation Engine；没有云服务、遥测、Prompt 缓存，也不要求托管账号。
- **Agent 无关。** Claude Code、OpenCode、Copilot、Codex、其他 CLI harness 或人类开发者都可以使用同一套 CLI 和 stdio MCP 接口。

## 安装

### Cargo（推荐）

```bash
cargo install carryctx
```

### npm（可选的 Wrapper/分发渠道）

```bash
npm install -g carryctx
# 或
bun add -g carryctx
```

npm 是可选的薄 Wrapper 与平台二进制分发渠道；原生二进制仍是 CarryCtx 的主要交付物。

### GitHub Releases

从 [Releases 页面](https://github.com/Xuepoo/carryctx/releases) 下载预编译二进制文件。

### Homebrew

```bash
brew tap Xuepoo/tap https://github.com/Xuepoo/homebrew-tap.git
brew install carryctx
```

### Scoop（Windows）

```powershell
scoop bucket add Xuepoo https://github.com/Xuepoo/scoop-bucket.git
scoop install carryctx
```

### AUR（Arch Linux）

AUR 上游服务故障，当前已暂停发布。恢复前请使用 Cargo 或 [GitHub Releases](https://github.com/Xuepoo/carryctx/releases) 中的二进制文件。恢复前，AUR 的 `carryctx` 和 `carryctx-bin` 软件包不可用。

## 快速开始

```bash
cd your-project
carryctx init --name billing --task-prefix BILL
carryctx agent register --name commander --provider claude-code --kind commander
carryctx task create --title "实现 CSV 导出器"                   # BILL-0001
carryctx task depend BILL-0002 --on BILL-0001                      # 规划依赖
carryctx task claim BILL-0001 --agent commander
carryctx session start --agent commander
carryctx progress note --task BILL-0001 "开始实现"
carryctx checkpoint --agent commander --done "开始实现"
carryctx resume --agent commander
```

每条命令都支持稳定的 `--format json` 信封结构，供脚本和 Agent 使用；人类可使用 text 或 markdown 输出。

## Team、Worktree 与 Handoff

```bash
carryctx agent register --name dev-1 --provider codex --kind subagent --role implementer
carryctx team create --name core --commander commander
carryctx team member add core --agent dev-1 --role implementer
carryctx task team set BILL-0001 --team core
carryctx worktree create BILL-0001
carryctx handoff create --task BILL-0001 --target dev-1 --summary "可以实现了" --agent commander
carryctx handoff accept HO-0001 --claim-task --agent dev-1
```

`team status` 与 `team context` 是从持久记录重建的只读投影，可以返回完整协调视图，也可以切片到某个 Agent 或任务。CarryCtx 记录 Team 与 Handoff；何时、在哪里启动参与者由 harness 决定。

## v0.8 操作安全

- **清理策略与 CLI：** 配置安全的 `keep` 或 `when_idle` 行为，并检查、显示、dry-run 或运行持久化的 `worktree cleanup` 请求。
- **终态修正：** 对终态任务使用经过授权且带 `--force` 的显式修正，并写入审计；普通生命周期转换仍受约束。
- **MCP 有界执行：** stdio MCP 子进程调用有最大时限，单个卡住的子进程不会冻结服务循环。
- **jj 防护：** 对不支持的 live jj-colocated Git 布局，Worktree 创建和清理会安全拒绝；需要时使用 jj 原生 Workspace 操作并执行绑定。

## 命令面

| 领域       | 命令                                                             |
| ---------- | ---------------------------------------------------------------- |
| 契约与状态 | `init`、`project`、`config`、`doctor`                            |
| 计划与执行 | `task`、`progress`、`checkpoint`、`resume`、`context`、`session` |
| 协作       | `agent`、`team`、`handoff`、`decision`                           |
| 隔离与对账 | `worktree`、`worktree cleanup`、`hooks`                          |
| 证据与分析 | `event`、`search`、`stats`、`graph`                              |
| Agent 集成 | `mcp`、`preset`、`skill`、`completions`                          |

`sync` 仅是显式 Snapshot 的本地文件复制机制，不是云同步，也不会为二进制增加网络能力。

## MCP

CarryCtx 通过 stdio MCP 工具向 Cursor、Claude Desktop 等客户端开放持久化状态：

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

## 文档

- 完整文档与指南：[carryctx.xuepoo.xyz](https://carryctx.xuepoo.xyz)
- Agent Skill 源码：[carryctx-skills](https://github.com/Xuepoo/carryctx-skills)
- 协议：[MIT](LICENSE)
