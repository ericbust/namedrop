"""A scripted Rex control server on a Unix socket, for the tests that run
the binary against one: the WebSocket upgrade, Rex's hello and welcome, and
one reply per request. It serves one connection at a time.

`answer(method)` returns a request's payload, or raises EOFError to close
the connection. Load it by path, as `python3 -I` puts no test folder on
`sys.path`.
"""
import contextlib
import json
import socket
import struct
import threading


def exact(conn, n):
    data = b''
    while len(data) < n:
        part = conn.recv(n - len(data))
        if not part:
            raise EOFError
        data += part
    return data


def frame(conn):
    a, b = exact(conn, 2)
    n = b & 127
    if n == 126:
        n = struct.unpack('!H', exact(conn, 2))[0]
    if n == 127:
        n = struct.unpack('!Q', exact(conn, 8))[0]
    mask = exact(conn, 4) if b & 128 else b''
    data = exact(conn, n)
    if mask:
        data = bytes(c ^ mask[i % 4] for i, c in enumerate(data))
    if a & 15 == 8:
        raise EOFError
    return json.loads(data)


def send(conn, value):
    data = json.dumps(value).encode()
    n = len(data)
    conn.sendall(bytes([0x81, n]) + data if n < 126 else bytes([0x81, 126]) + struct.pack('!H', n) + data)


class Server:
    def __init__(self, path, answer):
        self.socket = path
        self.answer = answer
        self.calls = []
        self.listener = socket.socket(socket.AF_UNIX)
        self.listener.bind(str(path))
        self.listener.listen()
        self.done = False
        self.thread = threading.Thread(target=self.serve)
        self.thread.start()

    def close(self):
        self.done = True
        with contextlib.suppress(OSError):
            c = socket.socket(socket.AF_UNIX)
            c.connect(str(self.socket))
            c.close()
        self.thread.join(3)
        self.listener.close()
        assert not self.thread.is_alive()

    def serve(self):
        while not self.done:
            try:
                conn, _ = self.listener.accept()
            except OSError:
                return
            with conn:
                conn.settimeout(3)
                try:
                    head = b''
                    while not head.endswith(b'\r\n\r\n'):
                        head += exact(conn, 1)
                    conn.sendall(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n'
                                 b'Sec-WebSocket-Protocol: rex.control.v1\r\n\r\n')
                    assert frame(conn)['type'] == 'hello'
                    send(conn, {'type': 'welcome', 'protocol': 'rex.control.v1', 'api_revision': 14,
                                'limits': {'max_message_bytes': 4194304}})
                    while True:
                        request = frame(conn)
                        self.calls.append(request['method'])
                        send(conn, {'type': 'response', 'id': request['id'], 'seq': 1,
                                    'payload': self.answer(request['method'])})
                except (EOFError, OSError):
                    pass
