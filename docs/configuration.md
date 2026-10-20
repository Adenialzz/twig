# 配置与存储

命令行阅读：`twig guide configuration`。

## 选择用户配置

默认文件是 `~/.twig/config.toml`。全局参数 `--config PATH` 可以放在子命令前后；相对路径以命令调用目录为基准。它同时选择登记表、用户启动配置和项目状态所在目录，不再读取默认用户配置。

```sh
twig --config ~/work/twig/config.toml ls
twig ls myapp --config ~/work/twig/config.toml
```

## 项目登记与默认启动配置

```toml
[projects.myapp]
path = "/home/me/projects/myapp"

[tmux]
panes = [
  { focus = true },
  { split = "right", percent = 35 },
]
```

推荐用 `twig add myapp --path /path/to/myapp` 登记。手工配置的项目路径须是绝对路径或以 `~/` 开头；仅写登记表不会完成项目初始化。twig 更新登记时保留 tmux 配置，但重新序列化 TOML，不保留手写注释。remote 以 Git 为准，不在登记表中维护。

## 项目启动配置

主仓库 `.twig/config.toml` 的 `[tmux]` 整体覆盖用户 `[tmux]`，不合并 Pane。项目未定义该节时才继承用户配置；空 `[tmux]` 或 `panes = []` 明确恢复单 Shell。配置错误直接报错。

项目配置始终从主仓库读取，不从任务分支或调用目录读取。字段、运行模式和实例见 [任务窗口](windows.md)（`twig guide windows`）。tmux 快捷键、鼠标和外观仍由 `~/.tmux.conf` 管理。

## 项目资源与任务状态

默认结构如下；使用其他配置文件时以该文件所在目录为数据根目录：

```text
~/.twig/
├── config.toml
├── config.lock
└── projects/
    └── <仓库 ID>/
        ├── config.json
        ├── state.json
        └── lock
```

- `config.lock`：写登记表期间的互斥锁。
- `config.json`：项目 version、主目录 main、Worktree 根目录 root、基础分支 base、合并目标 target、tmux session。
- `state.json`：任务名对应的 branch、base、path。
- `lock`：项目命令运行期间的互斥锁。

仓库 ID 来自规范化共享 Git 目录路径的稳定哈希。主仓库与关联 Worktree 共用状态，同一远端的不同本地克隆独立存储。别名不参与 ID。相同目录下的不同登记表共享项目状态；不同目录隔离内部状态，但不隔离 Git、Worktree 或 tmux 外部资源。

`add --base/--target/--root` 用于首次初始化。已初始化项目不能通过 add 重新指定 root 或改变 base/target；需要修改已有资源参数时编辑错误提示所指向的 config.json。修改路径不会自动移动资源，须先核实真实 Git/tmux 状态；不要把编辑状态文件当成迁移工具。

取消登记保留项目状态。twig 不自动迁移仓库路径，不清理仓库删除后的残留状态；也不读取或迁移旧 `.git/twig/` 数据。不要直接删除状态目录来重置仍在使用的任务，这不会清理外部资源，也不会使 twig 自动接管它们。

异常锁和资源不一致见 [故障排查](troubleshooting.md)（`twig guide troubleshooting`）。
