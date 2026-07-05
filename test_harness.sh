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

# config file (~/.koenrc via KOEN_CONFIG) supplies settings; env still wins
printf '# comment\nexport KOEN_FAKE_TRANSLATION="FROM CONFIG"\n' > "$dir/rc"
out=$(KOEN_CONFIG="$dir/rc" "$bin" "한국어 입력")
[ "$out" = "FROM CONFIG" ] || { echo "FAIL config load: $out"; exit 1; }
out=$(KOEN_CONFIG="$dir/rc" KOEN_FAKE_TRANSLATION="FROM ENV" "$bin" "한국어 입력")
[ "$out" = "FROM ENV" ] || { echo "FAIL env precedence: $out"; exit 1; }

# codex target: reply-in-Korean suffix appended after the swap
printf '한국어 입력\r' | KOEN_HARNESS_CMD="$dir/child.sh" KOEN_FAKE_TRANSLATION="SWAPPED" "$bin" codex >/dev/null 2>&1 || true
case "$(cat "$cap")" in
  "SWAPPED Reply in Korean"*) ;;
  *) echo "FAIL codex suffix: $(cat "$cap")"; exit 1 ;;
esac

# swap stashes the Korean original; --prompt-hook shows it once, then clears
printf '안녕하세요 세계\r' | KOEN_ORIG_FILE="$dir/orig" KOEN_HARNESS_CMD="$dir/child.sh" KOEN_FAKE_TRANSLATION="SWAPPED" "$bin" claude >/dev/null 2>&1 || true
[ "$(cat "$dir/orig")" = "안녕하세요 세계" ] || { echo "FAIL orig stash: $(cat "$dir/orig")"; exit 1; }
out=$(KOEN_ORIG_FILE="$dir/orig" "$bin" --prompt-hook)
[ "$out" = '{"systemMessage":"원문: 안녕하세요 세계"}' ] || { echo "FAIL prompt-hook: $out"; exit 1; }
out=$(KOEN_ORIG_FILE="$dir/orig" "$bin" --prompt-hook)
[ -z "$out" ] || { echo "FAIL prompt-hook not consumed: $out"; exit 1; }

# stop hook: last_assistant_message field is preferred (no transcript needed)
out=$(printf '{"last_assistant_message":"All done."}' | KOEN_FAKE_TRANSLATION="모두 완료했습니다." "$bin" --stop-hook)
case "$out" in
  '{"systemMessage":"모두 완료했습니다."}') ;;
  *) echo "FAIL stop-hook direct: $out"; exit 1 ;;
esac

# stop hook: falls back to reading the transcript file
tp="$dir/transcript.jsonl"
printf '%s\n%s\n' \
  '{"type":"assistant","message":{"content":[{"type":"text","text":"draft"}]}}' \
  '{"type":"assistant","message":{"content":[{"type":"text","text":"Done. I fixed the bug."}]}}' > "$tp"
out=$(printf '{"transcript_path":"%s"}' "$tp" | KOEN_FAKE_TRANSLATION="버그를 고쳤습니다." "$bin" --stop-hook)
case "$out" in
  '{"systemMessage":"버그를 고쳤습니다."}') ;;
  *) echo "FAIL stop-hook: $out"; exit 1 ;;
esac

# stop hook stays silent when the response is already Korean
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"이미 한국어로 된 응답입니다"}]}}' > "$tp"
out=$(printf '{"transcript_path":"%s"}' "$tp" | KOEN_FAKE_TRANSLATION="X" "$bin" --stop-hook)
[ -z "$out" ] || { echo "FAIL stop-hook korean skip: $out"; exit 1; }

# KOEN_REPLY=en disables the suffix
printf '한국어 입력\r' | KOEN_REPLY=en KOEN_HARNESS_CMD="$dir/child.sh" KOEN_FAKE_TRANSLATION="SWAPPED" "$bin" codex >/dev/null 2>&1 || true
[ "$(cat "$cap")" = "SWAPPED" ] || { echo "FAIL KOEN_REPLY=en: $(cat "$cap")"; exit 1; }

rm -rf "$dir"
echo "harness ok"
