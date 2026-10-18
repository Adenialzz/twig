# twig

用 Git Worktree 和 tmux 管理独立任务环境的轻量 Rust CLI。

一个项目对应一个 tmux Session，一个任务对应一个分支、Worktree 和窗口。窗口打开普通 Shell，Codex、Claude 或其他程序由你手动启动。twig 不调用 LLM，不自动合并，也不运行后台服务。

继续开发前请阅读 [AGENTS.md](AGENTS.md) 和 [DESIGN.md](DESIGN.md)，了解开发约定、设计决定、候选功能与交接状态。

## 安装

需要 Rust/Cargo、Git、tmux 和 Unix `ps`。当前在 macOS 上使用 Rust 1.93.1、Git 2.50.1、tmux 3.6a 验证；Linux 尚待实机验证。

```bash
cd /path/to/twig
make install
twig --help
```

`make install` 先检查 `gh` 和 `glab`，通过 Homebrew 安装缺少的工具，已在 PATH 中的工具直接跳过；缺少工具且没有 Homebrew 时停止并提示手动安装。随后安装或更新 twig，成功后打印 Zsh 补全和平台登录提示，不修改 Shell 配置、不自动登录。使用 PR/MR 前按需运行 `gh auth login` / `glab auth login`。

只安装 twig，或不使用 Make/Homebrew 时，可以直接执行 `cargo install --path . --locked`，平台 CLI 按需自行安装。确保 `~/.cargo/bin` 在 PATH 中。支持从 Bash、Zsh 调用；普通命令无需修改 Shell 配置，Tab 补全按下节启用。

仅构建而不安装：

```bash
make build
./target/release/twig --help
```

直接运行 `make` 也会构建 release 版本，等价于 `cargo build --release --locked`。

### Zsh 自动补全

安装或更新 twig 后，在 `~/.zshrc` 中加入：

```zsh
autoload -Uz compinit
compinit
source <(COMPLETE=zsh twig)
```

如果 Oh My Zsh 等配置已经运行 `compinit`，只需在它之后加入最后一行。重新打开终端，或在当前 Zsh 中执行上述配置即可启用。每次启动 Shell 都重新生成补全接入脚本，升级 twig 后重新打开终端即可。

支持命令、选项及以下动态值：

| 输入后按 Tab | 候选来源 |
| --- | --- |
| `twig open ` / `stop ` / `rm ` / `new ` | 项目已有任务；`new` 仍可输入新名称 |
| `twig pr `、`twig pr --remote `、`twig pr --provider ` | 已有任务、本地 Git remote、GitHub/GitLab 平台 |
| `twig --repo `、`twig clone ` | 用户登记表中的项目名 |
| `twig config show ` / `remove `、`twig config remote add/remove/default ` | 已登记项目名 |
| `twig config remote remove demo ` / `default demo ` | demo 登记的 remote 名 |
| `twig new auth --base `、`twig init --base ` / `--target `、`twig rm auth --target ` | 已有本地分支，不含远端跟踪分支 |
| `--config `、`--root `、`config add` 的 `--path ` | 文件或目录路径 |

动态候选按已输入前缀过滤，并识别光标前的 `--repo NAME`、`--config PATH`（也支持 `--repo=NAME`、`--config=PATH`）。例如 `twig --repo myapp open <Tab>` 可以在任意目录补全该项目的任务。关联 Worktree 共用任务候选。

带空格的配置路径可使用单引号、双引号或反斜杠转义。目录补全支持空格、冒号及 `--root=PATH` 写法，保留末尾 `/`，可以继续输入并补全子目录。

`twig open <Tab>`（以及 `new`、`stop`、`rm`）先展示任务名，再展示带说明的选项；Tab 选择也优先任务名。任务按名称字典序排列。输入 `twig open a<Tab>` 只匹配该前缀的任务，输入 `twig open --<Tab>` 只显示选项。升级后在当前 Zsh 重新执行 `source <(COMPLETE=zsh twig)` 即可加载新的展示规则。

补全只读本地数据，不联网、不创建配置或锁、不调用 tmux，也不执行正在输入的任务命令。非仓库目录、未初始化项目或未登记项目可能没有相应候选；配置损坏时不会修改或覆盖文件。

## 使用

### 登记常用项目与多个 remote

用户级登记表默认位于 `~/.twig/config.toml`。首次写入时自动创建目录和文件，也可以直接编辑 TOML：

```toml
[projects.myapp]
path = "~/projects/myapp"
main = "main"
default_remote = "origin"

[projects.myapp.remotes]
origin = "git@github.com:yourname/myapp.git"
upstream = "git@github.com:team/myapp.git"
```

用命令登记和克隆：

```bash
twig config add myapp \
  --url git@github.com:yourname/myapp.git \
  --path ~/projects/myapp \
  --main main
twig config remote add myapp upstream git@github.com:team/myapp.git

twig config list
twig config show myapp
twig clone myapp

# 在任意目录使用，不需要先 cd
twig --repo myapp new auth
twig --repo myapp ls
twig --repo myapp open auth
```

`config add` 的 `--remote` 默认是 `origin`，`--main` 默认是 `main`；`--path` 必填，相对路径在登记时转换为相对于调用目录的绝对路径。手动编辑 TOML 时，项目路径使用绝对路径或 `~/` 开头。项目名和 remote 名使用字母、数字、横线或下划线，并以字母或数字开头。

```bash
twig config remote default myapp upstream  # 设置首次 clone 的来源
twig config remote remove myapp origin     # 默认 remote 需先切换才能移除
twig config remove myapp                   # 仅取消登记，保留仓库、remote 和任务
```

登记操作不联网，不要求本地仓库已经存在，也不修改现有 Git remote。修改已有项目的路径、URL 或主分支可直接编辑 TOML；命令写入会重新序列化整个登记表，不保留手写注释。

`clone` 使用 `default_remote` 对应的 URL，克隆并检出 `main` 指定的分支，保留 Git 的进度和认证交互，再添加其他 remote 并初始化 twig。其他 remote 只添加配置，不自动 fetch。认证复用 Git、SSH 和 credential helper，不由 twig 保存凭据。

目标路径已存在时，只允许接续默认 remote URL 完全匹配、主分支存在的仓库；登记表中已存在的 remote 都会先核对 URL。不会覆盖目录、改写不同的 URL、切换已有分支或拉取更新。额外的 Git remote 会保留。若初始化失败，仓库保留，可修复原因后重新执行 `twig clone myapp`。仓库已有 twig 配置但基础/目标分支与登记的主分支冲突时，会报错而不覆盖。

已有本地仓库可以只登记，再执行 `twig --repo myapp init`；此时不需要 `clone`，也不会修改 Git remote。登记主分支只作为首次初始化默认值；初始化后，任务仍以项目自身配置和实际 Git 状态为准。更新登记表不会自动同步到已有仓库；显式重试 `clone` 会核对 remote 并添加缺少的 remote，不删除其他 remote。

需要独立登记表时可用 `twig --config /path/to/config.toml ...`，例如用于测试或不同机器配置；不带 `--repo` 的任务命令仍按当前目录识别项目；`init` 会向选定的登记表自动登记。

### 直接使用当前仓库

在已有、至少有一个提交的普通 Git 仓库中运行：

```bash
twig init                         # 优先 main，其次 master；自动登记项目
twig new auth                     # 创建 feature/auth 和 Shell 窗口，不自动进入
twig new search
twig ls
twig open auth                    # 在普通终端 attach，在 tmux 内 switch-client
# 在窗口中手动运行 codex 等程序
```

默认目录结构，无需移动原仓库：

```text
projects/
├── myapp/                        # 原仓库
└── myapp.worktrees/
    ├── auth/
    └── search/
```

首次 `init` 成功后自动写入 `~/.twig/config.toml`（或 `--config` 指定的文件），默认用主仓库目录名作为别名，此后可在任意目录使用 `twig --repo myapp ...`。目录名不符合项目命名规则或与其他路径重名时，用 `init --name myapp` 指定别名。冲突会在创建项目配置、Worktree 根目录和 Session 前报错。

同一路径已经登记时复用原别名和登记内容，不新增别名、不覆盖主分支或 remote；在关联 Worktree 内执行也登记主仓库。已有项目配置时，重复 `init` 只提示“当前项目已经初始化，无需重复执行。”并成功退出，不补登记、不操作 tmux、不创建锁，也不处理传入的初始化参数。新登记的 `main` 取项目基础分支，remote 留空，不自动导入 Git remote；首次 `config remote add` 会同时设为默认 remote，未配置默认 remote 时 `clone` 会明确报错。

初始化失败时不写入新登记，已创建的项目资源保留。若尚未保存项目配置，可修复后重试 `init`；若 `config.json` 已存在，重复 `init` 也只提示退出。此时缺失的全局登记需手动补充登记表，tmux 修复后可用 `new`/`open` 创建窗口。全局登记保存失败时也需手动补充登记表。

可在初始化时自定义：

```bash
twig init --name myapp --base develop --target main --root ../my-worktrees
twig new payment --base feature/auth
```

`--base` 必须是已有本地分支。`--target` 是安全删除时检查的合并目标，与任务创建来源独立。默认合并目标等于初始化时的基础分支。不自动 fetch、pull、push 或修改主分支。

Worktree 根目录必须与主工作目录分离，不能相互包含。任务名为 1–64 位 ASCII 字母、数字、`-` 或 `_`，首位为字母或数字；`main` 保留给主窗口。

### 提交 GitHub PR / GitLab MR

统一使用 `twig pr`，分别调用已安装并登录的 [GitHub CLI `gh`](https://cli.github.com/manual/gh_pr_create) 或 [GitLab CLI `glab`](https://docs.gitlab.com/cli/mr/create/)。Git 推送仍使用 Git 自身的认证；twig 不保存凭据。

```bash
twig pr                         # 在任务 Worktree（或其子目录）查找或创建草稿
twig pr auth --push              # 推送 auth 的已提交内容，再查找或创建
twig --repo myapp pr auth --push  # 从任意目录操作已登记项目
twig pr --remote origin --target main
twig pr --provider gitlab        # 自建 GitLab；自建 GitHub 使用 github
twig pr --title '修复登录超时' --body-file ./pr.md
```

- 省略任务名时按调用目录识别已登记的任务 Worktree；在主工作目录或其他目录需指定任务名。操作前验证 Worktree 归属和分支。
- remote 优先使用显式 `--remote`，否则使用任务分支的 upstream remote；没有 upstream 时仅在存在唯一 remote 的情况下自动选择。登记表的 `default_remote` 只用于 clone，不决定 PR/MR 的 remote。
- 若选中 upstream remote，源分支使用 upstream 对应的远端分支名；否则使用任务分支的同名远端分支。目标默认为项目配置的 `target`，可用 `--target` 覆盖。
- 根据 remote URL 自动识别 `github.com` / `gitlab.com`；其他域名必须指定 `--provider`。支持常见 HTTPS、HTTP、`git@host:group/repo.git` 和 `ssh://git@host:port/group/repo.git`，以及 GitLab 多级 subgroup。SSH 端口不作为平台 API 端口；不解析 SSH 主机别名、IPv6 地址或本地路径 remote。
- 查找同仓库、同源分支、同目标分支的**开放中** PR/MR。已有时输出链接，不重复创建，也不修改标题、正文或草稿状态；已关闭或合并的请求不复用。不带 `--push` 的已有请求查询不会同步本地新提交。
- 没有匹配请求时创建草稿。默认标题为任务最新提交标题，正文为该提交的完整说明；可用 `--title` / `--body-file` 提供正式内容。正文文件路径相对于调用目录，保留换行和特殊字符，不通过 Shell 插值。
- 创建前联网查询远端分支：目标分支须存在；不带 `--push` 时，远端源分支须与任务当前提交一致。带 `--push` 时只推送该任务分支，并设置 upstream；即使已有 PR/MR 也会先推送，再输出链接。不强推、不自动提交文件，也不顺带推送其他分支或 tag。
- 首版只支持同一远程仓库内的分支；fetch/push URL 指向不同仓库或 remote 存在多个 URL 时拒绝操作。跨 Fork PR/MR 请直接使用 `gh` / `glab`。

`pr` 是显式联网命令；`new/open/ls/stop/rm` 不因此增加联网行为。查找、认证或解析失败时直接报错；推送失败不创建请求。推送成功但创建失败时保留远端提交，可排除错误后重试。不会自动合并、关闭任务窗口、删除 Worktree 或本地分支。

## 命令

| 命令 | 行为 |
| --- | --- |
| `twig config add/list/show/remove` | 登记、查看和取消登记项目，不联网 |
| `twig config remote add/remove/default` | 管理项目的多个 remote 和默认克隆来源 |
| `twig clone NAME` | 克隆或接续已登记项目，配置 remote 并初始化 twig |
| `twig init [--name NAME] [--base BRANCH] [--target BRANCH] [--root PATH]` | 保存项目配置、创建主 Session 并自动登记 |
| `twig new NAME [--base BRANCH]` | 创建或接续任务环境，不自动启动任何 AI 工具 |
| `twig ls` | 列出 twig 登记的任务、实际 Git 状态与窗口状态 |
| `twig pr [NAME] [--push] [--remote REMOTE] [--target BRANCH] [--provider github\|gitlab]` | 联网查找或创建草稿 PR/MR；推送必须显式指定 |
| `twig open NAME` | 进入窗口；缺失时重建 Shell，不恢复进程或对话 |
| `twig stop NAME [-y]` | 确认后关闭窗口并终止其进程，保留代码和分支 |
| `twig rm NAME [--target BRANCH] [--keep-branch] [-y]` | 检查安全条件后移除任务 |

重复 `init` 只提示已初始化并退出，重复 `new` 复用已登记资源，重复 `stop`/`rm` 不会创建资源。不会接管外部已有的同名分支或 Worktree。修改已有初始化配置需要编辑下述 `config.json`，不是重新传 `init` 参数。

`ls` 中 `Open` 只表示窗口存在，不表示 Codex 正在运行。`Missing` 表示 Worktree 未创建或已移除，`Mismatch` 表示目录丢失、分支变化等需要人工检查的状态。

## 安全清理

推荐从主仓库的其他终端执行：

```bash
twig stop auth                    # 会确认终止窗口内的全部进程
# 自行审查、合并和推送代码
twig rm auth
twig rm payment --keep-branch     # 允许保留尚未合并的分支
```

默认删除要求：

- Worktree 分支和仓库归属仍与登记一致。
- 无未提交、未跟踪或被忽略的文件。构建产物也会阻止删除，请自行检查和清理；`ls` 的 Clean 遵循普通 Git status，不包含 ignored 文件。
- 分支提交是目标分支的祖先。Squash/rebase 合并后即使内容相同，也可能被拒绝；可用 `--keep-branch` 保留分支再人工处理。
- 配置 upstream 时，相对本地 upstream 引用没有额外提交。此检查不连接远端；远端状态的新鲜程度取决于你最近的 fetch/push。无 upstream 时明确提示无法判断推送状态，但允许删除已合并到本地目标的分支。
- 所有 Pane 都是无子进程的已知 Shell，或已经退出。前台/后台任务及未知进程状态都会阻止删除。请先退出任务，或明确执行 `stop`。

`--keep-branch` 跳过合并和推送检查，仍检查工作目录和活动进程。`-y` 仅跳过确认，不绕过安全检查。始终使用 `git worktree remove` 和 `git branch -d`，不提供强制删除开关。

删除顺序为窗口、Worktree、分支、任务记录。若 Git 拒绝某一步，已完成步骤不会撤销，其余资源和记录保留，错误会说明下一步。Git 的 `branch -d` 自身还会执行检查；例如 `--target` 与当前主工作目录分支不同，可能在 Worktree 移除后拒绝删除分支，此时可用 `rm --keep-branch` 完成记录清理。

## 状态、恢复与限制

用户级项目登记表位于 `~/.twig/config.toml`，写入时使用同目录的 `config.lock`。每个项目的配置和任务状态仍位于 `git rev-parse --git-common-dir` 对应目录的 `twig/` 下：

```text
twig/
├── config.json                  # version/main/root/base/target/session
├── state.json                   # 任务名 -> branch/base/path
└── lock                         # 命令运行期间的项目互斥锁
```

主目录和全部关联 Worktree 共享配置，文件不会进入 Git 提交。Session 名为 `twig-<仓库路径哈希>`，并通过 `@twig-repo` 验证归属。窗口通过 `@twig-feature` 标记和 tmux Window ID 识别，支持数字任务名和手动重命名窗口，不依赖固定编号。不同项目即使目录名相同也能并存。

- SSH 断线不影响 tmux server；重连后用 `twig open NAME`。未进行真实 SSH 断网测试。
- 窗口丢失：`open` 或 `new` 重建 Shell；程序和对话由用户自己恢复。
- 创建 Worktree 成功但 tmux 失败：保留代码与记录，修复原因后重复 `new NAME`。
- Worktree 创建本身失败：保留创建意图，排除冲突后重复 `new NAME`，或用 `rm NAME --keep-branch` 清理记录。
- Window 创建成功但归属标记设置失败：同名窗口不会被盲目接管，检查后手动重命名或关闭它，再重试。
- 手动删除目录：`ls` 报告 Mismatch。先检查 `git worktree prune --dry-run`，确认丢失目录不需要恢复后自行 prune，再用 `new` 重建；twig 不自动 prune。
- tmux server 丢失或机器重启：环境信息保留时可重建窗口，不恢复原进程。容器销毁后能否恢复取决于仓库、配置和程序历史是否持久化。
- 异常退出遗留锁：确认没有其他 twig 命令运行后，手动删除错误提示中的 lock 文件。锁只协调 twig，无法阻止其他程序同时修改 Git 或启动进程；清理期间应停止其他写入。
- 主工作目录或仓库迁移、裸仓库、非 UTF-8 路径不在 MVP 支持范围内。
- Git Worktree 不隔离端口、数据库、Git 配置、hooks 和外部缓存，需要用户自行安排。
- 尚未提供 `status`、`doctor`、TUI、跨 Fork PR/MR 或自动合并，这些不是已确定的实现计划。Codex `resume` 不在当前环境管理范围内。

## 开发与验证

运行 `make check`，依次执行以下检查，任一步失败即停止：

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

集成测试创建临时 Git 仓库，通过独立 socket 和空配置启动 tmux server，不连接日常 tmux Session。测试结束后关闭测试 server。覆盖完整生命周期、路径空格和引号、重复创建、跨 Worktree 识别、窗口与 server 恢复、部分创建失败、外部分支/窗口冲突、数字任务名、窗口改名、确认取消、确认期间新增文件保护、未提交/ignored/未合并/未推送保护、前后台进程保护及手动删除目录后的修复。

登记和克隆测试使用独立配置文件与本地临时 Git remote，覆盖多个 remote、自定义默认 remote/主分支、任意目录操作、重复克隆、克隆后初始化失败重试、目录及 URL 冲突、默认 remote 删除保护、配置错误不覆盖，以及取消登记后保留仓库。不读写真实 `~/.twig/config.toml`，未进行真实托管平台的网络认证测试。

跨 Session 切换通过真实 tmux control-mode 客户端验证。普通终端 attach 需要 TTY；自动化测试验证非交互失败时已重建窗口，真实终端 attach 的显示与键盘交互仍需手动验收。

补全测试需要 Zsh（含 `zpty`、`zselect` 模块），使用临时登记表和 Git 仓库，覆盖命令/选项/路径、项目/任务/remote/本地分支、跨 Worktree、全局参数位置和等号形式、缺失/损坏配置的只读行为，以及生成脚本在 Zsh 中的加载与调用。真实 Tab/ZLE 测试验证任务优先展示和选中、前缀过滤及带空格目录补全。

PR/MR 测试使用模拟 `gh` / `glab` 和本地临时 bare remote，真实执行本地 push/ls-remote，覆盖两平台参数、重复调用、upstream 映射、remote 选择、自建域名、正文保真、错误和非快进推送保护。测试不创建真实 PR/MR、不使用托管平台凭据；实际平台认证与创建仍待实机验证，需要安装支持上述参数的 `gh` / `glab`。
