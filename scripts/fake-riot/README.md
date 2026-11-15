# fake-riot — run the native app without a Riot Client

A fake Riot Client for this Linux box: a loopback HTTPS server that answers the
three endpoints `outpeek-app` reads, writes the lockfile it looks for, and lets
you switch the app between menus, agent select and an in-match score — live,
without restarting anything.

One command sets all of it up:

```bash
scripts/fake-riot/run.sh --mcp-port 8099
```

That creates a throwaway `LOCALAPPDATA`/`APPDATA` pair under a temp root,
starts the fake client, writes the lockfile, runs the app pointed at that root,
and tears everything down (server, lockfile, temp dir) when the app exits.

Files:

| File | What it is |
| --- | --- |
| `server.py` | The fake client. Python 3 stdlib only, binds `127.0.0.1`, writes the lockfile, logs request path + status. |
| `states/*.json` | Presence fixtures: `idle` (menus), `pregame` (agent select, Ascent, 0-0), `coregame` (in match, Ascent, 3-2). |
| `run.sh` | The one command above. |
| `mcp-shot.py` | Screenshots the running app through Slint's embedded MCP server (the proof step). |

## What it fakes

Everything is mirrored from the code that consumes it — read those before
changing a field:

| Endpoint | Body | Read by |
| --- | --- | --- |
| `GET /entitlements/v1/token` | `accessToken` (JWT-shaped, `pp.c = eu`), `token`, `subject` (PUUID) | `identity.rs::fetch_entitlements`, `rank.rs::region_from_access_token` |
| `GET /player-account/aliases/v1/active` | `game_name`, `tag_line`, `puuid` | `identity.rs::fetch_identity` |
| `GET /chat/v4/presences` | `presences[]` with the local player's `private` blob, base64 of the JSON in the state file | `identity.rs::fetch_presences` → `live_facts.rs::derive_facts` |
| anything else | `404` JSON | proves the app degrades |
| `GET /__fake/health`, `/__fake/states`, `/__fake/state?name=…` | harness control routes (no auth) | `run.sh`, you |

The lockfile is written to `<--lockfile-dir>/lockfile` in the real shape —
`Riot Client:<pid>:<port>:<password>:https` — and is removed on shutdown. The
password is random per run, never logged, and every Riot-shaped route requires
`Authorization: Basic riot:<password>` exactly like the real client (a wrong
password gets `401 BAD_AUTH`, so the credential path is exercised, not bypassed).

The presence blob carries what `derive_facts` reads:

```json
{
  "matchPresenceData": {
    "sessionLoopState": "INGAME",          // MENU | PREGAME | INGAME
    "matchMap": "Ascent",                  // display name or /Game/Maps/… path
    "queueId": "competitive",
    "gameScoreType": 1
  },
  "partyOwnerMatchScoreAllyTeam": 3,
  "partyOwnerMatchScoreEnemyTeam": 2
}
```

## What it deliberately does NOT fake

* **The remote shard `/mmr` read.** `rank.rs` calls
  `https://pd.eu.a.pvp.net/mmr/v1/players/<puuid>` with the entitlement triple.
  Faking that needs DNS/TLS interception for a real Riot hostname, which this
  harness refuses to do. Expect `RankView::error` ("Riot session expired" or an
  HTTP error) — **that is the harness working correctly**, and it is why rank on
  this path must come from Tracker Network (TRN, by handle) instead.
* **TRN itself.** No network is intercepted; the app's TRN client talks to the
  real service (or fails) exactly as it would on Windows.
* **A real Riot Client.** No login, no `/session/v1/sessions/...`, no
  `/player-account/lookup/v2/namesets-for-puuids`, no game logs, no LAN,
  no WebView2, no Win32/DWM. Windows-only facts still need the PC.
* **Input to VALORANT.** There is no game; the HUD remains gated by the
  Windows-only foreground check and stays hidden on this host.

## Switching states

`--state` picks the startup fixture; `/__fake/state` changes it on a running
server (takes effect on the app's next 2.5 s poll):

```bash
scripts/fake-riot/run.sh --state pregame          # start in agent select

# in another shell (the port is printed by run.sh and in the lockfile):
curl -sk "https://127.0.0.1:<port>/__fake/state?name=coregame"
curl -sk "https://127.0.0.1:<port>/__fake/states"     # list what exists
```

Editing a `states/*.json` file also works without a restart: the file is read
per request. `private` stays readable JSON in the file and is base64-encoded on
the wire, so a fixture can be edited like any other JSON.

Ports: `--port` (or `FAKE_RIOT_PORT`) pins the fake client's port; `0` picks a
free one. The app's own `--mcp-port` is a different port — everything after
`--state`/`--port` is forwarded to the app untouched.

## Screenshot proof (Slint MCP)

The app serves MCP when started with `--mcp-port PORT`; `run.sh` forwards the
flag. Build with `--features ui-debug` for the element tree, add
`SLINT_EMIT_DEBUG_INFO=1` at build time for named widgets. Screenshots work in
either build.

```bash
cargo build --manifest-path outpeek-app/Cargo.toml --features ui-debug
scripts/fake-riot/run.sh --mcp-port 8099 &
python3 scripts/fake-riot/mcp-shot.py --port 8099 --out /tmp/opencode/fake-riot.png
```

## Proving the offline path

The point of the account and TRN-profile caches is that the tracker still opens
on the last account with Riot closed. Prove it with the same data root:

```bash
# 1. one online run, keeping the temp data root
FAKE_RIOT_KEEP=1 scripts/fake-riot/run.sh --state coregame --mcp-port 8099
#    the app shows lil ga7ed#zngr and LIVE · Ascent · 3-2; run.sh prints the
#    root it kept and removes the lockfile on the way out

# 2. relaunch against that root, no server
root=/tmp/outpeek-fake-riot.XXXXXX        # the kept path from step 1
LOCALAPPDATA="$root/Local" APPDATA="$root/Roaming" \
  xvfb-run -a outpeek-app/target/debug/outpeek-app --mcp-port 8099
```

Expected, once the cache lane lands: the handle and the cached TRN profile
render from `$root/Roaming/Outpeek/account.json` and the profile cache, while
the live panel says the Riot Client is not running.

Expected **today** (this is the gap, see below): `Not signed in` and
`Riot Client not running`. The lockfile is genuinely gone — the fake client
deletes it on shutdown — so the offline half of the test is real; what is
missing is the write/read of the account cache in `worker.rs`.

## What the app still has to wire

Found while proving this harness (2026-11-15), for the lane that owns
`outpeek-app/src`:

1. `worker.rs` never calls `account::save_default` / `load_default`, so
   `%APPDATA%\Outpeek\account.json` is never written and a cold start without
   Riot shows `Not signed in`.
2. `worker.rs` never calls the TRN modules, so the rank panel has only the
   `/mmr` path — which fails off-Windows and on this harness by design.
3. There is no `[identity] resolved` log line, and `RUST_LOG` has no logger
   installed (no `set_logger`), so `RUST_LOG=info` currently changes nothing.
   The proof for identity is the server's `aliases → 200` line plus the handle
   rendered in the screenshot.

## Constraints and troubleshooting

* Stdlib only (`http.server`, `ssl`, `base64`, `json`, `subprocess` for
  `openssl`), no new runtime, no dependency file.
* Binds `127.0.0.1` literally; never `0.0.0.0`, never a LAN address.
* TLS: self-signed pair generated per run with `openssl req -x509` into a temp
  dir and deleted on exit, or pass `--cert`/`--key` to reuse one. The app
  accepts invalid certificates for loopback by design.
* `Address already in use` on `--mcp-port`: something else owns that port — on
  this box a static file server holds 8099 sometimes; pick another.
* No `DISPLAY`: `run.sh` wraps the app in `xvfb-run -a` automatically; set
  `FAKE_RIOT_XVFB=0` to opt out (e.g. with a real X server).
* No app binary: `run.sh` says so and prints the build command; set
  `OUTPEEK_BIN` to use a specific build.
* Secrets: none. The lockfile password is random, the tokens are obvious fakes,
  and no request body or credential is ever logged.

## Verified on this host (2026-11-15)

* `run.sh --state pregame --mcp-port 8107` under `xvfb-run`: app window
  1216×804, `Slint MCP server listening on http://127.0.0.1:8107/mcp`.
* Server log: `aliases/v1/active → 200`, `entitlements/v1/token → 200`,
  `chat/v4/presences → 200` every ~2.5 s.
* Screenshot: handle `lil ga7ed#zngr`, header `LIVE · Ascent · 0-0`; after
  `curl …/__fake/state?name=coregame` the next poll rendered `LIVE · Ascent · 3-2`
  with no restart (`/tmp/opencode/fake-riot.png`).
* Relaunch against the same data root with the server stopped: `Offline`,
  `Not signed in` — the offline half is real, the caches above are the gap.
