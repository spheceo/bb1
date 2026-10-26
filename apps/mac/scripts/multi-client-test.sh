#!/bin/zsh
# Two computers on one Mac at once, and one computer connecting twice, with a test host and test clients on this Mac.
# Each client gets its own data folder, as if they were separate computers.
set -uo pipefail
root=${0:A:h:h}
cd "$root"
swift build >/dev/null || exit 1
export BB1_NO_PROMPT=1 BB1_NO_CLIPBOARD=1 BB1_NO_AUDIO=1 BB1_LOG_NAME=test
T=$(mktemp -d)
port=47970
bin=.build/debug/bb1
pass=0; fail=0
result() { if [[ $1 == ok ]]; then echo "PASS  $2"; pass=$((pass+1)); else echo "FAIL  $2"; fail=$((fail+1)); fi; }

BB1_SUPPORT_DIR=$T/host $bin host --headless --synthetic --port $port >$T/host.log 2>&1 &
host_pid=$!
sleep 2
key=$(BB1_SUPPORT_DIR=$T/host $bin host --show-key)

client() { # client <dir> <seconds> → full output in $T/<dir>.<n>.out
  local dir=$1 seconds=$2 n=$3
  BB1_SUPPORT_DIR=$T/$dir perl -e 'alarm 40; exec @ARGV' $bin client --headless --seconds $seconds --host 127.0.0.1:$port --key "$key" >$T/$dir.$n.out 2>/dev/null
}
outcome() { grep -oE "OK.*|FAIL.*|ended.*" $T/$1.out | head -1; }

# Two computers: the second joins while the first is streaming; both keep their picture.
client a 12 1 & a=$!
sleep 4
client b 5 1
wait $a
[[ $(outcome b.1) == OK* ]] && result ok "second computer gets a picture" || result no "second computer ($(outcome b.1))"
[[ $(outcome a.1) == OK* ]] && result ok "first computer stays connected" || result no "first computer ($(outcome a.1))"
grep -q "reconnecting" $T/a.1.out && result no "first computer had to reconnect" || result ok "…without reconnecting"
grep -q "also connected:" $T/host.log && result ok "host streams to both at once" || result no "host never had both connected"

# One computer twice: the newer copy wins, the older one stops (no tug of war).
client c 10 1 & c=$!
sleep 3
client c 4 2
wait $c
[[ $(outcome c.2) == OK* ]] && result ok "same computer again gets the picture" || result no "same computer again ($(outcome c.2))"
[[ $(outcome c.1) == ended*"connected again"* ]] && result ok "…and its older copy stops" || result no "older copy ($(outcome c.1))"
n=$(grep -c "reconnected$" $T/host.log)
[[ $n == 1 ]] && result ok "replaced once, not back and forth" || result no "replaced $n times"

kill $host_pid 2>/dev/null
echo "$pass passed, $fail failed (logs in $T)"
(( fail == 0 ))
