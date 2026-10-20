# Control Mode 独立验收入口

正式实现位于 `src/workbench.rs`，由 Console 和 `control_mode.rs` 共用。Console 任务区 Enter 打开工作台，Ctrl-G d 返回导航；`new/open` 仍使用原生 tmux。用户操作见 [Console 手册](../docs/console.md)。

实现使用现有 Ratatui/Crossterm 和 `alacritty_terminal 0.26.0`，后者只解析屏幕快照中的字符和样式，不启动 GUI 或 PTY。tmux 是终端状态的唯一来源：Control Mode 通知标记变化，合并后捕获可见屏幕；连续输出刷新间隔至少 33 ms，空闲不查询屏幕。方向键、功能键和 bracketed paste 由 tmux 按当前程序模式编码。

## 独立运行

从仓库根目录执行，创建专用临时 server，不连接真实项目：

```sh
demo_dir=$(mktemp -d)
demo_socket="$demo_dir/tmux.sock"
tmux -S "$demo_socket" -f /dev/null new-session -d -s demo
cargo run --locked --example control_mode -- \
  --socket "$demo_socket" --session demo
```

需要交互终端，必须显式提供 socket 和已有 Session。独立入口中 Ctrl-G d 退出前端；Console 中该键返回导航。退出只结束本客户端，保留会话与进程。再次运行可以重新接入。分屏与尺寸调整会修改实际 Pane。

Ctrl-G 后按 `%` / `"` 分屏、`o` 切 Pane、`z` 放大、`h/j/k/l` 调比例、`n/p` 切窗口；再按 Ctrl-G 发送原始按键，Esc 取消。Shift-PgUp/PgDn 浏览 tmux 历史，继续输入回到底部。Ctrl-C 交给程序，Ctrl-D/exit 按 Shell 语义关闭 Pane。没有强制关闭操作。

确认实验资源不再需要后，仅清理该专用 server：

```sh
tmux -S "$demo_socket" kill-server
rmdir "$demo_dir"
```

## 验证

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --example control_mode
uv run examples/check_control_mode.py
```

库测试使用独立 socket、临时 FIFO 和文件验证屏幕样式、47/1047/1049 备用屏幕、历史及清理、半截转义序列、插入/换行、持续输出一致性、特殊键编码和中文多行粘贴。Console PTY 测试覆盖正式导航往返、跨项目、分屏缩放、焦点、粘贴、连接断开、进程保留与同 Session 嵌套拒绝。

Python 脚本通过 uv 管理 pyte，需要 Vim 和 tmux。它解析真实前端的 PTY 输出，验证 Vim 编辑中断开重连、继续保存、返回 Shell、termios 还原，以及有限输出期间 Ctrl-C 和继续输入的响应。所有文件与 server 均为临时资源；不访问真实 Agent 会话，不发送提示词或审批。

## 验证边界

目前在 Linux/tmux 3.8 验证。未验收 macOS、真实 SSH 断线、真实 Codex 人工交互、超链接、图像和完整扩展键盘协议；未进行高吞吐或慢链路压力测试。每次刷新读取有变化 Pane 的可见区域，仍要消费 control 输出流，33 ms 是调度限制而非帧率保证。超时或异常会返回导航并保留后台资源。

多客户端共用 tmux 的尺寸与窗口选择规则，不提供独立布局。工作台不捕获鼠标，文本选择与复制可使用外层终端；尚无内置选区、系统剪贴板或 tmux 按键绑定集成。原通知功能仍留在单独 stash，本次没有恢复或集成。
