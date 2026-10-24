# 独立验收入口

## 主控 Codex 协议验证

```sh
uv run examples/check_controller.py
```

需要 Codex CLI 和 uv；脚本只使用 Python 标准库。它在临时 CODEX_HOME 和专用 Unix socket 上启动真实 app-server，以本地固定 Responses 响应代替模型；不读取真实认证、不连接真实 daemon、不消耗模型额度、不发送飞书消息。模拟模型名称仅用于协议配置，不选择或调用线上模型。

在 Codex 0.163.0 验证：忙时 turn/start 加入当前轮次；所有 UI 客户端断开后 notify 继续执行；两个完成回调通过临时队列使主控在当前轮次完成后再处理反馈；主控调用独立 CLI 并读取模拟通知环境变量；重新连接可读取历史，重复完成事件不重复派发。

这是 RFC 第一阶段的协议原型，回调和队列由验收脚本模拟，尚未实现正式 Twig 命令。它使用前台启动、独立于客户端存活的 app-server，未覆盖受管理 daemon 重启、30 分钟卸载、回调崩溃或投递结果未知、人工输入竞争、tmux 启动配置及审批场景。不会据此宣称生产消息队列已具备可靠性。

## Control Mode 工作台

正式实现位于 `src/workbench.rs`，由 Console 和 `control_mode.rs` 共用。Console 任务区 Enter、`new/open` 都打开同一个工作台，Ctrl-W d 返回导航并选中对应任务。用户操作见 [Console 手册](../docs/console.md)。

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

需要交互终端，必须显式提供 socket 和已有 Session。独立入口中 Ctrl-W d 退出前端；Console 中该键返回导航。退出只结束本客户端，保留会话与进程。再次运行可以重新接入。分屏与尺寸调整会修改实际 Pane。

Ctrl-W 后按 `s` / `v` 分屏、`h/j/k/l` 按方向切 Pane、`o` 循环切 Pane、`z` 放大、`r` 重载 tmux 配置；无前缀 Alt-h/j/k/l 每次调比例 3 格、`n/p` 切窗口；再按 Ctrl-W 发送原始按键，Esc 取消。Shift-PgUp/PgDn 浏览 tmux 历史，继续输入回到底部。Ctrl-C 交给程序，Ctrl-D/exit 按 Shell 语义关闭 Pane。没有强制关闭操作。

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

Python 脚本通过 uv 管理 pyte，需要 Vim、Zsh 和 tmux。它解析真实前端的 PTY 输出，验证 Vim 编辑中断开重连、继续保存、返回 Shell、termios 还原，以及有限输出期间 Ctrl-C 和继续输入的响应。另使用临时 Zsh 配置验证 vi 模式光标切换、跨 Pane 样式同步、重连及退出重置。拖选测试使用模拟 SSH 环境捕获并解码 OSC 52，验证历史选择、画面冻结和松开自动复制，不访问真实剪贴板或外层 tmux。所有文件与 server 均为临时资源；不访问真实 Agent 会话，不发送提示词或审批。

## 验证边界

CI 在 Ubuntu 24.04（tmux 3.8）和 macOS Tahoe 26（Homebrew tmux）上执行同一份 PTY 验收，结果以对应提交为准。未验收真实 SSH 断线、真实 Codex 人工交互、超链接、图像和完整扩展键盘协议；未进行高吞吐或慢链路压力测试。每次刷新读取有变化 Pane 的可见区域，仍要消费 control 输出流，33 ms 是调度限制而非帧率保证。超时或异常会返回导航并保留后台资源。

多客户端共用 tmux 的尺寸与窗口选择规则，不提供独立布局。工作台支持鼠标点击、滚轮与边框拖动：程序启用鼠标模式时转发滚轮，否则浏览 tmux 历史；点击和拖选不转发。PTY 验收覆盖 SGR、传统和 UTF-8 鼠标协议、关闭鼠标模式及非活动分屏的坐标。支持 Pane 内拖选后松开自动复制，选区期间冻结显示；本机使用系统剪贴板，SSH 使用终端复制请求。尚无跨屏拖选、软换行合并或通用 tmux 按键绑定集成。

## 主控验收

`cargo build --locked && uv run examples/check_agent.py` 验证正式 Chat/CLI：全新主控首次打开及两轮回复上屏、受管任务、启动配置隔离、状态文件忽略、离线回调、忙时排队、未知投递对账、漏失回调补查、离线主控执行通知 CLI、标签切换草稿、Chat 拖选/Alt-C 复制（捕获 OSC 52）、中文长输入和 F4 详情、服务重启、飞书业务错误及停止/删除保护。依赖 Codex、tmux、Git、uv，使用独立 CODEX_HOME 与本地模型，不读取真实认证、不向真实会话输入、不发送真实飞书消息。协议单独实验为 `uv run examples/check_controller.py`。

主控验收还覆盖后台回复的浮动提醒与 Chat 未读、工作台提醒、卡片到期保留未读、进入 Chat 清除和不重复提醒。
