# /// script
# requires-python = ">=3.11"
# dependencies = ["pyte==0.8.2"]
# ///
"""PTY 验收：先 cargo build --example control_mode，再 uv run 本文件。"""

import fcntl
import faulthandler
import base64
import os
from pathlib import Path
import pty
import re
import select
import shlex
import struct
import subprocess
import tempfile
import termios
import time

import pyte


class Frontend:
    def __init__(self, binary, socket):
        self.master, self.slave = pty.openpty()
        os.set_blocking(self.master, False)
        fcntl.ioctl(
            self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0)
        )
        self.before = termios.tcgetattr(self.master)
        # PENDIN 是内核重排待输入的瞬时标志，不属于需要还原的 raw 模式配置。
        self.before[3] &= ~termios.PENDIN
        self.screen = pyte.Screen(100, 30)
        self.stream = pyte.ByteStream(self.screen)
        self.output = bytearray()

        def setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        self.process = subprocess.Popen(
            [str(binary), "--socket", socket, "--session", "demo"],
            stdin=self.slave,
            stdout=self.slave,
            stderr=self.slave,
            preexec_fn=setup,
            # 剪贴板验收只捕获 OSC 52，不碰真实系统剪贴板或外层 tmux。
            env={**{k: v for k, v in os.environ.items() if k not in ("TMUX", "TMUX_PANE")},
                 "TERM": "xterm-256color", "SSH_CONNECTION": "twig-pty-test"},
        )

    def send(self, text):
        written = os.write(self.master, text)
        assert written == len(text), f"PTY 输入未完整写入：{written}/{len(text)} 字节"

    def read_output(self, timeout):
        if not select.select([self.master], [], [], timeout)[0]:
            return False
        try:
            data = os.read(self.master, 65536)
        except BlockingIOError:
            return False
        self.output.extend(data)
        self.stream.feed(data)
        return bool(data)

    def settle(self):
        until = time.monotonic() + 2
        while self.read_output(0.15):
            assert time.monotonic() < until, "终端未停止刷新"

    def wait(self, needle):
        self.until(lambda: needle in "\n".join(self.screen.display), needle)

    def until(self, predicate, description):
        print(f"等待：{description}", flush=True)
        until = time.monotonic() + 5
        while time.monotonic() < until:
            self.read_output(0.05)
            # 先采样退出状态，避免退出条件判断后恰好正常退出却被下面误报。
            exited = self.process.poll() is not None
            if predicate():
                return
            assert not exited, (description, self.process.returncode, self.screen.display)
        raise AssertionError((description, self.screen.display, bytes(self.output[-1500:])))

    def detach(self):
        self.send(b"\x17d")
        self.until(lambda: self.process.poll() is not None, "前端退出")
        assert self.process.returncode == 0
        restored = termios.tcgetattr(self.master)
        restored[3] &= ~termios.PENDIN
        assert restored == self.before, (restored, self.before)
        self.settle()
        assert re.findall(rb"\x1b\[([0-6]) q", self.output)[-1] == b"0"

    def cursor(self, style):
        self.until(
            lambda: re.findall(rb"\x1b\[([0-6]) q", self.output)[-1:] == [str(style).encode()],
            f"光标样式 {style}",
        )

    def close(self):
        print(f"清理前端：PID {self.process.pid}", flush=True)
        try:
            if self.process.poll() is None:
                self.process.kill()
                # macOS 的 PTY 退出也可能等待输出排空；等待期间仍须读取 master。
                self.until(lambda: self.process.poll() is not None, "清理前端退出")
        finally:
            os.close(self.master)
            os.close(self.slave)


def main():
    binary = Path(__file__).resolve().parents[1] / "target/debug/examples/control_mode"
    with tempfile.TemporaryDirectory(prefix="twig-vim-pty-") as root:
        socket = root + "/tmux.sock"
        target = Path(root) / "note.txt"
        target.write_text("ORIGINAL_MARKER\n")

        def mux(*args):
            print(f"tmux：{args}", flush=True)
            return subprocess.check_output(["tmux", "-S", socket, *args], timeout=10)

        print("阶段：启动隔离 tmux", flush=True)
        mux(
            "-f", "/dev/null", "new-session", "-d", "-s", "demo",
            "-x", "100", "-y", "29", "env PS1='READY> ' /bin/sh",
        )
        clients = []
        try:
            print("阶段：Vim 编辑与断连", flush=True)
            first = Frontend(binary, socket)
            clients.append(first)
            first.wait("READY>")
            first.send(f"vim -Nu NONE -n {shlex.quote(str(target))}\r".encode())
            first.wait("ORIGINAL_MARKER")
            first.send(b"GoBEFORE_DETACH\x1b")
            first.wait("BEFORE_DETACH")
            pids = mux("list-panes", "-F", "#{pane_pid}")
            first.detach()
            assert mux("display-message", "-p", "#{pane_current_command}").strip() == b"vim"

            second = Frontend(binary, socket)
            print("阶段：重连、保存与历史复制", flush=True)
            clients.append(second)
            second.wait("BEFORE_DETACH")
            assert mux("list-panes", "-F", "#{pane_pid}") == pids
            second.send(b"GoAFTER_RECONNECT\x1b")
            second.wait("AFTER_RECONNECT")
            second.send(b":wq\r")
            second.wait("READY>")
            assert target.read_text() == (
                "ORIGINAL_MARKER\nBEFORE_DETACH\nAFTER_RECONNECT\n"
            )
            second.send(b"i=0; while [ $i -lt 60 ]; do printf 'SCROLL_%03d\\n' $i; i=$((i+1)); done\r")
            second.wait("SCROLL_059")
            second.send(b"\x1b[<64;2;3M" * 12)
            second.wait("SCROLL_010")
            second.settle()
            # 历史中的选区固定在开始拖动的画面，后台新增输出不能更改复制内容。
            row = next(y for y, line in enumerate(second.screen.display) if "SCROLL_010" in line)
            col = second.screen.display[row].index("SCROLL_010")
            second.output.clear()
            old_reverse = second.screen.buffer[row][col].reverse
            second.send(f"\x1b[<0;{col+1};{row+1}M\x1b[<32;{col+10};{row+1}M".encode())
            second.until(lambda: second.screen.buffer[row][col].reverse != old_reverse, "选区反显")
            mux("send-keys", "printf 'BACKGROUND_UPDATE\\n'", "Enter")
            second.send(f"\x1b[<0;{col+10};{row+1}m".encode())
            second.wait("已请求复制 10 chars")
            assert "已请求复制 10 chars" in second.screen.display[-3]
            assert "twig 实验工作台" in second.screen.display[-1]
            second.until(lambda: "已请求复制" not in "\n".join(second.screen.display), "复制提示自动消失")
            assert second.screen.buffer[row][col].reverse != old_reverse
            copies = re.findall(rb"\x1b\]52;c;([^\x07]*)\x07", second.output)
            assert [base64.b64decode(value) for value in copies] == [b"SCROLL_010"]
            assert "SCROLL_010" in second.screen.display[row]
            # Esc 只清除选区，不向程序发送；Ctrl-C 仍照常传给程序。
            second.send(b"\x1b")
            second.until(lambda: second.screen.buffer[row][col].reverse == old_reverse, "Esc 清除反显")
            # 单击不能重复写入剪贴板。
            second.output.clear()
            second.send(b"\x1b[<0;2;3M\x1b[<0;2;3m")
            second.settle()
            assert b"\x1b]52;" not in second.output
            second.send(b"\x1b[<65;2;3M" * 12)
            second.wait("SCROLL_059")
            # 有限输出负载：约 120 行，用于交互延迟回归，不是吞吐压测。
            # 在第十行暂停，等选区已经显示才继续，避免较慢 CI 在选区确认前耗尽负载。
            load_gate = Path(root) / "load-ready"
            load_script = Path(root) / "load.sh"
            # 明确负载的中断行为，避免不同 /bin/sh 对循环内子进程 SIGINT 的处理差异。
            load_script.write_text("trap 'exit 130' INT\ni=0; while [ $i -lt 120 ]; do printf 'LOAD_%s\\n' $i; "
                                   f"if [ $i -eq 10 ]; then while [ ! -f {shlex.quote(str(load_gate))} ]; do sleep 0.01; done; fi; "
                                   "i=$((i+1)); sleep 0.01; done; printf 'LOAD_%s\\n' FINISHED\n")
            second.send(f"PS1='LOAD_READY> '; sh {shlex.quote(str(load_script))}\r".encode())
            second.wait("LOAD_10")
            # 有选区时 Ctrl-C 仍中断 Shell 前台程序，不拦截为复制。
            second.send(b"\x1b[<0;1;3M\x1b[<32;5;3M")
            second.until(lambda: second.screen.buffer[2][0].reverse, "负载期间选区反显")
            load_gate.touch()
            second.until(lambda: "LOAD_11" in mux("capture-pane", "-p", "-S", "-150").decode().splitlines(), "选区就绪后继续输出")
            started = time.monotonic()
            try:
                # 中断可能清空终端输入队列；先确认 Shell 已恢复，再发送下一条命令。
                second.send(b"\x03")
                second.until(
                    # SIGINT 回显可能与新提示符同行（^CLOAD_READY>）。
                    lambda: any(line.rstrip().endswith("LOAD_READY>") for line in second.screen.display),
                    "Ctrl-C 后返回 Shell",
                )
                assert "LOAD_FINISHED" not in mux("capture-pane", "-p", "-S", "-150").decode().splitlines(), "负载自然结束，未被 Ctrl-C 中断"
                second.send(b"printf 'INPUT_%s\\n' RESPONSIVE\r")
                second.wait("INPUT_RESPONSIVE")
                latency = time.monotonic() - started
                assert latency < 2, latency
            except AssertionError:
                print("中断验收失败时的 tmux 实际画面：", flush=True)
                print(mux("capture-pane", "-p", "-S", "-150").decode(), flush=True)
                print(mux("list-panes", "-F", "#{pane_id} pid=#{pane_pid} command=#{pane_current_command} dead=#{pane_dead}").decode(), flush=True)
                raise

            # 隔离的模拟程序接收滚轮；右侧非活动 Pane 验证坐标和目标归属。
            receiver = Path(root) / "mouse-input"
            pane = mux("split-window", "-h", "-d", "-P", "-F", "#{pane_id}").strip().decode()
            for mode, enable in [("sgr", "1000h\\033[?1006h"),
                                 ("legacy", "1002h"),
                                 ("utf8", "1003h\\033[?1005h"),
                                 ("off", "1006h")]:
                receiver.write_bytes(b"")
                command = (
                    "stty raw -echo; printf '\\033[?1000l\\033[?1002l\\033[?1003l"
                    "\\033[?1005l\\033[?1006l\\033[?1049h\\033[?"
                    + enable + "MOUSE_READY_" + mode + "'; cat > " + shlex.quote(str(receiver))
                )
                mux("respawn-pane", "-k", "-t", pane, command)
                second.wait("MOUSE_READY_" + mode)
                left, top = map(int, mux("display-message", "-p", "-t", pane,
                                        "#{pane_left} #{pane_top}").split())
                # SGR 输入坐标从 1 开始；程序应收到 Pane 内的 (2, 3)。
                second.send(f"\x1b[<64;{left+2};{top+3}M\x1b[<65;{left+2};{top+3}M".encode())
                expected = (b"\x1b[<64;2;3M\x1b[<65;2;3M" if mode == "sgr"
                            else b"\x1b[M`\x22\x23\x1b[Ma\x22\x23")
                if mode == "off":
                    second.settle()
                    assert receiver.read_bytes() == b"", "未启用鼠标时不能发送转义序列"
                else:
                    second.until(lambda: receiver.read_bytes() == expected, "滚轮协议 " + mode)
                    # 左键点击和拖选不发送给程序。
                    second.send(f"\x1b[<0;{left+2};{top+3}M\x1b[<32;{left+5};{top+3}M"
                                f"\x1b[<0;{left+5};{top+3}m".encode())
                    second.settle()
                    assert receiver.read_bytes() == expected
                    second.send(b"\x1b")
                mux("select-pane", "-t", "%0")
            mux("kill-pane", "-t", pane)
            second.detach()
            print(f"通过：Vim 编辑与重连、保存、Shell、历史拖选复制与画面冻结、鼠标模式及分屏滚轮转发、终端还原；有限输出期间输入延迟 {latency:.3f}s")
            # 独立 Zsh 配置，不加载用户配置；ZLE 切换只输出光标序列。
            Path(root, ".zshrc").write_text(
                "bindkey -v\nKEYTIMEOUT=1\nPROMPT='ZSH_READY> '\n"
                "zle-keymap-select() {\n"
                "  if [[ $KEYMAP == vicmd ]]; then printf '\\e[2 q'; "
                "else printf '\\e[6 q'; fi\n}\n"
                "zle-line-init() { zle-keymap-select; }\n"
                "zle -N zle-keymap-select\nzle -N zle-line-init\n"
            )
            mux("respawn-pane", "-k", "-t", "%0",
                f"env ZDOTDIR={shlex.quote(root)} zsh -d")
            third = Frontend(binary, socket)
            print("阶段：Zsh 光标与重连", flush=True)
            clients.append(third)
            third.wait("ZSH_READY>")
            third.cursor(6)
            third.output.clear()
            third.send(b"\x1b")
            third.cursor(2)
            third.detach()
            fourth = Frontend(binary, socket)
            clients.append(fourth)
            fourth.wait("ZSH_READY>")
            fourth.cursor(2)
            # 另一个 Pane 使用下划线，切回时恢复 Zsh 的块状光标。
            other = mux("split-window", "-h", "-P", "-F", "#{pane_id}",
                        "printf '\\033[4 q'; exec sleep 30").decode().strip()
            fourth.cursor(4)
            mux("select-pane", "-t", "%0")
            fourth.cursor(2)
            mux("kill-pane", "-t", other)
            fourth.output.clear()
            fourth.send(b"i")
            fourth.cursor(6)
            fourth.detach()
            print("通过：Zsh vi 模式光标切换、跨 Pane 样式同步、重连恢复与退出重置")
        finally:
            try:
                for client in clients:
                    client.close()
            finally:
                subprocess.run(
                    ["tmux", "-S", socket, "kill-server"],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    timeout=5,
                )


if __name__ == "__main__":
    # 包括原生调用与清理在内的硬超时，避免只得到 CI 的 20 分钟取消日志。
    faulthandler.dump_traceback_later(60, exit=True)
    main()
    faulthandler.cancel_dump_traceback_later()
