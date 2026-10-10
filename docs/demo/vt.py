"""Headless terminal for the demo Herdr client.

vt.py COLS ROWS FIFO -- cmd...   runs cmd in a pty and serves commands from FIFO:
  keys <json string>   write bytes to the client
  snap <path>          dump the screen as truecolor ANSI
"""
import json, os, pty, select, sys, struct, fcntl, termios
import pyte

cols, rows, fifo = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
cmd = sys.argv[sys.argv.index('--') + 1:]


class Screen(pyte.Screen):
    # pyte forwards private CSI variants (e.g. CSI ? 6 n) to handlers that do
    # not accept them; answer only the public forms, and send replies back.
    def report_device_status(self, *args, **kwargs):
        if not kwargs.get('private'):
            super().report_device_status(*args)

    def select_graphic_rendition(self, *args, **kwargs):
        if not kwargs.get('private'):
            super().select_graphic_rendition(*args)

    def write_process_input(self, data):
        os.write(master, data.encode())


screen = Screen(cols, rows)
stream = pyte.ByteStream(screen)

NAMES = {'black': '000000', 'red': 'cd3131', 'green': '0dbc79', 'brown': 'e5e510', 'yellow': 'e5e510',
         'blue': '2472c8', 'magenta': 'bc3fbc', 'cyan': '11a8cd', 'white': 'e5e5e5',
         'brightblack': '666666', 'brightred': 'f14c4c', 'brightgreen': '23d18b', 'brightbrown': 'f5f543',
         'brightyellow': 'f5f543', 'brightblue': '3b8eea', 'brightmagenta': 'd670d6', 'brightcyan': '29b8db',
         'brightwhite': 'ffffff'}


def color(value):
    if value == 'default':
        return None
    value = NAMES.get(value, value)
    return value if len(value) == 6 else None


def snapshot(path):
    lines = []
    for y in range(rows):
        row, out, last = screen.buffer[y], [], None
        for x in range(cols):
            ch = row[x]
            if ch.data == '':
                continue  # right half of a wide character
            fg, bg = color(ch.fg), color(ch.bg)
            if ch.reverse:
                fg, bg = bg or '000000', fg or 'e5e5e5'
            style = (fg, bg, ch.bold, ch.italics, ch.underscore)
            if style != last:
                sgr = ['0'] + (['1'] if ch.bold else []) + (['3'] if ch.italics else []) + (['4'] if ch.underscore else [])
                if fg: sgr.append('38;2;%d;%d;%d' % tuple(int(fg[i:i + 2], 16) for i in (0, 2, 4)))
                if bg: sgr.append('48;2;%d;%d;%d' % tuple(int(bg[i:i + 2], 16) for i in (0, 2, 4)))
                out.append('\x1b[%sm' % ';'.join(sgr))
                last = style
            out.append(ch.data)
        lines.append(''.join(out) + '\x1b[0m')
    with open(path, 'w') as f:
        f.write('\r\n'.join(lines))


pid, master = pty.fork()
if pid == 0:
    os.execvp(cmd[0], cmd)
fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
if not os.path.exists(fifo):
    os.mkfifo(fifo)
control = os.open(fifo, os.O_RDONLY | os.O_NONBLOCK)
pending = b''
while True:
    ready, _, _ = select.select([master, control], [], [], 0.5)
    if master in ready:
        try:
            data = os.read(master, 65536)
        except OSError:
            break
        if not data:
            break
        stream.feed(data)
    if control in ready:
        chunk = os.read(control, 65536)
        if not chunk:  # writer closed; reopen so later writers are heard
            os.close(control)
            control = os.open(fifo, os.O_RDONLY | os.O_NONBLOCK)
            continue
        pending += chunk
        while b'\n' in pending:
            line, pending = pending.split(b'\n', 1)
            verb, _, arg = line.decode().partition(' ')
            if verb == 'keys':
                os.write(master, json.loads(arg).encode())
            elif verb == 'snap':
                snapshot(arg)
