# twig

用 Git Worktree 和 tmux 管理独立任务环境的轻量 Rust CLI。

一个项目对应一个 tmux Session，一个任务对应一个分支、Worktree 和窗口。默认普通 Shell，可配置分屏和启动程序。普通任务不编排 Agent；可选主控通过 Codex daemon 派发与跟进受管任务，不自动合并。

## 安装

需要 Git、tmux、Unix ps；源码安装还需要 Rust/Cargo：

```sh
cargo install --path . --locked
# 或 make install
```

确保 `~/.cargo/bin` 在 PATH 中。完整依赖和安装说明见 [安装指南](docs/installation.md)。

## 开始使用

```sh
twig add myapp --path ~/projects/myapp
twig new myapp feat/login
twig ls myapp
twig open myapp feat/login
```

仓库须已有提交，默认基础分支为本地 main/master。任务名直接作为分支名。同名本地分支或已 fetch 的远端跟踪分支存在时，CLI 和 Console 会询问是否使用，默认取消；本地优先，多个 remote 同名时选择来源，不自动 fetch。已被其他 Worktree 占用的本地分支不可使用。脚本使用 `new --no-open`，仍创建窗口并执行启动配置。Console 和 CLI 新建均拒绝已登记的同名任务，已有任务请使用 `open` 或从任务列表进入；失败留下记录时也不能用重复 `new` 恢复。

裸 `twig` 或 `twig console` 打开交互控制台：宽屏采用项目、任务、全局 Codex 监控和任务详情的 2×2 布局；左下监控独立刷新，不随选择或搜索改变。任务区 Enter 在 Twig 内打开终端工作台；`Ctrl-W d` 返回导航，分屏和进程继续由 tmux server 托管。`stop PROJECT NAME` 停止窗口和进程，`rm PROJECT NAME` 安全删除任务，`rm PROJECT` 只取消登记。清理前阅读 [安全清理](docs/cleanup.md)。

## 阅读文档

[用户手册目录](docs/index.md) 是完整入口，人和 Agent 使用同一份说明：

```sh
twig --help
twig new --help
twig guide
twig guide quickstart
twig guide --all
```

文档随二进制分发，离线输出 Markdown，不依赖 man 或分页器。常用主题：[命令参考](docs/commands.md)、[配置](docs/configuration.md)、[任务窗口](docs/windows.md)、[Console](docs/console.md)、[PR/MR](docs/pr.md)、[Agent 与脚本](docs/automation.md)、[故障排查](docs/troubleshooting.md)。

Zsh 补全在 compinit 之后加载 `source <(COMPLETE=zsh twig)`，详见 [补全指南](docs/completion.md)。

## 开发

先阅读 [AGENTS.md](AGENTS.md)、[DESIGN.md](DESIGN.md) 和 [CONTRIBUTING.md](CONTRIBUTING.md)。构建使用 `make build`，验证使用 `make check`（fmt、Clippy、测试）。生命周期测试使用临时仓库及独立 tmux socket，不操作真实项目；补全交互测试需要 Zsh。

用户手册以 docs/ 下的 Markdown 为源文件，编译时嵌入 twig guide。命令参数变化时同步更新 Clap 帮助和对应主题。

Console、`new` 和 `open` 统一使用 Control Mode 终端工作台；`Ctrl-W d` 返回 Console，`q` 退出，后台进程保留。工作台操作见 [Console 手册](docs/console.md)，独立验收入口和当前限制见 [实验说明](examples/README.md)。

Twig 打开期间会提示 Codex 新完成的轮次：导航和终端工作台共用右上角浮动卡片（每条 8 秒），正在查看的任务完成时静默，其他任务保留未读标记，进入对应任务终端后清除。卡片显示时按 `Ctrl-W f` 可直接跳到对应任务，支持跨项目。错过卡片后，可按 `Ctrl-W u` 打开未读任务列表，用 j/k 选择、Enter 进入。通知不持久化，详见 [Console 手册](docs/console.md)。

Console 导航页的列表、详情和终端工作台均支持鼠标拖选文字，松开自动复制，右下角显示字符数；选区期间画面固定，后台程序继续运行。`Esc` 清除选区，`Ctrl-C` 仍交给程序。本机使用系统剪贴板，SSH 通过终端请求复制，详见 [Console 手册](docs/console.md)。

Console 登记项目和新建任务均使用独立居中弹窗；任务创建成功后直接进入内置终端工作台，`Ctrl-W d` 返回导航并保留新任务选择。项目列表按最近成功创建任务或进入工作台的时间排序，记录随所选用户配置保存，重启后保留；仅浏览不更新。

工作台滚轮作用于鼠标所在 Pane：程序启用鼠标模式时交给程序滚动，否则浏览 tmux 历史。`Shift-PgUp/PgDn` 始终浏览 tmux 历史。

Console 在内容变化时重绘，状态查询按任务分批更新。进入终端工作台后暂停不可见的 Git 状态查询，返回导航立即刷新；Codex 监控和完成通知继续采集。选中复制在后台按顺序执行，先显示「正在复制」，完成后显示结果。

## 主控 Agent

`make install` 将 `twig-controller` skill 复制到 `~/.twig/controller/workspace/.agents/skills/twig-controller/`。主控通过该 skill 规范工作流，派活时要求子任务主动回报结果与阻塞，并由主控核对验收。

`twig chat` 打开主控，点击顶部居中的圆角 Console / Agent 标签或按 Tab 切换页面。需要已运行的 Codex daemon；不可用时只提示用户启动。受管任务使用 `twig new PROJECT TASK --agent`，通过 CLI 派活与完成回调跟进；退出 UI 不停止 Codex。主控默认 YOLO（无需审批、完整访问权限），页面顶部常驻红色提示；子任务权限策略不变。Agent 支持消息分层、代码块着色、拖选复制、Alt-C 复制回复和自动增高的输入框。离开 Agent 后，主控回复完成会浮动提醒，`Agent*` 标记未读；主控卡片显示时，在导航或工作台按 `Ctrl-W f` 直达 Agent，进入后清除未读。飞书通知读取 `TWIG_FEISHU_WEBHOOK_URL`。详见 [主控指南](docs/agent.md)（`twig guide agent`）。

主控与受管任务经 Twig 发送的消息，确认投递后也会标记接收方未读；历史消息不追溯，查看时静默，进入后清除。详见 [主控指南](docs/agent.md)。
