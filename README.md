# twig

用 Git Worktree 和 tmux 管理独立任务环境的轻量 Rust CLI。

一个项目对应一个 tmux Session，一个任务对应一个分支、Worktree 和窗口。默认普通 Shell，可配置分屏和启动程序。twig 不编排 Agent、不恢复对话、不自动合并。

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

仓库须已有提交，默认基础分支为本地 main/master。任务名直接作为分支名。脚本使用 `new --no-open`，仍创建窗口并执行启动配置。

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

Twig 打开期间会提示 Codex 新完成的轮次：导航和终端工作台共用右上角浮动卡片（每条 8 秒），正在查看的任务完成时静默，其他任务保留未读标记，进入对应任务终端后清除。卡片显示时按 `Ctrl-W f` 可直接跳到对应任务，支持跨项目。通知不持久化，详见 [Console 手册](docs/console.md)。

Console 导航页的列表、详情和终端工作台均支持鼠标拖选文字，松开自动复制，右下角显示字符数；选区期间画面固定，后台程序继续运行。`Esc` 清除选区，`Ctrl-C` 仍交给程序。本机使用系统剪贴板，SSH 通过终端请求复制，详见 [Console 手册](docs/console.md)。
