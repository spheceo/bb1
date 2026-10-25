#!/bin/zsh
# Runs host and client on this Mac and breaks things on purpose, to check that every
# recovery path brings the picture back without a manual reconnect.
#
#   scripts/recovery-test.sh            # test pattern (doesn't need Screen Recording)
#   scripts/recovery-test.sh --screen   # real screen capture
#
# Uses debug builds and port 47900 so it doesn't clash with an installed host.
set -uo pipefail
export BB1_NO_PROMPT=1  # no permission dialogs during tests
export BB1_NO_CLIPBOARD=1  # host and client share one clipboard here
export BB1_NO_AUDIO=1      # don't play the host's sound back into itself
export BB1_LOG_NAME=test  # keep the real host.log clean
root=${0:A:h:h}
cd "$root"
swift build >/dev/null || exit 1

source_flag=--synthetic
[[ "${1:-}" == "--screen" ]] && source_flag=""
port=47900
key=$(.build/debug/bb1 host --show-key)
out=$(mktemp -d)
host_pid=""

start_host() {
  .build/debug/bb1 host --headless $source_flag --port $port "$@" >>"$out/host.log" 2>&1 &
  host_pid=$!
}
step() { echo "[$(date +%H:%M:%S)] $*"; echo "=== $* ===" >>"$out/client.log"; }

start_host
sleep 2
.build/debug/bb1 client --headless --host 127.0.0.1:$port --key "$key" --seconds 52 >>"$out/client.log" 2>&1 &
client_pid=$!

sleep 7;  step "capture stops with an error (SIGUSR1)";     kill -USR1 $host_pid
sleep 7;  step "encoder silently stalls (SIGUSR2)";         kill -USR2 $host_pid
sleep 7;  step "host frozen for 3 s (SIGSTOP/SIGCONT)";     kill -STOP $host_pid; sleep 3; kill -CONT $host_pid
sleep 8;  step "host crashes and restarts";                 kill -9 $host_pid; sleep 1; start_host
sleep 9;  step "host restarted with 3% packet loss";        kill $host_pid; sleep 0.5; start_host --drop-percent 3
wait $client_pid
result=$?

kill $host_pid 2>/dev/null
echo
echo "---- client (one line per second) ----"
grep -vE "^20[0-9]{2}-" "$out/client.log"
echo
echo "---- host recoveries ----"
grep -E "FAULT|restarting|rebuilding|ended|connected|retry|stalled|failed" "$out/host.log" | sed 's/^20[0-9-]* //'
echo
echo "Logs: $out"
exit $result
