# 飞书机器人申请与配置

命令行阅读：`twig guide feishu`。

本指南从申请应用开始，配置可向 Twig 主控发消息、接收回复的飞书机器人。适用于国内飞书；当前实现不支持将 API 地址切换到国际版 Lark。开放平台的菜单名称可能调整，查找时优先使用下文的权限和事件标识。

## 先选对机器人

| 方式 | 能做什么 | 需要什么 |
| --- | --- | --- |
| 企业自建应用机器人（本文主线） | 单聊发需求，同步用户输入和 Codex 最终回复 | App ID、App Secret、允许用户 open_id、长连接事件订阅 |
| 群里的自定义 webhook 机器人 | Twig 主动向群里发通知 | webhook URL；不能接收用户发给 Twig 的消息 |

双向通道目前只处理允许用户的**单聊文本**，不处理群聊、附件、图片、卡片操作或审批。飞书和 Console 使用同一主控、项目登记与消息队列。多个允许用户也共享上下文，不是每人独立一个 Agent。

主控使用 **YOLO：无需审批、完整访问权限**。建议先只允许你自己；加入其他人意味着允许其向这台机器的主控下达任务，并在接入后看到主控的用户输入和最终回复，包括其他入口及其他允许用户的消息。

## 1. 个人用户准备团队身份

个人可以自己使用这套方案。推荐先在飞书创建一个自己的团队/组织，再以这个组织的身份创建企业自建应用，并以同一身份和机器人聊天。

1. 注册并登录飞书，在客户端的身份/组织切换入口找到创建团队或组织，按页面完成创建。
2. 切换到新组织。创建者成为该组织的管理员，可以管理应用发布；使用已有公司的组织时，则按公司规则请管理员审批。
3. 后续登录开放平台和在客户端聊天，都确认选中的是这个组织。

飞书官方的[机器人实践教程](https://open.feishu.cn/community/articles/7290834975460655106?lang=zh-CN)建议个人版用户先创建企业，以免应用无法审核；创建组织的入口与角色见[创建组织指南](https://www.feishu.cn/hc/en-US/articles/985882202335-step-1-create-an-organization-and-invite-members)。账号资格、认证和可用额度以当前后台提示为准。

如果你坚持用组织外的个人身份聊天，会涉及机器人对外共享及认证条件，见[外部用户单聊说明](https://open.feishu.cn/document/develop-robots/add-bot-to-external-group?lang=zh-CN)。首次配置使用同一组织身份即可，不必先开启对外共享。

## 2. 准备运行 Twig 的机器

需要：

- 已安装当前版本 Twig 及主控 skill，安装方法见[安装指南](installation.md)。
- Git、tmux、curl，以及已配置登录/模型、支持 app-server daemon 的 Codex。
- 能访问 `https://open.feishu.cn`，以及飞书服务发现返回的 WSS 长连接地址。
- 持续开机且联网的机器；无需公网 IP、域名、反向代理或入站 HTTP 端口。

Twig 后台和 Codex daemon 是两个进程。前者收发消息、管理队列，后者运行主控。由你启动 Codex：

```sh
codex app-server daemon start
```

如果 Codex 已运行，直接使用原服务。Twig 不接管其启动、重启或停止。这里只准备 Codex；Twig 后台等第 5 步凭证就绪后再启动。

## 3. 创建应用并开启机器人

1. 打开[飞书开发者后台](https://open.feishu.cn/app)，选择第 1 步的组织。
2. 点击「创建企业自建应用」，名称可填 `Twig`，按页面填写描述和图标。不要选择商店应用，也不要只在群里添加自定义 webhook 机器人。
3. 进入应用的「添加应用能力」，添加「机器人」能力，完成机器人名称等必要设置。
4. 在「凭证与基础信息」中找到 **App ID** 和 **App Secret**。App ID 通常以 `cli_` 开头。保存到自己的受限配置中，不发到聊天、截图、仓库或问题报告中。
5. 在「权限管理」中搜索并开通下面两个**应用身份**权限：

| 权限标识 | 用途 |
| --- | --- |
| `im:message.p2p_msg:readonly` | 接收用户发给机器人的单聊消息 |
| `im:message:send_as_bot` | 以应用机器人身份发送回复 |

权限名称可能随页面更新，以标识为准。Twig 的这条通道不需要为了收发消息而开通全量通讯录、全部群消息或文档权限。接收事件和回复接口分别见[接收消息事件](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/message/events/receive)、[发送消息 API](https://open.feishu.cn/document/server-docs/im-v1/message/create)与[回复消息 API](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/message/reply)。权限是否生效还取决于第 7 步的发布和审批。

## 4. 获取自己的 open_id

Twig 用发送者的 `open_id` 做允许列表。它通常以 `ou_` 开头，且**同一个人在不同应用下的 open_id 不同**。不能复制其他应用的 ID，也不能用机器人 ID、`user_id`、`union_id` 或 `chat_id` 替代。

最方便的方式是在 API 调试台选择自己，直接复制 Open ID：

1. 打开[获取单个用户信息](https://open.feishu.cn/document/server-docs/contact-v3/user/get)，点击页面里的「API 调试台」。
2. 在调试台选择刚创建的 **Twig 应用**，确认 App ID 与 `TWIG_FEISHU_APP_ID` 一致，同时核对组织身份。
3. 将查询参数 **`user_id_type` 设置为 `open_id`**，再打开用户 ID 参数旁的用户选择/获取 ID 工具。
4. 在组织成员中找到自己的账号，点击「复制成员 ID」，保存得到的完整 `ou_...`。选择的是你本人，不是机器人。

这里只借助调试台的选择工具获取 ID，**不必执行“获取单个用户信息”接口**，也不需要执行发送消息或修改用户的 API。如果页面没有选择工具，按官方[Open ID 获取说明](https://open.feishu.cn/document/home/user-identity-introduction/open-id)查找当前入口，不要为此盲目扩大通讯录权限。

将复制的 ID 写入 Twig 后台使用的环境文件（完整配置见第 5 步）：

```sh
TWIG_FEISHU_ALLOWED_USERS='ou_替换为当前应用下自己的完整ID'
```

已有后台需要在修改后有序重启，新的允许列表才会生效；仅在另一个终端 export 不会更新运行中的后台。

API 调试台选取用户 ID 和跨应用 ID 错误说明可查[官方错误码文档](https://open.feishu.cn/document/server-docs/api-call-guide/generic-error-code)。如果选不到自己，先核对组织身份、应用可用范围及页面要求的发布状态；必要时先发布一版，再继续配置。

## 5. 配置凭证并启动 Twig

双向通道需要三个环境变量，一起设置，均不能为空：

| 变量 | 填写内容 |
| --- | --- |
| `TWIG_FEISHU_APP_ID` | 当前应用的 App ID |
| `TWIG_FEISHU_APP_SECRET` | 同一应用的 App Secret |
| `TWIG_FEISHU_ALLOWED_USERS` | 自己的 open_id；多人用英文逗号分隔，例如 `ou_aaa,ou_bbb` |

三个变量全部不设置时禁用双向通道，只设置其中一部分会报错。生产环境不需要设置 `TWIG_FEISHU_API_BASE`；它仅允许官方地址或本机隔离测试地址。

### 首次手动启动

先创建受限的本地文件，使用编辑器填写，避免把真实密钥直接输入 Shell 命令历史：

```sh
mkdir -p ~/.config/twig
(umask 077; touch ~/.config/twig/environment)
chmod 600 ~/.config/twig/environment
# 用编辑器打开 ~/.config/twig/environment，填写下面三行并替换占位内容。
```

文件内容（不加 export，供后续 systemd 样例复用）：

```sh
TWIG_FEISHU_APP_ID='cli_替换为实际应用ID'
TWIG_FEISHU_APP_SECRET='替换为实际应用密钥'
TWIG_FEISHU_ALLOWED_USERS='ou_替换为当前应用下自己的ID'
```

Twig 不会自动读取这个文件。手动启动时，在同一个 Shell 中加载自己维护的文件：

```sh
set -a
. "$HOME/.config/twig/environment"
set +a
twig daemon start
twig daemon status
```

不要加载来源不明的环境文件，Shell 会执行其中的内容。若 Twig 已经运行，先 `twig daemon stop`，再用 `twig daemon status` 确认已报告未运行，之后加载环境并重新启动。停止会等待已接受的操作，不能因为尚未退出就反复启动。

`daemon status` 输出 JSON，先检查 `feishu.enabled` 为 `true`，稍后 `feishu.connected` 应变为 `true`。连接过程异步进行，第一次查询尚未连上不一定是错误；持续失败时看 `feishu.error`。连接成功仅证明长连接已建立，后面仍需配置事件和发布应用。

### 已使用系统服务或自定义配置

- **systemd**：仓库样例读取 `~/.config/twig/environment`。修改文件后运行 `systemctl --user restart twig.service`，再检查 `twig daemon status`。在终端执行 export 不会更新已运行的服务。
- **launchd**：将三个变量加入实际使用的 plist 的 `EnvironmentVariables`，本地文件设为 `0600`；卸载旧服务、等待退出，再按[后台指南](daemon.md)重新 bootstrap。plist 不会自动读取上述环境文件。
- **自定义配置路径**：所有操作使用相同的 `--config`，例如 `twig --config ~/.twig/work.toml daemon start`、`twig --config ~/.twig/work.toml daemon status`。服务中的路径也须一致，否则检查的是另一个实例。

这些变量属于 **Twig 后台的启动环境**，不是 `config.toml` 的配置项，也不需要传给或重启 Codex daemon。

## 6. 保存长连接并订阅消息事件

保持第 5 步的 Twig 后台运行，回到飞书应用后台：

1. 进入「事件与回调」→「事件配置」。
2. 编辑订阅方式，选择「使用长连接接收事件」并保存。保存时需要有在线长连接，所以先启动 Twig，再完成本步骤。Twig 已内置客户端，不需要再运行一份官方 SDK 示例。
3. 点击「添加事件」，搜索「接收消息」，选择标识为 **`im.message.receive_v1`** 的事件。页面可能显示「接收消息 v2.0」，仍以该标识为准。
4. 确认该事件已经出现在已订阅列表，且相关权限已申请。

这是事件配置，不是填写群机器人 webhook，也无需填写公网回调地址、Verification Token 或 Encrypt Key 到 Twig。协议实现参照[飞书官方长连接 SDK](https://github.com/larksuite/oapi-sdk-python/tree/v2_main/lark_oapi/ws)。

一个应用建议只交给一台目标机器、一个 Twig 配置实例接收。不要同时运行使用相同凭证的测试客户端或第二台 Twig，以免其他连接接收事件，导致目标机器漏收。

## 7. 发布应用并开放给自己

1. 进入「版本管理与发布」，创建版本，填写版本号和更新说明。
2. 核对机器人能力、消息权限和事件配置，设置应用可用范围，至少包含你用于聊天的组织账号。
3. 按页面提交审核/发布。若需要管理员审批，在管理后台完成，或请组织管理员处理。
4. 确认版本已发布，权限已生效。之后修改权限或能力时，按页面提示重新发布。

创建应用、保存配置和发布生效是不同步骤。审核是否自动通过由组织规则决定，见[自建应用发版审核指南](https://www.feishu.cn/hc/zh-CN/articles/374230668270-%E8%87%AA%E5%BB%BA%E5%BA%94%E7%94%A8%E5%8F%91%E7%89%88%E5%AE%A1%E6%A0%B8%E6%8C%87%E5%8D%97)。如果前面为获取 ID 等操作发布过初始版本，这里仍须核对最终配置已经生效。

## 8. 单聊验证

1. 在飞书客户端切换到应用所属组织，在工作台或搜索中找到 `Twig` 应用/机器人，打开单聊。
2. 发一条普通文本，例如：`只回复“Twig 飞书已连通”，不要执行文件或命令操作。`
3. 正常情况下收到已接受/排队反馈，并收到本次输入和最终回复，不补发接入前的历史。随后持续收到新输入及每轮最终回复。“已接受”不等于任务完成。
4. 用同一配置打开 `twig chat`，可以查看共享主控上下文。首次飞书消息会初始化或复用主控，不要求先打开 Console。
5. 关闭 Console 后再发一条文本，确认后台仍可处理；不要停止 Twig 或 Codex 服务。

首次合法单聊激活同步，只为已存在的内容建立基线，不补发历史；随后显示本次输入和完成后的最终回复。接入时正在运行的旧轮次，其接入后才生成的最终回复仍会同步。之后即使没有新的飞书输入，也持续同步进入主控会话的 Console/CLI/飞书用户输入和每轮最终回复。工具调用、工具结果、文件补丁、中间进度和子任务回报不在飞书展示，Console 继续保留完整历史。子任务内部会话仍不可通过飞书访问。

消息只标注「User」或「Codex」，长文本分段完整发送；不显示内部会话 ID、条目 ID 和投递状态，不流式发送中间回答。原始请求的接收、失败或中断反馈仍保留。你在飞书发出的输入也会同步为 User 消息，方便多个已接入单聊看到相同输入。

每个单聊独立保存进度；普通重启继续补发离线期间的新输入和最终回复，不重复已成功分段。已发送消息不撤回，结果未知的发送记录保留并继续阻塞该单聊，避免盲目重试。尚未发送合法单聊文本的允许用户不主动推送；移除允许用户并重启后停止同步。飞书发送成功不清除 Console 未读。

## 9. 持续运行、更新与密钥变更

验证通过后按[后台指南](daemon.md)设置 systemd 或 launchd。不要让手动后台与托管服务争用同一个配置。安装 Twig 不会自动安装服务。

系统托管负责进程生命周期，不保证休眠、断电、断网期间可用。Linux 用户服务如需注销后继续运行，应按本机策略配置 linger；macOS 用户 LaunchAgent 在登录后运行，不等于无登录常驻服务。Codex 服务也需保持可用。

重新安装二进制不会替换已运行进程中的代码。手动后台日常更新可用 `make install-restart`，仅重启可用 `twig daemon restart`，保留原飞书环境，不必重新加载 `.env`。变更 App Secret、允许用户或其他环境时，须 stop 并等待退出，加载新环境再 start；restart 不会重新读取 `.env`。托管实例使用服务管理器。首次升级到支持 restart 的版本仍需从原环境 stop/start 一次。无需仅因飞书配置变化重启共享 Codex。完整升级、备份和停止顺序见后台指南。

## 故障排查

先运行与服务相同配置的 `twig daemon status`，再按下面的现象定位：

| 现象 | 检查与处理 |
| --- | --- |
| 后台未运行或启动失败 | 查看启动错误；三项变量是否完整；后台 PATH 能否找到 curl；是否有其他实例占用同一配置 |
| `feishu.enabled` 为 false | 运行中的后台没有读取三项变量。核对服务环境和配置路径，加载后重启 Twig |
| `connected` 持续 false | 查看 `feishu.error`；核对 App ID/Secret 是否来自同一应用、机器人能力及 HTTPS/WSS 网络连通性 |
| `worker_errors` 非空或日志出现 CryptoProvider panic | 接收/发送线程已异常退出，查看 daemon.log 或服务日志；旧版缺少 TLS 加密提供者的缺陷已修复，更新 Twig 并有序重启。凭证认证成功不代表 WSS 握手成功 |
| 保存长连接提示没有在线客户端 | 保持 Twig 运行且 connected 为 true，核对开放平台当前选中的应用是否与凭证一致 |
| 已连接，但发消息完全没反馈 | 检查订阅事件、发布及权限、可用范围；确认是单聊纯文本、使用同一组织身份，且发送者 open_id 在允许列表中；检查其他机器或 SDK 是否也连接该应用 |
| 搜不到应用或无法单聊 | 核对已发布版本、用户可用范围及当前组织身份；不要先按外部用户路径排查同组织使用 |
| `feishu.message_errors` 有内容 | 按消息 ID 查看错误，再核对 Codex 可用性、主控绑定、投递和回复 API 的权限；必要时结合 `twig agent status`、`twig agent read` 查看主控状态 |
| 历史或回复尚未到齐 | 先看 `history_pending` 是否还有待发分段，再看 `history_error`（采集）与 `history_errors`（目的地发送）；检查主控是否忙碌、等待交互或投递失败，不要连续重发同一执行指令 |
| `feishu.unknown_replies` 非零 | 回复 HTTP 请求结果未知，可能已送达。先检查原飞书会话和对应持久化记录；该单聊后续历史会等待缺口处理，其他单聊不受影响。Twig 不自动重发，当前也没有自动重试未知回复命令 |
| 终端 export 后行为没变化 | 环境没有传入旧进程；重启正确的 Twig 实例，系统托管的要改服务配置 |
| 关闭窗口后失联 | 确认关闭的是 Console，后台及 Codex 仍运行；检查机器是否休眠、注销导致服务退出或网络中断 |

网络超时时检查 `http_proxy`、`https_proxy`、`no_proxy`，不要公开含代理凭证的完整环境。curl 的 HTTP 请求可使用其代理配置；当前原生 WSS 连接直接建立 TCP/TLS，不会自动套用 HTTP 代理。HTTP 能通不代表长连接可达。

运行目录在所选配置旁的 `daemon/<配置身份>/`。手动启动诊断看 `daemon.log`，运行故障看 `events.log`；托管服务还可查看 journal/launchd 日志。`feishu.json` 保存接收与请求反馈，`feishu-history.json` 保存完整主控条目水位与出站分段。两者可能包含消息正文、命令输出和补丁，不是可直接公开的诊断日志。

后台接收先落盘，再确认平台事件，并按平台消息 ID 去重。未知回复保留 `sending` 状态，不自动重发。不要删除 `feishu.json`、`feishu-history.json` 或队列记录来“重新试一次”，否则可能破坏去重并重复执行任务；先核对原会话和执行结果，再决定人工补发。

## 可选：只发通知的 webhook 机器人

如果只想让主控往群里提醒，可以独立配置这条旧通道，也可以和双向应用机器人同时使用：

1. 在目标飞书群的机器人管理中添加「自定义机器人」，按页面设置名称及安全规则，复制 webhook URL。
2. 将下面一项加到 Twig 后台使用的环境文件/服务配置，重启 Twig：

   ```sh
   TWIG_FEISHU_WEBHOOK_URL='替换为群机器人的实际webhook地址'
   ```

3. 手动验证（这条命令会真实发送消息）：

   ```sh
   twig agent notify --message 'Twig 通知通道测试'
   ```

该通道不需要前三项应用变量，不提供消息接收。机器人若要求关键词，正文须包含该关键词；当前 Twig 未实现 webhook 签名密钥配置，不能直接接入强制签名校验的机器人。请求超时可能已经送达，不自动重试。仅配置 URL 不会自动将每条 Agent 消息转发到群里，需由用户或主控显式调用通知命令。
