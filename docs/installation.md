# 安装与运行环境

命令行阅读：`twig guide installation`。

运行环境需要 Git、**tmux 3.8 或更新版本**（Linux 或 macOS）；源码安装还需要 Rust/Cargo 和 Make。

tmux 3.8 是当前支持基线：Ubuntu 24.04 和 macOS Tahoe 26（Apple Silicon）的 Rust 测试均使用该版本通过，Linux 另通过独立 Control Mode PTY 验收。CI 的 Rust 检查、独立 Control Mode 脚本和 Agent 隔离验收均覆盖这两个平台，最新验收结果以对应提交的 CI 为准。3.8 之后的版本需关注其兼容性，并非每个版本都已验收。

tmux 3.4 已确认会因缺少工作台查询的光标信息而失败；3.5–3.7 系列未完整验收，不承诺支持。历史上用 tmux 3.6a 运行过早期功能，不代表当前工作台兼容它。

安装前检查：

```sh
tmux -V
```

升级前确认现有任务已妥善处理。替换 tmux 可执行文件不会升级正在运行的 server；有旧 server 时可用 `tmux display-message -p '#{version}'` 核对服务端版本。不要为了升级直接终止仍承载任务的 server。

```bash
cd /path/to/twig
make install
twig --help
```

`make install` 安装或更新 twig，并复制更新主控 skill 到 `~/.twig/controller/workspace/.agents/skills/twig-controller/SKILL.md`；已有 skill 副本会被覆盖，自定义约定请另存。成功后打印 Zsh 补全提示，不修改 Shell 配置。

不使用 Make 时，可以直接执行 `cargo install --path . --locked`。该命令不复制主控 skill，使用主控时需自行将 `skills/twig-controller/SKILL.md` 复制到上述路径；自定义 `--config` 则复制到对应主控工作区的 `.agents/skills/twig-controller/SKILL.md`。确保 `~/.cargo/bin` 在 PATH 中。支持从 Bash、Zsh 调用；普通命令无需修改 Shell 配置，Tab 补全按下节启用。

已有通过 `daemon start` 启动的后台，更新时可执行：

```sh
make install-restart
# 自定义登记表：
make install-restart CONFIG="$HOME/.twig/work.toml"
# 只重启，不安装：
twig daemon restart
```

`install-restart` 先安装二进制与主控 skill，成功后才调用安装目录里的 Twig 重启指定后台；任一安装步骤失败都不重启。安装目录按 `CARGO_INSTALL_ROOT`、`CARGO_HOME`、`~/.cargo` 的优先级确定；使用 Cargo 配置中的自定义 install.root 时，通过 `TWIG_BIN=/绝对路径/bin/twig` 指定目标。`CONFIG` 只选择重启的后台，主控 skill 仍安装到默认工作区；自定义配置需按上文复制 skill。

重启沿用原后台环境，不重新加载 `.env`；新 Shell 的 export 不会覆盖原环境。未运行的后台须先 `daemon start`；前台 `daemon run` 或 systemd/launchd 托管实例使用原启动方式或服务管理器。首次升级时旧后台若还不支持 restart，先从原环境有序 stop/start 一次，之后即可使用新入口。操作细节见 [后台指南](daemon.md)。

仅构建而不安装：

```bash
make build
./target/release/twig --help
```

直接运行 `make` 也会构建 release 版本，等价于 `cargo build --release --locked`。

Rust/Cargo 只用于源码构建和安装，CI 使用 Rust stable。普通任务管理不需要 Codex。Console 的 Codex 观察功能需要本机已有服务。

普通 `make install` 不自动启动或重启后台。业务入口使用前执行 `twig daemon start`；持续托管及升级切换见 [后台指南](daemon.md)。

首次使用见 [快速开始](quickstart.md)（`twig guide quickstart`）。安装后可直接执行 `twig guide` 阅读离线文档；当前没有单独安装的 man 页面，也不会自动启动分页器。需要分页时用 `twig guide --all | less`。
