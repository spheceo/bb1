#!/bin/zsh
# Checks every way of signing in, with a test host and a test client on this Mac.
# Host and client get separate data folders, as if they were two Macs.
set -uo pipefail
root=${0:A:h:h}
cd "$root"
swift build >/dev/null || exit 1
export BB1_NO_PROMPT=1 BB1_NO_CLIPBOARD=1 BB1_NO_AUDIO=1 BB1_LOG_NAME=test
T=$(mktemp -d)
port=47960
bin=.build/debug/bb1
tsip=$(tailscale ip -4 2>/dev/null | head -1)
pass=0; fail=0
result() { if [[ $1 == ok ]]; then echo "PASS  $2"; pass=$((pass+1)); else echo "FAIL  $2"; fail=$((fail+1)); fi; }

BB1_SUPPORT_DIR=$T/host BB1_TEST_PAIRING_CODE_FILE=$T/code $bin host --headless --synthetic --port $port >$T/host.log 2>&1 &
host_pid=$!
sleep 2
key=$(BB1_SUPPORT_DIR=$T/host $bin host --show-key)

client() { # client <dir> <args...>  → prints OK/FAIL/ended lines
  local dir=$1; shift
  BB1_SUPPORT_DIR=$T/$dir perl -e 'alarm 40; exec @ARGV' $bin client --headless --seconds 6 "$@" 2>/dev/null | grep -oE "OK.*|FAIL.*|ended.*" | head -1
}

r=$(client a --host 127.0.0.1:$port --key "$key");          [[ $r == OK* ]] && result ok "old pairing key works" || result no "old pairing key ($r)"
[[ -s $T/a/known-hosts.json ]] && result ok "…and it was turned into a remembered pairing" || result no "no remembered pairing saved"
r=$(client a --host 127.0.0.1:$port);                        [[ $r == OK* ]] && result ok "remembered pairing signs in with no key" || result no "remembered pairing ($r)"

rm -f $T/code
r=$( (while [[ ! -s $T/code ]]; do sleep 0.2; done; cat $T/code; echo) | BB1_SUPPORT_DIR=$T/b perl -e 'alarm 40; exec @ARGV' $bin client --headless --seconds 6 --host 127.0.0.1:$port --pair 2>/dev/null | grep -E "OK|FAIL|ended" | head -1)
[[ $r == *OK* ]] && result ok "6-digit code pairing works" || result no "code pairing ($r)"
r=$(client b --host 127.0.0.1:$port);                        [[ $r == OK* ]] && result ok "…and next time it connects with no code" || result no "after code pairing ($r)"

rm -f $T/code
r=$(echo 000000 | BB1_SUPPORT_DIR=$T/c perl -e 'alarm 40; exec @ARGV' $bin client --headless --seconds 6 --host 127.0.0.1:$port --pair 2>/dev/null | grep -E "OK|FAIL|ended" | head -1)
[[ $r == *ended* ]] && result ok "a wrong code is refused ($r)" || result no "wrong code was accepted? ($r)"

# A Mac can't reach its own Tailscale address, so Tailscale sign-in is only testable from a second Mac:
#   BB1_TS_TEST_HOST=100.x.y.z scripts/auth-test.sh   (run on the client Mac against a running host)
if [[ -n ${BB1_TS_TEST_HOST:-} ]]; then
  tsip=$BB1_TS_TEST_HOST; port=47800
  r=$(client d --host $tsip:$port --tailscale);              [[ $r == OK* ]] && result ok "Tailscale sign-in works (via $tsip)" || result no "Tailscale sign-in ($r)"
  r=$(client d --host $tsip:$port);                          [[ $r == OK* ]] && result ok "…and then it's remembered" || result no "after Tailscale sign-in ($r)"
  r=$(client e --host 127.0.0.1:$port --tailscale);          [[ $r == *ended* ]] && result ok "Tailscale sign-in refused over a non-Tailscale address" || result no "Tailscale over LAN was accepted? ($r)"
else
  echo "SKIP  Tailscale sign-in (needs a second Mac: see the comment above)"
fi

r=$(client f --host 127.0.0.1:$port </dev/null);                        [[ $r == *ended* ]] && result ok "an unknown Mac without a code is refused" || result no "unknown Mac got in? ($r)"
kill $host_pid 2>/dev/null
echo
echo "$pass passed, $fail failed. Logs: $T"
[[ $fail == 0 ]]
