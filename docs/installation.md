# 安装与运行环境

命令行阅读：`twig guide installation`。

需要 Rust/Cargo、Git、tmux 和 Unix `ps`。历史人工验证环境为 macOS、Rust 1.93.1、Git 2.50.1、tmux 3.6a，这不是最低版本要求；Console 的 Control Mode 工作台另在 Linux、tmux 3.8 上完成自动化 PTY 验收；macOS 工作台尚未验收。

```bash
cd /path/to/twig
make install
twig --help
```

`make install` 安装或更新 twig，并复制更新主控 skill 到 `~/.twig/controller/workspace/.agents/skills/twig-controller/SKILL.md`；已有 skill 副本会被覆盖，自定义约定请另存。它不检查或安装 `gh`、`glab`，也不依赖 Homebrew。成功后打印 Zsh 补全和平台工具安装、登录提示，不修改 Shell 配置、不自动登录。使用 PR/MR 前按需自行安装 `gh` 或 `glab`，再运行 `gh auth login` / `glab auth login`。

不使用 Make 时，可以直接执行 `cargo install --path . --locked`。该命令不复制主控 skill，使用主控时需自行将 `skills/twig-controller/SKILL.md` 复制到上述路径；自定义 `--config` 则复制到对应主控工作区的 `.agents/skills/twig-controller/SKILL.md`。确保 `~/.cargo/bin` 在 PATH 中。支持从 Bash、Zsh 调用；普通命令无需修改 Shell 配置，Tab 补全按下节启用。

仅构建而不安装：

```bash
make build
./target/release/twig --help
```

直接运行 `make` 也会构建 release 版本，等价于 `cargo build --release --locked`。

运行时需要 Git、tmux 和 Unix `ps`；Rust/Cargo 只用于源码构建和安装。普通任务管理不需要 Codex、gh 或 glab。Console 的 Codex 观察功能需要本机已有服务；PR/MR 按平台需要 gh 或 glab。

首次使用见 [快速开始](quickstart.md)（`twig guide quickstart`）。安装后可直接执行 `twig guide` 阅读离线文档；当前没有单独安装的 man 页面，也不会自动启动分页器。需要分页时用 `twig guide --all | less`。
