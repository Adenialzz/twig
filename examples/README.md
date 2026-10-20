# Control Mode 终端原型

`control_mode.rs` 是独立实验入口，不改变 `twig`、`new`、`open` 或 Console 的当前行为。用于验证 tmux Control Mode 变化通知 → 屏幕快照 → Ratatui 显示 → 输入转发的链路，不是正式可用的 Console 替代品。

已在 Linux、tmux 3.8 上验证。使用现有 Ratatui/Crossterm；`alacritty_terminal 0.26.0` 仅加入开发依赖，仅用于解析屏幕快照中的字符和样式，不启动 Alacritty GUI 或 PTY。终端查询的应答仍由 tmux 负责，前端不会重复发送应答或执行剪贴板请求。

首版使用的 vt100 0.15.2 在超过一屏的历史回滚测试中触发内部溢出，且缺少插入等模式；新版又与现有 Ratatui 的字符宽度依赖冲突，因此已替换终端核心，没有添加本地兼容补丁。

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

`Shift-PgUp/PgDn` 浏览历史。直接按页读取 tmux 保留的历史，不在前端复制完整历史；布局变化会回到历史底部。Ctrl-C 传递给程序，不退出前端。Shell 中 Ctrl-D/exit 按 Shell 自身语义退出，可关闭 Pane；没有单独的强制关闭快捷键。

结束实验时，先确认该专用 server 中没有要保留的工作，再运行：

```sh
tmux -S "$demo_socket" kill-server
rmdir "$demo_dir"
```

## 当前边界

- 支持普通字符、常用导航键、Ctrl/Alt 字符、基础组合方向键和 bracketed paste；尚未覆盖功能键、完整扩展键盘协议、鼠标、文本选择与剪贴板。
- tmux 是终端状态的唯一来源。`%output` 只标记变化，前端合并通知后读取有变化 Pane 的可见区域、光标和输入模式；连续输出时刷新间隔至少 33 ms，空闲不轮询屏幕。不将原始输出继续喂给本地模拟器，避免猜测重连前的 SGR、备用屏幕、插入模式或半截转义状态。
- 单 Pane 的状态和屏幕查询作为同一组不等待外部事件的内建命令提交；历史范围也在 server 内计算。目前只验证 tmux 3.8，不承诺跨版本兼容。
- 已验证 47/1047/1049 备用屏幕、颜色状态、半截 CSI、插入和行末换行、历史清理，以及 Vim 编辑、断开重连、继续保存和返回 Shell。软换行和重排由 tmux 负责，前端显示其物理行；字符宽度差异、超链接和图像仍未验收。
- 每次刷新需读取并解析整块可见区域；33 ms 是调度限制，不是帧率保证。大量输出时仍需读取 Control Mode 输出流，吞吐、CPU 和慢链路尚未验收。输入模式来自最近快照，扩展键盘和模式切换竞态仍需完善。
- 协议命令同步等待；大段粘贴、输出背压和渲染调度仍需优化。异常积压或查询超时显式退出，后台资源保留。
- 只连接一个 Session。暂不集成 twig 项目/任务导航、通知或多客户端尺寸协调；也未验证真实 SSH 断线、macOS 和真实 Codex 交互。

以上缺口解决并验收前，不接管正式 Console 的任务进入流程。

## 验证

```sh
cargo test --locked --example control_mode
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --example control_mode
uv run examples/check_control_mode.py
```

示例中的 11 项自动测试覆盖输出通知识别、常用键编码、中文与颜色渲染、真实分屏/缩放/PID 保留，以及三种备用屏幕返回、重连前颜色状态、清空历史、主屏幕历史、半截转义序列、插入模式、行末换行和持续输出时快照一致性。均使用独立 tmux socket。

首版另通过临时 PTY 驱动验证实际键盘输入、焦点切换、Ctrl-C、退出与 termios 还原；该临时驱动尚未纳入持久回归测试。

`check_control_mode.py` 是实际 Vim 的 PTY 验收，需要本机安装 Vim、tmux 和 uv；uv 按脚本声明安装 pyte。脚本在临时目录创建独立 server 和测试文件，对前端输出进行终端解析后核对显示，验证 Vim 运行期间断开、重连、继续编辑保存、返回 Shell 和 termios 还原，最后仅清理该测试 server。原型在屏幕或输入状态变化时重绘，不再在空闲时持续刷新。
