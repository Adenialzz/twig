# 命令参考

命令行阅读：`twig guide commands`。某条命令的参数速查使用 `twig <命令> --help` 或 `twig help <命令>`。

## 语法与名称

`PROJECT` 表示已登记的项目别名；`NAME` 表示任务名（add 的 NAME 是新项目别名）。`[参数]` 表示可选，`<参数>` 表示必填，这些括号不要输入。选项值含空格时按所在 Shell 的规则加引号。

- 项目别名：1–64 个 ASCII 字母、数字、横线或下划线，首位须为字母或数字。
- 任务名：总长 1–64 个 ASCII 字符，允许 `/` 分段，每段沿用上述规则；不允许空段，`main` 保留给主窗口。任务名直接作为新分支名。
- 所有命令支持 `--config PATH`：选择用户登记表及同目录项目状态，默认 `~/.twig/config.toml`。不按当前目录推断项目。
- `-h/--help` 输出帮助，顶层 `-V/--version` 输出版本。

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

## new：创建或接续任务

```sh
twig new myapp auth
twig new myapp feat/login --base develop --no-open
```

PROJECT 和 NAME 必填。`--base BRANCH` 选择已有本地起点，默认项目 base；不会 fetch。`--no-open` 仅跳过进入，仍创建窗口并执行启动配置。默认成功后打开 Twig 内置终端工作台，Ctrl-W d 返回 Console 并选中该任务；非交互环境进入失败时已经创建的资源仍保留。

重复 new 接续自己登记的资源，不接管外部同名分支、目录或窗口。已有任务按记录中的分支/路径接续；传入与原记录不同的 --base 会报错，不会重置分支。任务路径不能与已有任务相互包含，中间目录不能是符号链接或其他仓库。创建失败不强制回滚；恢复方式见 `twig guide troubleshooting`。

## open：进入已有任务

```sh
twig open myapp auth
```

PROJECT 和 NAME 必填。已有窗口直接复用；缺失窗口按当前配置重建并启动程序。Worktree 缺失或归属错误时报错。打开 Twig 内置终端工作台，需要交互终端；Ctrl-W d 返回 Console 并选中该任务，q 退出导航。不会切换外层 tmux 客户端；在目标项目自身 Session 中打开工作台会被拒绝，请从 tmux 外或其他 Session 使用。不会恢复原程序状态或 Agent 对话。主工作目录通过 Console 进入。

## stop：停止任务窗口

```sh
twig stop myapp auth
twig stop myapp auth --yes
```

PROJECT 和 NAME 必填。关闭窗口及其全部进程，保留 Worktree、分支和任务记录。默认询问 `[y/N]`；`-y/--yes` 跳过确认。已登记任务的窗口不存在时成功返回，不重建窗口。重新启动用 open，或 `new --no-open` 准备而不进入。

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

## pr：查找或创建草稿 PR/MR

```sh
twig pr myapp auth
twig pr myapp auth --push --remote origin --target main
```

PROJECT 和 NAME 必填。这是显式联网命令；已有匹配的开放请求则返回链接，否则创建草稿。

| 选项 | 含义与默认值 |
| --- | --- |
| --push | 显式推送任务分支并设置 upstream；默认不推送，不强推 |
| --remote REMOTE | Git remote；默认 upstream remote，否则唯一 remote |
| --target BRANCH | 远端目标分支；默认项目 target |
| --provider github\|gitlab | 公共域名自动识别，自建域名必须指定 |
| --title TEXT | 新草稿标题，默认任务最新提交标题 |
| --body-file PATH | 新草稿正文文件，默认最新提交完整说明；相对调用目录解析 |

已有请求不修改标题/正文。带 --push 时即使请求已存在也先推送。只支持同仓库请求；完整预检、认证、URL 限制和失败处理见 [PR/MR](pr.md)（`twig guide pr`）。

## console：交互面板

```sh
twig
twig console
```

两者相同，需要 stdin/stdout 都为终端。可浏览项目、任务与可选 Codex 状态；提交表单创建资源、确认后删除任务、任务区 Enter 准备并进入窗口。快捷键和状态含义见 [Console](console.md)（`twig guide console`）。

## 输出与错误

结果和帮助写 stdout，错误/诊断写 stderr。成功通常为 0，执行失败和取消确认为 1，参数解析失败为 2；帮助与版本返回 0。成功表示命令执行完成，不表示启动程序就绪、测试通过或开发任务完成。自动化用法见 [Agent 与脚本](automation.md)（`twig guide automation`）。
