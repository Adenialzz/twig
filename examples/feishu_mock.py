"""仅供隔离验收：本机飞书长连接与应用 API，不连接真实平台。"""
import base64
import hashlib
import json
import select
import socket
import struct
import threading


def integer(value):
    output = bytearray()
    while value > 127:
        output.append((value & 127) | 128)
        value >>= 7
    output.append(value)
    return bytes(output)


def blob(field, data):
    return integer(field << 3 | 2) + integer(len(data)) + data


def frame(kind, payload=b"", method=0):
    headers = {"type": kind}
    if method == 1:
        headers.update(sum="1", seq="0", message_id="test-frame")
    data = b"\x08\x00\x10\x00\x18\x07\x20" + integer(method)
    for key, value in headers.items():
        data += blob(5, blob(1, key.encode()) + blob(2, value.encode()))
    return data + blob(8, payload)


def fields(data):
    offset = 0
    def read_integer():
        nonlocal offset
        value, shift = 0, 0
        while True:
            byte = data[offset]
            offset += 1
            value |= (byte & 127) << shift
            if byte < 128:
                return value
            shift += 7
    result = {}
    while offset < len(data):
        tag = read_integer()
        if tag & 7 == 0:
            value = read_integer()
        else:
            assert tag & 7 == 2
            length = read_integer()
            value = data[offset:offset + length]
            offset += length
        result.setdefault(tag >> 3, []).append(value)
    return result


class Feishu:
    def __init__(self):
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(2)
        self.socket = None
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.ready = threading.Event()
        self.acks = []
        self.replies = []
        self.error = None
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def api(self, handler):
        path = handler.path
        if path not in ("/callback/ws/endpoint", "/open-apis/auth/v3/tenant_access_token/internal") and not path.startswith("/open-apis/im/v1/messages/"):
            return False
        body = json.loads(handler.rfile.read(int(handler.headers["Content-Length"])))
        if path == "/callback/ws/endpoint":
            assert body == {"AppID": "test-app", "AppSecret": "test-secret"}
            value = {"code": 0, "data": {"URL": f"ws://127.0.0.1:{self.listener.getsockname()[1]}/?service_id=7", "ClientConfig": {"PingInterval": 1}}}
        elif "tenant_access_token" in path:
            value = {"code": 0, "tenant_access_token": "test-token"}
        else:
            assert handler.headers["Authorization"] == "Bearer test-token"
            self.replies.append((path, json.loads(body["content"])["text"]))
            value = {"code": 0, "data": {"message_id": f"reply-{len(self.replies)}"}}
        encoded = json.dumps(value).encode()
        handler.send_response(200)
        handler.send_header("Content-Length", str(len(encoded)))
        handler.end_headers()
        handler.wfile.write(encoded)
        return True

    def send(self, data):
        assert self.ready.wait(10), self.error
        header = bytes([0x82, len(data)]) if len(data) < 126 else b"\x82\x7e" + struct.pack("!H", len(data))
        with self.lock:
            self.socket.sendall(header + data)

    def event(self, message, user="ou_allowed", chat_type="p2p"):
        event = {"header": {"event_type": "im.message.receive_v1"}, "event": {
            "sender": {"sender_type": "user", "sender_id": {"open_id": user}},
            "message": {"message_id": message, "chat_id": "oc_test", "chat_type": chat_type,
                        "message_type": "text", "content": json.dumps({"text": "offline-feishu-request"})}}}
        self.send(frame("event", json.dumps(event).encode(), 1))

    def serve(self):
        try:
            while not self.stop.is_set():
                if not select.select([self.listener], [], [], .1)[0]:
                    continue
                connection, _ = self.listener.accept()
                self.socket = connection
                with connection:
                    connection.settimeout(3)
                    def read(size):
                        result = b""
                        while len(result) < size:
                            chunk = connection.recv(size - len(result))
                            if not chunk:
                                raise EOFError()
                            result += chunk
                        return result
                    header = b""
                    while not header.endswith(b"\r\n\r\n"):
                        header += read(1)
                    key = next(line.split(b":", 1)[1].strip() for line in header.split(b"\r\n") if line.lower().startswith(b"sec-websocket-key:"))
                    accept = base64.b64encode(hashlib.sha1(key + b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest())
                    connection.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + b"\r\n\r\n")
                    self.ready.set()
                    try:
                        while not self.stop.is_set():
                            if not select.select([connection], [], [], .1)[0]:
                                continue
                            opcode, length = read(2)
                            if opcode & 15 == 8:
                                break
                            assert opcode & 15 == 2 and length & 128
                            size = length & 127
                            if size == 126:
                                size = struct.unpack("!H", read(2))[0]
                            mask = read(4)
                            packet = bytes(b ^ mask[i % 4] for i, b in enumerate(read(size)))
                            decoded = fields(packet)
                            if decoded[4][0] == 0:
                                self.send(frame("pong", b'{"PingInterval":1}'))
                            else:
                                self.acks.append(json.loads(decoded[8][0]))
                    except EOFError:
                        pass
                    finally:
                        self.ready.clear()
        except BaseException as error:
            if not self.stop.is_set():
                self.error = repr(error)

    def close(self):
        self.stop.set()
        self.thread.join(timeout=5)
        self.listener.close()
        assert not self.thread.is_alive(), "飞书模拟线程未退出"
        assert self.error is None, self.error
