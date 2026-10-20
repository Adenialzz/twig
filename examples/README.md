# Control Mode 终端原型

`control_mode.rs` 是独立实验入口，不改变 `twig`、`new`、`open` 或 Console 的当前行为。用于验证 tmux Control Mode → 终端模拟 → Ratatui 显示 → 输入转发的链路，不是正式可用的 Console 替代品。

已在 Linux、tmux 3.8 上验证。使用现有 Ratatui/Crossterm；`vt100 0.15.2` 仅加入开发依赖。最新版 vt100 0.18.1 与当前 Ratatui 固定的 unicode-width 版本冲突，正式库选型仍待兼容性评估。

## 在独立环境运行

从仓库根目录执行。创建专用临时 tmux server，不连接真实项目：

```sh
demo_dir=$(mktemp -d)
demo_socket="$demo_dir/tmux.sock"
tmux -S "$demo_socket" -f /dev/null new-session -d -s demo
cargo run --locked --example control_mode -- \
  --socket "$demo_socket" --session demo
```

原型要求交互终端，必须显式指定 socket 和已有 Session。连接前检查会话存在；程序退出只结束自己创建的 tmux 客户端，保留后台会话与进程。可以再次执行同一条 cargo 命令重新接入。连接及缩放会影响该会话的窗口尺寸，分屏操作会创建实际 Pane。

需要测试 Codex 时，在这个专用会话中手动启动并操作；自动测试不发送 Agent 提示词或审批。

## 操作

普通按键直接交给当前 Pane。`Ctrl-G` 是原型前缀，随后按：

| 按键 | 行为 |
| --- | --- |
| `d` | 断开前端，保留后台进程 |
| `%` / `"` | 左右 / 上下分屏，沿用当前 Pane 目录 |
| `o` | 切换到下一个 Pane |
| `z` | 放大 / 还原当前 Pane |
| `h/j/k/l` | 调整当前 Pane 大小 |
| `n/p` | 切换到下一个 / 上一个窗口 |
| `Ctrl-G` | 向程序发送原始 Ctrl-G |
| `Esc` | 取消前缀 |

`Shift-PgUp/PgDn` 浏览当前前端已收集的历史。Ctrl-C 传递给程序，不退出前端。Shell 中 Ctrl-D/exit 按 Shell 自身语义退出，可关闭 Pane；没有单独的强制关闭快捷键。

结束实验时，先确认该专用 server 中没有要保留的工作，再运行：

```sh
tmux -S "$demo_socket" kill-server
rmdir "$demo_dir"
```

## 当前边界

- 支持普通字符、常用导航键、Ctrl/Alt 字符、基础组合方向键和 bracketed paste；尚未覆盖功能键、完整扩展键盘协议、鼠标、文本选择与剪贴板。
- 接入时恢复可见屏幕、光标、光标可见性、应用方向键及粘贴模式。尚未恢复备用屏幕及其保存的主屏幕、完整历史、滚动区域、其他模式与未完成的转义序列。
- 屏幕捕获和光标模式查询目前是分开的命令，持续输出时的快照一致性尚未解决。静态 Shell 重连通过不代表 Codex/Vim 等全屏程序重连已经可靠。
- 布局变化会重新捕获屏幕，并丢失前端已收集的历史；初次连接不加载 tmux 历史。当前历史功能仅用于验证。
- 协议命令同步等待；大段粘贴、输出背压和渲染调度仍需优化。异常积压或查询超时显式退出，后台资源保留。
- 只连接一个 Session。暂不集成 twig 项目/任务导航、通知或多客户端尺寸协调；也未验证真实 SSH 断线、macOS 和真实 Codex 交互。

以上缺口解决并验收前，不接管正式 Console 的任务进入流程。

## 验证

```sh
cargo test --locked --example control_mode
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

示例中的自动测试覆盖协议字节解码、常用键编码、中文与颜色渲染，以及独立 tmux socket 上的真实分屏、放大、缩放、客户端退出后 PID 保留和文本恢复。另已通过临时 PTY 驱动验证实际键盘输入、焦点切换、Ctrl-C、退出与 termios 还原；该临时驱动尚未纳入持久回归测试。
