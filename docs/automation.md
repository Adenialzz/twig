# Agent 与脚本调用

命令行阅读：`twig guide automation`。人和 Agent 使用同一套命令与文档，不需要专用输出格式。

## 先发现，再执行

```sh
twig --help
twig guide
twig new --help
twig guide windows
```

顶层帮助用来找命令，子命令帮助说明参数和关键副作用，guide 提供完整规则。需要一次获取全部说明时执行 `twig guide --all`。帮助和 guide 离线可用，不读取登记表，不依赖 Git/tmux 或终端；输出可以保存或作为上下文读取。

项目必须显式选择，不能把当前目录当作目标：

```sh
twig ls
twig ls myapp
twig new myapp fix/bug --no-open
```

从 new 的输出或 ls 的路径列取得 Worktree 路径，在该目录执行自己的编辑、测试和 Git 命令。twig 不会替调用者切换父进程工作目录，也不把任务创建成功解释为任务完成。

## 非交互行为

| 操作 | 调用方式与注意事项 |
| --- | --- |
| 阅读文档 | --help / guide，直接输出文本，不自动打开分页器 |
| 查看资源 | ls / ls PROJECT；任务列表使用项目锁，不启动程序 |
| 创建环境 | new PROJECT NAME --no-open；仍创建 tmux 窗口并执行配置命令 |
| 进入窗口 | open 和默认 new 有终端交互；自动化准备环境用 new --no-open |
| 浏览面板 | 裸 twig / console 需要终端，脚本不要用它取得状态 |
| 停止或删除 | 用户已授权对应操作时才使用 --yes；它仅跳过确认 |
| 取消登记 | rm PROJECT 立即执行，不弹确认 |
| PR/MR | pr 显式联网，只有 --push 才推送；预先准备认证 |

确认只接受 y 或 Y；空输入、EOF 或其他回答取消。如果 stdin 保持打开且没有输入，确认会等待；需要无人值守时明确指定 --yes，不依赖超时杀死写操作。

## 判断结果

stdout 包含正常结果，stderr 包含错误和诊断；同时检查退出码与内容。退出码 0 通常成功，1 是执行失败或取消，2 是参数错误。ls 的 Mismatch 可伴随 0 返回，应读取状态与诊断。文本表格不承诺固定列宽，不要依赖空格位置解析带空格路径。

- new 成功：环境已准备，不代表 Pane 程序启动成功或服务就绪。
- ls 的 Open：窗口存在，不代表 Agent 正在运行。
- Clean：普通 Git status 干净，不包括 ignored 文件，也不证明已经合并/推送。
- Console 空闲或最近轮次完成：不表示开发任务已完成。
- pr 成功：请求已找到或创建，不表示通过审查、CI 或合并。

## 副作用与重试

普通命令不隐式 fetch/pull/push，但创建窗口时用户配置的程序可能联网、写文件或启动服务。--no-open 不能用于试运行。

new 拒绝已登记的同名任务。open 重用已有窗口，不重复运行启动命令；窗口缺失时会启动新程序。失败可能已经创建分支、Worktree、窗口或远端提交，先阅读错误和 ls，按 [故障排查](troubleshooting.md) 操作。不要通过删除配置或强制清理来达到“重新开始”。

用 --config 可选择独立登记表，但不同登记表仍可能访问同一份外部 Git/tmux 资源。避免同一项目的并发写操作；锁只协调 twig，无法保护其他程序的写入。清理时确认目标、保存文件并停止相关活动进程。

完整清理规则见 `twig guide cleanup`，远端操作见 `twig guide pr`。

## 可选主控工作流

`twig chat` 提供主控入口。主控通过 `new PROJECT NAME --agent --no-open` 建立受管 session，再通过 `agent send --project PROJECT --task NAME --message 内容` 派活。`agent read` 返回会话历史，`agent status` 返回本地绑定和消息状态；sent 仅表示 Codex 接受，须另外核对任务结果。普通 new 保留原行为。详见 [主控 Agent](agent.md)。

## 已有普通任务与受管任务的通信边界

`twig agent send/read` 面向主控和已绑定的受管任务，受管绑定同时带来持久化投递队列、完成回调和生命周期保护。普通任务没有绑定，不代表其中已有 Codex session 不能通信；也不能假定每个普通任务都运行 Codex，或都能被当前 daemon 看见。

先用 `twig ls PROJECT` 确认 Worktree 路径，再查询当前 daemon 的 `thread/loaded/list`，逐个用 `thread/read` 核对 thread ID、cwd、状态及必要的历史；同目录多个候选时不能仅凭 cwd 选一个。当前服务可见且身份已核对的已有 session，可在用户授权目标和消息范围后通过底层 Codex 接口向原 thread 发送。发送前复查状态，忙碌或不可见时先核对现场；结果未知不能自动重发。具体握手、分页、状态与结果核对见 [普通任务中的现有 Codex session](agent.md#普通任务中的现有-codex-session)。

“不迁移或接管旧任务”限制的是 Twig 受管生命周期，不禁止读取或经授权直接通信。直接通信不会自动增加 Twig 绑定、投递队列、完成回调或受管 session 停止保护，也不能依靠 `agent drain` 核对这些直接请求。普通任务原有资源和进程检查不变；不要通过 new 重建旧任务来绕过未绑定提示。

主控派活规范由 `twig-controller` skill 提供，入口见 [主控 Skill](agent.md#主控-skill)。每次派活都要求子任务主动回报结果或阻塞，明确回报命令所用的可执行文件和配置绝对路径；自动完成回调不替代结果回报。
