# 安全清理

命令行阅读：`twig guide cleanup`。参数速查：`twig rm --help`、`twig stop --help`。

推荐从主仓库的其他终端执行：

```bash
twig stop myapp auth                    # 会确认终止窗口内的全部进程
# 自行审查、合并和推送代码
twig rm myapp auth
twig rm myapp payment --keep-branch     # 允许保留尚未合并的分支
```

默认删除要求：

- Worktree 分支和仓库归属仍与登记一致。
- 无未提交、未跟踪或被忽略的文件。构建产物也会阻止删除，请自行检查和清理；`ls` 的 Clean 遵循普通 Git status，不包含 ignored 文件。
- 分支提交是目标分支的祖先。Squash/rebase 合并后即使内容相同，也可能被拒绝；可用 `--keep-branch` 保留分支再人工处理。
- 配置 upstream 时，相对本地 upstream 引用没有额外提交。此检查不连接远端；远端状态的新鲜程度取决于你最近的 fetch/push。无 upstream 时明确提示无法判断推送状态，但允许删除已合并到本地目标的分支。
- 所有 Pane 都是无子进程的已知 Shell，或已经退出。前台/后台任务及未知进程状态都会阻止删除。请先退出任务，或明确执行 `stop`。

`--keep-branch` 跳过合并和推送检查，仍检查工作目录和活动进程。`-y` 仅跳过确认，不绕过安全检查。CLI 使用 `git worktree remove` 和 `git branch -d`，不提供公开的强制删除开关。

删除顺序为窗口、Worktree、分支、任务记录。若 Git 拒绝某一步，已完成步骤不会撤销，其余资源和记录保留，错误会说明下一步。Git 的 `branch -d` 自身还会执行检查；例如 `--target` 与当前主工作目录分支不同，可能在 Worktree 移除后拒绝删除分支，此时可用 `rm --keep-branch` 完成记录清理。

## 取消登记与删除任务

`twig rm myapp` 立即取消项目登记，不要求确认；保留仓库、任务、分支、窗口和内部状态。之后可用 `twig add myapp --path /path/to/myapp` 重新登记。

`twig rm myapp auth` 才会删除具体任务，默认询问确认。`--target`、`--keep-branch`、`--delete-ignored` 必须同时提供任务名。目标分支默认使用项目配置的 target；`--target BRANCH` 只覆盖这一次合并检查。

任务已经不在 twig 记录中时，重复 rm 成功返回，不操作同名外部资源。停止已登记任务的缺失窗口也成功返回；stop 的任务记录本身必须存在。

## 被忽略的文件

默认保护 ignored 文件，包括构建目录、`.env` 和本地数据。检查后可显式运行：

```sh
twig rm myapp auth --delete-ignored
```

命令在确认前列出要删除的 ignored 文件/目录；目录项包含目录内全部内容。该选项仅放行 ignored 文件，未提交、未跟踪文件及活动进程仍阻止删除。它不执行 git clean，也不强制移除 Worktree。加 `-y` 会跳过确认，因此先核对数据是否仍需要。

Console 的 `/delete` 或 `dd` 提供相同清理能力，默认取消；「删除本地分支」默认勾选（取消勾选则保留分支），勾选后跳过合并和推送检查，以 `git branch -D` 删除本地分支，文件和活动进程保护仍生效，「同时删除被忽略文件」默认勾选；确认前须核对清单，详见 [Console](console.md)（`twig guide console`）。

「主分支目录拉取远端最新」默认勾选；确认后先在项目登记的主工作目录执行 `git pull --ff-only`，使用当前分支的 upstream，不切换分支、不自动 stash。有未提交或未跟踪文件、缺少 upstream、网络失败或历史分叉时停止并保留任务；取消勾选即可仅删除。拉取成功后重新检查删除保护，再清理任务；若后续清理失败，已拉取的主目录更新不会撤销。

清理不删除远端分支、不合并代码、不关闭 PR/MR。部分失败的处理见 [故障排查](troubleshooting.md)。

## 受管 Codex 任务

`new --agent` 创建的任务还受 Codex 活动状态和待发消息保护。先用 stop 中断轮次并取消尚未投递的消息，再检查文件；结果未知的 sending 记录需核对后用 `agent cancel --id ID` 取消，取消记录不撤回已发内容。`.twig/state.md` 是 ignored 文件，CLI 删除须显式 `--delete-ignored`。Twig 不关闭共享 Codex daemon。
