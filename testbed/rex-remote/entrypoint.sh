#!/bin/sh
# Admits the lab's one key, mounted read-only at /lab/authorized_keys, and runs sshd. B's Rex
# server is started by `testbed rex-remote up` as the user nd, because a container has no
# systemd to start it.
set -eu
install -o nd -g nd -m 0600 /lab/authorized_keys /home/nd/.ssh/authorized_keys
exec /usr/sbin/sshd -D -e
