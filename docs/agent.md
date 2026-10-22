# 主控 Agent

命令行阅读：`twig guide agent`。

Twig 的可选主控使用已有 Codex app-server daemon。主控可以查看项目、创建受管任务、向任务会话派活并跟进完成反馈；需要用户决策时通过飞书通知。普通 Console 和 Shell 任务不依赖主控。

## 开始使用

需要支持 app-server daemon 的 Codex（当前验收版本 0.163.0）；飞书通知另外需要 curl。

```sh
# 填入你自己的飞书机器人 URL；不要提交到仓库。
export TWIG_FEISHU_WEBHOOK_URL='你的飞书机器人 webhook URL'
# 由用户自行启动；Twig 不启动、重启或更新共享服务。
codex app-server daemon start

twig chat
```

已经运行的 daemon 不会自动继承当前 Shell 新设置的环境变量。通知 CLI 在 Codex 的执行环境运行，须确保该环境能读取 `TWIG_FEISHU_WEBHOOK_URL`；设置后可通过主控请求通知检查。若需要重启 Codex 以更新环境，先自行确认没有正在执行的任务。Twig 不会为通知配置而重启服务。

首次打开 Agent 创建主控工作目录和会话，已有会话则恢复原 thread。daemon 不可用时显示启动提示，F5 重试；不会回退到另一种模型服务。

- 点击顶部居中的圆角 Console / Agent 标签，或按 Tab：Agent 与 Console 导航相互切换，保留 Console 选择。
- Enter：发送消息；Alt-Enter：换行；支持粘贴、左右方向键、Home/End、退格和 Delete。输入框随内容增高，超出高度时跟随光标滚动。
- 滚轮 / PgUp/PgDn：浏览聊天记录；Ctrl-End 回到最新消息；Ctrl-U 清空输入。
- 在对话或输入区拖选文字，松开复制；Alt-C 复制最近一条主控完整回复（保留 Markdown）。选区固定当前区域画面，Esc 或继续输入清除，后台会话继续运行。
- F4：展开或收起工具输出、投递 ID 和会话 ID。错误信息始终显示。
- Ctrl-Y / Ctrl-N：同意或拒绝显示的命令或文件审批。复杂表单类输入目前需在原生 Codex 中处理，F4 可查看会话 ID。
- Ctrl-Q：退出 Agent 和 Twig；不会停止 Codex daemon 或任务。

Console 的标签点击与 Tab 只在普通导航状态触发，表单、帮助、搜索和工作台保留原有键盘语义。工作台先用 Ctrl-W d 返回导航，再切换 Agent。

Agent 顶部显示 Codex 返回的当前会话模型及推理强度，随会话刷新；未返回时标记为模型未知或推理未指定，不读取本机默认配置猜测。该信息是会话配置，不是逐轮执行遥测。Agent 区分用户消息、主控回复、任务反馈与工具摘要，支持标题和代码块着色；工具输出默认收起。对话和输入框随终端宽度展开，两侧各保留两列留白，浏览历史时新回复不会将视图拉到底部。复制复用 Console 的剪贴板机制：SSH 只显示已请求复制，不宣称系统剪贴板已确认更新。主控结束本轮后可以等待任务反馈；完成事件会再次唤醒它，无需保持 Agent 打开。主控忙碌时，Twig 消息先排队；第一版没有独立的暂停按钮或自主预算配置。

离开 Agent 后，主控完成新一轮回复时，Console 和工作台显示 8 秒浮动提醒；多个主控回复合并提醒。任务完成卡片优先，主控卡片随后显示。顶部 `Agent*` 表示有未读，工作台底部显示「Agent 有新消息」；导航按 Tab 查看，工作台先 Ctrl-W d 返回再按 Tab。进入 Agent 清除主控未读和卡片，查看 Agent 期间收到的完成事件静默。首次发现的历史轮次不提醒；提醒只在 Twig 打开期间采集，不持久化，不自动发飞书。

## 创建与派发任务

主控使用与用户相同的 CLI，普通任务仍使用原有启动配置。受管任务显式使用 `--agent`：

```sh
twig agent init
twig new myapp fix/login --agent --no-open
twig agent send --project myapp --task fix/login --message '修复登录失败，补充回归测试，更新任务状态。'
twig agent read --project myapp --task fix/login
```

`new --agent` 创建 Worktree、`.twig/state.md` 和绑定的 Codex session，再创建专用 tmux 窗口，以 `codex resume --remote ... THREAD_ID` 连接原 session。它替代本任务的 `[tmux]` 启动配置，不额外执行配置中的启动命令，避免重复启动 Agent。普通 `new` 不创建受管 session 或状态文件。

新任务状态文件记录目标、进展、阻塞、下一步和验证结果，由任务 Agent 维护，主控读取。该文件通过共享 Git 目录 `info/exclude` 中的精确规则 `/.twig/state.md` 忽略，不忽略项目 `.twig/config.toml`。若新分支已经包含同名文件，报告冲突并保留现场，不覆盖。不迁移或接管旧任务。

失败可能已经创建 Worktree、session 或窗口。先检查 `twig ls` 和 `twig agent status`，不要重复执行 new 或删除记录来重试。

`open` 重建缺失的受管窗口时连接原 session。`stop` 中断受管任务当前轮次、取消其尚未投递的消息并关闭窗口；重新 open 才允许继续投递。共享 daemon 不会被停止。删除仍保护活动会话、排队/待核对消息和文件；`.twig/state.md` 属于 ignored 文件，须按现有规则显式选择 `--delete-ignored` 才可删除。

## 主控 CLI

```sh
twig agent init                         # 初始化或检查主控绑定
twig agent send --message '汇总项目进度' # 向主控发送
twig agent read                         # 主控历史 JSON
twig agent status                       # 绑定、消息状态与事件记录 JSON
twig agent drain                        # 补查完成事件、出队并核对未确认请求
twig agent cancel --id MESSAGE_ID        # 取消排队/待核对记录，不撤回远端工作
twig agent notify --message '需要你确认验收条件'
```

`send` 和 `read` 同时指定 `--project`、`--task` 时针对受管任务。所有命令遵循顶层 `--config`；`agent status` 只读本地记录，不创建资源。`agent event` 是受管 Codex 的内部完成回调，不需要手工调用。

主控工具没有额外白名单或授权系统，沿用 Codex 的权限配置与用户指令。Git/tmux 归属、文件保护、显式推送等现有规则不变。第一版不自动处理复杂审批表单；离线等待审批的任务需要用户回到原生会话处理。

## 持久化与恢复

默认主控目录为 `~/.twig/controller/`：`workspace/` 保存主控 AGENTS.md 和全局 state.md，`runtime.json` 保存 session 绑定、消息和完成事件。非默认配置文件名使用配置路径标识隔离主控数据，避免同目录多个配置串用。Codex 自身保存对话与轮次历史。

Twig 不运行独立后台 server。Codex notify 启动短命令转交完成事件；主控自己的完成回调用于消费队列，不会生成无限的自我通知。回调使用创建会话时的 Twig 可执行文件绝对路径，因此不要在任务运行中删除该二进制。安装或移动后，应重新进入 Agent，并核对受管任务的运行配置。

消息状态包括 queued（等待）、sending（请求发出但结果待核对）、sent（Codex 已接受）、failed（归属等检查失败）、cancelled（停止任务时取消）。sent 不代表任务已完成。对 sending 消息，drain 尝试用客户端消息 ID 核对历史；无法确认时不自动重发，也不跳过它发送后续消息。请查看会话现场后处理，不要盲目重复派活。确认不再需要投递后可用 `agent cancel --id ID` 解除队列阻塞；取消记录不会中断或撤回已经被 Codex 接受的工作。

daemon 重启后，进入 Agent 或运行 drain 可恢复已有历史的绑定会话和队列；空主控尚无 rollout 时，Agent 或 `agent init` 会确认本地没有消息或完成记录后重建空绑定，保留 AGENTS.md 和全局 state.md；存在任何消息记录则拒绝自动替换。Agent 遇到连接失效会重新检查，连接仍不可用时用 F5 重试。没有新回调或客户端操作时，不承诺自动恢复。当前完成回调不保证覆盖失败、中断和等待审批事件，Agent/Console 可查看这些状态。

忙时排队由 Twig 命令锁协调。若同时通过原生 Codex 终端输入，检查空闲到发送之间仍有竞争，Codex 可能将消息并入该轮次；不要把它视为跨所有客户端的严格事务队列。

## 飞书通知

通知读取 `TWIG_FEISHU_WEBHOOK_URL`，向飞书机器人发送文本。缺少变量或飞书返回错误时明确失败，不影响其他命令。URL 不写入任务文件、消息记录或命令行参数。第一版支持仅需 webhook URL 的机器人；如机器人额外要求关键词，消息需包含所配置的关键词；暂不实现签名密钥配置。

请求超时可能已经送达，不自动重试以避免重复通知。主控或用户核对后可再次调用。自动化验收只使用本地模拟 HTTP 服务，不发送真实飞书消息。
