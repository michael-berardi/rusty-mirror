#!/usr/bin/env bash
# End-to-end proof on a real Tauri app (macOS): builds Tally with the mirror,
# runs it fully off-screen, drives every CLI command, then proves the consumer
# build is clean. Needs no human, touches nothing outside a temp directory,
# and tears down everything it started, pass or fail.
#
#   scripts/e2e.sh            # build + test
#   KEEP_SHOTS=dir scripts/e2e.sh   # also copy captures to dir for a look
set -euo pipefail

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
[ "$(uname)" = Darwin ] || { echo "e2e: native capture is macOS-only for now" >&2; exit 2; }
command -v python3 >/dev/null || { echo "e2e: python3 is needed for JSON assertions" >&2; exit 2; }

# Short on purpose: unix socket paths top out around 104 bytes.
work=$(mktemp -d /tmp/rm-e2e.XXXXXX)
export RUSTY_MIRROR_HOME="$work/home" TALLY_DATA="$work/data" TALLY_HIDDEN=1
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$repo/target"}
app_pid="" app_start="" passed=0

stopped_pid=""
stop_app() {
  if [ -n "$app_pid" ]; then
    # Only stop the process we launched: same pid, same start time, same binary.
    if [ "$(ps -p "$app_pid" -o lstart= 2>/dev/null)" = "$app_start" ] &&
       ps -p "$app_pid" -o comm= 2>/dev/null | grep -q '/tally$'; then
      kill "$app_pid" 2>/dev/null || true
      for _ in $(seq 50); do kill -0 "$app_pid" 2>/dev/null || break; sleep 0.1; done
      kill -0 "$app_pid" 2>/dev/null && kill -9 "$app_pid" 2>/dev/null || true
    fi
    kill -0 "$app_pid" 2>/dev/null && { echo "e2e: pid $app_pid still running" >&2; return 1; }
    stopped_pid=$app_pid app_pid=""
  fi
}

cleanup() {
  local status=$?
  stop_app || status=1
  if [ $status -ne 0 ] && [ -f "$work/tally.log" ]; then echo "--- tally.log"; tail -20 "$work/tally.log"; fi
  rm -rf "$work"
  [ -e "$work" ] && echo "e2e: could not remove $work" >&2
  if [ $status -eq 0 ]; then echo "e2e: PASS ($passed checks)"; else echo "e2e: FAIL after $passed checks" >&2; fi
  exit $status
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

step() { printf '\n== %s\n' "$*"; }
ok() { passed=$((passed + 1)); echo "   ok: $*"; }
# expect <python expression over `r`> <json>
expect() { python3 -c 'import json,sys; r=json.loads(sys.argv[2]); sys.exit(0 if eval(sys.argv[1]) else 1)' "$1" "$2" \
  || { echo "   expected: $1"; echo "$2" | head -40; exit 1; }; }

step "build"
cargo build -q -p rusty-mirror-cli
RUSTY_MIRROR=1 "$repo/examples/tally/build.sh" >/dev/null
cargo build -q -p tally --features mirror-debug
rm_bin="$CARGO_TARGET_DIR/debug/rusty-mirror"
rmc() { "$rm_bin" --app dev.rustymirror.tally "$@" || { local c=$?; echo "   (rusty-mirror $1 exited $c)" >&2; return $c; }; }
ok "CLI and mirror-enabled Tally built"

step "launch Tally off-screen"
mkdir -p "$TALLY_DATA"
"$CARGO_TARGET_DIR/debug/tally" >"$work/tally.log" 2>&1 &
app_pid=$!
app_start=$(ps -p "$app_pid" -o lstart=)
for _ in $(seq 200); do [ -S "$RUSTY_MIRROR_HOME/dev.rustymirror.tally/control.sock" ] && break; sleep 0.1; done
out=$(rmc doctor); expect 'r["ok"] and r["state"]["open"] is False' "$out"
ok "control socket is up; no mirror open yet"

step "open"
out=$(rmc open --seconds 300); expect 'r["ok"] and r["result"]["stateHooked"]' "$out"
out=$(rmc state); expect 'r["result"]["open"] and not r["result"]["visible"] and not r["result"]["focused"]' "$out"
ok "mirror open, invisible and unfocused"
# The page loads asynchronously; wait until the app has rendered.
for _ in $(seq 100); do
  out=$(rmc eval "return document.querySelector('#build')?.textContent" || true)
  python3 -c 'import json,sys; sys.exit(0 if json.loads(sys.argv[1]).get("value")=="v1" else 1)' "$out" 2>/dev/null && break
  sleep 0.1
done
expect 'r["value"] == "v1"' "$out"
ok "mirror renders the real frontend"

step "seed and local interaction"
out=$(rmc eval 'return window.__RUSTY_MIRROR_SEED__.state'); expect 'r["value"] == {"tally": 0}' "$out"
out=$(rmc eval "for (let i = 0; i < 3; i++) document.querySelector('#plus').click(); return document.querySelector('#count').textContent")
expect 'r["value"] == "3"' "$out"
ok "seeded from the primary; clicks work locally"

step "effects are denied natively"
out=$(rmc eval "try { await window.__TAURI__.core.invoke('save_tally', { value: 99 }); return 'saved' } catch (e) { return String(e) }")
expect '"not allowed in Rusty Mirror" in r["value"]' "$out"
[ ! -e "$TALLY_DATA/tally.txt" ] || { echo "   save_tally wrote to disk from the mirror"; exit 1; }
out=$(rmc eval "return await window.__TAURI__.core.invoke('get_tally')"); expect 'r["value"] == 0' "$out"
ok "save_tally rejected, nothing written, primary state untouched"
out=$(rmc eval "try { await fetch('https://example.com/'); return 'reached' } catch (e) { return 'blocked' }")
expect 'r["value"] == "blocked"' "$out"
out=$(rmc eval "localStorage.setItem('mirror-only', 'yes'); return window.open('https://example.com') === null")
expect 'r["value"] is True' "$out"
ok "network, popups and persistent storage are fenced off"
out=$(rmc eval 'throw new Error("deliberate")' || true); expect 'not r["ok"] and "deliberate" in r["error"]' "$out"
ok "script errors come back as errors (exit 1)"

step "capture"
out=$(rmc capture "$work/shots/embedded.png"); expect 'r["ok"] and not r["image"]["looksBlank"] and r["mirrorVisible"] is False' "$out"
rmc resize 1280x800 >/dev/null
out=$(rmc capture "$work/shots/wide.png"); expect 'r["ok"] and r["image"]["width"] >= 1280' "$out"
out=$(rmc capture "$work/shots/wide.png" || true); expect 'not r["ok"]' "$out"
ok "hidden native captures, resized, never overwritten"

step "stage, refresh, rollback"
"$repo/examples/tally/build.sh" "$work/consumer-dist" >/dev/null
out=$(rmc stage "$work/consumer-dist" || true); expect 'not r["ok"] and "consumer build" in r["error"]' "$out"
RUSTY_MIRROR=1 "$repo/examples/tally/build.sh" "$work/v2" >/dev/null
sed -i '' "s/const BUILD = 'v1'/const BUILD = 'v2'/" "$work/v2/app.js"
out=$(rmc stage "$work/v2"); expect 'r["ok"] and not r["alreadyStaged"]' "$out"
rev=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["revision"])' "$out")
out=$(rmc refresh "$rev"); expect 'r["ok"]' "$out"
for _ in $(seq 100); do
  out=$(rmc eval "return [document.querySelector('#build')?.textContent, document.querySelector('#count')?.textContent]" || true)
  python3 -c 'import json,sys; sys.exit(0 if json.loads(sys.argv[1]).get("value")==["v2","3"] else 1)' "$out" 2>/dev/null && break
  sleep 0.1
done
expect 'r["value"] == ["v2", "3"]' "$out"
out=$(rmc eval "return localStorage.getItem('mirror-only')"); expect 'r["value"] == "yes"' "$out"
ok "new revision loaded with the mirror's own state carried across"
out=$(rmc capture "$work/shots/v2.png"); expect 'r["ok"]' "$out"
out=$(rmc rollback); expect 'r["ok"]' "$out"
for _ in $(seq 100); do
  out=$(rmc eval "return document.querySelector('#build')?.textContent" || true)
  python3 -c 'import json,sys; sys.exit(0 if json.loads(sys.argv[1]).get("value")=="v1" else 1)' "$out" 2>/dev/null && break
  sleep 0.1
done
expect 'r["value"] == "v1"' "$out"
out=$(rmc apply || true); expect 'not r["ok"] and "embedded" in r["error"]' "$out"
ok "rollback is mirror-first; apply refuses an unverified embedded target"

step "apply"
rmc refresh "$rev" >/dev/null
out=$(rmc apply); expect 'r["ok"] and r["result"]["primaryRevision"] == "'"$rev"'"' "$out"
for _ in $(seq 50); do
  out=$(rmc state); python3 -c 'import json,sys; sys.exit(0 if sys.argv[2] in (json.loads(sys.argv[1])["result"].get("primaryUrl") or "") else 1)' "$out" "$rev" && break
  sleep 0.1
done
expect '"'"$rev"'" in r["result"]["primaryUrl"]' "$out"
ok "primary navigated to the verified revision"

step "close"
out=$(rmc close); expect 'r["ok"]' "$out"
out=$(rmc state); expect 'r["result"]["open"] is False' "$out"
out=$(rmc close || true); expect 'not r["ok"]' "$out"
ok "closed; closing twice is an error, not a shrug"

step "run owns the lifecycle"
out=$("$rm_bin" --app dev.rustymirror.tally run --seconds 60 -- sh -c '"$0" state | grep -q "\"open\": true"' "$rm_bin" || true)
expect 'r["ok"] and r["exitCode"] == 0 and not r["stillOpen"]' "$out"
out=$("$rm_bin" --app dev.rustymirror.tally run --seconds 60 -- false || true)
expect 'not r["ok"] and r["exitCode"] == 1 and r["closed"] and not r["stillOpen"]' "$out"
ok "run closes the mirror on success and on failure"

if [ -n "${KEEP_SHOTS:-}" ]; then mkdir -p "$KEEP_SHOTS"; cp "$work"/shots/*.png "$KEEP_SHOTS"/; echo "   captures copied to $KEEP_SHOTS"; fi

step "stop Tally"
stop_app
ok "Tally stopped (pid $stopped_pid)"

step "consumer build is clean"
"$repo/examples/tally/build.sh" "$work/consumer-dist" >/dev/null
out=$(rmc audit "$work/consumer-dist" --capabilities "$repo/examples/tally/src-tauri/capabilities"); expect 'r["ok"]' "$out"
set +e; out=$(rmc audit "$work/v2"); code=$?; set -e
[ $code -eq 3 ] || { echo "   audit of a mirror build exited $code, wanted 3"; exit 1; }
out=$(rmc audit "$CARGO_TARGET_DIR/debug/tally" || true); expect 'not r["ok"]' "$out"
"$repo/examples/tally/build.sh" >/dev/null
cargo build -q -p tally
out=$(rmc audit "$CARGO_TARGET_DIR/debug/tally"); expect 'r["ok"]' "$out"
ok "consumer frontend and binary carry no mirror; mirror builds are caught"
