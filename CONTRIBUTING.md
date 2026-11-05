# 贡献指南（CONTRIBUTING.md）

## 1. 分支管理

### 分支策略

- `main` 为主分支，始终保持稳定、可运行。
- 新功能、较大 Bug 修复及重构使用独立分支开发。
- 简单的文档更新、小型修复或维护工作允许直接提交到 `main`。
- 不同功能尽量使用独立分支，避免混合无关修改。
- 功能分支完成后及时合并并清理。

### 分支命名

采用 `<type>/<description>` 格式，使用小写英文，单词间以 `-` 分隔。

| 前缀 | 用途 | 示例 |
|---|---|---|
| `feat/` | 新功能 | `feat/agent-memory` |
| `fix/` | Bug 修复 | `fix/session-timeout` |
| `refactor/` | 代码重构 | `refactor/tool-runtime` |
| `chore/` | 工程维护 | `chore/update-deps` |
| `docs/` | 文档修改 | `docs/setup-guide` |
| `test/` | 测试相关 | `test/agent-runtime` |

### 合并策略

**默认使用 Squash Merge**，将功能分支的多个 Commit 合并为主分支上的单个 Commit。

- **Squash Merge（默认）**：适用于绝大多数功能开发、Bug 修复和重构。
- **Direct Commit（允许）**：适用于简单、低风险的修改。
- **Merge Commit（例外）**：仅在需要保留完整分支历史时使用。
- **Rebase**：可用于整理本地提交历史，不强制使用。

合并要求：

- 合并前确认功能符合预期，相关测试通过。
- Squash Commit Message 应准确概括整个分支的变更。
- 优先使用符合 Commit 规范的 PR 标题作为 Squash Commit 标题。
- 合并后删除不再使用的功能分支。
- 避免重写共享分支历史。

## 2. Commit 规范

遵循 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/v1.0.0/) 规范。

### 基本格式

```text
<type>(<scope>): <description>
```

- `type`：使用约定的英文类型。
- `scope`：可选，使用英文，表示修改的模块或组件。
- `description`：**必须使用中文**，简洁描述本次变更。

### 常用类型

| 类型 | 含义 |
|---|---|
| `feat` | 新功能 |
| `fix` | Bug 修复 |
| `refactor` | 不改变外部行为的代码重构 |
| `chore` | 工程维护 |
| `docs` | 文档变更 |
| `test` | 测试相关 |
| `perf` | 性能优化 |
| `style` | 代码格式调整 |
| `build` | 构建系统变更 |
| `ci` | CI/CD 配置变更 |
| `revert` | 撤销已有提交 |

### 示例

```text
feat(cli): 新增 Worktree 管理命令
fix(agent): 修复工具执行超时问题
refactor(runtime): 简化会话生命周期管理
chore(deps): 更新项目依赖
docs: 更新安装说明
test(memory): 增加持久化测试
perf(search): 优化搜索响应速度
```

### 提交要求

- `type` 和 `scope` 使用英文，`description` 必须使用中文。
- 描述应简洁明确，优先使用「新增」「修复」「优化」「移除」「重构」等动词开头。
- 每个 Commit 尽量只包含一个逻辑完整的变更。
- 不混合无关修改。
- 对于不兼容变更，使用 `!` 或 `BREAKING CHANGE:` 标记。
- Commit Body 可使用中文解释变更背景、原因和实现细节。

### Squash Commit 规范

功能分支内允许多个中间 Commit，不要求提前整理为单个 Commit。

Squash Merge 时：

- 最终 Commit 应描述整个功能或修复，而不是某个中间步骤。
- 最终 Commit Message 必须遵循上述规范。
- 必要时在 Commit Body 中说明背景、关键设计或兼容性影响。
- 不保留无意义的 WIP、临时调试等提交信息。

示例：

```text
# 功能分支内的提交
feat(memory): 新增记忆存储功能
test(memory): 增加记忆模块测试
fix(memory): 修复空记忆处理问题
refactor(memory): 简化记忆接口

# Squash Merge 后在 main 上的提交
feat(memory): 新增持久化记忆能力
```

**原则：开发分支允许灵活、细粒度提交；主分支保持简洁、清晰、可追溯的提交历史。**

## 持续集成

[CI 工作流](.github/workflows/ci.yml) 在 PR、推送到 main 和手动触发时运行，文档改动也执行。`check`、`terminal`、`agent` 均覆盖 Ubuntu 24.04 和 macOS 26，共六项检查：

- `check`：格式检查、严格 Clippy、Rust 测试和对应平台的服务示例校验。
- `terminal`：构建 Control Mode example，顺序运行十次隔离 PTY 验收，遇错立即失败。
- `agent`：固定 Node 22、Python 3.11 和 Codex 0.164.0，构建正式二进制，使用隔离 Twig 后台、app-server、本地固定模型和模拟飞书验收，不使用真实用户凭证或会话。

Linux 从源码构建 tmux 3.8，macOS 通过 Homebrew 安装。每项 Job 限时 20 分钟，Agent 验收步骤限时 5 分钟，失败日志保留 7 天。同分支或 PR 的新运行取消旧运行，一个平台失败不取消其他平台检查；当前不包含自动发布。实际结果以对应提交的 CI 为准，本地运行和隔离边界见 [独立验收入口](examples/README.md)。
