# 快速开始

命令行阅读：`twig guide quickstart`。尚未安装时先看 [安装](installation.md)。

## 工作方式

twig 用 Git Worktree 和 tmux 管理本地任务环境。一个项目对应一个 tmux Session；一个任务对应一个分支、Worktree 和窗口，窗口可分成多个 Pane。

项目通过登记别名定位，任务命令始终显式指定项目与任务，不根据当前目录猜测。普通任务由用户自行开发或运行 Agent；可选主控支持受管 Codex 任务的派活与跟进，见 [主控 Agent](agent.md)。Twig 不自动提交、合并或部署。

## 登记已有仓库

准备一个至少有一次提交的普通 Git 仓库；需要克隆时先自行运行 git clone。

```sh
twig add myapp --path ~/projects/myapp
twig ls
twig ls myapp
```

首次 add 默认选择已有本地 main，其次 master；其他分支用 `--base develop` 指定。它创建项目配置、Worktree 根目录和 tmux 主窗口，并登记项目；不联网、不要求 remote。项目别名不必等于目录名。

## 创建和进入任务

```sh
twig new myapp feat/login
```

新任务分支就是 `feat/login`，不另加前缀。默认 Worktree 位于主仓库旁的 `myapp.worktrees/feat/login/`。成功后进入任务窗口，在其中正常编辑、测试、提交代码或手工启动 Agent。

只准备环境、不进入窗口：

```sh
twig new myapp fix/bug --no-open
twig ls myapp
twig open myapp fix/bug
```

`--no-open` 仍创建窗口并执行配置的启动命令，不是 dry-run。重复 new 对已登记同名任务直接报错，请使用 open 或 Console 任务列表进入。普通任务的 open 在窗口缺失时按启动配置重建，不恢复原进程或对话；受管任务则连接绑定的 Codex session。

Console、new 和 open 使用相同的 Twig 终端工作台。按 `Ctrl-W d` 返回 Console 导航并保留进程，再按 q 退出；不需要使用 tmux 前缀。输入 exit 会退出当前 Shell，可能关闭 Pane。

## 查看控制台

```sh
twig
```

裸 twig 与 twig console 都进入交互面板。Tab 切换 Console / Agent，1 / 2 聚焦项目 / 任务，方向键选择，任务区 Enter 进入窗口，? 查看帮助，q 退出。浏览不会创建资源；提交表单、确认删除及进入缺失窗口会执行相应操作。完整说明见 [Console](console.md)（`twig guide console`）。

## 完成与清理

提交、推送、PR/MR、审查和合并由 Git 及托管平台负责，可让 Codex 直接使用 gh/glab 完成。

在其他终端确认任务进程可以结束后：

```sh
twig stop myapp feat/login
# 自行完成代码审查、合并及必要的推送
twig rm myapp feat/login
```

stop 关闭窗口和进程但保留代码；rm 默认要求目录干净、没有 ignored 文件、没有活动进程，且分支通过合并与 upstream 检查。希望保留分支可用 `--keep-branch`。详见 [安全清理](cleanup.md)（`twig guide cleanup`）。

## 下一步

- 查询参数与默认值：`twig <命令> --help` 或 [命令参考](commands.md)。
- 配置分屏和启动命令：`twig guide windows`。
- 从 Agent 或脚本调用：`twig guide automation`。
- 处理失败和资源恢复：`twig guide troubleshooting`。
