# Ubuntu 24.04 booting systemd, with a user manager for `dev`, for `.github/linux-check.sh`.
FROM ubuntu:24.04
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      systemd systemd-sysv dbus dbus-user-session git ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd -m -s /bin/bash dev
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
