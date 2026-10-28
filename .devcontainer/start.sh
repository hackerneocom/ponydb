#!/usr/bin/env bash
set -euo pipefail

if [[ $(id -u) -ne 0 ]]; then
  exec sudo -- "$0" "$@"
fi

umask 077
install -d -m 0755 /run/sshd
install -d -m 0700 /etc/ssh/host_keys
chmod 1777 /workspace

# Docker creates a missing bind-mount source directory as root, preventing
# developer from creating Codex's state database or daemon directory.
install -d -m 0755 /home/developer/.codex
if [[ "$(stat -c %u /home/developer/.codex)" != "$(id -u developer)" ]]; then
  chown -R developer:developer /home/developer/.codex
fi

# Bind mounts can leave their parent owned by root, preventing tools such as
# gh from creating config directories. Only change the parent itself.
install -d -o developer -g developer -m 0700 /home/developer/.config

# The Cursor credential bind mount may be created by Docker as root when the
# host has never installed/logged in to Cursor CLI. Empty credentials are OK.
install -d -m 0700 /home/developer/.config/cursor
if [[ "$(stat -c %u /home/developer/.config/cursor)" != "$(id -u developer)" ]]; then
  chown -R developer:developer /home/developer/.config/cursor
fi

# Host gh credentials are optional. Keep an empty/new mount writable so login
# inside the container works even when the host has never installed gh.
install -d -m 0700 /home/developer/.config/gh
if [[ "$(stat -c %u /home/developer/.config/gh)" != "$(id -u developer)" ]]; then
  chown -R developer:developer /home/developer/.config/gh
fi

if [[ ! -f /etc/ssh/host_keys/ssh_host_ed25519_key ]]; then
  ssh-keygen -q -t ed25519 -N '' -f /etc/ssh/host_keys/ssh_host_ed25519_key
fi

# OpenSSH accepts method "none" only when the password is empty and both
# PermitEmptyPasswords and PasswordAuthentication are enabled.
passwd -d developer
echo 'SSH: authentication is disabled for developer; anyone who can reach this port can log in.' >&2

config=/etc/ssh/sshd_config_devcontainer
/usr/sbin/sshd -t -f "$config"
exec /usr/sbin/sshd -D -e -f "$config"
