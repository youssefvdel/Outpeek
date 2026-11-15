# Outpeek native app (Slint) — architecture and verification

The Slint app (`outpeek-app/`) is the replacement for the Tauri/React shell. It
is one process with two windows and no webview. The Tauri app still exists in
`src-tauri/` + `src/` until feature parity; nothing in the native app depends on
it at runtime.

## Shape

```
outpeek-app  (binary + lib)
├─ main.rs          process start: args → fonts → windows → worker → loop
├─ worker.rs        one tokio poller: lockfile → identity → rank → facts
├─ identity.rs      lockfile creds, aliases, entitlements, presences (loopback)
├─ rank.rs          /mmr parse, region from the access token, tier + RR
├─ live_facts.rs    presence → phase/map/score (path-included from src-tauri)
├─ model.rs         pure view model: labels, states, formatting
├─ settings_model.rs, badges.rs, assets.rs, app_window.rs, hud.rs, fonts.rs
└─ ui/              .slint views, components, assets (badges), icons (generated)

outpeek-overlay (lib + binary)
├─ ui/overlay.slint  the HUD
├─ hud.rs            facts plumbing + sync_visibility
├─ win_style.rs      Win32 recipe (overlay only)
├─ roles.rs          which window may get click-through/topmost
└─ main.rs           the standalone gate binary (probe/health/soak/mcp)
```

Window roles are a typed decision (`roles.rs`): the HUD gets
transparent/topmost/click-through/toolwindow, the app window never does. A test
enforces it — applying the overlay recipe to the app window looks like a frozen
UI.

Session state lives in `settings.json` beside the HUD toggle: the window size,
the shell view and the active tab. A relaunch opens where the last one closed,
and a stored value this build does not understand (an old tab id, a 40 px
window) falls back instead of opening a dead page or an unusable window.
`--tab` still wins over the remembered tab — it is the CI/screenshot override.

## Data flow

One poller in `worker.rs` (2.5 s, 30 s idle backoff, skipped when no Riot client)
reads the loopback and pushes view-model state into Slint globals on the event
loop. The HUD reads the same in-memory state — no IPC, no second process, no
NDJSON in the app path. The standalone overlay binary keeps stdin NDJSON for
headless gates.

Two sources, deliberately separate:

- **Riot Client (loopback)** — identity, presence, phase, and the on-demand
  `/mmr` rank read. Only available while the client runs.
- **Tracker Network (keyless HTTPS)** — the season profile, the last 20
  matches, and the agent/map breakdowns. Always tried, with or without Riot.
  The request carries the `OverwolfClient` user agent and `?source=overwolf-2`
  (the shape the RE'd Overwolf client uses). Parsed results are cached
  (`trn-cache.json`, `trn-matches.json`, 24 h TTL) so the last read is the
  first frame of a Riot-less launch.
  TRN is the one host that **refuses Windows' Schannel fingerprint**: the same
  URL answers 200 to curl/OpenSSL and 403 to Schannel over HTTP/1.1, with the
  same public IP. That is why this transport alone uses a rustls + HTTP/2
  client (`reqwest`), while the Riot transports keep `native-tls` (Schannel on
  Windows, which Riot's loopback needs for its self-signed cert).
  `outpeek-app --trn-probe [HANDLE]` prints `TRN OK …` / `TRN FAILED …` and
  exits — headless, so a transport question can be answered over SSH.

A local `/mmr` row wins rank + RR; TRN fills everything else and the RR cell
then prints `NO RR` instead of a zero nobody measured.

### Tabs

| Tab | Source | Notes |
|---|---|---|
| Overview | TRN profile + `/mmr` | four headline numerals, metric strip, top agent, peak acts, score |
| Matches | TRN `matches` | last 20 games, every cell pre-formatted by Rust |
| Agents | TRN `agent` segments | same profile payload as Overview — never a second request, so the season cannot disagree |
| Maps | TRN `map` segments | map card art from valorant-api `listViewIcon`; TRN's own map URL is a 3840×2160 splash, past the art budget |
| Live Match | Riot loopback + `glz` + `/mmr` | the lobby you are in: both teams, agent, level, Riot ID, public rank + RR, party size, incognito. Ranks are per player and cached 15 min; `RANK HIDDEN` / `UNRATED` / `NOT READ` are states, never a guessed rank |

The Agents and Maps rows are what Tracker Network's endpoint returns (today 5
agents, 7 maps), not a full roster; both pages say so in their note band rather
than implying completeness. Rust owns the active tab (`--tab` at launch, the
header tabs at runtime), so the underline and the page are one state.

## Rendering and Windows integration

- Slint 1.18, winit backend, `renderer-femtovg-wgpu` (DX12 on Windows). The
  OpenGL path is dead on the target machine (NVIDIA ICD refuses
  `wglMakeCurrent`), and the software renderer is not compiled in so a broken
  GPU path fails closed instead of rendering opaque.
- App window: frameless (`no-frame`), 6 px resize band, drawn titlebar with a
  `WindowMoveArea` drag surface; DWM `ROUND` corners with a `SetWindowRgn`
  fallback; `BORDER_COLOR=NONE`; no system caption (a caption surface was the
  pale band we could not recolour).
- HUD window: recipe applied while hidden (bounded retry until Slint creates the
  HWND), `WS_EX_TRANSPARENT | LAYERED | NOACTIVATE | TOOLWINDOW`, `HTTRANSPARENT`
  subclass, no `WS_EX_LAYERED` omission tricks — LAYERED is required, `TRANSPARENT`
  alone does not pass clicks across processes (Tao/winit do the same).

## Assets

- Fonts: Outfit 400–700 and DM Sans 400/500/700 are OFL and **embedded**
  (`include_bytes!`), registered before the first window. Without them Windows
  falls back to Segoe UI and every metric changes.
- Icons: generated from `lucide-react`'s `__iconNode` by
  `scripts/gen-icons.ts` → `ui/components/icons.slint`; `icons-check.ts` fails
  `verify` on drift. Slint `Path` accepts Lucide's arcs, so geometry is exact
  (verified by rendering both and diffing ink pixels).
- Badges: the app's own artwork (`src/assets/...`) copied byte-identical into
  `ui/assets/badges/`, provenance in its README.
- Riot art (rank crests, agent portraits, map cards) is **never bundled** —
  fetched at runtime and cached under the app data dir, with drawn fallbacks.
  Fetching is allowlisted to exactly `media.valorant-api.com`,
  `titles.trackercdn.com` and `trackercdn.com` (the last carries the `_thumb`
  art for agents the mirror does not list yet). The worker resolves URLs; the
  UI thread decodes them scaled (128 px portraits, 320 px map cards).

## Verification workflow

The point of the native stack: UI work is verified on the Linux host, without
the user's PC.

```bash
cargo test --manifest-path outpeek-app/Cargo.toml      # pure model + assets
cargo xwin check --manifest-path outpeek-app/Cargo.toml \
  --target x86_64-pc-windows-msvc                      # Win32 paths compile
xvfb-run -a ./outpeek-app/target/debug/outpeek-app --mcp-port 8099 --tab agents
```

`--tab overview|matches|agents|maps` opens a page directly: Rust owns the tab,
so a screenshot cannot disagree with the underline.

With `--features ui-debug` the app's embedded Slint MCP server exposes
`list_windows`, `get_element_tree`, `click_element`, `take_screenshot`; that is
how navigation, focus and renders are proven. Without the feature the tree
reports `debugInfoMissing` and only window-level tools answer.

Windows-only facts stay on the PC: DWM frame and rounding, transparency,
click-through, the VALORANT foreground gate, live lockfile data, and the shared
event-loop cost while both windows composite.

## Build

```bash
cargo build --release --manifest-path outpeek-app/Cargo.toml                    # host
cargo xwin build --release --manifest-path outpeek-app/Cargo.toml \
  --target x86_64-pc-windows-msvc                                              # Windows exe
```

Measured 2026-11-15: Windows exe 21.2 MB; Linux release 37.9 MB → 28.1 MB
stripped. Size is fonts + badges + MCP + renderer, not the toolkit.

## Not ported yet

Store, Crosshair, Resolution Switch, Riot Chat, Pre-Picker, Accounts, the
config editor, GPU report, updates and installer, plus the in-game Live Match
tab and the HUD edit mode. Overview, Matches, Agents and Maps are live; the
remaining blocks render an em dash with what they need — never a fabricated
number.
