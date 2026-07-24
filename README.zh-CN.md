# CarryCtx

面向 Coding Agent 的本地优先（Local-first）项目状态与上下文连续性管理器。

CarryCtx 是一个命令行工具（CLI），用于在 Coding Agent 的会话（Session）、编辑器窗口以及 Git Worktree 之间保存和恢复项目上下文。它提供了结构化任务管理、进度追踪、基于 Checkpoint（快照）的状态捕获以及 Session 生命周期管理 —— 所有数据均由本地 SQLite 数据库统一存储与支持。

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

---

## 💡 Agent Skill 配置

使用官方 `skills` CLI 工具可以将 CarryCtx Skill 直接下载并安装到本地 Agent 环境中，使 AI Coding Agent 拥有首类上下文管理能力：

```bash
npx skills add https://github.com/Xuepoo/carryctx-skills --skill carryctx
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
carryctx progress complete PX-0001 "完成了用户表设计"
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

- Agent Skill 源码与规范：[carryctx-skills](https://github.com/Xuepoo/carryctx-skills)

---

## 📄 开源协议

[MIT License](LICENSE)
