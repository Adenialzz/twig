# Twig 后台

Twig 后台使用同一个二进制，以 `--config` 隔离实例。当前已提供生命周期入口；业务迁移进度见源码仓库的 DAEMON_DESIGN.md，此阶段尚未将现有 Console/CLI 业务切换到后台。

```sh
twig daemon start           # 显式后台启动，等待就绪
twig daemon status          # 查看当前配置的后台状态
twig daemon stop            # 有序停止，不停止任务或 Codex
twig daemon run             # 前台运行，供系统服务托管
```

业务状态存储在配置目录下，后台自身记录位于 `daemon/<配置身份>/`。后台启动日志写入该目录的 `daemon.log`，Unix socket 位于当前系统临时目录内的用户私有目录中。

后台不启动、重启或停止共享 Codex daemon。安装二进制不会自动启动 Twig 后台；帮助、guide 和补全不启动后台。
