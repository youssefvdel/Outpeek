# AGENTS.md — Outpeek

Instructions for AI coding agents. Human docs: `README.md`, `ROADMAP.md`, `RELEASING.md`.

## Project overview

Outpeek is a Windows desktop VALORANT companion. Fully Rust + Slint now:
one process, two windows (tracker shell + in-game HUD overlay).

Safety rules (non-negotiable): read-only. No memory reading, no injection,
no mutating Riot calls. Never claim raw/hidden MMR — Riot does not expose it.
Never display a GPU/display state that was not read back — mark unverified
rows UNVERIFIED.

Note: "agents" in this codebase means VALORANT game characters, not AI
agents.

## Tech stack

- UI: Slint 1.19 (`winit` + `femtoVG-wgpu`, DX12 on Windows), two crates,
  on **edition 1.19** (set in both `build.rs` via
  `CompilerConfiguration::with_edition((1, 19))`).
- Backend: Rust (edition 2021), Win32 via `windows-rs`, tokio runtime.
- Data: Riot Client loopback (lockfile creds) + Tracker Network (keyless,
  rustls + HTTP/2).
- Target: `x86_64-pc-windows-msvc`, Windows 10/11 only.
- Separate `website/` Vite site (reconlab.app), independent package.

## Directory map

- `outpeek-app/` — the app: tracker window + HUD window, one event loop.
  `Cargo.toml` (name `outpeek-app`, version = release version),
  `src/` (worker, trn, live, identity, model, settings, hwnds),
  `ui/` (Slint: `app.slint`, `state.slint`, `components/`).
- `outpeek-overlay/` — the HUD surface lib + headless test binary.
  `src/lib.rs` (`hud`, `win_style`, `roles`, generated `ui`),
  `ui/overlay.slint` + `tokens.slint` (shared with the app by include
  path, never copied). The `outpeek-overlay` binary is `--probe` /
  `--health` / `--show-test` / `--soak` only — not shipped.
- `legacy/` — the OLD Tauri v2 + React tree, reference only. Do NOT run
  it (known-corrupted from an unsupervised run). May be read for porting
  logic/art. Its `src-tauri/src/*.rs` modules are compiled **by path**
  into `outpeek-app` (see below) — do not rename them.
- `website/` — standalone marketing site. Own `package.json`.
- `contracts/` — shared TRN error-surface contract. `ops/` — notices.
  `docs/` — live docs + history. `design/`, `assets/brand/` — mockups.

## Setup / build / dev / test

```bash
cargo build -p outpeek-app                      # Linux check (HUD inert)
cargo test -p outpeek-overlay -p outpeek-app    # 11 + 45 tests
cargo build --release --manifest-path outpeek-app/Cargo.toml   # Windows ship
# -> outpeek-app/target/release/outpeek-app.exe
cargo xwin check --manifest-path outpeek-app/Cargo.toml --target x86_64-pc-windows-msvc
```

- Flags: `--mcp-port PORT` (Slint UI-driving MCP), `--trn-probe [HANDLE]`,
  `OUTPEEK_SMOKE_EXIT_MS`, `OUTPEEK_BENCH_REPAINTS`, `OUTPEEK_LOOP_PROBE`.
- Bump: edit `version` in `outpeek-app/Cargo.toml` +
  `outpeek-overlay/Cargo.toml` + `legacy/src-tauri/Cargo.toml` +
  `legacy/package.json` + `website/index.html` (`softwareVersion`).
  `legacy/scripts/bump-version.js` still does the first four from
  `legacy/` (run it there); `website/index.html` is hand-edited.
- Legacy verify (`legacy/scripts/verify.ts`, `bun run verify`) is
  legacy-rooted; new work verifies with `cargo test` + Slint MCP
  screenshots.

## Code conventions

Slint / Rust:

- Rust owns every value; `.slint` renders, never formats a fact. Empty
  string = "not reported" = em dash, never a zero.
- Stay frameless: app window `no-frame: true` + drawn `titlebar.slint`.
  The HUD recipe (`win_style::apply`) touches the OVERLAY hwnd only —
  never the app window (`roles::wants_clickthrough` is tested).
- Shared UI is imported, never copied: `outpeek-app/build.rs` include
  paths `../outpeek-overlay/ui`. Keep both crates as siblings or fix
  `Cargo.toml` path dep + `build.rs` includes together.
- `outpeek-app/src/lib.rs` `#[path = "../../legacy/src-tauri/src/..."]`
  compiles five legacy modules by path (`fs_migrate`, `live_facts`,
  `overlay_renderer`, `paths`, `riot_http`). Renaming `legacy/` needs
  these five lines updated.
- `cargo fmt` clean; no new warnings. Windows-only paths guard missing
  registry keys. Never add memory-read, injection, input-automation, or
  mutating Riot-client deps.
- Icons: `outpeek-app/ui/components/icons.slint` is generated from
  `lucide-react` (`legacy/` scripts). Badges are byte-identical art in
  `outpeek-app/ui/assets/badges/`.

General:

- `ROADMAP.md` records verified dead-ends. Read it before re-researching.
- Keep changes minimal; do not reformat unrelated files.

## UI fidelity — the highest-priority rule in this repo

The tracker's UI is a **pixel match of the Tracker Network desktop client**,
rendered in Outpeek's dark-violet theme. The reference is not a mood board: it
is the spec. Every change to a data surface is judged against it, and "it
compiles" or "it renders" is not evidence of anything.

The reference screenshots are committed in `docs/ui-reference/` (read its
README for which crop pins what), and the measured values are recorded inline in
the `.slint` files (row heights, column widths, type steps). When a measurement
is missing, measure the committed screenshot instead of guessing.

### Measured baseline (Matches surface, from the 1475 px reference)

| Element | Value |
| --- | --- |
| Match row panel | 66 px tall, 8 px radius, 1 px raised border, 10 px gap, 4 px result spine, 44 px portrait, rank medal 28 px, kebab at the right edge |
| Day band | 50 px, 8 px radius, date at 16 px/700 secondary ink |
| Round score | 16 px mono/700, our rounds mint / theirs coral |
| Numeric cells | one `StatCell`: micro label over 16 px mono/700 value, centred, 10 px gutter; the day band's K/D/A stacks small `90 K // 95 D // 40 A` over big `0.95 K/D/A`; sub-1.0 K/D prints `coral-dim` |
| List body | radius 16, 1 px border, 14/16 px padding, holds bands + panels |
| Day count | bordered 30x24 chip beside the date |
| Strip | 62 px season cell + three agent cells, each with a win bar |
| Verdict | wins mint `//` faint, losses coral |
| Maps table | one body card, 34 px header, 56 px rows; banner first column, two 28 px top agents (whole `%`), Win % \| Wins \| Losses \| K/D \| ADR \| ACS \| signed per-round DDΔ |

### Non-negotiables before saying a UI change is done

1. **Look at the built window.** `cargo build --features ui-debug`, run with
   `--mcp-port`, `take_screenshot`, and read the image. A change nobody
   screenshotted is unverified.
2. **Check the minimum size, not just the comfortable one.** The shell's floor
   is `min-width 1280` / `min-height 720` and `main.rs` clamps a restored size
   to it. Screenshot at that floor: nothing may clip, collide, or elide.
3. **No fixed widths that can overrun a row.** Cells share the row
   (`horizontal-stretch` + `min-width`); a row of fixed-width cells is how the
   last agent cell got clipped. If a `width` is unavoidable, the row must still
   fit the floor arithmetic — write the sum in a comment.
4. **Header, rows and day bands agree.** Column order and widths come from one
   place: K/D, K/D/A, DDΔ, HS %, ACS. A label may never sit over a different
   column's numbers.
5. **Bars anchor left.** `x: 0` plus a clamped share; a centred fill reads as a
   different value.
6. **Numbers never touch.** A value and the value beside it need a visible
   gutter; if two numbers read as one, the tail gets its own layout.
7. **No invented data, no invented chrome.** Empty is an em dash. A chip that
   names a state appears only when the state needs explaining.
8. **Portraits and art never collapse a row.** A missing image draws the glyph
   at the same size the image would occupy.

Ship the screenshot with the change. If a detail is off by a pixel step
(4 px vs 8 px), it is still off.

## Do NOT touch

- Generated: `outpeek-app/target/`, `outpeek-overlay/target/`,
  `legacy/node_modules/`, `legacy/dist/`, `legacy/.webview2-cache/`,
  `legacy/src-tauri/target/`, `legacy/src-tauri/binaries/`,
  `legacy/src-tauri/webview2-runtime/`, `*.local`, `*.tsbuildinfo`.
- Secrets / local: `.env`, `logs/`, `*.log`.
- Tooling state: `.hermes/`, `.agents/`, `.cortexkit/`, `.opencode/`.

## Git / worktrees

- Small focused commits; do not commit secrets.
- Never use `git stash` — stashes leak across worktrees. Commit, or use
  a temp branch / worktree-local patch instead.
- Respect root `.gitignore` (covers `legacy/`, targets, caches).

### Ratio, not absolute numbers

The reference page is 1475 px wide with a 1068 px card, so it sets 72 px rows
and 18 px values. Our shell's floor is 1280 px, which leaves a ~936 px card.
Copying the absolute numbers there makes the surface read one step too big, so
the baseline above is the reference's **ratio** at our width. If the window
floor ever changes, re-derive the table instead of adding a second set.
