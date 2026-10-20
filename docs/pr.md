# 提交 PR / MR

命令行阅读：`twig guide pr`。参数速查：`twig pr --help`。

统一使用 `twig pr`，分别调用已安装并登录的 [GitHub CLI `gh`](https://cli.github.com/manual/gh_pr_create) 或 [GitLab CLI `glab`](https://docs.gitlab.com/cli/mr/create/)。Git 推送仍使用 Git 自身的认证；twig 不保存凭据。

```bash
twig pr myapp auth                       # 查找或创建草稿
twig pr myapp auth --push                # 推送提交，再查找或创建
twig pr myapp auth --remote origin --target main
twig pr myapp auth --provider gitlab      # 自建平台需指定
twig pr myapp auth --title '修复登录超时' --body-file ./pr.md
```

- 项目别名与任务名均必填，不按当前目录猜测目标。操作前验证 Worktree 归属和分支。
- remote 优先使用显式 `--remote`，否则使用任务分支的 upstream remote；没有 upstream 时仅在存在唯一 remote 的情况下自动选择。
- 若选中 upstream remote，源分支使用 upstream 对应的远端分支名；否则使用任务分支的同名远端分支。目标默认为项目配置的 `target`，可用 `--target` 覆盖。
- 根据 remote URL 自动识别 `github.com` / `gitlab.com`；其他域名必须指定 `--provider`。支持常见 HTTPS、HTTP、`git@host:group/repo.git` 和 `ssh://git@host:port/group/repo.git`，以及 GitLab 多级 subgroup。SSH 端口不作为平台 API 端口；不解析 SSH 主机别名、IPv6 地址或本地路径 remote。
- 查找同仓库、同源分支、同目标分支的**开放中** PR/MR。已有时输出链接，不重复创建，也不修改标题、正文或草稿状态；已关闭或合并的请求不复用。不带 `--push` 的已有请求查询不会同步本地新提交。
- 没有匹配请求时创建草稿。默认标题为任务最新提交标题，正文为该提交的完整说明；可用 `--title` / `--body-file` 提供正式内容。正文文件路径相对于调用目录，保留换行和特殊字符，不通过 Shell 插值。
- 创建前联网查询远端分支：目标分支须存在；不带 `--push` 时，远端源分支须与任务当前提交一致。带 `--push` 时只推送该任务分支，并设置 upstream；即使已有 PR/MR 也会先推送，再输出链接。不强推、不自动提交文件，也不顺带推送其他分支或 tag。
- 首版只支持同一远程仓库内的分支；fetch/push URL 指向不同仓库或 remote 存在多个 URL 时拒绝操作。跨 Fork PR/MR 请直接使用 `gh` / `glab`。

`pr` 是显式联网命令；`new/open/ls/stop/rm` 不因此增加联网行为。查找、认证或解析失败时直接报错；推送失败不创建请求。推送成功但创建失败时保留远端提交，可排除错误后重试。不会自动合并、关闭任务窗口、删除 Worktree 或本地分支。

后续清理见 [安全清理](cleanup.md)（`twig guide cleanup`）。
