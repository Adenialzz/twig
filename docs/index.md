# twig 用户手册

twig 用 Git Worktree 和 tmux 管理独立任务环境。人和 Agent 使用同一份文档，按问题逐步深入即可。

## 从哪里开始

```sh
twig --help               # 找到命令
twig new --help           # 查看某个命令的参数和关键行为
twig guide quickstart    # 按步骤完成首次使用
twig guide windows       # 阅读一个主题
twig guide --all          # 输出完整手册
```

`twig guide` 输出本导航。所有 guide 内容随二进制分发，离线可读，不要求终端，不读取配置或创建资源。输出是 Markdown 文本，可重定向保存，或用 `twig guide --all | less` 分页。当前不单独安装 man 页面。

裸 `twig` 会打开交互控制台，不是帮助命令。

## 主题目录

| 主题 | 阅读目标 | 命令 |
| --- | --- | --- |
| [安装](installation.md) | 依赖、构建、安装和更新 | twig guide installation |
| [快速开始](quickstart.md) | 从登记项目到完成任务 | twig guide quickstart |
| [命令参考](commands.md) | 每条命令的参数、默认值、约束与副作用 | twig guide commands |
| [配置与存储](configuration.md) | --config、登记表、覆盖规则和状态文件 | twig guide configuration |
| [任务窗口](windows.md) | 分屏、启动命令、shell/process 模式 | twig guide windows |
| [Console](console.md) | 面板操作、快捷键、Codex 状态和历史 | twig guide console |
| [安全清理](cleanup.md) | stop/rm、文件保护和部分失败 | twig guide cleanup |
| [PR/MR](pr.md) | 平台认证、显式推送和草稿请求 | twig guide pr |
| [Shell 补全](completion.md) | 安装 Zsh 补全及候选规则 | twig guide completion |
| [Agent 与脚本](automation.md) | 非交互调用、结果判断和重试 | twig guide automation |
| [故障排查](troubleshooting.md) | 诊断错误、恢复资源和支持边界 | twig guide troubleshooting |

## 三个基本约定

- 项目是登记别名；任务操作显式指定项目和任务，不按当前目录猜测。
- 创建窗口可能启动用户配置的程序，--no-open 只是不进入窗口。
- 普通任务命令不隐式 fetch/pull/push；pr 是显式联网操作，--push 才允许推送。用户启动程序的联网行为由程序决定。

源码贡献者另见仓库根目录的 DESIGN.md、AGENTS.md 与 CONTRIBUTING.md；这些开发记录不是用户操作手册。

- [主控 Agent 与 Chat](agent.md)：`twig guide agent`。
