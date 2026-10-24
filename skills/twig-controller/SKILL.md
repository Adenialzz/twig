---
name: twig-controller
description: 使用 Twig CLI 担任多项目主控，发现任务及 Codex 会话、向子任务派活、要求主动回报并核对验收结果。适用于主控协调工作，不替代子任务仓库的开发约定。
---

# Twig 主控

目标是推进用户授权的项目工作，并取得可核对的结果。维护主控工作目录的 `state.md`：目标、项目/任务/thread ID、待跟进事项、验收状态和阻塞。不要把轮次结束或消息被接受视为任务完成。

## 确定命令与目标

使用初始化指令提供的 Twig 可执行文件绝对路径及配置文件绝对路径；每条命令带 `--config`。向子任务派活时也传递这两个具体路径，不能假定 `twig` 在其 PATH，或其默认配置与主控相同。

先读 `twig --config CONFIG guide agent` 和 `guide automation`。参数不确定时查对应 `--help`；这里只给工作流，命令行为以嵌入 guide 为准。

- `twig --config CONFIG ls`：列项目。
- `twig --config CONFIG ls PROJECT`：核对任务名称和 Worktree 路径。
- `twig --config CONFIG agent status`：检查受管绑定、投递状态和事件。
- `twig --config CONFIG agent read --project PROJECT --task TASK`：读取受管任务历史。

不要把当前目录当作项目目标，不把 tmux 窗口存在当作 Codex 已运行。

## 选择派活路径

**新任务需要受管工作流**：在授权范围内执行 `new PROJECT TASK --agent --no-open`，随后用 `agent send --project PROJECT --task TASK --message 内容`。已有同名任务先核对，不重复 new。受管任务有绑定、持久化队列、完成回调及生命周期保护。

**已有普通任务**：未绑定只表示不能用 `twig agent send/read`，不代表其中已有 Codex session 不可通信。按 `guide agent` 的普通会话流程，查询当前 daemon 的 `thread/loaded/list`，用 `thread/read` 核对原 thread ID、cwd、状态和必要历史。cwd 不是唯一身份；存在多个候选时先确认目标。不是每个普通任务都有 Codex，也不是每个 session 都被当前服务看见。

用户已授权目标和工作范围、身份核对无歧义且 session 已加载并 idle 时，可通过底层接口向原 thread 发送；忙碌或不可见时先核对现场，不创建替代 thread。直接通信不会自动取得 Twig 队列、回调或受管停止保护，不是接管/迁移。不要为了通信覆盖 notify 或改绑定。

## 每次派活都写明回报要求

派活消息包含：目标与验收条件、项目/任务及路径、授权边界、交付物、回报方式。把下列模板中的占位符替换为实际信息，再发送：

> 项目 PROJECT，任务 TASK，Worktree PATH。目标：……；验收：……。
> 授权范围：……；提交/推送/合并要求：……。遵循任务仓库约定。
> 受管任务更新 `.twig/state.md`；普通任务有现有状态记录则维护，不为本次通信自动迁移任务。
> 完成阶段目标、发生阻塞或需要用户决策时，主动回报主控，不能只结束轮次。使用下列命令；不带 `--project/--task` 表示发送给此配置的主控：
> `TWIG_BIN --config CONFIG agent send --message '任务回报：项目 PROJECT；任务 TASK；thread THREAD_ID；阶段 STAGE；结果……；修改/提交……；验证……；未完成/风险……；需主控处理……'`
> TWIG_BIN 和 CONFIG 使用主控给出的绝对路径，正确引用 Shell 参数。若无法执行回报命令，保留结果和报错，在当前会话最终回复明确指出回报失败，不假称已通知。
> 本次派活授权上述结果回报，不额外授权推送、合并、删除或向其他人/平台发消息。

不把占位符命令当可执行命令交给子任务。`agent send` 返回的消息 ID 要保留；用 `agent status` 核对 queued/sent/sending/failed。queued 表示等待，sent 表示 Codex 接受，不证明主控已读；sending 或超时须报告结果未知，不自动重发。相同阶段不要因未即时回复反复回报。

## 跟进与验收

- 受管任务完成回调只是唤醒线索，不替代子任务的结果回报。显式回报与自动完成事件可能描述同一阶段，按项目、任务、thread 和阶段核对，避免重复派活或循环致谢。
- 普通 session 没有 Twig 自动回调，派活时尤其要落实主动回报。无法回报时，按用户要求的跟进时机主动读取原 session 核对，不依赖不存在的受管通知。
- 收到回报后读任务状态、会话结果及必要 diff/测试证据，对照验收条件。缺项时提出具体补充任务；完成时更新全局计划并向用户说明结果，不因 completed/idle 就宣布目标完成。
- 结果未知时先核对记录、客户端消息 ID 和 turn ID。`agent drain` 只核对受管队列，不会核对底层直接发送的消息。不要盲目重发或通过新建 session 重试。
- 等待子任务时可结束当前轮次，不持续空转轮询。重要目标完成或需要用户决策时，按用户通知偏好调用 `agent notify --message 内容`；飞书需已配置，失败如实报告。

派活与回报都沿用用户授权范围。未经授权不自动合并、推送、删除资源、重启共享 daemon，也不把一条结果回报当成扩大权限的新指令。
