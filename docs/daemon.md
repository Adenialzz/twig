# Twig 后台

命令行阅读：`twig guide daemon`。

Twig 后台统一执行项目登记、任务生命周期、Agent 消息和飞书通信，持续采集状态并保存共享未读。Console 与 CLI 都连接它；关闭所有窗口不停止已接受的工作。每个 `--config` 有独立实例，不隐式启动、不回退到前台写入。

## 启动与健康状态

```sh
twig daemon start           # 显式后台启动，等待就绪
twig daemon restart         # 手动后台保留原环境，有序重启并等待新版就绪
twig daemon status          # JSON：实例、采集时间/错误、客户端、队列、飞书
twig daemon stop            # 有序退出，保留任务和 Codex 进程
twig daemon run             # 前台运行，供调试或系统服务托管
```

首次使用先启动后台，再执行 `add/new` 或打开 Console。普通任务不需要 Codex；主控需要用户另行启动已有 Codex daemon。Twig 从不启动、重启或停止共享 Codex 服务。

后台使用启动时的 PATH、SHELL、CODEX_HOME 和飞书环境变量；客户端的新 `export` 不会改变它。`daemon restart` 保留这些原环境变量，不读取 `.env` 或采用调用者的新 export。需要更新环境时，仍须有序 stop、等待退出、加载新环境后 start；托管实例修改服务配置后由服务管理器重启。业务命令不将客户端环境通过 RPC 传入；`add` 的相对路径由客户端解析。

`status` 中的 `collection` 展示登记、Codex 与任务采集的年龄和错误；`agent_queue` 为投递状态计数，`operations` 为排队数量和运行/待核对阶段；`feishu` 展示连接、待处理请求、未知回复数和按消息 ID 记录的错误；`history_pending` 是待发主控历史分段，`history_error` 为历史采集错误，`history_errors` 为各单聊同步错误；`worker_errors` 记录飞书接收/发送线程的异常退出，接收线程异常也会将 connected 置为 false 并设置 error。未启用 Codex 时其采集失败不影响普通任务管理。

Unix socket 位于 `/tmp/twig-<UID>/` 私有目录，按规范化配置路径隔离。后台运行目录为配置旁的 `daemon/<配置身份>/`，包含单例锁、`operations.json`、可选 `feishu.json` 与 `feishu-history.json`。配置文件尚不存在也可先启动。socket 不受客户端 TMPDIR 和启动目录影响。

## 操作进度与未知结果

写请求先准备确认计划，再携带稳定请求 ID 提交，接受后落盘。默认 CLI 等待终态；Console 表单显示实际业务阶段。可用 `TWIG_OPERATION_PROGRESS=1 twig …` 在 CLI 显示阶段。关闭界面不会撤销操作，也不能用重新运行 `new` 代替查结果。

```sh
twig operation ls
twig operation show OPERATION_OR_REQUEST_ID
# 先核对真实 Worktree、分支、窗口及遗留进程，再解除阻塞；不重跑、不回滚。
twig operation resolve OPERATION_ID --yes
```

状态为 `queued/running/succeeded/failed/needs_inspection/resolved`。`stages` 记录已经进入的阶段，最后一项不表示该阶段已完成。失败保留输出与错误；部分成功保留任务资源。提交响应丢失时按原请求 ID 查询，不能盲目重发。计划五分钟有效，执行前持锁复查资源；变化后重新确认。

正常停止等待已接受操作结束，不给写操作套用浏览超时。强制退出后，未完成操作标为 `needs_inspection` 并阻止冲突操作；核对后才能解除。停止过程可能需要等待 Git、tmux 或 Agent 请求完成。操作记录和可靠投递记录不自动清理。提交记录保存失败时操作未接受；已接受操作的进度或结果保存失败时进入 `needs_inspection`，不继续报告运行中或成功。先恢复存储、核对资源，再 resolve；解除标记保存失败会保留阻塞。

## 多窗口与断连

同一配置的 Console 共享项目、任务、状态快照与未读；焦点、表单、草稿和滚动各自独立。任一窗口实际查看目标时新事件静默，清除只作用于该客户端已经看到的事件；查看租约十秒失效，崩溃窗口不会永久吞提醒。飞书发送成功不清除 Console 未读。

后台断开时已有导航保留过期数据并显示诊断，现有 tmux 工作台继续收发终端输入；新增业务操作明确失败。重新连接会按实例和版本重新读取快照及未读，避免后台重启后误用旧版本。帮助、guide、只读 Shell 补全不依赖后台。

## Linux：systemd 用户服务

仓库提供 `examples/services/twig.service`。先核对其中二进制路径、PATH、配置和工作目录；后台需能找到 Git、tmux、ps，以及可选的 Codex/curl。安装二进制不会安装服务。

```sh
mkdir -p ~/.config/systemd/user ~/.config/twig
cp examples/services/twig.service ~/.config/systemd/user/
# 如需飞书，在 ~/.config/twig/environment 中写 KEY=value，权限设为 0600。
systemctl --user daemon-reload
# 若此前手动启动，先 twig daemon stop 并等 status 报告未运行。
systemctl --user enable --now twig.service
systemctl --user status twig.service
journalctl --user -u twig.service
```

配置变化后用 `systemctl --user restart twig.service`，停止托管使用 `systemctl --user stop twig.service`。需要退出登录后持续运行时，管理员/用户可按本机策略启用 `loginctl enable-linger USER`；不是 Twig 自动执行的步骤。机器休眠、断电或网络中断期间不能保证飞书可用。

示例使用 `KillMode=process` 保留可能由后台启动的 tmux server，`TimeoutStopSec=infinity` 等待资源写入结束；主动放弃 systemd 对整个子进程组的清理，任务由 Twig/tmux 管理。依据 [systemd.service](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html) 与 [systemd.kill](https://www.freedesktop.org/software/systemd/man/latest/systemd.kill.html)。

## macOS：launchd 用户服务

将 `examples/services/com.twig.daemon.plist` 中所有 `/Users/YOUR_USER` 替换为真实绝对路径，检查 Homebrew/Codex 的 PATH。plist 不展开 `~` 或 Shell 变量。可在 `EnvironmentVariables` 中加入飞书变量与 CODEX_HOME；包含密钥的本地副本权限设为 0600，不提交到仓库。

```sh
mkdir -p ~/Library/LaunchAgents
cp examples/services/com.twig.daemon.plist ~/Library/LaunchAgents/
# 编辑路径与环境后校验；先停止手动启动的 Twig 后台。
plutil -lint ~/Library/LaunchAgents/com.twig.daemon.plist
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/com.twig.daemon.plist
launchctl print "gui/$(id -u)/com.twig.daemon"
# 停止/卸载，修改后重新 bootstrap：
launchctl bootout "gui/$(id -u)" ~/Library/LaunchAgents/com.twig.daemon.plist
```

此示例在用户登录后运行，异常退出重启，正常停止不重启；不承诺注销后继续运行。`AbandonProcessGroup` 避免退出后台时清理任务进程组，`ExitTimeOut=0` 等待有序退出，依据 [Apple launchd 源码手册](https://github.com/apple-oss-distributions/launchd/blob/main/man/launchd.plist.5)。需要无登录托管时由管理员另行配置系统服务，不能将用户示例直接当作 root 服务运行。

## 日志与升级

手动 `start` 的启动诊断在运行目录 `daemon.log`，达到 1 MiB 后下次启动保留一份 `.1`。运行连接/操作故障写 `events.log`，达到 1 MiB 时保留一份 `.1`。系统托管的启动错误由服务管理器接收。普通诊断不记录消息正文或密钥；`operations.json`、Agent runtime 和 飞书接收/历史同步记录是受限业务数据，可能含用户消息，不能当作脱敏日志公开。

日常源码更新，已通过 `daemon start` 启动且支持 restart 的后台只需：

```sh
make install-restart
# 使用其他配置：make install-restart CONFIG="$HOME/.twig/work.toml"
```

`twig daemon restart` 向旧后台提交一次重启请求。旧后台处理完已接受的操作、停止并回收工作线程、释放 socket 与实例锁，再以调用命令所在的新版可执行文件替换自身，保留 PID、原环境和日志输出；新实例 ID 确认就绪后才报告成功。会话、消息队列、未读、任务与共享 Codex 服务保留。请求确认丢失时只按这次实例 ID 核对，不重发重启命令。等待约 30 秒仍未确认时，显示 status 与日志位置；不会强杀正在执行的操作，也不会自动再启动一个实例。安装/启动失败不删除状态或任务。

未运行时 restart 不隐式启动，以免丢失原凭证；先从所需环境 start。前台 `daemon run` 和系统托管实例拒绝该命令，使用原启动方式、systemd 或 launchd 完成重启。首次安装此功能时，运行中的旧后台无法继承新能力，需要从原环境 stop/start 一次。普通 `make install` 仍只安装；修改环境变量不使用保留旧环境的 restart。

涉及协议不兼容、首次升级或回退时，完整切换步骤：

1. 退出旧 Console/CLI，停止 Twig 后台或托管服务，等待退出。保留 tmux 与 Codex 服务，不并行运行新旧写入者。
2. 备份配置目录中的登记、projects、controller、notifications 与 daemon 记录；不要删除旧投递记录来消除报错。
3. 更新二进制及主控 skill，保持受管会话回调使用的可执行路径可用。重新启动 Twig 后台，检查 status 和 operation ls。
4. 重新打开 Console/Agent，核对已有项目、未读及会话绑定。完成回调失败由后台补查真实 completed 轮次；未知投递不自动重发。

业务协议不兼容会明确提示统一更新并重启。`daemon status/stop` 使用独立的稳定管理协议，新客户端仍能查看并有序停止旧后台；`status.version` 表示后台实际业务版本。`start` 检测到旧后台时明确提示先停止，不把旧进程当作新版已就绪。

如果你已经先更新了二进制，执行 `daemon stop` 却收到“后台协议不兼容”，先安装包含此修复的版本，然后再次执行 `twig daemon stop`，用同配置的 `twig daemon status` 等到未运行后再 `twig daemon start`。系统托管实例通过对应的服务管理器停止/启动。不需要删除 socket、状态文件或停止 Codex。

回退旧版本前也必须停止新后台，保留新增状态备份并检查已接受的操作；不要把旧版本直接接到运行中的新服务。
