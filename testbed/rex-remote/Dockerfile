# Host B of the remote-host lab: Debian with Superlogical's signed Rex package and an sshd
# that admits one key, which the lab mounts at /lab/authorized_keys when the container starts.
# The image holds no key. See testbed/README.md, "The remote-host lab".
FROM debian:trixie-slim

# The Rex package version, as `apt-cache madison rex` prints it, and the fingerprint of the
# key that signs Superlogical's apt repository. The build stops when the downloaded keyring
# holds any other primary key.
ARG REX_VERSION=0.1.0~preview.18
ARG REX_KEY_FINGERPRINT=E28636AC71905C6DDE40C1EDFAA6B80F641307A7
ARG REX_REPO=https://pkgs.superlogical.com/rex

RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      openssh-server ca-certificates curl gnupg procps python3 sqlite3 busybox zsh \
 && rm -rf /var/lib/apt/lists/*

# What the official installer (https://pkgs.superlogical.com/rex/install.sh) does for apt,
# with the key checked against REX_KEY_FINGERPRINT and the version pinned. Debian has an
# unrelated package also named rex, so a pin makes apt take rex from Superlogical only, and
# the installed package must name Superlogical as its maintainer.
RUN install -d -m 0755 /etc/apt/keyrings \
 && curl -fsSL "$REX_REPO/keys/rex-archive-keyring.gpg" -o /etc/apt/keyrings/rex-archive-keyring.gpg \
 && primaries=$(gpg --show-keys --with-colons /etc/apt/keyrings/rex-archive-keyring.gpg \
      | awk -F: '$1 == "pub" { want = 1; next } want && $1 == "fpr" { print $10; want = 0 }') \
 && test "$primaries" = "$REX_KEY_FINGERPRINT" \
 && printf 'Types: deb\nURIs: %s/apt\nSuites: preview\nComponents: main\nArchitectures: %s\nSigned-By: /etc/apt/keyrings/rex-archive-keyring.gpg\n' \
      "$REX_REPO" "$(dpkg --print-architecture)" > /etc/apt/sources.list.d/rex.sources \
 && printf 'Package: rex\nPin: origin pkgs.superlogical.com\nPin-Priority: 1001\n' > /etc/apt/preferences.d/rex \
 && apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "rex=$REX_VERSION" \
 && dpkg-query -W -f '${Maintainer}\n' rex | grep -qi superlogical \
 && rm -rf /var/lib/apt/lists/* \
 && rex version

# In the lab, `docker pause` caught sshd's pre-auth child mid-call, which died of SIGSYS on resume,
# and OpenSSH 10's per-source penalties then refused A for 90 s (check 8). The lab turns the
# penalties off; a real B keeps them.
RUN useradd -m -s /bin/bash nd \
 && install -d -o nd -g nd -m 0700 /home/nd/.ssh \
 && install -d -m 0755 /run/sshd \
 && printf '%s\n' 'PasswordAuthentication no' 'PerSourcePenalties no' > /etc/ssh/sshd_config.d/lab.conf
COPY entrypoint.sh /usr/local/sbin/lab-entrypoint
EXPOSE 22 7297
CMD ["/usr/local/sbin/lab-entrypoint"]
