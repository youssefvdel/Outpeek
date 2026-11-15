#!/usr/bin/env bash
# One command: fake Riot Client + Outpeek app, on this host, no PC.
#
#   scripts/fake-riot/run.sh                       # menus
#   scripts/fake-riot/run.sh --state coregame      # in a match, 3-2
#   scripts/fake-riot/run.sh --mcp-port 8099       # rest of argv goes to the app
#   FAKE_RIOT_KEEP=1 scripts/fake-riot/run.sh      # keep the temp data dir
#   OUTPEEK_BIN=… scripts/fake-riot/run.sh         # non-default app binary
#
# What it sets up:
#   * a throwaway %LOCALAPPDATA%/%APPDATA% pair under a temp root (nothing in
#     your real profile is read or written);
#   * server.py, which writes the Riot Client lockfile there and answers the
#     loopback API on 127.0.0.1;
#   * the app with LOCALAPPDATA/APPDATA pointed at that root.
#
# Teardown (exit, Ctrl-C, SIGTERM) stops the server, removes the lockfile and
# deletes the temp root unless FAKE_RIOT_KEEP=1. The server's own log — request
# path + status per line — is printed at the end either way.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

root="$(mktemp -d "${TMPDIR:-/tmp}/outpeek-fake-riot.XXXXXX")"
local_app="$root/Local"
roaming="$root/Roaming"
config_dir="$local_app/Riot Games/Riot Client/Config"
mkdir -p "$config_dir" "$roaming"

state="${FAKE_RIOT_STATE:-idle}"
port="${FAKE_RIOT_PORT:-0}"
server_log="$root/fake-riot.log"
server_pid=""

# `--state`/`--port` configure the fake client; everything else goes to the app
# untouched (e.g. `--mcp-port 8099`).
app_args=()
while (($#)); do
  case "$1" in
    --state) state="${2:?--state needs a name}"; shift 2 ;;
    --state=*) state="${1#*=}"; shift ;;
    --port) port="${2:?--port needs a number}"; shift 2 ;;
    --port=*) port="${1#*=}"; shift ;;
    *) app_args+=("$1"); shift ;;
  esac
done

cleanup() {
  local status=$?
  trap - EXIT INT TERM
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  echo
  # Startup (incl. the identity handshake) and the tail (incl. the last polls)
  # unless the log is short enough to print whole.
  echo "--- fake-riot ---"
  if [[ -f "$server_log" ]]; then
    lines="$(wc -l <"$server_log")"
    if (( lines <= 48 )); then
      cat "$server_log"
    else
      head -n 8 "$server_log"
      echo "[… $((lines - 48)) line(s) omitted …]"
      tail -n 40 "$server_log"
    fi
  fi
  if [[ "${FAKE_RIOT_KEEP:-0}" == "1" ]]; then
    echo "--- kept: $root ---"
  else
    rm -rf "$root"
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

python3 "$here/server.py" \
  --state "$state" \
  --port "$port" \
  --lockfile-dir "$config_dir" \
  >"$server_log" 2>&1 &
server_pid=$!

# Readiness: the lockfile exists AND loopback HTTPS answers. Waiting on the
# socket alone is not enough — the app's first call would race the write.
lockfile="$config_dir/lockfile"
ready=""
for _ in $(seq 1 100); do
  if ! kill -0 "$server_pid" 2>/dev/null; then
    echo "fake-riot: server exited during startup" >&2
    cat "$server_log" >&2
    exit 1
  fi
  if [[ -f "$lockfile" ]]; then
    port="$(cut -d: -f3 "$lockfile")"
    if python3 - "$port" >/dev/null 2>&1 <<'PY'
import ssl, sys, urllib.request

port = sys.argv[1]
context = ssl.create_default_context()
context.check_hostname = False
context.verify_mode = ssl.CERT_NONE
with urllib.request.urlopen(
    f"https://127.0.0.1:{port}/__fake/health", context=context, timeout=2
) as response:
    sys.exit(0 if response.status == 200 else 1)
PY
    then
      ready=1
      break
    fi
  fi
  sleep 0.1
done
if [[ -z "$ready" ]]; then
  echo "fake-riot: not ready after 10s" >&2
  cat "$server_log" >&2
  exit 1
fi

bin="${OUTPEEK_BIN:-}"
if [[ -z "$bin" ]]; then
  for candidate in "$repo/outpeek-app/target/debug/outpeek-app" \
                   "$repo/outpeek-app/target/release/outpeek-app"; do
    [[ -x "$candidate" ]] && bin="$candidate" && break
  done
fi
if [[ -z "$bin" || ! -x "$bin" ]]; then
  echo "fake-riot: no app binary found; build it first:" >&2
  echo "  cargo build --manifest-path outpeek-app/Cargo.toml --features ui-debug" >&2
  echo "  (or set OUTPEEK_BIN=/path/to/outpeek-app)" >&2
  exit 1
fi

echo "fake-riot: ready on 127.0.0.1:$port (state=$state)"
echo "fake-riot: lockfile $lockfile"
echo "fake-riot: data root $root"
echo "fake-riot: switch state: curl -sk \"https://127.0.0.1:$port/__fake/state?name=coregame\""
echo "--- outpeek ---"

run=("$bin")
if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]] \
   && [[ "${FAKE_RIOT_XVFB:-1}" != "0" ]] \
   && command -v xvfb-run >/dev/null 2>&1; then
  echo "fake-riot: no display; wrapping the app in xvfb-run -a"
  run=(xvfb-run -a "$bin")
fi

set +e
LOCALAPPDATA="$local_app" \
APPDATA="$roaming" \
RUST_LOG="${RUST_LOG:-info}" \
"${run[@]}" "${app_args[@]}"
status=$?
set -e
exit "$status"
