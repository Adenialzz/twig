# twig 设计决定与开发交接

本文记录截至 2026-10-18 已确认的需求、实现边界和工程状态，供后续独立会话接续。使用方法以 [README.md](README.md) 为准，开发约定见 [AGENTS.md](AGENTS.md)。工程状态是交接时的快照，后续修改前应重新核实。

## 目标与需求演变

用户希望在同一项目里并行开展多个任务，用 Git Worktree 隔离工作文件和分支，用 tmux 保持长期终端任务，并支持 SSH 断线后重新连接。任务不限于新功能，也可以是修复、重构或实验。

最初方案暂名 `feat`，原始需求位于另一个仓库 `dotfiles/doc.md`，曾设想自动启动并恢复 Codex。讨论后用户明确选择了以下最终方案：

- 名称改为 `twig`；可执行命令为 `twig`，Cargo 包名为 `twig-cli`。
- 使用 Rust。起初曾建议 Go，以减少实现成本；用户希望尝试 Rust，因此采用 Rust，不代表性能需求迫使选用它。
- 在 `/Users/song/JJ_Projects/twig` 独立建仓库。dotfiles 主要管理配置和轻量脚本，twig 有独立的构建、测试和迭代需求，不嵌入 dotfiles，也未添加子模块。
- 不自动启动 Codex。`new`、`open` 都只准备 Shell，用户自行运行 Codex、Claude 或其他程序；不依赖 Codex Desktop 或 Codex CLI。
- 删除采用保守保护，允许保留分支；没有自动合并、强制删除或远程 PR 自动处理。
- 增加用户级常用项目登记表，支持远程 URL、多个 remote、默认克隆来源、主分支和本地路径；通过显式 `clone` 创建环境。

原始 `doc.md` 是历史背景，不是高于本文件的待实现清单。尤其不要重新引入默认启动 Codex、Codex 状态列或对话恢复。

## 已确认的产品边界

一个项目对应一个 tmux Session；一个任务对应一个 `feature/<name>` 分支、Worktree 和窗口。默认一个 Pane，用户可以自行分屏。创建新任务不触碰其他任务的工作文件或窗口。

工具负责环境生命周期，不负责 Agent 编排、任务拆分、Agent 间通信、自动调用 LLM、自动合并或部署。实现保持同步执行，无后台守护进程。

普通任务命令不联网，不隐式 fetch、pull 或 push。创建起点必须是已有本地分支；基础分支和合并检查目标独立，例如任务可从 `feature/auth` 开始，但最终检查是否合并到 `main`。

remote 名称和 URL 由 Git 实际配置管理。用户级登记表提供初始化信息，不是持续同步服务：

- `config` 命令只读写登记表，既不克隆，也不改已有仓库的 remote。
- `clone` 使用默认 remote，检出登记的主分支，添加其他 remote，再初始化 twig；其他 remote 不自动 fetch。
- 重试 `clone` 时核对现有目录、remote URL、主分支和 twig 配置，不覆盖冲突；可添加缺少的 remote，不删除额外 remote，也不更新已有分支。
- `default_remote` 用于克隆来源，不是所有分支的统一推送目标。分支的 pull/push upstream 继续由 Git 管理。
- 不带 `--repo` 时按当前目录识别仓库；带 `--repo NAME` 时从登记表解析路径，允许在任意目录操作。

## 架构与事实来源

一个 Cargo 包，依赖 `clap`、`clap_complete`、`serde`、`serde_json`、`toml` 和 `anyhow`；测试使用 `tempfile`。通过 `std::process::Command` 调用 Git、tmux、ps，参数分别传入。错误补充操作上下文，失败不静默降级。

Makefile 仅封装 Cargo 命令：`make build`（默认目标）构建 release，`make install` 安装或更新并打印 Zsh 补全配置提示，`make check` 依次执行格式检查、Clippy 和测试。不自动修改用户 Shell 配置。

| 文件 | 职责 |
| --- | --- |
| `src/main.rs` | CLI 路由、项目初始化、任务创建/进入/停止/清理流程 |
| `src/completion.rs` | 复用 CLI 定义解析补全上下文，只读查询项目、任务、分支和 remote 候选 |
| `src/completion.zsh` | Zsh 候选展示：普通值与选项分别分组，普通值优先 |
| `src/registry.rs` | 用户级 TOML 登记表、路径解析、remote 登记和 clone |
| `src/config.rs` | 共享 Git 目录中的项目配置、任务记录、锁、名称和 Session 标识 |
| `src/git.rs` | 外部命令执行及 Git 分支、合并关系、Worktree 查询 |
| `src/tmux.rs` | Session/Window 归属、窗口恢复、进程检查、进入和切换 |
| `tests/workflow.rs` | 临时仓库及独立 tmux server 上的集成测试 |
| `tests/completion.rs` | 临时登记表与仓库中的补全协议测试，以及 Zsh 脚本加载验证 |

配置与状态分为两层：

1. **用户登记表**：默认 `~/.twig/config.toml`，可通过 `--config` 覆盖。保存项目别名、路径、主分支、默认 remote 和 remote URL 映射；首次写入时创建。不是任务状态数据库。
2. **项目配置与任务记录**：位于共享 Git 目录的 `twig/config.json` 和 `twig/state.json`，主工作目录与全部关联 Worktree 共用。项目配置保存主目录、Worktree 根目录、基础/目标分支、Session；任务记录保存名称、分支、基础分支和路径。

Git 查询决定 Worktree、分支、修改和合并的实际状态；tmux 查询决定窗口是否存在。记录只补充归属和创建意图，不替代实际检测。

Session 名由仓库共享 Git 目录的绝对路径哈希生成，并使用 `@twig-repo` 标记归属，避免同名项目冲突。窗口通过 `@twig-feature` 标记和 Window ID 定位，不依赖固定编号；数字任务名和手动改窗口名有测试覆盖。

文件写入采用临时文件再 rename；创建排他锁文件协调 twig 命令。它不是跨 Git/tmux 的事务，也不能阻止用户或其他程序同时修改资源。异常退出遗留锁需人工确认后删除，不自动猜测锁是否失效。

## 生命周期与安全决定

- `new` 先记录创建意图，再创建分支/Worktree/窗口。失败时报告部分成功，保留可诊断、可接续的资源。选择保留而不是强制回滚，避免误删已产生的工作。
- 重复 `new` 复用自己登记的资源，不自动接管外部已有的同名分支或窗口。
- `open` 复用窗口，窗口缺失时重建 Shell；不承诺恢复终端进程或 AI 对话。
- `ls` 的窗口存在状态不能被解读为 Codex 正在运行。
- `stop` 会终止窗口内进程，需确认；它不删除 Worktree 或分支。
- `rm` 默认拒绝未提交、未跟踪、ignored 文件、未合并分支和仍有活动进程的窗口；确认后再检查，以防等待期间发生修改。
- 有 upstream 时，推送检查基于本地 upstream 引用；没有 upstream 时提示无法判断，但允许删除已合并到本地目标的分支。此处没有远端实时保证。
- `--keep-branch` 保留分支，跳过合并/推送检查，仍保护目录和活动进程；`-y` 只跳过确认。
- 删除按窗口、Worktree、分支、任务记录执行，使用 Git 自带的安全删除命令。中途失败保留剩余资源和记录，不默认使用 `branch -D` 或 `worktree remove --force`。

进程检查是对已知 Shell 和子进程的保守检测，不是通用任务运行状态协议。清理期间仍应停止其他写入。Worktree 不隔离端口、数据库、共享 Git 配置、hooks 或外部缓存。

## 已完成与未承诺的功能

已完成：`init`、`new`、`ls`、`open`、`stop`、`rm`、`config add/list/show/remove`、`config remote add/remove/default`、`clone`、全局 `--repo` 和 `--config`，以及 Zsh 动态补全。具体参数与限制见 README。

补全采用 `clap_complete` 的 `unstable-dynamic` 接口，复用现有命令定义，在普通命令解析与执行之前分流。Zsh 通过 `source <(COMPLETE=zsh twig)` 加载；因上游协议尚未稳定，Shell 每次启动时重新生成接入脚本，不保存长期静态副本。上下文只解析光标前的完整参数，支持全局参数位于子命令前后以及等号写法。项目名和 remote 来自登记表，任务来自共享状态，本地分支通过 Git 查询。补全不获取锁、不写状态、不联网、不调用 tmux；查询失败输出 stderr 诊断并返回空候选，生成的 Zsh 脚本隐藏诊断以免打断输入。当前文档与验收范围为 Zsh，其他 Shell 尚未验证。

Zsh 展示通过自定义 `EnvCompleter` 注册脚本，候选生成与转义继续委托 Clap。普通值、目录和选项分组注册，确保任务先展示、先选中，避免 Zsh 将带说明的选项移到无说明的任务之前。无需设置全局 Zsh 样式。回归测试通过 Zsh `zpty` 驱动真实 Tab 和 ZLE，检查显示顺序及最终选中的输入，不能仅凭候选协议输出顺序判断菜单顺序。

传入 Rust 前使用 Zsh 的 `Q` 参数标志去除词中的引号和转义，不执行变量或命令替换。展示脚本仅将未转义的冒号视为说明分隔符，并先识别目录，再识别选项，避免 `--root=PATH/` 被追加空格。真实交互测试覆盖配置路径的三种引用形式及空格/冒号目录的连续两级补全。

下列事项仅是讨论过的候选，不是已授权的实现队列，也没有确定优先级：

- `status`：详细提交、文件变化、ahead/behind 等信息。
- `doctor`：诊断配置、Worktree 与 tmux 状态不一致。
- 交互选择、fzf、TUI、PR 状态集成。
- 允许 `--base origin/main`，以及显式 `--fetch`。当前均未实现，后续需先讨论语义。

原始需求中的 Codex `resume` 和 Turn 完成通知属于早期想法。由于最终不管理 Codex 生命周期，它们不在当前范围内，不能直接当作“第二阶段必做功能”。

Linux 实机、真实 SSH 断线重连、真实终端 attach 的键盘和显示、托管平台网络认证仍是待验证项。当前不支持裸仓库、非 UTF-8 路径、仓库迁移自动修复。Squash/rebase 合并可能无法通过祖先关系检查；`ls` 的 Clean 不含 ignored 文件，而删除检查包含它们。这些限制不应被隐藏，也不意味着要立即扩大功能范围。

## 交接快照：2026-10-18

- 已在 macOS 上使用 Rust/Cargo 1.93.1、Git 2.50.1、tmux 3.6a 构建和验证。
- 本次增加 Zsh 动态补全，并修正任务与选项的展示和选择顺序。通过 `make check` 执行格式检查、Clippy 和测试，5 组补全测试和原有 13 组工作流测试全部通过。
- 测试使用临时 Git 仓库、独立 tmux socket 和临时登记表。clone 测试用本地 Git remote，没有测试真实服务商认证。跨 Session 切换使用真实 tmux control-mode 客户端验证。
- 本次已构建 debug 二进制，验证带空格和引号的可执行文件路径、Zsh 脚本加载，以及真实 Tab/ZLE 的展示和选择顺序；未自动安装新版 twig 或修改用户 Shell 配置。启用补全前按 README 安装并重新加载配置。
- 本次在 `autocomplete` Worktree 开发，开始时工作区干净，基线提交为 `d15e3f9`；补全改动尚未提交。后续仍须重新检查 Git 状态。
- 未为用户创建或填入真实 `~/.twig/config.toml`。它由首次登记命令创建，项目 URL 和路径须由用户提供。
- 最初开发曾使用临时构建副本，但后续开发以本仓库为准，不依赖 `/tmp` 文件或上一段聊天上下文。

后续会话先读 AGENTS、README 和本文，检查工作区与用户当前目标，再讨论具体改动；不要按候选列表自行展开功能。更新功能时同步更新相应文档，并刷新已经过时的交接状态。
