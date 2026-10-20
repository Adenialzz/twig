# 任务窗口启动配置

twig 可以在创建任务窗口时，自动分屏并在各个 Pane（窗格）中启动程序。命令行阅读：`twig guide windows`。本文介绍配置位置、字段和执行规则；登记表与状态文件见 [配置与存储](configuration.md)（`twig guide configuration`）。

## 快速开始

在项目**主仓库根目录**创建 `.twig/config.toml`：

```toml
[tmux]
panes = [
  { focus = true },
  { split = "right", percent = 35, command = ["npm", "run", "dev"] },
]
```

然后创建任务：

```bash
twig new myapp auth
```

其中 `myapp` 是已登记的项目别名。窗口左侧约占 65%，启动普通 Shell，并获得键盘焦点；右侧约占 35%，在 Shell 中自动执行 `npm run dev`，结束后回到提示符。两个 Pane 的工作目录都是 `auth` 的 Worktree。

示例命令要求项目提供对应的 npm 脚本并已准备好依赖。twig 不会自动安装依赖。

## 配置位置与优先级

| 文件 | 用途 |
| --- | --- |
| `<主仓库>/.twig/config.toml` | 当前项目的启动配置，可纳入 Git 管理 |
| `~/.twig/config.toml` | 用户默认启动配置，同时保存 `[projects]` 项目登记信息 |
| `~/.tmux.conf` | Shell、进程及 server 设置；所有 Twig 入口的工作台按键与外观由 Twig 管理 |

使用 `twig --config /path/config.toml ...` 时，指定文件同时替代用户级配置和登记表，不再读取默认的 `~/.twig/config.toml`。

读取规则如下：

1. 项目文件定义了 `[tmux]`，就整体使用项目配置。
2. 项目文件不存在，或未定义 `[tmux]`，则使用用户配置的 `[tmux]`。
3. 两级都未定义时，保持原有单 Shell 行为。

**两级配置不合并 Pane。** 项目配置中的空 `[tmux]` 或 `panes = []` 都表示单 Shell，可以用来取消用户默认分屏。配置文件无法读取或格式错误时会报错，不静默退回默认值。

项目配置始终取自主仓库，而不是调用命令时的目录，也不是任务 Worktree 中的副本。从 Console、其他目录或任务目录执行，选择规则一致。只修改任务分支里的配置副本不会改变启动行为。

内部的 `projects/<仓库 ID>/config.json` 保存项目资源参数，不用于配置 Pane。

### 用户配置与项目登记共存

可以在现有用户配置中添加 `[tmux]`，保留原有 `[projects]` 内容：

```toml
[tmux]
panes = [
  { focus = true },
  { split = "right", percent = 35 },
]

[projects.myapp]
path = "/path/to/myapp"
```

`twig add` 和 `twig rm PROJECT` 写入登记信息时会保留 `[tmux]` 配置值，但重新序列化 TOML 后不会保留手写注释和原始排版。

## 字段说明

`[tmux]` 下的 `panes` 是按创建顺序排列的数组。

| 字段 | 类型 | 含义与默认行为 |
| --- | --- | --- |
| `panes` | Pane 数组 | 省略或设为 `[]` 时使用单 Shell |
| `mode` | `"shell"` 或 `"process"` | 默认 shell；process 直接运行程序并保留退出现场 |
| `command` | 字符串数组 | 第一个元素为程序，其余为参数；shell 模式可省略，process 模式必须提供 |
| `split` | `"right"` 或 `"down"` | 从前一个 Pane 向右或向下分割；首个 Pane 不设置，后续 Pane 必须设置 |
| `percent` | 整数，1–99 | 新 Pane 占被分割空间的百分比，默认 50；首个 Pane 不设置 |
| `focus` | 布尔值 | `true` 表示初始键盘焦点；最多一个 Pane 设置为 true，未指定时聚焦首个 Pane |

首个 Pane 可以写成 `{}`，表示普通 Shell。配置不接受未知字段。

### 分屏比例如何计算

每次分割的对象都是**前一个 Pane**，不是整个窗口。例如：

```toml
[tmux]
panes = [
  { focus = true },
  { split = "right", percent = 35, command = ["npm", "run", "dev"] },
  { split = "down", percent = 40 },
]
```

先把窗口右侧 35% 分给第二个 Pane，再把第二个 Pane 下方 40% 分给第三个 Pane。最终布局大致如下，实际尺寸受终端字符格和边框影响：

```text
┌──────────────────────────┬──────────────┐
│                          │ 开发服务     │
│ 主 Shell                 │ 右侧高度 60% │
│ 宽度约 65%               ├──────────────┤
│                          │ 辅助 Shell   │
│                          │ 右侧高度 40% │
└──────────────────────────┴──────────────┘
```

当前不支持指定任意目标 Pane、命名布局或多个窗口模板。终端空间不足以继续分割时会报错并保留已创建资源。

## 命令与工作目录

所有 Pane 都从任务 Worktree 根目录启动。程序及参数分别填写：

```toml
command = ["npm", "run", "dev"]
command = ["cargo", "test", "--locked"]
command = ["./scripts/dev"]
```

以上是三个独立字段示例，选一个放进对应 Pane，不能在同一个 Pane 中重复定义 `command`。

不要写成 `command = ["npm run dev"]`，这会尝试寻找名为 `npm run dev` 的程序。数组不能为空，第一个元素也不能为空。

默认 `shell` 模式运行 tmux `default-shell` 指定的登录 Shell。twig 把 command 的各个参数逐个引用，作为第一条命令输入 Shell。`process` 模式直接启动指定程序，不启动交互式 Shell，也不加载其启动配置。两种模式的参数均按字面值传递：

- `$VAR`、`~`、`*` 不会展开。
- `|`、`>`、`&&` 不会被当成管道、重定向或命令连接符。
- shell 模式正常加载自己的启动配置和环境；process 模式使用 tmux 的进程环境。两种模式均执行外部程序，不把程序名解析成 Shell 别名或函数。
- 相对程序路径基于任务 Worktree；脚本需要存在于任务分支中，并具备相应的执行权限和解释器。

需要环境准备、多个步骤或条件判断时，建议把逻辑写入项目脚本，再使用 `command = ["./scripts/dev"]`。各 Pane 的程序不会等待前一个程序完成，配置顺序不表示依赖就绪顺序。

未配置任何 Pane 时，twig 沿用原有 tmux 默认启动行为；配置了非空 Pane 列表后，省略 `command` 的 Pane 使用 `default-shell` 登录 Shell。

## 两种运行模式

| 行为 | `shell`（默认） | `process` |
| --- | --- | --- |
| 启动方式 | 登录 Shell，可自动执行第一条命令 | 直接运行 command 指定的程序 |
| command 是否必填 | 否 | 是 |
| 命令结束 | 回到 Shell 提示符，可继续输入 | 保留 dead Pane、输出和退出码 |
| Ctrl-C | 通常中断前台程序，回到提示符 | 由程序处理；若退出则保留 Pane |
| Ctrl-D | 空提示符下退出 Shell，关闭 Pane | 运行时由程序处理；退出后无效 |

同一窗口可以混合使用。例如主终端和开发服务保留 Shell，测试 Pane 保留退出现场：

```toml
[tmux]
panes = [
  { focus = true },
  { split = "right", command = ["npm", "run", "dev"] },
  { split = "down", mode = "process", command = ["npm", "test"] },
]
```

`process` 的 dead Pane 表示程序已经退出，不一定是错误；退出码 0 通常表示成功。该模式没有 Shell 接手，无法在其中继续输入命令。需要关闭它时使用自己的 tmux 关闭 Pane 操作，或在合适时机显式 stop/open 重建任务窗口。已有窗口不会因为再次 open 而重启程序。

### process 适用场景

`process` 适合让一个 Pane 专门运行一个程序、持续查看输出，并在程序退出后保留现场。

| 场景 | command 示例 | 用处 |
| --- | --- | --- |
| Web 开发服务 | `["npm", "run", "dev"]` | 持续显示服务日志，崩溃后保留错误与退出码 |
| 测试监听 | `["npm", "run", "test:watch"]` | 持续查看测试结果 |
| 构建监听 | `["npm", "run", "build:watch"]` | 持续查看编译结果和错误 |
| GPU 状态监控 | `["watch", "-n", "0.5", "nvidia-smi"]` | 每 0.5 秒刷新 NVIDIA GPU 状态 |
| 容器日志跟踪 | `["docker", "compose", "logs", "-f"]` | 专门查看容器日志；停止跟踪不等于停止容器 |
| 任务 Worker | `["./scripts/worker"]` | 观察任务处理情况，退出后保留错误 |
| 一次性检查 | `["cargo", "test", "--locked"]` | 跑完保留结果与退出码 |
| 临时端口转发 | `["ssh", "-N", "-L", "5433:localhost:5432", "dev-host"]` | 保持转发连接，断开后查看原因 |

示例要求项目提供对应脚本，且环境中已安装所需工具；SSH 示例中的主机和端口需要按实际环境修改。

长期运行的服务可以这样配置：

```toml
[tmux]
panes = [
  { focus = true },
  { split = "right", mode = "process", command = ["npm", "run", "dev"] },
]
```

从 tmux detach 后，服务仍会运行；服务崩溃或被 Ctrl-C 停止后，保留输出和退出状态，**twig 不会自动重启它**。这适合开发环境中的专用服务窗口，不提供服务守护或故障恢复。多个任务同时启动服务时，端口需要由项目脚本或配置自行区分，twig 不自动分配端口。

选择时看是否需要继续输入命令：

- **主要观察一个程序的输出**：使用 `process`。
- **经常 Ctrl-C、调整参数、重新运行或执行其他命令**：使用默认 `shell`。Shell 模式同样可以长期运行服务，detach 后也会继续运行。

## 常用配置

### 左右两个普通 Shell

```toml
[tmux]
panes = [
  { focus = true },
  { split = "right", percent = 35 },
]
```

### 上方开发终端，下方测试输出

```toml
[tmux]
panes = [
  { focus = true },
  { split = "down", percent = 30, command = ["cargo", "test", "--locked"] },
]
```

测试结束后下方 Pane 回到 Shell 提示符，可以继续输入命令；输出保留在终端历史中，不会自动重新运行测试。

### 显式启动 Agent，右侧保留 Shell

```toml
[tmux]
panes = [
  { command = ["codex"], focus = true },
  { split = "right", percent = 35 },
]
```

需要环境中已安装对应程序。twig 只负责启动，不管理 Agent 生命周期、不恢复对话、不发送提示词或审批。程序退出后回到 Shell 提示符，不会自动重启。

### 项目取消用户默认分屏

在项目主仓库的 `.twig/config.toml` 中写入：

```toml
[tmux]
panes = []
```

## 何时执行

| 操作 | 启动配置是否执行 |
| --- | --- |
| `new` 创建任务窗口 | 执行 |
| `new --no-open` 创建任务窗口 | 执行，只是不进入窗口 |
| `open`、重复 `new` 或 Console Enter，窗口已存在 | 不执行，不重新排列 Pane |
| `open`、重复 `new` 或 Console Enter，任务窗口缺失 | 执行，重新创建窗口 |
| Console 新建任务表单 | 创建窗口时执行，成功后仍留在 Console |
| `add` 创建项目 main 窗口 | 不应用任务布局，保持原有普通 Shell |
| Console 浏览、`ls`、Shell 补全 | 不执行 |

修改配置后，已有窗口不会发生变化。需要在现有任务中应用新配置时，先确认可以结束窗口中的进程，再执行：

```bash
twig stop myapp auth
twig open myapp auth
```

`stop` 会要求确认，并终止任务窗口中的全部进程；Worktree、分支和代码保留。重新打开会按当前配置启动新进程，不恢复之前的运行状态。

## 退出、失败和资源保留

`shell` 模式下，启动命令成功或失败退出后回到提示符，可继续运行其他命令，必要时用 `$?` 查看上一条命令的退出码。输出可能已进入 tmux 滚动历史，可用自己的复制模式或滚动快捷键查看。twig 不等待开发服务或测试结束，也不把命令输入 Shell 当作“服务已就绪”。

- Ctrl-C：由前台程序处理；通常中断程序后回到 Shell，不关闭 Pane。
- Ctrl-D：在空提示符下按 Shell 的正常行为退出，Pane 随之关闭，不留下 dead Pane；最后一个 Pane 退出时窗口关闭。
- 想保留窗口和进程：所有 Twig 工作台入口均按 `Ctrl-G d` 返回 Console 导航，再按 q 退出；无需原生 tmux detach。

`process` 模式的退出输出和状态由 tmux 保留，不会回到 Shell，也不会自动重启；同窗的 Shell Pane 仍按普通方式关闭。

以上遵循 Shell 和程序自己的信号、退出设置；例如程序可以自行处理 Ctrl-C，Shell 可以配置忽略 EOF。当前交互回归覆盖 sh、Bash、Zsh。

配置校验失败时不创建任务窗口；若 `new` 已创建 Worktree，则保留 Worktree，修正配置后可重试。

分屏或启动过程部分失败时，twig 保留窗口并标记初始化未完成。再次 `new` 或 `open` 会提示检查，不重复执行已经可能启动的命令。可以直接使用 tmux 检查资源；需要重新初始化时，修正问题后显式 `stop` 再 `open`。

命令本身找不到程序、返回非零或异常退出时，检查对应 Pane 的输出。运行中的程序仍受 `stop` 确认和 `rm` 活动进程检查保护；生成的未跟踪文件或 ignored 文件也仍受删除保护。

此功能不包含 Worktree 一次性初始化，不自动复制 `.env`、安装依赖或分配端口。不同任务启动开发服务时，需要由项目脚本处理端口等资源冲突。显式配置的命令可能联网，其行为由命令本身决定。
