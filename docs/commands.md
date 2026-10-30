# 命令参考

命令行阅读：`twig guide commands`。某条命令的参数速查使用 `twig <命令> --help` 或 `twig help <命令>`。

## 语法与名称

`PROJECT` 表示已登记的项目别名；`NAME` 表示任务名（add 的 NAME 是新项目别名）。`[参数]` 表示可选，`<参数>` 表示必填，这些括号不要输入。选项值含空格时按所在 Shell 的规则加引号。

- 项目别名：1–64 个 ASCII 字母、数字、横线或下划线，首位须为字母或数字。
- 任务名：总长 1–64 个 ASCII 字符，允许 `/` 分段，每段沿用上述规则；不允许空段，`main` 保留给主窗口。任务名直接作为新分支名。
- 所有命令支持 `--config PATH`：选择用户登记表及同目录项目状态，默认 `~/.twig/config.toml`。不按当前目录推断项目。
- `-h/--help` 输出帮助，顶层 `-V/--version` 输出版本。

## daemon 与 operation

普通任务 CLI 和 Console 始终本地执行，无需后台；Agent、受管任务及飞书需要显式启动 `twig daemon start`，通信失败不回退执行；`--config` 同时选择后台实例。`daemon restart` 有序重启手动后台并保留原环境；前台和托管实例使用原启动方式重启。`daemon run` 供前台调试和系统托管，`daemon status` 查询健康状态，`daemon stop` 等待已接受工作后退出，保留 tmux 与 Codex。

`twig operation ls` 列本地及后台的持久化操作，无需后台即可读取；本地操作 ID 以 `local-` 开头。`twig operation show ID` 可按操作或请求 ID 查询，`twig operation resolve ID --yes` 在人工核对资源后解除待核对阻塞，不重跑或回滚。响应丢失先按原 ID 查询，不重复发起。`TWIG_OPERATION_PROGRESS=1` 可显示业务阶段。详细恢复步骤见 [后台指南](daemon.md)。

## guide：阅读离线手册

```sh
twig guide
twig guide quickstart
twig guide --all
```

无主题时输出导航；指定主题只输出对应文档；--all 输出索引及全部主题，不能与主题同时使用。输出为 UTF-8 Markdown，不联网、不读取配置、不执行 Git/tmux，不要求终端。可自行管道给 less。

## add：登记并初始化

```sh
twig add myapp --path ~/projects/myapp
twig add myapp --path /repo --base develop --target main --root /worktrees/myapp
```

| 参数 | 含义与默认值 |
| --- | --- |
| NAME | 项目别名，必填 |
| --path PATH | 已有 Git 仓库路径，必填；支持 ~/、绝对及相对路径；子目录/关联 Worktree 定位到主仓库 |
| --base BRANCH | 首次初始化的已有本地基础分支；默认 main，其次 master |
| --target BRANCH | 安全删除时的本地合并检查目标；默认 base |
| --root PATH | Worktree 根目录；默认主仓库旁的 `<目录名>.worktrees`；相对路径基于调用目录 |

仓库须非裸且已有提交，root 与主工作目录不能相互包含。首次初始化准备配置和主窗口；已有配置时只复用并补登记，不重建窗口。同路径复用原别名，不添加第二个别名；别名指向其他仓库则拒绝。已初始化项目传入冲突 base/target 或重新指定 root 会报错。失败保留已创建资源，修正后可再次 add。

## ls：列出项目或任务

```sh
twig ls
twig ls myapp
```

无项目时按别名列出登记路径；指定项目时列出任务、分支、窗口、Git 状态与 Worktree 路径，不包含主工作目录作为任务。不会启动任务程序，不联网。项目任务查询使用项目锁。

Open/Closed 表示 tmux 窗口存在/不存在，不代表 Agent 运行状态；Clean/Dirty 是普通 Git 状态，不包含 ignored 文件；Missing 表示未创建或已移除的 Worktree；Mismatch 表示目录、分支或归属异常，应阅读 stderr 的具体原因。命令可能显示 Mismatch 后仍成功退出，不能只凭退出码判断所有任务健康。

## new：创建新任务

```sh
twig new myapp auth
twig new myapp feat/login --base develop --no-open
```

PROJECT 和 NAME 必填。`--base BRANCH` 选择已有本地基础分支，默认项目 base。新建任务分支前默认 fetch 所选 base 的远端 upstream，并从取得的远端提交创建；不更新本地 base 或主工作目录，也不包含本地 base 独有的未推送提交。缺少远端 upstream 或 fetch 失败时停止，不创建任务记录、分支或窗口。`--no-fetch` 使用本地 base，适用于离线、纯本地仓库或需要本地提交的情况。复用已有分支时保留原提交，不执行 fetch。 `--no-open` 仅跳过进入，仍创建窗口并执行启动配置。默认成功后打开 Twig 内置终端工作台，Ctrl-W d 返回 Console 并选中该任务；非交互环境进入失败时已经创建的资源仍保留。

同名本地分支或远端跟踪分支存在时，new 询问是否使用它创建任务，默认取消。本地同名分支优先；只有远端跟踪分支时，使用本机已 fetch 的引用，多个 remote 同名则输入编号选择，再确认。远端来源确认后创建本地分支并建立 upstream 跟踪关系；本地来源直接复用。两者均保留已有提交并忽略 --base。不自动 fetch，不保证引用反映远端实时状态；需要更新时先自行 git fetch。分支被其他 Worktree 占用时显示路径并拒绝；非交互环境请改用交互终端或 Console 确认。已登记同名任务时直接报错，包括创建失败留下记录、窗口或 Worktree 缺失的情况；不复用、不恢复、不修改已有资源。已有任务用 `twig open PROJECT NAME` 或 Console 任务列表进入；Worktree 缺失时先按故障排查指南处理。不会接管外部目录或窗口。任务路径不能与已有任务相互包含，中间目录不能是符号链接或其他仓库。创建失败不强制回滚；恢复方式见 `twig guide troubleshooting`。

## open：进入已有任务

```sh
twig open myapp auth
```

PROJECT 和 NAME 必填。已有窗口直接复用；缺失窗口按当前配置重建并启动程序。Worktree 缺失或归属错误时报错。打开 Twig 内置终端工作台，需要交互终端；Ctrl-W d 返回 Console 并选中该任务，q 退出导航。不会切换外层 tmux 客户端；在目标项目自身 Session 中打开工作台会被拒绝，请从 tmux 外或其他 Session 使用。普通任务不会恢复原程序状态或 Agent 对话；通过 `new --agent` 创建的受管任务连接原 Codex session。主工作目录通过 Console 进入。

## stop：停止任务窗口

```sh
twig stop myapp auth
twig stop myapp auth --yes
```

PROJECT 和 NAME 必填。关闭窗口及其全部进程，保留 Worktree、分支和任务记录。默认询问 `[y/N]`；`-y/--yes` 跳过确认。已登记任务的窗口不存在时成功返回，不重建窗口。重新启动用 open；重复 new 会报任务已存在。

## rm：取消登记或删除任务

```sh
twig rm myapp
twig rm myapp auth
twig rm myapp auth --keep-branch --delete-ignored --yes
```

只给 PROJECT 时立即取消登记，保留全部资源及状态。给 NAME 时安全删除任务，默认询问确认。

| 任务选项 | 行为 |
| --- | --- |
| --target BRANCH | 本次合并检查的本地目标，默认项目 target |
| --keep-branch | 保留本地分支，跳过合并/upstream 检查，仍检查文件和进程 |
| --delete-ignored | 显式允许移除 ignored 文件及目录内容，仍保护未提交/未跟踪文件 |
| -y, --yes | 仅跳过确认，不绕过检查 |

没有 force 选项。默认拒绝脏文件、ignored 文件、活动进程、未合并或相对本地 upstream 尚未推送的提交。完整条件、删除顺序和部分失败见 [安全清理](cleanup.md)（`twig guide cleanup`）。

## console：交互面板

```sh
twig
twig console
```

两者相同，需要 stdin/stdout 都为终端。可浏览项目、任务与可选 Codex 状态；提交表单创建资源、确认后删除任务、任务区 Enter 准备并进入窗口。快捷键和状态含义见 [Console](console.md)（`twig guide console`）。

## 输出与错误

结果和帮助写 stdout，错误/诊断写 stderr。成功通常为 0，执行失败和取消确认为 1，参数解析失败为 2；帮助与版本返回 0。成功表示命令执行完成，不表示启动程序就绪、测试通过或开发任务完成。自动化用法见 [Agent 与脚本](automation.md)（`twig guide automation`）。

## 主控 Agent 与受管任务

`twig chat` 打开主控对话，点击顶部 Console / Agent 标签或按 Tab 切换页面。`new PROJECT NAME --agent --no-open` 新建受管任务，使用专用 Codex 窗口，替代普通启动配置；`agent send/read/status/drain/cancel/notify` 提供派发、查询、恢复和通知。`stop` 会中断受管轮次并取消待发消息，重新 open 才允许继续派发。完整用法见 [主控 Agent](agent.md)（`twig guide agent`）。
