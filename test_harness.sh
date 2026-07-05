#!/bin/sh
# Offline pty harness smoke test — no API calls (KOEN_FAKE_TRANSLATION).
# The child pty's line discipline applies our backspaces exactly like a real
# input box, so the capture file shows what the "upper model" would receive.
set -e
bin=./target/release/koen
dir=$(mktemp -d)
cap="$dir/cap.txt"
printf '#!/bin/sh\ncat > %s\n' "$cap" > "$dir/child.sh"
chmod +x "$dir/child.sh"

# exit status ignored: the child gets SIGHUP when the pty closes after stdin
# EOF; the assertion is the captured content, not the code
run() { KOEN_HARNESS_CMD="$dir/child.sh" KOEN_FAKE_TRANSLATION="$2" "$bin" claude >/dev/null 2>&1 || true; }

printf '안녕하세요 세계\r' | run in "ENGLISH SWAP"
[ "$(cat "$cap")" = "ENGLISH SWAP" ] || { echo "FAIL korean swap: $(cat "$cap")"; exit 1; }

printf 'hello english\r' | run in "X"
[ "$(cat "$cap")" = "hello english" ] || { echo "FAIL passthrough: $(cat "$cap")"; exit 1; }

printf '/goal 한국어 목표\r' | run in "X"
[ "$(cat "$cap")" = "/goal 한국어 목표" ] || { echo "FAIL slash skip: $(cat "$cap")"; exit 1; }

[ "$("$bin" "plain english, no api call")" = "plain english, no api call" ] || { echo "FAIL cli passthrough"; exit 1; }

rm -rf "$dir"
echo "harness ok"
