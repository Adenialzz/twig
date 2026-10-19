# twig

用 Git Worktree 和 tmux 管理独立任务环境的轻量 Rust CLI。

一个项目对应一个 tmux Session，一个任务对应一个分支、Worktree 和窗口。默认打开普通 Shell；可显式配置任务窗口的分屏和启动程序。twig 不编排 Agent，不自动合并，也不运行后台服务。

继续开发前请阅读 [AGENTS.md](AGENTS.md) 和 [DESIGN.md](DESIGN.md)，了解开发约定、设计决定、候选功能与交接状态。

## 安装

需要 Rust/Cargo、Git、tmux 和 Unix `ps`。当前在 macOS 上使用 Rust 1.93.1、Git 2.50.1、tmux 3.6a 验证；Linux 尚待实机验证。

```bash
cd /path/to/twig
make install
twig --help
```

`make install` 只安装或更新 twig，不检查或安装 `gh`、`glab`，也不依赖 Homebrew。成功后打印 Zsh 补全和平台工具安装、登录提示，不修改 Shell 配置、不自动登录。使用 PR/MR 前按需自行安装 `gh` 或 `glab`，再运行 `gh auth login` / `glab auth login`。

不使用 Make 时，可以直接执行 `cargo install --path . --locked`。确保 `~/.cargo/bin` 在 PATH 中。支持从 Bash、Zsh 调用；普通命令无需修改 Shell 配置，Tab 补全按下节启用。

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
| `twig ls ` / `new ` / `open ` / `stop ` / `rm ` / `pr ` | 已登记项目别名 |
| `twig open myapp ` / `new myapp ` / `stop myapp ` / `rm myapp ` / `pr myapp ` | 该项目的已有任务；`new` 仍可输入新名称 |
| `twig new myapp auth --base `、`twig rm myapp auth --target ` | 该项目已有本地分支，不含远端跟踪分支 |
| `twig add myapp --path /repo --base ` / `--target ` | 指定路径的已有本地分支 |
| `twig pr myapp auth --remote ` / `--provider ` | Git 实际 remote / 支持的平台 |
| `--config `、`--path `、`--root ` | 文件或目录路径 |

动态候选按输入前缀过滤，识别光标前的项目别名和 `--config PATH`（也支持 `--config=PATH`）。从任意目录操作同一别名，任务候选相同。

带空格的配置路径可使用单引号、双引号或反斜杠转义。目录补全支持空格、冒号及 `--root=PATH` 写法，保留末尾 `/`，可以继续输入并补全子目录。

`twig open myapp <Tab>`（以及 `new`、`stop`、`rm`）先展示任务名，再展示带说明的选项；Tab 选择也优先任务名。任务按名称字典序排列。输入 `twig open myapp a<Tab>` 只匹配该前缀的任务，输入 `twig open myapp --<Tab>` 只显示选项。升级后在当前 Zsh 重新执行 `source <(COMPLETE=zsh twig)` 即可加载新的展示规则。

补全只读本地数据，不联网、不创建配置或锁、不调用 tmux，也不执行正在输入的任务命令。未初始化、未登记或路径缺失的项目可能没有相应候选；配置损坏时不会修改或覆盖文件。

## 全局 Console（控制台）

完整操作说明见 [Console 使用指南](docs/console-usage.md)，包含项目登记、界面操作、状态含义、历史查询、快捷键及常见问题。

在任意目录执行 `twig` 或 `twig console`，浏览全局登记的项目和任务，查看 Git、tmux 与 Codex 会话状态，并进入相应 Shell。

```bash
twig                          # 全局面板；按当前目录定位项目/任务
twig --config ./config.toml    # 指定登记表
```

宽终端左侧为项目列表，右侧从上到下依次为任务列表、Codex 会话列表、详情，共四个可见区域。`Tab` 只在项目、任务、会话三个列表之间切换；详情不单独获取焦点，用 `PgUp/PgDn` 滚动。窄终端按焦点切换页面，会话页同时显示列表与详情。项目内包含「主工作目录」和已登记任务，项目/任务按名称稳定排列。主窗口和任务窗口缺失时，Enter 按现有 `open` 语义准备 Shell，再进入；缺失 Worktree、错误归属和同名窗口冲突会明确报错。

| 按键 | 操作 |
| --- | --- |
| `↑/↓` 或 `j/k` | 选择项目、任务或会话 |
| `gg` / `G` | 跳到当前列表顶部 / 底部（尊重搜索筛选） |
| `Tab` / `Shift+Tab` | 切换区域；窄终端切页 |
| `Ctrl-W` 后按 `h/j/k/l` | 按方向切换项目、任务、会话列表 |
| `Enter` | 项目区切到任务列表并选中主工作目录，任务区进入 Shell，会话区展开最近轮次 |
| `/`、`Esc` | 搜索并显示命令候选；输入中取消并恢复原选择，输入结束后清空筛选/返回 |
| `h` | 切换已加载会话与历史会话 |
| `n` | 项目区登记已有仓库；任务区新建任务；会话区历史下一页 |
| `a` | 历史查询切换包含子目录；逐页扫描并显示进度 |
| `PgUp/PgDn` | 滚动详情 |
| `r`、`?`、`q` / `Ctrl+C` | 刷新、帮助、退出 |

在项目区按 `n`，填写已有 Git 仓库路径和别名，未初始化时自动初始化，已初始化时补登记；同路径复用、别名冲突不覆盖，不要求 remote。在任务区按 `n`，填写任务名及起点分支（默认项目配置，允许填写其他已有本地分支）。登记表单在路径字段按 `Enter` 校验路径并自动填入目录名作为别名，再按 `Enter` 提交；别名可修改。任务表单按 `Enter` 提交。`Tab` 切字段、`Esc` 取消，长错误用 `PgUp/PgDn` 滚动。项目路径输入支持本地目录候选：上下选择，有候选时 `Tab` 补全并保留 `/`，无候选时切字段；`Shift+Tab` 始终切字段。支持 `~/`、相对路径和空格路径，不创建目录。

创建成功后选中新项目/任务并留在控制台，按 `Enter` 再进入 Shell。执行期间显示进度提示，等待操作结束，不强制取消；失败保留输入和已创建资源，修正后可重试。新建空仓库与远程克隆暂未纳入表单。

从普通终端进入 tmux 后，detach 会返回 Console，保留项目、任务选择和焦点；从 tmux 内进入时切换到目标窗口，Console 留在原窗口，切回即可继续使用。退出面板保留原有 Shell 和 Agent。非交互环境明确报错，脚本继续使用 `ls`、`ls myapp` 等子命令。

`/` 默认实时搜索当前列表，同时显示 `/new`、`/delete`、`/refresh`、`/help`、`/quit` 的前缀匹配候选，不默认选中。用上下键选择、`Tab` 补全、`Enter` 执行选中项；没有选中时，只有完整关键字触发命令，否则确认搜索。`//new` 强制搜索文本 `new`。`Esc` 取消输入，命令执行和取消都会恢复输入前的筛选与选择。`/new` 按原焦点打开项目登记或任务表单，会话区只提示切区。

**Codex 为可选观察功能**：需要 PATH 中的 Codex CLI 和已经运行的本机 app-server，首个验证版本为 0.163.0。面板通过 `codex app-server daemon version` 发现 socket，使用 Unix socket 上的 WebSocket 只读查询；不启动服务、不恢复会话、不提交审批或提示词，也不读取 Codex 私有数据库。基础项目导航不依赖 Codex 是否可用。

一个目录可对应多个会话，按路径组件和最深工作目录关联，支持从子目录启动。状态区分运行中、等待审批、等待输入、空闲、当前服务未加载、会话错误和未知。空闲不是任务完成；最近轮次的完成/失败/中断单独展示。CLI/VS Code 来源标签不代表当前所在 Pane，选择会话只查看详情。

默认隐藏 Codex 标记为 `ephemeral=true` 的临时会话，已加载列表、历史列表和状态统计均不包含它们；不会删除或关闭这些会话。

历史查询默认精确匹配工作目录；`a` 扩大到子目录后，使用 `n` 继续扫描。列表使用数据库只读查询，不触发历史文件修复；源数据尚未登记到 Codex 数据库的记录可能不可见。只观察当前服务可见会话，其他服务或 `codex --no-daemon` 实例不保证能显示实时状态。多 Pane 会话不按目录猜测对应 Pane。

项目/任务记录约每 10 秒、tmux/Codex 约每 2 秒、选中项目 Git 约每 5 秒刷新（间隔从上一轮完成后计算）。浏览时单个查询有 2 秒超时，不堆积请求；错误和过期结果明确标记。历史与最新轮次按需读取，`r` 可更新已展开的轮次。会话创建、更新时间以北京时间（UTC+8）显示。Git「干净」不包含 ignored 文件，也不表示可以安全删除。

浏览不创建登记表、配置、锁或窗口，不执行 fetch/pull/push。空登记表显示初始化提示；坏项目和 Codex 断连不阻止浏览其他项目。完整设计与验证记录见 [Console 方案](docs/console.md)。

正常刷新时不显示秒数；Git 或 Codex 状态超过 30 秒未成功更新时显示固定的「状态过期」提示，查询失败时直接提示异常。可按 `r` 刷新。

任务列表使用带表头的对齐表格；区域较窄时将任务/分支、Git/窗口分别合并为两行，保留 Codex 列。过长内容在列内截断。

## 使用

### 登记项目与创建任务

所有项目名都是登记别名，不要求与目录名或远程仓库名相同。任务命令必须显式指定项目，不按当前目录推断；`ls` 不带项目列出项目，带项目列出该项目的任务。

```bash
# 在任意目录登记已有、至少有一个提交的 Git 仓库
twig add myapp --path ~/projects/myapp
twig ls                           # 列出项目
twig ls myapp                     # 列出项目中的任务
twig new myapp auth               # 创建任务并进入 Shell
twig new myapp search --no-open    # 只创建环境
twig open myapp auth              # 进入已有任务
# 在窗口中手动运行 codex 等程序
```

`add` 同时初始化项目环境并登记路径，首次初始化优先使用本地 `main`，其次 `master`。不会创建仓库或自动克隆，不要求 remote；需要远程仓库时先执行 `git clone`，remote 也通过 Git 自行管理。

```bash
git clone git@github.com:yourname/myapp.git ~/projects/myapp
twig add myapp --path ~/projects/myapp

# 首次初始化时自定义；相对路径基于命令调用目录
twig add myapp --path ~/projects/myapp --base develop --target main --root ../my-worktrees
twig new myapp feat/login --base auth
twig new myapp fix/bug
```

项目别名为 1–64 位 ASCII 字母、数字、横线或下划线，首位须为字母或数字。`--path` 必填，支持 `~/`、绝对路径和相对路径；关联 Worktree 或子目录会定位到主仓库。同路径复用已有别名，不新建别名；别名指向其他仓库时拒绝覆盖。

已初始化时 `add` 复用项目配置并补充登记，不重建窗口；传入冲突的基础/目标分支或重新指定根目录会报错。初始化失败保留资源，修复后可再次 `add` 补登记，再用 `new`/`open` 或控制台 Enter 准备窗口。修改已有项目配置请编辑 `config.json`。

用户配置默认位于 `~/.twig/config.toml`，其中 `[projects]` 保存别名和本地路径，也可配置 `[tmux]` 启动行为：

```toml
[projects.myapp]
path = "/path/to/myapp"
```

手动编辑时路径须为绝对路径或 `~/` 开头。命令写入会重新序列化 TOML，不保留手写注释。基础分支、合并目标和 Worktree 根目录保存在项目配置中，remote 以 Git 为准。

```bash
twig rm myapp                     # 仅取消项目登记，保留仓库、任务、窗口和状态
twig rm myapp auth                # 安全删除具体任务
```

取消登记后，可用 `add` 重新登记并继续访问已有任务。任务删除选项如 `--keep-branch`、`--delete-ignored`、`--target` 必须同时指定任务名，不能用于取消项目登记。

通过 `twig --config /path/to/config.toml ...` 选择其他登记表。登记表所在目录也是数据根目录，不同目录隔离项目配置与任务记录，同目录的登记表共享状态；Git、Worktree、tmux 等外部资源仍共享。

默认目录结构无需移动原仓库：

```text
projects/
├── myapp/                        # 原仓库
└── myapp.worktrees/
    ├── auth/
    └── search/
```

Worktree 根目录必须与主工作目录分离，不能相互包含。`--base` 必须为已有本地分支；`--target` 用于安全删除的合并检查，默认等于基础分支。不自动 fetch、pull、push 或修改主分支。

任务名直接作为分支名，不自动添加前缀。总长为 1–64 位，可用 `/` 分段；每段为 ASCII 字母、数字、横线或下划线，并以字母或数字开头，不允许空段。`feat/login` 对应 `<root>/feat/login/`，tmux 窗口名也是 `feat/login`；`main` 保留给主窗口。已有任务使用登记的分支与路径，不自动改名。

### 任务窗口初始化

完整字段、示例及交互说明见 [任务窗口启动配置](docs/tmux-startup.md)。

在主仓库根目录的 `.twig/config.toml` 中配置项目启动方式，或在用户级 `~/.twig/config.toml` 中配置默认值。指定 `--config PATH` 时，PATH 同时替代用户配置和登记表，不再读取默认用户文件。

```toml
[tmux]
panes = [
  { focus = true },
  { split = "right", percent = 35, command = ["./scripts/dev"] },
  { split = "down", percent = 40 },
]
```

首个 Pane 不设置 split/percent。后续 Pane 从上一个 Pane 分割，`right` 表示右侧、`down` 表示下方；`percent` 是新 Pane 占分割前空间的百分比，默认 50，范围 1–99。最多一个 `focus = true`，默认聚焦首个 Pane。

`command` 为程序与参数数组。Pane 的 `mode` 默认为 `shell`，启动 tmux 的 `default-shell` 登录 Shell；配置了 command 时，将其作为 Shell 的第一条命令执行。显式 `mode = "process"` 时必须提供 command，直接运行程序，退出后保留 dead Pane、输出和退出码；不加载交互式 Shell 配置。所有 Pane 的工作目录都是任务 Worktree。参数逐个引用，不展开 `$VAR`、`~`、管道或 `&&`；复杂逻辑请写入项目脚本，脚本须在任务分支中存在。显式配置也可启动 Codex/Claude 等工具，但 twig 不恢复对话、不发送提示词或审批、不自动重启程序。

项目文件始终从已登记项目的主仓库读取，不按调用目录或任务分支寻找。项目定义 `[tmux]` 时整体覆盖用户配置，不合并 Pane；未定义则继承用户配置。`[tmux]` 下 `panes = []` 可显式恢复单 Shell。配置错误会报错。

仅在创建任务窗口时应用，包括窗口缺失后的 `new`、`open` 和 Console Enter；已有窗口不重排、不重复执行命令。`--no-open` 仍创建窗口并启动配置的命令，只是不进入窗口。`add` 创建的 `main` 窗口保持原有普通 Shell。

shell 模式中，命令完成、失败或被 Ctrl-C 中断后回到 Shell 提示符，可以继续操作，输出保留在终端历史中。空提示符下 Ctrl-D 按 Shell 的正常行为退出并关闭 Pane，不留下 dead Pane；最后一个 Pane 关闭时窗口关闭。process 模式由程序处理 Ctrl-C/Ctrl-D，程序退出后没有 Shell 接手，Ctrl-D 不再起作用。同一窗口可混用两种模式。窗口初始化中途失败保留资源，后续进入会提示检查，可手动修复后继续使用 tmux，或显式 `twig stop PROJECT TASK` 再 `open` 重建。运行中的命令仍受 stop 确认和 rm 活动进程检查保护。

快捷键、鼠标、外观继续由 `~/.tmux.conf` 管理。这里不执行 Worktree 一次性准备，也不自动复制文件或安装依赖。用户显式配置的程序可能联网或产生其他副作用，由该程序决定。

### 提交 GitHub PR / GitLab MR

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

## 命令

| 命令 | 行为 |
| --- | --- |
| `twig` / `twig console` | 全局项目、任务与 Codex 状态面板 |
| `twig add PROJECT --path PATH [--base BRANCH] [--target BRANCH] [--root PATH]` | 登记并初始化已有仓库 |
| `twig ls [PROJECT]` | 不带项目列出项目，带项目列出任务及实际状态 |
| `twig new PROJECT NAME [--base BRANCH] [--no-open]` | 创建或接续任务环境，默认进入 Shell |
| `twig open PROJECT NAME` | 进入任务窗口；缺失时按启动配置重建 |
| `twig stop PROJECT NAME [-y]` | 确认后关闭任务窗口及进程，保留代码和分支 |
| `twig rm PROJECT` | 取消项目登记，保留所有资源和项目状态 |
| `twig rm PROJECT NAME [--target BRANCH] [--keep-branch] [--delete-ignored] [-y]` | 安全移除指定任务 |
| `twig pr PROJECT NAME [--push] [--remote REMOTE] [--target BRANCH] [--provider github\|gitlab]` | 查找或创建草稿 PR/MR，推送必须显式指定 |

旧 `init`、`config`、`clone`、`--repo` 已移除，不提供兼容别名；旧登记表中的 `main`、`default_remote` 和 `remotes` 字段也已移除。重复 `new` 接续已有任务，重复任务 `stop`/`rm` 不创建资源；不会接管外部已有的同名分支或 Worktree。

`new` 默认与 `open` 一样进入任务窗口：普通终端 attach，在 tmux 内切换客户端；重复 `new` 也会进入已有窗口，不重复执行启动命令。脚本或批量创建时使用 `--no-open`。若进入失败（例如非交互终端没有 TTY），已创建的环境保留，可在交互式终端执行 `twig open PROJECT NAME`。

`ls` 中 `Open` 只表示窗口存在，不表示 Codex 正在运行。`Missing` 表示 Worktree 未创建或已移除，`Mismatch` 表示目录丢失、分支变化等需要人工检查的状态。

## 安全清理

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

`--keep-branch` 跳过合并和推送检查，仍检查工作目录和活动进程。`-y` 仅跳过确认，不绕过安全检查。始终使用 `git worktree remove` 和 `git branch -d`，不提供强制删除开关。

删除顺序为窗口、Worktree、分支、任务记录。若 Git 拒绝某一步，已完成步骤不会撤销，其余资源和记录保留，错误会说明下一步。Git 的 `branch -d` 自身还会执行检查；例如 `--target` 与当前主工作目录分支不同，可能在 Worktree 移除后拒绝删除分支，此时可用 `rm --keep-branch` 完成记录清理。

## 状态、恢复与限制

用户级项目登记表位于 `~/.twig/config.toml`，写入时使用同目录的 `config.lock`。每个项目的配置和任务状态位于登记表同目录的 `projects/<仓库 ID>/` 下，默认布局为：

```text
~/.twig/
├── config.toml                  # 项目登记表
├── config.lock                  # 登记表写入锁
└── projects/
    └── <仓库 ID>/
        ├── config.json          # version/main/root/base/target/session
        ├── state.json           # 任务名 -> branch/base/path
        └── lock                 # 命令运行期间的项目互斥锁
```

仓库 ID 为规范化后的共享 Git 目录绝对路径的稳定哈希。主目录与关联 Worktree 使用同一份状态，同一远程仓库的不同本地克隆独立存储；修改登记别名不改变 ID。读取配置时核对仓库归属。

这是不兼容的存储变更：不读取或迁移旧 `.git/twig/` 数据，也不删除旧数据及现有分支、Worktree、tmux 资源。重新初始化不会接管已有任务资源。取消登记不删除状态，删除仓库后的残留状态也不自动清理；仓库移动后的自动修复仍不支持。

主目录和全部关联 Worktree 共享配置，文件不会进入 Git 提交。Session 名为 `twig-<仓库路径哈希>`，并通过 `@twig-repo` 验证归属。窗口通过 `@twig-feature` 标记和 tmux Window ID 识别，支持数字任务名和手动重命名窗口，不依赖固定编号。不同项目即使目录名相同也能并存。

- SSH 断线不影响 tmux server；重连后用 `twig open PROJECT NAME`。未进行真实 SSH 断网测试。
- 窗口丢失：`open` 或 `new` 按启动配置重建；不恢复之前的进程状态或对话。
- 创建 Worktree 成功但 tmux 失败：保留代码与记录，修复原因后重复 `new NAME`。
- Worktree 创建本身失败：保留创建意图，排除冲突后重复 `new NAME`，或用 `rm NAME --keep-branch` 清理记录。
- Window 创建成功但归属标记设置失败：同名窗口不会被盲目接管，检查后手动重命名或关闭它，再重试。
- 手动删除目录：`ls` 报告 Mismatch。先检查 `git worktree prune --dry-run`，确认丢失目录不需要恢复后自行 prune，再用 `new` 重建；twig 不自动 prune。
- tmux server 丢失或机器重启：环境信息保留时可重建窗口，不恢复原进程。容器销毁后能否恢复取决于仓库、配置和程序历史是否持久化。
- 异常退出遗留锁：确认没有其他 twig 命令运行后，手动删除错误提示中的 lock 文件。锁只协调 twig，无法阻止其他程序同时修改 Git 或启动进程；清理期间应停止其他写入。
- 主工作目录或仓库迁移、裸仓库、非 UTF-8 路径不在 MVP 支持范围内。
- Git Worktree 不隔离端口、数据库、Git 配置、hooks 和外部缓存，需要用户自行安排。
- 尚未提供独立 `status`、`doctor`、跨 Fork PR/MR 或自动合并，这些不是已确定的实现计划。Codex `resume` 不在当前环境管理范围内。

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

### 从控制台删除任务

任务区选中任务后输入 `/delete` 或按 `dd`，查看项目、分支和 Worktree 路径，再确认删除。默认取消；Tab/左右键切换确认，↑↓ 或 j/k 选择选项，空格勾选/取消，Esc 取消。复用 `twig rm` 的文件、进程、合并和 upstream 检查，无强制删除；保留分支时跳过合并与推送检查。主工作目录不可删除。成功后选中主工作目录，详情见 [删除任务说明](docs/console-usage.md#删除任务)。

删除 Worktree 默认保护 ignored 文件。确认不再需要后，可使用 `twig rm PROJECT NAME --delete-ignored`，或在控制台删除弹窗查看清单并用 ↓ 或 j 选中「同时删除被忽略文件」，按空格开启。目录项包含其全部内容（例如 `.venv/`），不限于缓存。未提交、未跟踪文件及活动进程仍阻止删除；不使用 Git 强制删除。
