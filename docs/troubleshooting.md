# 故障排查与恢复

命令行阅读：`twig guide troubleshooting`。

## 先确定状态

```sh
twig --version
twig ls
twig ls myapp
```

核对 --config、项目别名、任务名、路径和 stderr 的错误。不要只凭窗口存在或 Git Clean 判断任务健康。Git 与 tmux 的实际查询结果优先于 twig 状态记录；手工修改外部资源后可能需要处理不一致。

## 常见情况

| 现象 | 含义与处理 |
| --- | --- |
| 找不到项目 | 用 ls 检查别名和 --config；未登记时 add。不会自动扫描磁盘 |
| 未找到 main/master | add 时用 --base 指定已有本地分支；仓库至少需要一次提交 |
| 初始化部分成功 | 保留资源；修复错误后再次 add 补登记，通过 Console Enter 准备主窗口 |
| new 的进入失败 | 非交互终端可能没有 TTY，但环境已经创建；用 ls 检查，交互终端 open；后续脚本用 --no-open |
| Worktree 已创建，tmux 失败 | 修复 tmux/配置后使用 open PROJECT NAME，不删除已有工作 |
| 无法识别 Pane 光标样式 | 检查 tmux 客户端和 server 版本；当前支持基线为 3.8，3.4 已确认不兼容。升级可执行文件后旧 server 仍可能运行，见 installation |
| Worktree 创建失败 | 保留记录及现场，先核对任务记录、分支与路径，按 Git 实际状态手动恢复；重复 new 会报任务已存在。若放弃，可用 rm PROJECT NAME --keep-branch 清理，仍需通过安全检查 |
| 窗口丢失 | open 按当前配置重建；不恢复原程序或对话 |
| 窗口初始化未完成 | 直接用 tmux 检查部分创建的 Pane；修正配置，确认可终止进程后 stop，再 open；不会自动重复启动 |
| 命令找不到/退出非零 | 检查对应 Pane 输出、任务分支中的脚本及依赖；shell 模式回到提示符，process 模式保留退出现场 |
| 同名窗口归属冲突 | 不会自动接管；核实后由用户重命名或关闭冲突窗口，再重试 |
| Worktree 显示 Mismatch | 检查目录存在、当前分支与仓库归属；不要盲目覆盖任务记录 |
| rm 拒绝删除 | 按错误检查脏文件、ignored 文件、活动进程和合并/upstream；见 cleanup |
| Worktree 已删但分支删除失败 | Git branch -d 有额外检查；分支和记录仍保留，可用 rm PROJECT NAME --keep-branch 完成清理 |

## 手动删除过 Worktree 目录

在主仓库检查 `git worktree list` 和 `git worktree prune --dry-run`。先核对并保留需要恢复的数据，再按 Git 的 Worktree 状态与任务记录手动恢复路径和分支；恢复后使用 `twig open PROJECT NAME`。重复 `new` 会因已有登记而拒绝，`open` 也不会重建缺失的 Worktree。twig 不自动 prune，不把缺失目录当成可以随意删除的资源。

## 锁冲突

锁路径会显示在错误中。先确认是否有其他 twig 命令运行；正常操作期间不要删除锁。仅在确认进程已经退出且锁确实遗留后，手动删除错误指出的 lock 文件。不要删除整个项目状态目录。

## 断线与重启

tmux server 存活时，SSH 重连后用 `twig open PROJECT NAME` 进入。tmux server 丢失或机器重启后可按记录重建窗口，但不恢复进程内存或 Agent 对话。容器销毁后的恢复依赖仓库和配置等文件是否持久化。

## Console 和 Codex

裸 twig 在非终端环境报错；读取文档用 guide，查询项目用 ls。Codex 不可用不阻止基本项目导航；需要 PATH 中存在 Codex CLI 及已运行的本机 app-server。面板不会启动或重启服务。

会话缺失可能是服务不同、历史尚未登记、工作目录不同、临时会话被隐藏，或尚未创建实际会话。历史子目录查询需要按 a 开启，再按 n 继续扫描。详细状态含义见 [Console](console.md)（`twig guide console`）。

## 支持边界

不支持裸仓库、非 UTF-8 路径、主仓库迁移自动修复或自动接管外部任务。旧 init/config/clone/--repo 及旧登记字段没有兼容入口；克隆和 remote 管理由 Git 负责。不读取或迁移旧 `.git/twig/` 数据。

Worktree 不隔离端口、数据库、Git 配置、hooks 和外部缓存。twig 不自动分配端口、复制 .env、安装依赖、自动合并或强制删除；服务冲突应由项目脚本和配置解决。

文件位置见 [配置与存储](configuration.md)（`twig guide configuration`）。
