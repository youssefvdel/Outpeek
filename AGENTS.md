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

- UI: Slint 1.18 (`winit` + `femtoVG-wgpu`, DX12 on Windows), two crates.
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
