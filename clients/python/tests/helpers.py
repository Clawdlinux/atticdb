import itertools
import json
import os
import contextlib
import shutil
import socket
import subprocess
import threading
import time

TOKEN = "token-a-0123456789abcdefghijklmnopqrstuv"
BIN = os.path.abspath(os.environ["ZERODB_BIN"])
# Socket paths must fit sun_path (104 bytes on macOS), so the default is short, as in
# the Rust tests. Set ZERODB_TEST_DIR to a short directory when the checkout is deep.
ROOT = os.environ.get("ZERODB_TEST_DIR") or os.path.join(
    os.path.dirname(__file__), "..", "..", "..", "target", "s"
)
NEXT = itertools.count()


def wait_for(check, what, timeout=30.0):
    """Polls `check` until it returns a true value."""
    deadline = time.monotonic() + timeout
    while not (result := check()):
        assert time.monotonic() < deadline, f"timed out waiting for {what}"
        time.sleep(0.01)
    return result


def turns(*contents):
    return [{"role": "user", "content": content} for content in contents]


class Server:
    """`zerodb serve` on TCP port 0 and the default socket, with tenant `a`."""

    def __init__(self):
        os.makedirs(ROOT, exist_ok=True)
        self.dir = os.path.abspath(os.path.join(ROOT, f"py{os.getpid():x}-{next(NEXT)}"))
        os.mkdir(self.dir, 0o700)
        self.data = os.path.join(self.dir, "d")
        os.mkdir(self.data, 0o700)
        self.config = os.path.join(self.dir, "c.json")
        fd = os.open(self.config, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as file:
            json.dump({"tenants": [{"name": "a", "token": TOKEN}]}, file)
        self.log_path = os.path.join(self.dir, "serve.log")
        self.process = None
        self.start()

    def start(self):
        offset = os.path.getsize(self.log_path) if os.path.exists(self.log_path) else 0
        # Tests read request lines to see unknown answers and rejected retries.
        args = ["serve", "--config", self.config, "--data-dir", self.data, "--log-requests"]
        with open(self.log_path, "ab") as log:
            self.process = subprocess.Popen(
                [BIN, *args, "--listen", "127.0.0.1:0"],
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=log,
            )

        def serving():
            assert self.process.poll() is None, f"server exited: {self.log()}"
            lines = self.log()[offset:].splitlines(keepends=True)
            ready = (line for line in lines if line.startswith("zerodb: serving "))
            return next((line for line in ready if line.endswith("\n")), None)

        fields = dict(f.split("=", 1) for f in wait_for(serving, "serving").split()[2:])
        self.instance = fields["instance"]
        self.url = {"http": f"http://{fields['tcp']}", "unix": f"unix://{fields['unix']}"}
        self.socket = fields["unix"]

    def log(self):
        with open(self.log_path) as log:
            return log.read()

    def kill(self):
        self.process.kill()
        self.process.wait()

    def close(self):
        self.kill()
        shutil.rmtree(self.dir)


def send_without_reading(path, body):
    """Writes one append request on the Unix socket and returns the open socket."""
    data = json.dumps(body).encode()
    head = f"POST /v1/append HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\n"
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(path)
    sock.sendall(f"{head}Content-Length: {len(data)}\r\n\r\n".encode() + data)
    return sock


@contextlib.contextmanager
def trickle(path, seconds):
    """A Unix socket server that reads a request, then sends a response head one byte
    every 0.1 s for `seconds`, until the client hangs up."""
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(path)
    listener.listen()

    def serve():
        conn, _ = listener.accept()
        with conn:
            conn.recv(1 << 16)
            head = b"HTTP/1.1 200 \r\nX-Pad: " + b"x" * int(seconds * 10)
            with contextlib.suppress(OSError):
                for byte in head[: int(seconds * 10)]:
                    time.sleep(0.1)
                    conn.sendall(bytes([byte]))

    thread = threading.Thread(target=serve)
    thread.start()
    try:
        yield
    finally:
        thread.join()
        listener.close()
