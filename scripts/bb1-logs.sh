#!/usr/bin/env bash
# Read bb1's logs on another of your devices over Tailscale (any bb1, Mac or Windows, serves them on
# port 47802 to devices on the same Tailscale account).
#
#   scripts/bb1-logs.sh pc                    list what the PC has
#   scripts/bb1-logs.sh pc client.log         the PC's client log (last 4 MB)
#   scripts/bb1-logs.sh pc host.log 200000    the last 200 KB
#
# TS_SOCKET=/path/to/tailscaled.sock goes through that tailscaled (e.g. a userspace one) instead of curl.
set -euo pipefail
device=${1:?usage: bb1-logs.sh DEVICE [LOG] [TAIL_BYTES]}
path=/
[[ -n "${2:-}" ]] && path="/logs/$2${3:+?tail=$3}"
if [[ -n "${TS_SOCKET:-}" ]]; then
  printf 'GET %s HTTP/1.0\r\n\r\n' "$path" | tailscale --socket="$TS_SOCKET" nc "$device" 47802 | sed '1,/^\r$/d'
else
  curl -fsS --max-time 30 "http://$device:47802$path"
fi
