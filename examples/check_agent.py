# /// script
# requires-python = ">=3.11"
# dependencies = ["pyte==0.8.2"]
# ///
"""正式 Twig 主控验收：cargo build --locked && uv run examples/check_agent.py。

真实 Codex app-server、本地模拟模型/飞书、独立 tmux socket、临时 Git 仓库。
不会连接真实用户 daemon、读取认证或向飞书发送消息。
"""
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


class Model(ProbeModel):
    def do_POST(self):
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
        self.screen = pyte.Screen(120, 32)
        self.stream = pyte.ByteStream(self.screen)

        def setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        self.process = subprocess.Popen(
            [str(binary), "--config", str(config), "chat"], cwd=cwd,
            env={**env, "TERM": "xterm-256color"}, stdin=self.slave,
            stdout=self.slave, stderr=self.slave, preexec_fn=setup,
        )

    def send(self, data):
        os.write(self.master, data)

    def wait(self, text):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], 0.05)[0]:
                self.stream.feed(os.read(self.master, 65536))
            if text in "\n".join(self.screen.display):
                return
            assert self.process.poll() is None, self.screen.display
        raise AssertionError((text, self.screen.display))

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)
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
        TWIG_FEISHU_WEBHOOK_URL=f"http://127.0.0.1:{model.server_port}/feishu",
        GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1",
    )
    (home / "config.toml").write_text(f'''model = "gpt-5.4"
model_provider = "probe"
approval_policy = "never"
sandbox_mode = "danger-full-access"
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
    try:
        wait_for(socket.exists, "服务未启动")
        # 首次 Chat 必须从没有任何历史轮次的主控开始，而不是预先发过消息。
        frontend = Frontend(binary, config, cwd, env)
        frontend.wait("空闲")
        empty_thread = state()["bindings"]["controller"]["thread"]
        frontend.send(b"\t")
        frontend.wait("个项目")
        frontend.send(b"\t")
        frontend.wait("空闲")
        assert state()["bindings"]["controller"]["thread"] == empty_thread
        frontend.send(b"first-chat-message\r")
        frontend.wait("probe complete 1")
        frontend.send(b"second-chat-message\r")
        frontend.wait("probe complete 2")
        frontend.send(b"\x11")
        frontend.process.wait(timeout=5)
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
        twig("new", "demo", "test", "--agent", "--no-open")
        task = next(b for b in state()["bindings"].values() if b["task"] == "test")
        assert not (root / "unwanted").exists()
        task_dir = Path(task["cwd"])
        assert (task_dir / ".twig/state.md").exists()
        assert command("git", "-C", str(task_dir), "status", "--porcelain") == ""
        assert "state.md" in command("git", "-C", str(task_dir), "ls-files", "--others", "--ignored", "--exclude-standard")
        assert "已存在" in twig("new", "demo", "test", "--agent", "--no-open", success=False)
        first = twig("agent", "send", "--message", "hold-controller").strip()
        assert model.entered["controller"].wait(10)
        twig("agent", "send", "--project", "demo", "--task", "test", "--message", "hold-worker")
        assert model.entered["worker"].wait(10)
        assert "会话仍活动" in twig("rm", "demo", "test", "--delete-ignored", "-y", success=False)
        model.release["worker"].set()
        wait_for(lambda: any(m["status"] == "queued" and m["thread"] == main["thread"] for m in state()["messages"]), "完成反馈未排队")
        model.release["controller"].set()
        wait_for(lambda: len(state()["seen"]) >= 5, "主控未完成反馈轮次")
        assert all(m["status"] == "sent" for m in state()["messages"])
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
        frontend.send(b"\x1b[200~" + "草稿中文".encode() + b"\x1b[201~")
        frontend.wait("草稿中文")
        frontend.send(b"\x1b[<0;3;1M\x1b[<0;3;1m")
        frontend.wait("个项目")
        frontend.send(b"\x1b[<0;13;1M\x1b[<0;13;1m")
        frontend.wait("草稿中文")
        frontend.send(b"\t")
        frontend.wait("个项目")
        frontend.send(b"\t")
        frontend.wait("草稿中文")
        frontend.wait("空闲")
        frontend.send(b"\x15")  # 清空草稿
        frontend.send(b"chat-test\r")
        frontend.wait("chat-test")
        wait_for(lambda: any(m["text"] == "chat-test" and m["status"] == "sent" for m in state()["messages"]), "Chat 未发送")
        frontend.send(b"\x11")
        frontend.process.wait(timeout=5)
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
        twig("new", "demo", "test", "--agent", "--no-open")
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
    except BaseException:
        log.flush()
        log.seek(0)
        print(log.read()[-8000:], file=sys.stderr)
        raise
    finally:
        if frontend:
            frontend.close()
        for event in model.release.values():
            event.set()
        subprocess.run([real_tmux, "-S", str(root / "tmux.sock"), "kill-server"], capture_output=True)
        if server.poll() is None:
            os.killpg(server.pid, signal.SIGTERM)
            server.wait(timeout=10)
        model.shutdown()
        model.server_close()
        serving.join(timeout=2)
        log.close()


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="twig-agent-check-") as directory:
        check(Path(directory))
