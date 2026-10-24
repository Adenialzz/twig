# twig

用 Git Worktree 和 tmux 管理并行开发任务，通过主控 Codex Agent 派活、跟进与验收。

一个项目对应一个 tmux Session，一个任务拥有独立的分支、Worktree 和任务窗口。你可以自己在任务中开发，也可以让主控协调多个受管 Codex 会话。

Twig 提供两个可切换的页面：**Console** 管理项目、任务和终端工作台；**Agent** 与主控对话、查看进展。普通任务可以独立使用，无需 Codex。

## 核心能力

- **任务隔离**：为每项工作创建独立分支与 Worktree，统一登记、进入、停止和清理。
- **终端工作台**：在 Twig 内使用 Shell、编辑器或任务启动程序，支持分屏、滚动和拖选复制；离开界面后任务进程继续运行。
- **主控协作**：通过 Codex daemon 创建受管任务、派发消息、跟进完成回调，要求子任务回报结果和阻塞，并核对验收。
- **状态与提醒**：查看 Codex 会话状态，接收消息送达和轮次完成通知，从通知直接跳转任务或主控；重要事项可由主控发送飞书通知。

## 安装

基础环境需要 Git、**tmux 3.8 或更新版本**和 Unix `ps`；源码安装还需要 Rust/Cargo 和 Make。可先用 `tmux -V` 检查版本，支持范围见 [安装指南](docs/installation.md)。在仓库目录执行：

```sh
make install
```

确保 `~/.cargo/bin` 在 `PATH` 中。此命令安装 Twig，并将主控 skill 复制更新到 `~/.twig/controller/workspace/.agents/skills/twig-controller/`，不写入 Codex 主目录。

| 可选能力 | 额外依赖 |
| --- | --- |
| 主控与受管 Codex 任务 | 支持 app-server daemon 的 Codex，且服务已启动 |
| 飞书通知 | curl、`TWIG_FEISHU_WEBHOOK_URL` |
| GitHub / GitLab PR | 已安装并登录的 gh / glab |

也可用 `cargo install --path . --locked` 只安装二进制；使用主控时需另行复制 skill。完整说明及平台验证范围见 [安装指南](docs/installation.md)。

## 快速开始

### 自己管理任务

先准备一个已有提交的 Git 仓库，登记后创建任务：

```sh
twig add myapp --path ~/projects/myapp
twig new myapp feat/login
```

创建成功后直接进入任务的终端工作台。按 `Ctrl-W d` 返回 Console，任务进程保留。

```sh
twig                         # 打开 Console
twig ls myapp                # 列出项目任务
twig open myapp feat/login   # 再次进入已有任务
```

普通任务默认启动 Shell，也可配置分屏和启动程序。已有任务使用 `open`，不要重复 `new`。完成后可用 `stop` 停止进程、`rm` 清理资源；操作前阅读 [快速开始](docs/quickstart.md) 和 [安全清理](docs/cleanup.md)。

### 让主控协调任务

完成安装和项目登记后，由你启动 Codex daemon，再打开 Agent 页面：

```sh
codex app-server daemon start
twig chat
```

例如告诉主控：

> 在 myapp 创建一个受管任务，排查登录失败并补充回归测试。要求子任务回报修改和验证结果，由你检查是否达到验收标准；先不要提交或推送。

主控通过 Twig CLI 操作项目，按内置 skill 派活并跟进。Agent 页面分别标识你的输入、子任务回报和轮次结束通知，子任务消息展示来源项目与任务。也可以自己创建受管任务和发送需求：

```sh
twig new myapp fix/login --agent --no-open
twig agent send --project myapp --task fix/login --message '排查登录失败，补充回归测试，更新 .twig/state.md 并回报结果。'
```

受管任务拥有绑定的 Codex session、持久化消息队列和完成回调。`.twig/state.md` 记录任务目标、进展与验证结果，主控工作区默认位于 `~/.twig/controller/workspace/`。

主控默认 **YOLO：无需审批、完整访问权限**，页面顶部常驻红色提示；子任务沿用 Codex 自身权限配置。

飞书通知可选，读取 `TWIG_FEISHU_WEBHOOK_URL`。需让执行通知命令的 Codex 环境能读取该变量；已经运行的 daemon 不会自动继承新终端里的 `export`。配置方式、消息恢复及已有会话的通信边界见 [主控指南](docs/agent.md)。

## 常用交互

`Ctrl-W d` 表示先按 `Ctrl-W`，再按 `d`。

| 位置 | 按键 | 操作 |
| --- | --- | --- |
| Console 普通导航 / Agent | `Tab` | 切换两个页面，保留选择和草稿 |
| Console 普通导航 | `1` / `2`、`Enter` | 聚焦项目 / 任务，进入所选项 |
| 终端工作台 | `Ctrl-W d` | 返回 Console，保留任务进程 |
| Console / 工作台 | `Ctrl-W f` | 跳转当前通知对应的任务或主控；无卡片时不操作 |
| Console / 工作台 | `Ctrl-W u` | 打开未读任务列表 |
| Agent | `Enter` / `Alt-Enter` | 发送 / 换行 |
| Agent | `Ctrl-C` | 有输入时清空，无输入时退出 Twig |

通知未读会持久化，退出重进仍保留，进入对应任务或 Agent 后清除。默认保存在 `~/.twig/notifications.json`，不修改配置文件；重启后补查已建立采集基线的新事件。工作台中的 `Ctrl-C` 发送给 Pane 内程序；Console 的表单、搜索和帮助有各自的键位规则。完整操作见 [Console](docs/console.md) 和 [主控指南](docs/agent.md)。

项目和任务列表按最近成功创建任务或进入工作台的时间排序，重启后保留；仅浏览不更新，重排保留当前选择。

## 行为边界

- Twig 不运行自有后台服务，也不启动、重启或停止共享 Codex daemon；服务不可用时提示用户启动。
- 受管任务通过 `new --agent` 新建，不自动迁移或接管旧任务。普通任务的已有 Codex session 是否可通信，需核对当前 daemon 中的会话并取得用户授权。
- 消息送达和一轮回复完成都不代表任务通过验收。投递结果未知时按消息 ID 核对，不自动重发。
- 普通任务命令不隐式 fetch、pull 或 push。`pr --push` 是显式推送入口；不自动合并，清理仍执行文件、进程和归属检查。

## 完整文档

[用户手册](docs/index.md) 按主题提供完整说明，也随二进制分发，支持离线阅读：

```sh
twig --help
twig new --help
twig guide quickstart
twig guide agent
twig guide --all
```

常用主题：[配置](docs/configuration.md) · [任务窗口与启动命令](docs/windows.md) · [PR/MR](docs/pr.md) · [Shell 补全](docs/completion.md) · [Agent 与脚本](docs/automation.md) · [故障排查](docs/troubleshooting.md)。

## 开发

构建用 `make build`，格式检查、Clippy 和 Rust 测试用 `make check`。终端与主控验收使用临时仓库、独立服务和模拟模型，不操作真实用户会话或 webhook。

[GitHub Actions CI](.github/workflows/ci.yml) 在 PR、推送到 `main` 或手动触发时运行：`check` 和 `terminal` 均覆盖 Ubuntu 24.04 与 macOS Tahoe 26（Apple Silicon），共四个检查。`check` 分步执行与 `make check` 相同的格式、Clippy 和 Rust 测试；`terminal` 构建并执行 Control Mode PTY 验收。Linux Job 从官方源码构建 tmux 3.8，macOS 通过 Homebrew 安装。每个 Job 限时 20 分钟，同分支或 PR 的新运行取消旧运行；文档改动同样检查，一个平台失败不取消另一个平台的检查。当前 CI 不包含真实 Codex app-server 验收或自动发布。

开发约定见 [AGENTS.md](AGENTS.md)，贡献流程见 [CONTRIBUTING.md](CONTRIBUTING.md)，架构与决策记录见 [DESIGN.md](DESIGN.md)，隔离验收入口见 [examples/README.md](examples/README.md)。
