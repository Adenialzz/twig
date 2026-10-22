# /// script
# requires-python = ">=3.11"
# ///
"""隔离验证 Codex 0.163.0：uv run examples/check_controller.py。

真实 app-server + 本地固定 Responses 响应，不读取真实认证、不调用远端模型。
仅验证协议与进程行为，不是正式主控、消息队列或完整 WebSocket 客户端。
"""

import base64
import fcntl
import hashlib
import http.server
import json
import os
from pathlib import Path
import shlex
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time


class Client:
    """仅用于本机验收的同步 WebSocket JSON-RPC 客户端。"""

    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(10)
        self.socket.connect(str(path))
        key = base64.b64encode(os.urandom(16)).decode()
        self.socket.sendall(
            (
                f"GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n"
                f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                "Sec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        header = b""
        while not header.endswith(b"\r\n\r\n"):
            header += self.read_bytes(1)
        expected = base64.b64encode(
            hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()
        )
        assert b"101 Switching Protocols" in header and expected in header, header
        self.next_id = 0
        self.call("initialize", {
            "clientInfo": {"name": "twig_controller_probe", "version": "0.1"},
            "capabilities": {"experimentalApi": True},
        })
        self.send({"method": "initialized"})

    def frame(self, opcode, data):
        mask = os.urandom(4)
        size = len(data)
        if size < 126:
            header = bytes([0x80 | opcode, 0x80 | size])
        else:
            header = bytes([0x80 | opcode, 0xfe]) + struct.pack("!H", size)
        self.socket.sendall(header + mask + bytes(
            value ^ mask[index % 4] for index, value in enumerate(data)
        ))

    def send(self, value):
        self.frame(1, json.dumps(value).encode())

    def read_bytes(self, size):
        data = b""
        while len(data) < size:
            chunk = self.socket.recv(size - len(data))
            if not chunk:
                raise EOFError("Codex 连接关闭")
            data += chunk
        return data

    def read(self):
        while True:
            opcode, length = self.read_bytes(2)
            assert opcode & 0x80 and not length & 0x80, "验收不支持分片或服务端掩码"
            size = length & 127
            if size == 126:
                size = struct.unpack("!H", self.read_bytes(2))[0]
            elif size == 127:
                size = struct.unpack("!Q", self.read_bytes(8))[0]
            data = self.read_bytes(size)
            if opcode & 15 == 9:
                self.frame(10, data)
                continue
            assert opcode & 15 == 1, (opcode, data)
            return json.loads(data)

    def call(self, method, params):
        self.next_id += 1
        self.send({"id": self.next_id, "method": method, "params": params})
        while True:
            result = self.read()
            if result.get("id") == self.next_id and "method" not in result:
                assert "error" not in result, result
                return result["result"]

    def start(self, thread, message):
        return self.call("turn/start", {
            "threadId": thread,
            "input": [{"type": "text", "text": message, "text_elements": []}],
        })["turn"]["id"]

    def thread(self, thread):
        return self.call("thread/read", {
            "threadId": thread, "includeTurns": True,
        })["thread"]

    def close(self):
        self.socket.close()


def relay(root, event):
    """模拟将来 Twig notify CLI；本地锁仅协调本验收中的两个回调。"""
    with (root / "relay.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        client = Client(root / "server.sock")
        try:
            controller = (root / "controller").read_text()
            state_file = root / "relay.json"
            state = json.loads(state_file.read_text())
            event_id = event["thread-id"] + ":" + event["turn-id"]
            if event_id in state["seen"]:
                return
            state["seen"].append(event_id)
            if event["thread-id"] != controller:
                state["pending"].append(event_id)
            current = client.thread(controller)
            state["observed_status"].append(current["status"]["type"])
            if state["pending"] and current["status"]["type"] == "idle":
                client.start(controller, "controller followup")
                state["sent"].extend(state["pending"])
                state["pending"] = []
            # 本实验不注入投递后崩溃，正式实现仍须处理结果未知的窗口。
            temporary = state_file.with_suffix(".tmp")
            temporary.write_text(json.dumps(state))
            temporary.replace(state_file)
        finally:
            client.close()


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        self.send_response(404)
        self.end_headers()

    def do_POST(self):
        assert self.path == "/v1/responses", self.path
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        inputs = request["input"]
        user_index = max(i for i, item in enumerate(inputs) if item.get("role") == "user")
        content = inputs[user_index]["content"]
        message = content if isinstance(content, str) else " ".join(
            item.get("text", "") for item in content
        )
        gate = "controller" if "hold-controller" in message else "worker"
        if "hold-" in message:
            self.server.entered[gate].set()
            assert self.server.release[gate].wait(15), "模拟响应等待超时"
        with self.server.counter_lock:
            self.server.count += 1
            index = self.server.count
        assert index < 20, "出现意外模型循环"
        item = {
            "id": f"msg_{index}", "type": "message", "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": f"probe complete {index}", "annotations": []}],
        }
        has_output = any(item.get("type") == "function_call_output" for item in inputs[user_index + 1:])
        if "controller followup" in message and not has_output:
            command = shlex.join([
                sys.executable, str(Path(__file__).resolve()),
                "--notification-cli", str(self.server.root),
            ])
            item = {
                "id": f"fc_{index}", "type": "function_call", "call_id": f"call_{index}",
                "name": "exec_command",
                "arguments": json.dumps({"cmd": command, "yield_time_ms": 1000}),
            }
        events = [
            {"type": "response.created", "response": {"id": f"resp_{index}", "status": "in_progress", "output": []}},
            {"type": "response.output_item.added", "output_index": 0, "item": item},
            {"type": "response.output_item.done", "output_index": 0, "item": item},
            {"type": "response.completed", "response": {
                "id": f"resp_{index}", "status": "completed", "output": [item],
                "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15},
            }},
        ]
        data = "".join(
            "event: " + event["type"] + "\ndata: " + json.dumps(event) + "\n\n"
            for event in events
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def wait_for(predicate, description):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError(description)


def check(root):
    home, cwd = root / "codex", root / "work"
    home.mkdir()
    cwd.mkdir()
    model = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Model)
    model.root = root
    model.count = 0
    model.counter_lock = threading.Lock()
    model.entered = {name: threading.Event() for name in ("worker", "controller")}
    model.release = {name: threading.Event() for name in ("worker", "controller")}
    serving = threading.Thread(target=model.serve_forever, daemon=True)
    serving.start()
    notify = [sys.executable, str(Path(__file__).resolve()), "--notify", str(root)]
    (home / "config.toml").write_text(f'''model = "gpt-5.4"
model_provider = "probe"
notify = {json.dumps(notify)}
[features]
code_mode = false
[model_providers.probe]
name = "Local deterministic probe"
base_url = "http://127.0.0.1:{model.server_port}/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
''')
    # 只传工具查找所需环境，不复制真实认证或 Codex 配置。
    env = {key: value for key, value in os.environ.items() if key in ("PATH", "LANG", "HOME", "TMPDIR")}
    env.update(CODEX_HOME=str(home), TWIG_FEISHU_WEBHOOK_URL="local-probe-no-network")
    state_file = root / "relay.json"
    state_file.write_text(json.dumps({"seen": [], "pending": [], "sent": [], "observed_status": []}))
    log = (root / "server.log").open("w+")
    process = subprocess.Popen(
        ["codex", "app-server", "--listen", "unix://" + str(root / "server.sock")],
        cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=log, start_new_session=True,
    )
    clients = []
    try:
        wait_for(lambda: (root / "server.sock").exists() or process.poll() is not None, "服务未启动")
        assert process.poll() is None, "隔离 app-server 启动失败"
        client = Client(root / "server.sock")
        clients.append(client)
        params = {
            "cwd": str(cwd), "approvalPolicy": "never", "sandbox": "danger-full-access",
            "baseInstructions": "Local protocol test.", "developerInstructions": "Local protocol test.",
        }
        controller = client.call("thread/start", params)["thread"]["id"]
        worker = client.call("thread/start", params)["thread"]["id"]
        (root / "controller").write_text(controller)
        main_turn = client.start(controller, "hold-controller")
        assert model.entered["controller"].wait(10)
        first = client.start(worker, "hold-worker")
        assert model.entered["worker"].wait(10)
        second = client.start(worker, "busy-input")
        assert first == second, "本版本忙时语义已变化，请重新评审"
        client.close()
        clients.clear()
        model.release["worker"].set()
        wait_for(lambda: len(json.loads(state_file.read_text())["pending"]) == 1, "离线任务完成未入队")
        assert not (root / "notification.json").exists(), "忙时不应提前推进主控"
        print("PASS: busy turn/start 加入原轮次；UI 断开后 notify 入队", flush=True)
        model.release["controller"].set()
        wait_for(lambda: (root / "notification.json").exists(), "主控未执行独立通知 CLI")
        wait_for(lambda: len(json.loads(state_file.read_text())["seen"]) == 3, "主控后续轮次未完成")
        state = json.loads(state_file.read_text())
        assert not state["pending"] and len(state["sent"]) == 1, state
        assert "active" in state["observed_status"] and "idle" in state["observed_status"], state
        notification = json.loads((root / "notification.json").read_text())
        assert notification["configured"] == "local-probe-no-network"
        print("PASS: 主控完成回调出队，后续轮次执行 CLI 并继承模拟飞书环境变量", flush=True)
        client = Client(root / "server.sock")
        clients.append(client)
        main = client.thread(controller)
        task = client.thread(worker)
        assert len(main["turns"]) == 2 and main["turns"][0]["id"] == main_turn, main
        assert all(turn["status"] == "completed" for turn in main["turns"]), main
        assert len(task["turns"]) == 1 and task["turns"][0]["status"] == "completed", task
        # 重放任务事件必须不再派发。
        relay(root, {"thread-id": worker, "turn-id": first})
        assert len(client.thread(controller)["turns"]) == 2
        print("PASS: 重连读取持久化历史，重复完成事件不重复派发", flush=True)
    except BaseException:
        log.flush()
        log.seek(0)
        print(log.read(), file=sys.stderr)
        raise
    finally:
        for client in clients:
            client.close()
        for event in model.release.values():
            event.set()
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=10)
        model.shutdown()
        model.server_close()
        serving.join(timeout=2)
        log.close()


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--notify":
        relay(Path(sys.argv[2]), json.loads(sys.argv[3]))
    elif len(sys.argv) > 1 and sys.argv[1] == "--notification-cli":
        # 只写临时文件；绝不向飞书或其他真实地址发请求。
        (Path(sys.argv[2]) / "notification.json").write_text(json.dumps({
            "configured": os.environ["TWIG_FEISHU_WEBHOOK_URL"],
        }))
    else:
        print(subprocess.check_output(["codex", "--version"], text=True).strip(), flush=True)
        with tempfile.TemporaryDirectory(prefix="twig-controller-check-") as directory:
            check(Path(directory))


if __name__ == "__main__":
    main()
