#!/usr/bin/env bash
set -euo pipefail
umask 077

if [[ "$#" -lt 1 || "$#" -gt 2 ]]; then
  echo "Usage: $0 <tunnel_ssh_route> [ssh_user]" >&2
  exit 64
fi

route="$1"
ssh_user="${2:-${DAYWEAVE_SSH_USER:-dayweave}}"

if [[ ! "${route}" =~ ^ssh-[a-z0-9]+\.tunnel\.applications\.eu-north1\.nebius\.cloud:443$ ]]; then
  echo "The SSH route is not an expected DayWeave eu-north1 tunnel route." >&2
  exit 1
fi
reserved_users=" _apt admin backup bin daemon games gnats irc landscape list lp mail man messagebus news nobody pollinate proxy root sshd sync sys syslog systemd-network systemd-timesync tcpdump tss ubuntu uucp uuidd www-data "
if [[ ! "${ssh_user}" =~ ^[a-z_][a-z0-9_-]{0,30}$ || "${reserved_users}" == *" ${ssh_user} "* ]]; then
  echo "The SSH username is invalid or reserved." >&2
  exit 1
fi
for command_name in nebius ssh; do
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    echo "${command_name} is required." >&2
    exit 1
  fi
done

host="${route%:443}"
exec ssh \
  -o 'ProxyCommand=nebius --no-browser --no-check-update tunnel connect --stdio %h:%p' \
  -p 443 \
  "${ssh_user}@${host}"
