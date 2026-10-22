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
