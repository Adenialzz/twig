# /// script
# requires-python = ">=3.11"
# dependencies = ["pyte==0.8.2"]
# ///
"""PTY 验收：先 cargo build --example control_mode，再 uv run 本文件。"""

import fcntl
import os
from pathlib import Path
import pty
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
        fcntl.ioctl(
            self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0)
        )
        self.before = termios.tcgetattr(self.slave)
        self.screen = pyte.Screen(100, 30)
        self.stream = pyte.ByteStream(self.screen)

        def setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        self.process = subprocess.Popen(
            [str(binary), "--socket", socket, "--session", "demo"],
            stdin=self.slave,
            stdout=self.slave,
            stderr=self.slave,
            preexec_fn=setup,
            env={**os.environ, "TERM": "xterm-256color"},
        )

    def send(self, text):
        os.write(self.master, text)

    def wait(self, needle):
        until = time.monotonic() + 5
        while time.monotonic() < until:
            if select.select([self.master], [], [], 0.05)[0]:
                self.stream.feed(os.read(self.master, 65536))
            if needle in "\n".join(self.screen.display):
                return
            assert self.process.poll() is None, self.screen.display
        raise AssertionError((needle, self.screen.display))

    def detach(self):
        self.send(b"\x17d")
        self.process.wait(timeout=4)
        assert self.process.returncode == 0
        assert termios.tcgetattr(self.slave) == self.before

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait()
        os.close(self.master)
        os.close(self.slave)


def main():
    binary = Path(__file__).resolve().parents[1] / "target/debug/examples/control_mode"
    with tempfile.TemporaryDirectory(prefix="twig-vim-pty-") as root:
        socket = root + "/tmux.sock"
        target = Path(root) / "note.txt"
        target.write_text("ORIGINAL_MARKER\n")

        def mux(*args):
            return subprocess.check_output(["tmux", "-S", socket, *args])

        mux(
            "-f", "/dev/null", "new-session", "-d", "-s", "demo",
            "-x", "100", "-y", "29", "env PS1='READY> ' /bin/sh",
        )
        clients = []
        try:
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
            second.send(b"\x1b[<65;2;3M" * 12)
            second.wait("SCROLL_059")
            # 有限输出负载：约 120 行，用于交互延迟回归，不是吞吐压测。
            second.send(b"i=0; while [ $i -lt 120 ]; do printf 'LOAD_%s\\n' $i; i=$((i+1)); sleep 0.01; done\r")
            second.wait("LOAD_10")
            started = time.monotonic()
            second.send(b"\x03printf 'INPUT_%s\\n' RESPONSIVE\r")
            second.wait("INPUT_RESPONSIVE")
            latency = time.monotonic() - started
            assert latency < 2, latency
            second.detach()
            print(f"通过：Vim 编辑与重连、保存、Shell、终端还原；有限输出期间输入延迟 {latency:.3f}s")
        finally:
            for client in clients:
                client.close()
            subprocess.run(
                ["tmux", "-S", socket, "kill-server"],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )


if __name__ == "__main__":
    main()
