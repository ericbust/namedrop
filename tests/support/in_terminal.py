"""Run a command with a terminal COLUMNS wide as its output, and relay what it prints.

usage: in_terminal.py COLUMNS COMMAND [ARGS...]
"""
import fcntl, os, pty, select, struct, subprocess, sys, termios

master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 50, int(sys.argv[1]), 0, 0))
child = subprocess.Popen(sys.argv[2:], stdin=subprocess.DEVNULL, stdout=slave, stderr=slave)
os.close(slave)
# A server the command leaves running may keep the terminal open, so stop
# reading once the command itself has exited and nothing more arrives.
while select.select([master], [], [], 0.2)[0] or child.poll() is None:
    try:
        chunk = os.read(master, 65536)
    except OSError:
        break
    if not chunk:
        break
    sys.stdout.buffer.write(chunk)
os.close(master)
sys.exit(child.wait(timeout=10))
