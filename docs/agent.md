# 主控 Agent

命令行阅读：`twig guide agent`。

Twig 的可选主控使用已有 Codex app-server daemon。主控可以查看项目、创建受管任务、向任务会话派活并跟进完成反馈；需要用户决策时通过飞书通知。普通 Console 和 Shell 任务不依赖主控。

## 开始使用

需要支持 app-server daemon 的 Codex（当前隔离验收版本 0.164.0）；飞书通知另外需要 curl。

```sh
# 由用户自行启动；Twig 不启动、重启或更新共享服务。
codex app-server daemon start
twig daemon start
twig chat
```

飞书是可选通道，申请和配置见[飞书机器人指南](feishu.md)。凭证由 Twig 后台读取；已运行的后台不自动继承新终端环境。修改后有序重启 Twig 后台即可，不需要为此重启 Codex。服务托管时显式配置环境，见 [后台指南](daemon.md)。

进入 Agent 或按 F5 重连时，全屏居中显示「连接中…」；连接成功后展示对话界面，空会话不显示欢迎内容。连接期间可按 Tab 返回、Ctrl-C 清空已有草稿或退出，暂不接受新输入。首次打开 Agent 创建主控工作目录和会话，已有会话则恢复原 thread。daemon 不可用时显示启动提示，F5 重试；不会回退到另一种模型服务。

- 点击顶部居中的圆角 Console / Agent 标签，或按 Tab：Agent 与 Console 导航相互切换，保留 Console 选择。
- Enter：发送消息，立即显示在对话中，不显示发送中标记；投递异常时显示错误。Alt-Enter：换行；支持粘贴、左右方向键、Home/End、退格和 Delete。输入框随内容增高，超出高度时跟随光标滚动。
- 滚轮 / PgUp/PgDn：浏览聊天记录；Ctrl-End 回到最新消息；Ctrl-U 清空输入。
- 在对话或输入区拖选文字，松开复制；Alt-C 复制最近一条主控完整回复（保留 Markdown）。选区固定当前区域画面，Esc 或继续输入清除，后台会话继续运行。
- F4：展开或收起完整命令、工具输出、文件清单、投递 ID 和会话 ID。失败状态始终显示，长错误输出默认预览末尾，F4 查看完整内容。
- 主控默认 YOLO，页面顶部红色显示「YOLO · 无需审批 · 完整访问权限」。不提供 Ctrl-Y / Ctrl-N 审批快捷键；工具补充信息、登录等交互需在原生 Codex 中处理，F4 可查看会话 ID。
- Ctrl-C：有输入时清空输入（包含纯空白），无输入时退出 Twig；不会中断主控轮次、停止 Codex daemon 或任务。页面切换只使用 Tab 或点击标签，F2 不再切换。

Console 的标签点击与 Tab 只在普通导航状态触发，表单、帮助、搜索和工作台保留原有键盘语义。工作台先用 Ctrl-W d 返回导航，再切换 Agent。

Agent 顶部显示 Codex 返回的当前会话模型及推理强度，随会话刷新；未返回时标记为模型未知或推理未指定，不读取本机默认配置猜测。该信息是会话配置，不是逐轮执行遥测。Agent 按持久化来源区分「你」「飞书」「主控」「子任务 · 项目/任务」「轮次结束 · 项目/任务」与工具摘要，正文前缀不影响角色；旧消息或未声明来源的 CLI 消息显示「来源未知」。主控正文支持 Markdown 标题、加粗、斜体、列表、引用、行内代码、链接文本、语法高亮代码块和自适应表格；窄屏表格可转为逐字段展示。工具使用紧凑操作行区分执行中、完成、失败和拒绝：命令默认预览两行，输出预览三行，失败优先展示输出末尾；文件修改默认展示前三个路径和变更类型。截断提示显示省略数量，F4 展开完整记录。命令、工具输出和用户输入按原始文本展示，不解释其中的 Markdown。对话和输入框随终端宽度展开，两侧各保留两列留白，浏览历史时新回复不会将视图拉到底部。复制复用 Console 的剪贴板机制：SSH 只显示已请求复制，不宣称系统剪贴板已确认更新。主控结束本轮后可以等待任务反馈；完成事件会再次唤醒它，无需保持 Agent 打开。主控忙碌时，Twig 消息先排队；第一版没有独立的暂停按钮或自主预算配置。

离开 Agent 后，主控完成新一轮回复时，Console 和工作台显示 8 秒浮动提醒；多个主控回复合并提醒。任务完成卡片优先，主控卡片随后显示。顶部 `Agent*` 表示有未读，工作台底部显示「Agent 有新消息」；主控卡片显示期间，在导航或工作台按 Ctrl-W f 直接进入 Agent，保留工作台进程。卡片消失后 Ctrl-W f 不跳转；仍可在导航按 Tab 查看，或从工作台先 Ctrl-W d 返回再按 Tab。进入 Agent 清除主控未读和卡片，查看 Agent 期间收到的完成事件静默。首次发现的历史轮次不提醒；未读与采集进度会持久化，重启后恢复未读并补查已观察会话的新完成，不自动发飞书。存储位置见 [Console 通知说明](console.md#本轮完成通知)。

## 主控 Skill

主控使用 `twig-controller` skill，源文件为仓库 `skills/twig-controller/SKILL.md`。`make install` 将它复制更新到 `~/.twig/controller/workspace/.agents/skills/twig-controller/SKILL.md`，不安装到 Codex 主目录。该副本随安装覆盖更新，自定义约定请放在其他文件。打开 Agent 或执行 `agent init` 不再安装或修改 skill；新主控指令只引用工作目录下的 `.agents/skills/twig-controller/SKILL.md`，已有主控需显式阅读。

使用 `cargo install` 或直接运行构建产物时，需自行复制该 skill；使用自定义 `--config` 时，将 skill 放到相应主控工作目录的同一相对路径，默认安装不访问这些独立工作目录。

skill 规定每次派活必须传递实际 Twig 可执行文件、配置路径和回报命令，要求子任务阶段完成或阻塞时用 `agent send --from-project PROJECT --from-task TASK --message ...` 主动回报主控（不带接收方 `--project/--task` 参数）。回报包括任务身份、结果、修改、验证和未完成事项。受管完成回调仅作为唤醒线索，主控仍须核对验收；普通会话尤其不能依赖自动回调。

子任务回报应提供 `--from-project` 和 `--from-task`（必须成对使用）；它们是发送方声明，用于界面展示，不是身份认证，也不会接管普通会话。安装更新后的 skill 后，已有主控需重新读取 skill，后续派活才能使用新回报命令。

## 创建与派发任务

主控使用与用户相同的 CLI，普通任务仍使用原有启动配置。受管任务显式使用 `--agent`：

```sh
twig agent init
twig new myapp fix/login --agent --no-open
twig agent send --project myapp --task fix/login --message '修复登录失败，补充回归测试，更新任务状态。'
twig agent read --project myapp --task fix/login
```

`new --agent` 创建 Worktree、`.twig/state.md` 和绑定的 Codex session，再创建专用 tmux 窗口，以 `codex resume --remote ... THREAD_ID` 连接原 session。它替代本任务的 `[tmux]` 启动配置，不额外执行配置中的启动命令，避免重复启动 Agent。普通 `new` 不创建受管 session 或状态文件。

新任务状态文件记录目标、进展、阻塞、下一步和验证结果，由任务 Agent 维护，主控读取。该文件通过共享 Git 目录 `info/exclude` 中的精确规则 `/.twig/state.md` 忽略，不忽略项目 `.twig/config.toml`。若新分支已经包含同名文件，报告冲突并保留现场，不覆盖。不迁移或接管旧任务指受管生命周期边界，不禁止读取或在用户授权下直接与已有 Codex session 通信。

失败可能已经创建 Worktree、session 或窗口。先检查 `twig ls` 和 `twig agent status`，不要重复执行 new 或删除记录来重试。

`open` 重建缺失的受管窗口时连接原 session。`stop` 中断受管任务当前轮次、取消其尚未投递的消息并关闭窗口；重新 open 才允许继续投递。共享 daemon 不会被停止。删除仍保护活动会话、排队/待核对消息和文件；`.twig/state.md` 属于 ignored 文件，须按现有规则显式选择 `--delete-ignored` 才可删除。

## 普通任务中的现有 Codex session

未绑定不等于不可通信。需要区分两层能力：

| 类型 | 通信方式 | Twig 提供的保障 |
| --- | --- | --- |
| Twig 受管任务 | `twig agent send/read --project PROJECT --task TASK` | 明确的 session 绑定、持久化投递队列、完成回调和受管生命周期保护 |
| 普通任务中的现有 Codex session | 当前 daemon 可见时，经核对与授权，通过底层 Codex app-server 接口通信 | 不自动获得受管绑定、投递队列、完成回调或受管 session 的停止/删除保护；普通任务原有资源与进程检查不变 |

普通任务可能只有 Shell，也可能运行 Codex；其 session 不一定被当前 daemon 看见。`twig agent send/read` 的未绑定错误仅说明该命令不支持此目标，不能据此断言 session 不存在或无法通信，也不要为获得绑定而重复 new、迁移或替换原 session。

发现与核对路径：

1. 用同一 `--config` 下的 `twig ls PROJECT` 取得目标任务 Worktree 的完整路径。用 `codex app-server daemon version` 查询当前服务和 socket，不启动或重启服务。
2. 通过支持该服务传输方式的 Codex app-server 客户端连接原 socket，完成 `initialize` / `initialized` 握手。分页调用 `thread/loaded/list`，再对候选 ID 调用 `thread/read`（`threadId`、`includeTurns: false`），核对返回的 ID、规范化后的 cwd 和状态。这里列出的是 RPC 方法，不是 Twig 子命令。
3. cwd 只用于筛选，不能单独证明是目标会话。同一目录可能有多个 session；结合用户指定 ID、会话标题和必要的历史核对，无法唯一确认时先请用户指定。读取历史按当前协议使用 `thread/turns/list` 分页，支持时也可用 `thread/read` 的 `includeTurns: true`；空会话可能尚无 rollout。列表未命中只表示当前服务未发现，不能推断其他服务或原生终端中没有会话。
4. 向原 session 发送前必须有用户对目标及消息内容/任务范围的授权；已有明确授权无需重复确认。发送前再次核对 ID、cwd 和状态。对已加载且 idle 的目标，可用底层 `turn/start` 指定原 `threadId` 和文本 input；不要创建替代 thread。忙碌、等待审批或不可见时先核对现场，不盲目投递或恢复。idle 不是锁，其他客户端可能在检查后开始新轮次。
5. 记录请求、目标 ID、客户端消息 ID（当前协议支持时）和返回的 turn ID，再查询历史核对结果。超时或断连可能已经送达；无法确认时报告结果未知，不自动重发。接受消息和完成一轮都不等于任务通过验收。

这条路径不修改 Twig 绑定、notify 配置或任务生命周期，也不会让 `twig agent drain` 替直接发送的消息排队或对账。底层调用仍遵循 Codex 自身权限和审批；接口参数以当前安装版本为准，参考 [Codex app-server 官方协议](https://learn.chatgpt.com/docs/app-server)。Twig 本次不提供普通 session 的直接发送 CLI。

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

主控创建、恢复和通过 Twig 投递新轮次时显式使用无需审批、完整访问权限（YOLO）；已有主控在下次初始化、恢复或投递时应用。正在执行的轮次不会因此被中断或重新发送。受管子任务仍沿用 Codex 自身权限配置。主控工具没有额外白名单或授权系统，仍须遵循用户指令。Git/tmux 归属、文件保护、显式推送等现有规则不变。第一版不自动处理复杂审批表单；离线等待审批的任务需要用户回到原生会话处理。

## 持久化与恢复

默认主控目录为 `~/.twig/controller/`：`workspace/` 保存主控 AGENTS.md 和全局 state.md，`runtime.json` 保存 session 绑定、消息和完成事件。非默认配置文件名使用配置路径标识隔离主控数据，避免同目录多个配置串用。Codex 自身保存对话与轮次历史。初始化指令和 workspace/AGENTS.md 不会随 Twig 升级自动覆盖；已有主控需要重新阅读最新 `twig guide agent` 和 `twig guide automation` 获取边界说明。

Twig 后台持续调度消息及补查完成状态。Codex notify 启动短命 CLI 客户端向后台转交完成事件；主控自己的完成回调用于消费队列，不会生成无限的自我通知。回调使用创建会话时的 Twig 可执行文件绝对路径，因此不要在任务运行中删除该二进制。安装或移动后，应重新进入 Agent，并核对受管任务的运行配置。

消息状态包括 queued（等待）、sending（请求发出但结果待核对）、sent（Codex 已接受）、failed（归属等检查失败）、cancelled（停止任务时取消）。sent 不代表任务已完成。对 sending 消息，drain 尝试用客户端消息 ID 核对历史；无法确认时不自动重发，也不跳过它发送后续消息。请查看会话现场后处理，不要盲目重复派活。确认不再需要投递后可用 `agent cancel --id ID` 解除队列阻塞；取消记录不会中断或撤回已经被 Codex 接受的工作。

daemon 重启后，进入 Agent 或运行 drain 可恢复已有历史的绑定会话和队列；空主控尚无 rollout 时，Agent 或 `agent init` 会确认本地没有消息或完成记录后重建空绑定，保留 AGENTS.md 和全局 state.md；存在任何消息记录则拒绝自动替换。Agent 遇到连接失效会重新检查，连接仍不可用时用 F5 重试。没有 Console 或完成回调时，Twig 后台也会周期补查并推进队列。当前完成回调不保证覆盖失败、中断和等待审批事件，Agent/Console 可查看这些状态。

忙时排队由 Twig 命令锁协调。若同时通过原生 Codex 终端输入，检查空闲到发送之间仍有竞争，Codex 可能将消息并入该轮次；不要把它视为跨所有客户端的严格事务队列。

Agent 页面只维护后台客户端；发送先由后台持久化，投递、历史与状态读取由后台完成。切页保留草稿和拒收错误；退出前将前端已接受的发送命令转交后台，后台已接受的消息继续执行，退出不停止 Codex。单个受管会话补查或投递失败会报告具体会话及错误，不阻止主控连接或其他会话投递；未知投递仍不自动重发。

## 飞书双向应用机器人

首次接入请按[飞书机器人申请与配置指南](feishu.md)操作（`twig guide feishu`），包含个人创建团队、获取 open_id、权限申请、先启动客户端再保存长连接、发布验证与故障排查。

第一版支持企业自建应用机器人、允许用户单聊和文本消息，使用长连接接收 `im.message.receive_v1`，通过应用消息 API 回复。群聊、附件、卡片按钮、审批和多用户独立主控不在范围内。多个允许用户共享同一主控上下文与完整访问权限，因此只加入可信的用户 open_id。

在飞书开放平台启用机器人，申请 `im:message.p2p_msg:readonly`（接收单聊）与 `im:message:send_as_bot`（应用身份发消息）权限，发布并让目标用户处于应用可用范围。事件配置选长连接并订阅接收消息事件；不需要在本机开放公网 HTTP 端口。权限说明见 [飞书机器人文档](https://open.feishu.cn/document/develop-robots/add-bot-to-external-group?lang=zh-CN)，长连接帧实现参照 [官方 SDK](https://github.com/larksuite/oapi-sdk-python/tree/v2_main/lark_oapi/ws)。

```sh
export TWIG_FEISHU_APP_ID='你的应用 ID'
export TWIG_FEISHU_APP_SECRET='你的应用密钥'
export TWIG_FEISHU_ALLOWED_USERS='ou_用户一,ou_用户二'
# 已在运行时先有序停止并等待退出，再从此环境启动。
twig daemon start
twig daemon status
```

三个变量必须一起设置；不配置则禁用双向通道。生产 API 固定使用 `https://open.feishu.cn`；`TWIG_FEISHU_API_BASE` 仅允许官方地址或隔离验收的本机 HTTP 地址。连接需要 HTTPS 与服务发现返回的 WSS 地址可达。密钥放入本地受限环境文件或服务配置，不写到仓库、任务或日志。

后台先持久化接收记录，再确认平台事件；按平台 message_id 去重，以稳定 Twig 消息 ID 进入共享队列。首次合法单聊激活该目的地，并补齐当前主控完整历史。之后持续同步 Console/CLI/飞书输入、主控回复与进展、工具调用及结果、文件补丁、子任务回报和轮次错误，不需要再发一条飞书消息才能收到后续进展。排队、失败和取消的主控输入也保留。

Console 和飞书共用历史解析与来源映射。Console 的工具详情用 F4 展开，飞书按条目 ID 分段发送，内容变化追加“更新”版本；后台重启从持久化进度继续。飞书仅同步主控会话，不读取或提供子任务内部会话访问。多个已接入的允许用户均能看到完整主控历史，包括其他允许用户和其他入口的消息。

`daemon status` 的 `feishu.history_pending` 是待发历史分段数，`history_error` 为采集错误，`history_errors` 为目的地错误，`unknown_replies` 包含结果未知的历史发送。未知写结果保留 sending，阻塞该目的地后续历史，不自动重发；核对原飞书会话和受限的 `feishu.json`、`feishu-history.json` 后再决定人工处理，不能删除记录诱导重新派活或重播。完整操作说明见[飞书指南](feishu.md)。

## 飞书主动 webhook 通知

通知读取 `TWIG_FEISHU_WEBHOOK_URL`，向飞书机器人发送文本。缺少变量或飞书返回错误时明确失败，不影响其他命令。URL 不写入任务文件、消息记录或命令行参数。此主动通知渠道支持仅需 webhook URL 的机器人；如机器人额外要求关键词，消息需包含所配置的关键词；暂不实现签名密钥配置。

请求超时可能已经送达，不自动重试以避免重复通知。主控或用户核对后可再次调用。自动化验收只使用本地模拟 HTTP 服务，不发送真实飞书消息。

通过 `twig agent send` 和完成回调产生的消息，在确认投递（`sent`）后也按接收方标为未读：发往子任务的消息进入该任务未读，发往主控的消息点亮 `Agent*`。任务消息卡片显示「Agent 收到新消息」，沿用跳转、查看时静默及进入后清除的规则；同一任务在未读列表合并计数，按最近事件排序。排队、失败和结果未知的消息不计入；首次读取已送达历史只建立基线，按消息 ID 去重。原有轮次完成提醒保留，未读跨重启保留，进入对应任务或 Agent 后清除。此范围不包含绕过 Twig 直接调用 Codex 接口发送的消息。

未读采集遇到配置读取或 Git 查询失败时，Console 显示诊断并在后续采集中重试；确认任务取消登记或绑定已变化后丢弃旧消息提醒。同一任务的完成与消息通知共享跳转身份，进入后可正确关闭当前卡片。
