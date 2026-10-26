# /// script
# requires-python = ">=3.11"
# dependencies = ["pyte==0.8.2"]
# ///
"""正式 Twig 主控验收：cargo build --locked && uv run examples/check_agent.py。

真实 Codex app-server、本地模拟模型/飞书、独立 tmux socket、临时 Git 仓库。
不会连接真实用户 daemon、读取认证或向飞书发送消息。
"""
import base64
import errno
import re
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import select
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time

import pyte
sys.dont_write_bytecode = True
from check_controller import Client, Model as ProbeModel, wait_for
from feishu_mock import Feishu


class Model(ProbeModel):
    # 产品验收比协议实验多出恢复、切页及通知跳转轮次。
    request_limit = 30

    def do_POST(self):
        if self.server.feishu.api(self):
            return
        if self.path == "/feishu":
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            self.server.notices.append(body)
            self.send_response(200)
            self.end_headers()
            self.wfile.write(json.dumps({"code": self.server.feishu_code}).encode())
            return
        try:
            super().do_POST()
        except (BrokenPipeError, ConnectionResetError):
            # 活动轮次中断后，模拟模型的响应连接应被客户端关闭。
            pass


class Frontend:
    def __init__(self, binary, config, cwd, env):
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 120, 0, 0))
        self.before = termios.tcgetattr(self.slave)
        self.output = bytearray()
        self.screen = pyte.Screen(120, 32)
        self.stream = pyte.ByteStream(self.screen)

        def setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        self.process = subprocess.Popen(
            [str(binary), "--config", str(config), "chat"], cwd=cwd,
            env={**env, "TERM": "xterm-256color", "SSH_CONNECTION": "twig-test"}, stdin=self.slave,
            stdout=self.slave, stderr=self.slave, preexec_fn=setup,
        )

    def send(self, data):
        os.write(self.master, data)

    def copied(self):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], 0.05)[0]:
                data = os.read(self.master, 65536)
                self.output.extend(data)
                self.stream.feed(data)
            matches = re.findall(rb"\x1b\]52;c;([^\x07]+)\x07", self.output)
            if matches:
                return base64.b64decode(matches[-1]).decode()
        raise AssertionError("没有收到 OSC 52 复制请求")

    def wait(self, text, timeout=15, history=False, absent=False):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], 0.05)[0]:
                data = os.read(self.master, 65536)
                self.output.extend(data)
                self.stream.feed(data)
            try:
                display = self.screen.display
            except IndexError:
                # PTY 分片可能停在宽字符差分重绘中间，等待后续字节完成该帧。
                continue
            history_end = next((y for y, line in enumerate(display) if "滚轮 / PgUp" in line or "Ctrl-End 回到最新" in line), len(display))
            found = text in "\n".join(display[5:history_end] if history else display)
            if found != absent:
                return
            assert self.process.poll() is None, self.screen.display
        raise AssertionError((text, self.screen.display))

    def absent_for(self, text, seconds=3):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], 0.05)[0]:
                data = os.read(self.master, 65536)
                self.output.extend(data)
                self.stream.feed(data)
            try:
                display = "\n".join(self.screen.display)
            except IndexError:
                continue
            assert text not in display, display
            assert self.process.poll() is None

    def wait_exit(self, timeout=10):
        deadline = time.monotonic() + timeout
        while True:
            # 退出期间继续消费 PTY，避免终端还原输出填满缓冲区后阻塞子进程。
            if select.select([self.master], [], [], 0.05)[0]:
                try:
                    data = os.read(self.master, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    data = b""  # Linux PTY 从端关闭时报告 EIO。
                self.output.extend(data)
                self.stream.feed(data)
            code = self.process.poll()
            if code is not None:
                return code
            if time.monotonic() >= deadline:
                raise AssertionError(f"PTY 退出超时：PID={self.process.pid}\n{self.output[-8000:].decode(errors='replace')}")

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
            self.wait_exit(timeout=5)
        os.close(self.master)
        os.close(self.slave)


def check(root):
    binary = Path(__file__).resolve().parents[1] / "target/debug/twig"
    home, cwd, shim = root / "codex", root / "repo", root / "bin"
    for path in (home, cwd, shim):
        path.mkdir()
    config, socket = root / "config.toml", root / "server.sock"
    real_codex, real_tmux = shutil.which("codex"), shutil.which("tmux")
    assert real_codex and real_tmux
    version = subprocess.check_output([real_codex, "--version"], text=True).strip()
    print(version, flush=True)
    model = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Model)
    model.feishu = Feishu()
    model.root = root
    model.count = 0
    model.counter_lock = threading.Lock()
    model.notices = []
    model.feishu_code = 0
    model.followup_command = shlex.join([
        str(binary), "--config", str(config), "agent", "notify", "--message", "离线主控已收到任务反馈",
    ])
    model.entered = {name: threading.Event() for name in ("worker", "controller")}
    model.release = {name: threading.Event() for name in ("worker", "controller")}
    serving = threading.Thread(target=model.serve_forever, daemon=True)
    serving.start()
    # 只替换服务发现，所有实际会话操作仍进入本次独立 app-server。
    discovery = json.dumps({"status": "running", "socketPath": str(socket)})
    codex_shim = (
        '#!/bin/sh\nif [ "$1 $2 $3" = "app-server daemon version" ]; then\n'
        f"printf '%s\\n' {shlex.quote(discovery)}\nelse\n"
        f'exec {shlex.quote(real_codex)} "$@"\nfi\n'
    )
    (shim / "codex").write_text(codex_shim)
    (shim / "codex").chmod(0o700)
    (shim / "tmux").write_text(
        "#!/bin/sh\nexec " + shlex.join([real_tmux, "-S", str(root / "tmux.sock"), "-f", "/dev/null"]) + ' "$@"\n'
    )
    (shim / "tmux").chmod(0o700)
    env = {k: v for k, v in os.environ.items() if k in ("PATH", "HOME", "LANG")}
    env.update(
        CODEX_HOME=str(home), PATH=str(shim) + ":" + env["PATH"],
        TWIG_FEISHU_APP_ID="test-app", TWIG_FEISHU_APP_SECRET="test-secret",
        TWIG_FEISHU_ALLOWED_USERS="ou_allowed", TWIG_FEISHU_API_BASE=f"http://127.0.0.1:{model.server_port}",
        TWIG_FEISHU_WEBHOOK_URL=f"http://127.0.0.1:{model.server_port}/feishu",
        GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1",
    )
    (home / "config.toml").write_text(f'''model = "gpt-5.4"
model_provider = "probe"
model_reasoning_effort = "high"
approval_policy = "on-request"
sandbox_mode = "read-only"
[features]
code_mode = false
[model_providers.probe]
name = "Local deterministic probe"
base_url = "http://127.0.0.1:{model.server_port}/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
''')

    def command(*args, success=True):
        result = subprocess.run(args, env=env, cwd=cwd, capture_output=True, text=True, timeout=30)
        assert result.returncode == 0 if success else result.returncode != 0, (args, result.stdout, result.stderr)
        return result.stdout if success else result.stderr

    def twig(*args, **kwargs):
        return command(str(binary), "--config", str(config), *args, **kwargs)

    command("git", "init", "-b", "main")
    command("git", "config", "user.name", "probe")
    command("git", "config", "user.email", "probe@example.test")
    command("git", "commit", "--allow-empty", "-m", "initial")
    state_file = root / "controller/runtime.json"

    def state():
        return json.loads(state_file.read_text())

    log = (root / "server.log").open("w+")

    def start_server():
        return subprocess.Popen(
            [real_codex, "app-server", "--listen", "unix://" + str(socket)],
            cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=log, start_new_session=True,
        )

    server = start_server()
    frontend = None
    twig_log = (root / "twig-daemon.log").open("w+")
    twig_daemon = subprocess.Popen(
        [str(binary), "--config", str(config), "daemon", "run", "--detached"], cwd=cwd, env=env,
        stdin=subprocess.DEVNULL, stdout=twig_log, stderr=twig_log,
    )
    try:
        wait_for(socket.exists, "服务未启动")
        wait_for(lambda: '"instance"' in twig("daemon", "status"), "Twig 后台未就绪")
        # 首次 Chat 必须从没有任何历史轮次的主控开始，而不是预先发过消息。
        frontend = Frontend(binary, config, cwd, env)
        frontend.wait("空闲")
        frontend.wait("YOLO · 无需审批 · 完整访问权限")
        frontend.wait("gpt-5.4 · 推理 high")
        frontend.send(b"\x1bOQ")  # F2 不再切换页面
        frontend.absent_for("个项目", 1)
        frontend.send("清空测试".encode())
        frontend.wait("清空测试")
        frontend.send(b"\x03" + "已清空".encode())
        frontend.wait("已清空")
        frontend.absent_for("清空测试", 1)
        frontend.send(b"\x15   \x03" + "空白已清空".encode())
        frontend.wait("空白已清空")  # 纯空白也先清空，不能退出
        frontend.send(b"\x15")
        empty_thread = state()["bindings"]["controller"]["thread"]
        frontend.send(b"\t")
        frontend.wait("个项目")
        frontend.send(b"\t")
        frontend.wait("空闲")
        frontend.wait("gpt-5.4 · 推理 high")
        assert state()["bindings"]["controller"]["thread"] == empty_thread
        # 空 session 没有 rollout，服务重启后必须恢复入口而不是永远卡在旧 ID。
        workspace = Path(state()["bindings"]["controller"]["cwd"])
        instructions = (workspace / "AGENTS.md").read_bytes()
        skill_file = workspace / ".agents/skills/twig-controller/SKILL.md"
        assert not skill_file.exists(), "agent init 不应安装 skill"
        skill_file.parent.mkdir(parents=True)
        skill_file.write_text("---\nname: twig-controller\ndescription: 隔离测试的自定义主控指令\n---\n用户自行安装的主控 skill\n")
        skill_content = skill_file.read_bytes()
        (workspace / "state.md").write_text("用户保留的全局计划")
        frontend.send(b"\x03")
        frontend.wait_exit(timeout=5)
        frontend.close()
        frontend = None
        server.terminate()
        server.wait(timeout=10)
        socket.unlink(missing_ok=True)
        server = start_server()
        wait_for(socket.exists, "空会话服务重启失败")
        # 只要有未决消息，即使 rollout 丢失也不得悄悄换一个主控。
        empty_state = state()
        guarded = json.loads(json.dumps(empty_state))
        guarded["messages"].append({"id": "pending-check", "thread": empty_thread, "text": "must-preserve", "status": "sending", "turn": None, "error": None})
        state_file.write_text(json.dumps(guarded))
        assert "拒绝创建替代会话" in twig("agent", "init", success=False)
        assert state() == guarded
        state_file.write_text(json.dumps(empty_state))
        frontend = Frontend(binary, config, cwd, env)
        wait_for(lambda: state()["bindings"]["controller"]["thread"] != empty_thread, "空主控绑定未恢复")
        frontend.wait("空闲")
        frontend.wait("gpt-5.4 · 推理 high")
        assert (workspace / "AGENTS.md").read_bytes() == instructions
        assert skill_file.read_bytes() == skill_content
        assert (workspace / "state.md").read_text() == "用户保留的全局计划"
        print("PASS: 空主控卸载后重建绑定，保留 AGENTS.md 和全局计划", flush=True)
        frontend.send(b"first-chat-message\r")
        frontend.wait("probe complete 1")
        frontend.wait("▎ 你")
        assert next(m for m in state()["messages"] if m["text"] == "first-chat-message")["source"] == {"kind": "user"}
        frontend.absent_for("主控有新消息")
        with (state_file.parent / "lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            frontend.send(b"hold-controller\r")
            frontend.wait("hold-controller", timeout=1, history=True)
            assert not any(m["text"] == "hold-controller" for m in state()["messages"])
            fcntl.flock(lock, fcntl.LOCK_UN)
        assert model.entered["controller"].wait(10)
        frontend.send(b"\t")
        frontend.wait("个项目")
        model.release["controller"].set()
        frontend.wait("主控有新消息")
        frontend.wait("Agent*")
        time.sleep(8.5)
        frontend.wait("Tab 切换标签")
        frontend.absent_for("主控有新消息", 1)
        frontend.send(b"\x17f")
        frontend.absent_for("YOLO", 1)
        frontend.wait("Agent*")
        frontend.send(b"\t")
        frontend.wait("probe complete 2")
        assert "\n".join(frontend.screen.display).count("hold-controller") == 1
        frontend.absent_for("Agent*", 1)
        frontend.send(b"\t")
        frontend.wait("个项目")
        frontend.absent_for("主控有新消息")
        frontend.absent_for("Agent*", 1)
        frontend.send(b"\t")
        frontend.wait("probe complete 2")
        model.entered["controller"].clear()
        model.release["controller"].clear()
        print("PASS: 主控后台回复浮动提醒、Chat 未读、卡片到期保留未读、查看后清除且不重复", flush=True)
        # 有历史的会话被卸载时，正在打开的 Chat 应重新加载原 ID。
        original = state()["bindings"]["controller"]["thread"]
        server.terminate()
        server.wait(timeout=10)
        # 等 Chat 实际观察到断连，再恢复服务并按 F5；不能把旧画面的“空闲”当作已重连。
        frontend.wait("未连接 · F5 重试")
        socket.unlink(missing_ok=True)
        server = start_server()
        wait_for(socket.exists, "有历史会话服务重启失败")
        frontend.send(b"\x1b[15~")
        control = Client(socket)
        wait_for(lambda: control.call("thread/read", {"threadId": original, "includeTurns": False})["thread"]["status"]["type"] == "idle", "Chat 未恢复卸载会话")
        assert state()["bindings"]["controller"]["thread"] == original
        frontend.wait("空闲")
        frontend.wait("gpt-5.4 · 推理 high")
        frontend.send(b"after-unload\r")
        frontend.wait("probe complete 3")
        control.close()
        # 只捕获 OSC 52，不写真实系统剪贴板。
        reply = "probe complete 3"
        row = next(y for y, line in enumerate(frontend.screen.display) if reply in line)
        col = frontend.screen.display[row].index(reply)
        frontend.output.clear()
        frontend.send(f"\x1b[<0;{col+1};{row+1}M\x1b[<32;{col+len(reply)};{row+1}M\x1b[<0;{col+len(reply)};{row+1}m".encode())
        assert frontend.copied() == reply
        frontend.wait("已请求复制")
        frontend.output.clear()
        frontend.send(b"\x1bc")  # Alt-C，复制完整回复。
        assert frontend.copied() == reply
        frontend.wait("已请求复制")
        frontend.send(b"\x1b[14~")  # F4
        frontend.wait(original)
        frontend.send(b"\x1b[14~")
        long_draft = "第一行\n" + "中文输入" * 180 + "末尾可见"
        frontend.send(b"\x1b[200~" + long_draft.encode() + b"\x1b[201~")
        frontend.wait("末尾可见")
        frontend.send(b"\x1b[H")
        frontend.wait("第一行")
        frontend.send(b"\x15")
        print("PASS: Chat 拖选复制、完整回复复制、诊断展开和中文长输入光标跟随", flush=True)
        print("PASS: Chat 中有历史会话被卸载后恢复原 ID 并继续对话", flush=True)
        frontend.send(b"\x03")
        frontend.wait_exit(timeout=5)
        frontend.close()
        frontend = None
        print("PASS: 全新主控首次打开 Chat，连续两轮输入及回复实际显示", flush=True)
        main = json.loads(twig("agent", "init"))
        assert json.loads(twig("agent", "init"))["thread"] == main["thread"]
        twig("add", "demo", "--path", str(cwd))
        # 受管任务不执行普通 tmux 启动命令。
        (cwd / ".twig").mkdir()
        (cwd / ".twig/config.toml").write_text(
            '[tmux]\n[[tmux.panes]]\ncommand = ["touch", ' + json.dumps(str(root / "unwanted")) + ']\n'
        )
        twig("new", "demo", "--no-fetch", "test", "--agent", "--no-open")
        task = next(b for b in state()["bindings"].values() if b["task"] == "test")
        assert not (root / "unwanted").exists()
        task_dir = Path(task["cwd"])
        assert (task_dir / ".twig/state.md").exists()
        assert command("git", "-C", str(task_dir), "status", "--porcelain") == ""
        assert "state.md" in command("git", "-C", str(task_dir), "ls-files", "--others", "--ignored", "--exclude-standard")
        assert "已存在" in twig("new", "demo", "--no-fetch", "test", "--agent", "--no-open", success=False)
        first = twig("agent", "send", "--from-project", "demo", "--from-task", "test", "--message", "hold-controller").strip()
        assert next(m for m in state()["messages"] if m["id"] == first)["source"] == {"kind": "task", "project": "demo", "task": "test"}
        assert model.entered["controller"].wait(10)
        twig("agent", "send", "--project", "demo", "--task", "test", "--message", "hold-worker")
        assert model.entered["worker"].wait(10)
        assert "会话仍活动" in twig("rm", "demo", "test", "--delete-ignored", "-y", success=False)
        model.release["worker"].set()
        wait_for(lambda: any(m["status"] == "queued" and m["thread"] == main["thread"] for m in state()["messages"]), "完成反馈未排队")
        model.release["controller"].set()
        wait_for(lambda: len(state()["seen"]) >= 6, "主控未完成反馈轮次")
        assert all(m["status"] == "sent" for m in state()["messages"])
        assert any(m.get("source") == {"kind": "completion", "project": "demo", "task": "test"} for m in state()["messages"])
        assert model.notices == [{"msg_type": "text", "content": {"text": "离线主控已收到任务反馈"}}]
        print("PASS: 未打开 UI 时主控调用真实 Twig 通知 CLI，继承环境并送达模拟飞书", flush=True)
        # 模拟接受响应丢失：历史中的 clientId 应能恢复发送结果。
        data = state()
        next(m for m in data["messages"] if m["id"] == first)["status"] = "sending"
        state_file.write_text(json.dumps(data))
        twig("agent", "drain")
        assert next(m for m in state()["messages"] if m["id"] == first)["status"] == "sent"
        print("PASS: 受管任务唯一绑定、启动配置隔离、完成回调、忙时排队与发送结果对账", flush=True)

        frontend = Frontend(binary, config, cwd, env)
        frontend.wait("空闲")
        frontend.wait("gpt-5.4 · 推理 high")
        frontend.send(b"\x1b[200~" + "草稿中文".encode() + b"\x1b[201~")
        frontend.wait("草稿中文")
        frontend.send(b"\x1b[<0;53;2M\x1b[<0;53;2m")
        frontend.wait("个项目")
        frontend.send(b"\x1b[<0;65;2M\x1b[<0;65;2m")
        frontend.wait("草稿中文")
        frontend.send(b"\t")
        frontend.wait("个项目")
        frontend.send(b"\t")
        frontend.wait("草稿中文")
        frontend.wait("空闲")
        frontend.wait("gpt-5.4 · 推理 high")
        frontend.send(b"\x15")  # 清空草稿
        frontend.absent_for("主控有新消息")
        model.entered["controller"].clear()
        model.release["controller"].clear()
        frontend.send(b"chat-test hold-controller\r")
        assert model.entered["controller"].wait(10)
        frontend.send(b"\t")
        frontend.wait("个项目")
        frontend.send(b"2\r")
        frontend.send(b"printf 'CHAT_NOTICE_WORKBENCH\\n'\r")
        frontend.wait("CHAT_NOTICE_WORKBENCH")
        model.release["controller"].set()
        frontend.wait("主控有新消息")
        frontend.wait("Agent 有新消息")
        frontend.wait("Ctrl-W f 查看 Agent")
        pane_pids = command("tmux", "list-panes", "-a", "-F", "#{pane_id}:#{pane_pid}")
        frontend.send(b"\x17f")
        frontend.wait("chat-test hold-controller")
        assert command("tmux", "list-panes", "-a", "-F", "#{pane_id}:#{pane_pid}") == pane_pids
        frontend.absent_for("Agent*", 1)
        print("PASS: 工作台 Ctrl-W f 直达主控、清除未读并保留 Pane 进程", flush=True)
        frontend.send(b"\x03")
        frontend.wait_exit(timeout=5)
        assert frontend.process.returncode == 0
        assert termios.tcgetattr(frontend.slave) == frontend.before
        frontend.close()
        frontend = None
        assert server.poll() is None
        print("PASS: Chat PTY 中文输入、鼠标及 Tab 标签往返草稿保留、发送、终端恢复及退出不停止服务", flush=True)

        twig("agent", "notify", "--message", "通知测试")
        assert model.notices[-1] == {"msg_type": "text", "content": {"text": "通知测试"}}
        model.feishu_code = 19024
        assert "未接受" in twig("agent", "notify", "--message", "失败测试", success=False)
        model.feishu_code = 0
        print("PASS: 飞书本地模拟请求及业务错误判定", flush=True)

        # 服务重启后持久会话不可替换为新 thread。
        server.terminate()
        server.wait(timeout=10)
        socket.unlink(missing_ok=True)
        server = start_server()
        wait_for(socket.exists, "重启后无 socket")
        twig("agent", "send", "--message", "after-restart")
        wait_for(lambda: any(m["text"] == "after-restart" and m["status"] == "sent" for m in state()["messages"]), "重启后未恢复")
        assert json.loads(twig("agent", "init"))["thread"] == main["thread"]
        print("PASS: app-server 重启后恢复原主控 session", flush=True)

        model.entered["worker"].clear()
        model.release["worker"].clear()
        twig("agent", "send", "--project", "demo", "--task", "test", "--message", "hold-worker")
        assert model.entered["worker"].wait(10)
        queued = twig("agent", "send", "--project", "demo", "--task", "test", "--message", "queued-before-stop").strip()
        twig("stop", "demo", "test", "-y")
        assert next(m for m in state()["messages"] if m["id"] == queued)["status"] == "cancelled"
        model.release["worker"].set()
        assert "已停止" in twig("agent", "send", "--project", "demo", "--task", "test", "--message", "must-not-send", success=False)
        twig("rm", "demo", "test", "--delete-ignored", "-y")
        assert not task_dir.exists()
        print("PASS: 中断活动轮次、取消排队、停止后拒绝派发及显式删除", flush=True)
        # 删除并重建同名任务，不能命中留存的旧绑定。
        twig("new", "demo", "--no-fetch", "test", "--agent", "--no-open")
        new_task = next(b for b in state()["bindings"].values() if b["task"] == "test" and b["thread"] != task["thread"])
        message = twig("agent", "send", "--project", "demo", "--task", "test", "--message", "new-instance").strip()
        assert next(m for m in state()["messages"] if m["id"] == message)["thread"] == new_task["thread"]
        wait_for(lambda: any(key.startswith(new_task["thread"] + ":") for key in state()["seen"]), "重建任务未完成首次轮次")
        twig("agent", "drain")
        assert json.loads(twig("agent", "read", "--project", "demo", "--task", "test"))["thread"]["id"] == new_task["thread"]
        print("PASS: 同名任务重建后定位新 session，恢复跳过已删除实例", flush=True)
        # 模拟没有 notify 的原生客户端轮次，恢复时须从服务端补查完成事实。
        client = Client(socket)
        wait_for(lambda: client.call("thread/read", {"threadId": new_task["thread"], "includeTurns": False})["thread"]["status"]["type"] == "idle", "新任务未空闲")
        client.call("thread/resume", {"threadId": new_task["thread"], "config": {"notify": []}})
        turn = client.call("turn/start", {"threadId": new_task["thread"], "input": [{"type": "text", "text": "missed-callback", "text_elements": []}]})["turn"]["id"]
        def completed():
            result = client.call("thread/read", {"threadId": new_task["thread"], "includeTurns": True})
            return any(t["id"] == turn and t["status"] == "completed" for t in result["thread"]["turns"])
        wait_for(completed, "无回调轮次未完成")
        key = new_task["thread"] + ":" + turn
        assert key not in state()["seen"]
        twig("agent", "drain")
        assert key in state()["seen"]
        count = len([m for m in state()["messages"] if turn in m["text"]])
        twig("agent", "drain")
        assert count == 1 == len([m for m in state()["messages"] if turn in m["text"]])
        client.socket.close()
        print("PASS: 漏失完成回调由 drain 补查，重复恢复不重复反馈", flush=True)
        # 即使全局配置受限，主控恢复仍使用 YOLO，子任务保留原策略。
        client = Client(socket)
        restored = client.call("thread/resume", {"threadId": main["thread"]})
        assert restored["approvalPolicy"] == "never", restored
        assert restored["sandbox"]["type"] == "dangerFullAccess", restored
        child = client.call("thread/resume", {"threadId": new_task["thread"]})
        assert child["approvalPolicy"] == "on-request", child
        assert child["sandbox"]["type"] == "readOnly", child
        client.call("thread/resume", {"threadId": main["thread"], "approvalPolicy": "on-request", "sandbox": "read-only"})
        twig("agent", "init")
        restored = client.call("thread/resume", {"threadId": main["thread"]})
        assert restored["approvalPolicy"] == "never", restored
        assert restored["sandbox"]["type"] == "dangerFullAccess", restored
        client.socket.close()
        print("PASS: 受限默认配置下主控 YOLO、已有主控更新与子任务权限不变", flush=True)

        frontend = Frontend(binary, config, cwd, env)
        frontend.wait("空闲")
        # 持有隔离记录锁，确保两条消息仍在 worker 输入队列时切页。
        with (state_file.parent / "lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            frontend.send(b"before-tab-one\r")
            frontend.wait("before-tab-one", history=True)
            frontend.send(b"before-tab-two\r")
            frontend.wait("before-tab-two", history=True)
            frontend.send(b"\t")
            frontend.wait("个项目")
            fcntl.flock(lock, fcntl.LOCK_UN)
        wait_for(lambda: sum(m["text"].startswith("before-tab-") and m["status"] == "sent" for m in state()["messages"]) == 2, "切页后消息未全部持久化和投递")
        history = json.loads(twig("agent", "read"))["thread"]["turns"]
        texts = [part.get("text") for turn in history for item in turn["items"] if item["type"] == "userMessage" for part in item["content"]]
        assert texts.count("before-tab-one") == texts.count("before-tab-two") == 1
        frontend.send(b"\t")
        frontend.wait("before-tab-two", history=True)
        frontend.send(b"\x03")
        frontend.wait_exit(timeout=10)
        frontend.close()
        frontend = None
        print("PASS: 连续发送后立即切回 Console，全部持久化并各投递一次", flush=True)
        # 切页期间发生的拒收仍须在回到 Agent 后还原草稿并报告错误。
        frontend = Frontend(binary, config, cwd, env)
        frontend.wait("空闲")
        with (state_file.parent / "lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            data = state()
            data["bindings"]["controller"]["stopped"] = True
            state_file.write_text(json.dumps(data))
            frontend.send(b"rejected-while-hidden\r")
            frontend.wait("rejected-while-hidden", history=True)
            frontend.send(b"\t")
            frontend.wait("个项目")
            fcntl.flock(lock, fcntl.LOCK_UN)
        frontend.send(b"\t")
        frontend.wait("任务已停止")
        frontend.wait("rejected-while-hidden")
        assert not any(m["text"] == "rejected-while-hidden" for m in state()["messages"])
        frontend.send(b"\x15\x03")
        frontend.wait_exit(timeout=10)
        frontend.close()
        frontend = None
        with (state_file.parent / "lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            data = state()
            data["bindings"]["controller"]["stopped"] = False
            state_file.write_text(json.dumps(data))
        print("PASS: 切页期间发送被拒绝，返回后恢复草稿并保留错误", flush=True)


        frontend = Frontend(binary, config, cwd, env)
        frontend.wait("空闲")
        model.entered["controller"].clear()
        model.release["controller"].clear()
        frontend.send(b"navigation-follow hold-controller\r")
        assert model.entered["controller"].wait(10)
        navigation_reply = f"probe complete {model.count + 1}"
        frontend.send(b"\t")
        frontend.wait("个项目")
        model.release["controller"].set()
        frontend.wait("主控有新消息")
        frontend.wait("Ctrl-W f 查看 Agent")
        frontend.send(b"\x17f")
        frontend.wait(navigation_reply, history=True)
        # “消息送达”和“轮次完成”是两个事件；两者采集并清除后才具备“无卡片”前提。
        def navigation_read():
            message = next(m for m in state()["messages"] if m["text"] == "navigation-follow hold-controller")
            if not message.get("turn"):
                return False
            saved = json.loads((root / "notifications.json").read_text())
            expected = {f"message:{message['id']}", f"turn:{message['thread']}:{message['turn']}"}
            return expected.issubset(saved["seen"]) and not any(e["thread"] == message["thread"] for e in saved["unread"])
        wait_for(navigation_read, "导航测试轮次的送达/完成事件尚未采集并清除")
        frontend.absent_for("Agent*", 1)
        frontend.send(b"\t")
        frontend.wait("个项目")
        # 导航文字可能先于整帧刷新到达；等旧 Agent 标题消失后再测试无卡片快捷键。
        frontend.wait("YOLO", absent=True)
        frontend.send(b"\x17f")
        frontend.absent_for("YOLO", 1)
        frontend.wait("个项目")
        frontend.send(b"q")
        frontend.wait_exit(timeout=10)
        frontend.close()
        frontend = None
        print("PASS: 导航 Ctrl-W f 跳主控并清除未读，无卡片时不跳转", flush=True)

        # 空子任务没有 rollout，daemon 重启后不能拖垮正常主控。
        twig("new", "demo", "--no-fetch", "empty", "--agent", "--no-open")
        broken = next(b for b in state()["bindings"].values() if b["task"] == "empty")
        server.terminate()
        server.wait(timeout=10)
        socket.unlink(missing_ok=True)
        server = start_server()
        wait_for(socket.exists, "空子任务实验重启失败")
        failed = twig("agent", "send", "--project", "demo", "--task", "empty", "--message", "missing-rollout").strip()
        assert next(m for m in state()["messages"] if m["id"] == failed)["status"] == "queued"
        assert broken["thread"] in twig("agent", "drain", success=False)
        frontend = Frontend(binary, config, cwd, env)
        frontend.wait("YOLO · 无需审批 · 完整访问权限")
        frontend.wait("thread not loaded")
        frontend.send(b"healthy-controller-after-failure\r")
        wait_for(lambda: any(m["text"] == "healthy-controller-after-failure" and m["status"] == "sent" for m in state()["messages"]), "子任务错误阻断主控发送")
        healthy_turn = next(m["turn"] for m in state()["messages"] if m["text"] == "healthy-controller-after-failure")
        client = Client(socket)
        def healthy_reply():
            turns = client.call("thread/read", {"threadId": main["thread"], "includeTurns": True})["thread"]["turns"]
            return next((turn for turn in turns if turn["id"] == healthy_turn and turn["status"] == "completed"), None)
        wait_for(healthy_reply, "主控接收后未完成回复")
        reply = next(item["text"] for item in healthy_reply()["items"] if item["type"] == "agentMessage")
        frontend.wait(reply, history=True)
        client.socket.close()
        failed_record = next(m for m in state()["messages"] if m["id"] == failed)
        assert failed_record["status"] == "queued" and failed_record["error"]
        frontend.send(b"\x03")
        frontend.wait_exit(timeout=10)
        frontend.close()
        frontend = None
        print("PASS: 失效子任务明确报错，主控仍可连接和投递，不重发异常消息", flush=True)

        model.feishu.event("om_group", chat_type="group")
        model.feishu.event("om_denied", user="ou_stranger")
        model.feishu.event("om_offline")
        model.feishu.event("om_offline")
        wait_for(lambda: len(model.feishu.acks) >= 4, "飞书事件未确认")
        def synced_text():
            return "\n".join(text for _, text in model.feishu.replies)

        def history_caught_up():
            current = json.loads(twig("agent", "read"))["thread"]
            if current["status"]["type"] != "idle":
                return False
            history = current["turns"]
            text = synced_text()
            latest = history[-1]
            expected = [item["text"] for item in latest["items"] if item["type"] == "agentMessage"][-1:]
            health = json.loads(twig("daemon", "status"))["feishu"]
            assert not health["history_error"] and not health["history_errors"], health
            assert not health["unknown_replies"], health
            return (expected and all(value in text for value in expected)
                    and not health["history_pending"] and not health["pending"])

        # 首次仅补最近一轮；等待完成后的最终回复与队列清空。
        def wait_history():
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                if history_caught_up():
                    return
                time.sleep(.3)
            raise AssertionError("主控最终回复未同步：" + twig("daemon", "status"))

        wait_history()
        messages = [m for m in state()["messages"] if m["text"] == "offline-feishu-request"]
        assert len(messages) == 1, messages
        assert messages[0]["source"] == {"kind": "feishu", "user": "ou_allowed"}
        assert "before-tab-one" not in synced_text()
        assert "子任务 · demo/test" not in synced_text()
        assert "【User】" in synced_text() and "【Codex】" in synced_text()
        assert "主控会话" not in synced_text() and "【工具】" not in synced_text()
        assert not any("om_group" in path or "om_denied" in path for path, _ in model.feishu.replies)
        # 没有新的飞书输入也同步 CLI 用户输入和最终回复，隐藏子任务回报正文。
        cli_id = twig("agent", "send", "--message", "cli-after-feishu-subscription").strip()
        report_id = twig("agent", "send", "--from-project", "demo", "--from-task", "test",
                         "--message", "child-report-after-subscription").strip()
        wait_for(lambda: all(next(m for m in state()["messages"] if m["id"] == ident)["status"] == "sent"
                             for ident in (cli_id, report_id)), "跨入口消息未投递")
        wait_history()
        assert "cli-after-feishu-subscription" in synced_text()
        assert "【User】\nchild-report-after-subscription" not in synced_text()
        # 子任务内部输入属于其他 thread，不应作为历史条目进入飞书。
        assert "\nhold-worker\n" not in synced_text()
        assert "\nnew-instance\n" not in synced_text()
        replies_before_restart = list(model.feishu.replies)
        previous_instance = json.loads(twig("daemon", "status"))["instance"]
        twig("daemon", "restart")
        restarted = json.loads(twig("daemon", "status"))
        assert restarted["instance"] != previous_instance
        assert restarted["pid"] == twig_daemon.pid
        assert restarted["feishu"]["enabled"]
        model.feishu.event("om_offline")
        wait_for(lambda: len(model.feishu.acks) >= 5, "重连事件未确认")
        time.sleep(3)
        assert len([m for m in state()["messages"] if m["text"] == "offline-feishu-request"]) == 1
        assert model.feishu.replies == replies_before_restart
        print("PASS: 飞书最近一轮补齐、跨入口输入与最终回复、子任务回报过滤及保留环境重启去重", flush=True)

    except BaseException:
        twig_log.flush()
        twig_log.seek(0)
        print(twig_log.read()[-8000:], file=sys.stderr)
        log.flush()
        log.seek(0)
        print(log.read()[-8000:], file=sys.stderr)
        raise
    finally:
        if frontend:
            frontend.close()
        for event in model.release.values():
            event.set()
        if twig_daemon.poll() is None:
            twig_daemon.terminate()
            twig_daemon.wait(timeout=30)
        twig_log.close()
        subprocess.run([real_tmux, "-S", str(root / "tmux.sock"), "kill-server"], capture_output=True)
        if server.poll() is None:
            os.killpg(server.pid, signal.SIGTERM)
            server.wait(timeout=10)
        model.shutdown()
        model.server_close()
        model.feishu.close()
        serving.join(timeout=2)
        log.close()


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="twig-agent-check-") as directory:
        check(Path(directory))
