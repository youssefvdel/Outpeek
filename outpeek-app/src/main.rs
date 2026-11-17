#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// No console window in release. One process hosts both windows, so there is no
// child process to flash a console.

//! Outpeek app: one process, two windows.
//!
//! * `AppWindow` — the tracker shell (frameless, drawn titlebar, resizable,
//!   taskbar).
//! * `OverlayWindow` — the HUD from `outpeek-overlay`'s compiled UI, created
//!   hidden at startup and shown only while VALORANT is focused and the HUD
//!   toggle is on.
//!
//! One poller publishes `Event`s; this file applies them to both windows on the
//! UI thread. No stdin protocol, no `--parent-pid`, no second executable.

slint::include_modules!();

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use outpeek_app::app_window::{self, WindowCommand};
use outpeek_app::assets::ArtView;
use outpeek_app::hud::{self, PLATFORM_SUPPORTS_HUD};
use outpeek_app::live_view::{LivePlayerUi as LivePlayerRow, LiveView};
use outpeek_app::model::{self, ClientState, RankView, TrackerSnapshot};
use outpeek_app::overlay_renderer::OverlayRendererSetting;
use outpeek_app::settings_model::{self, McpPort, PathState};
use outpeek_app::trn_profile::{ProfileState, ProfileView};
use outpeek_app::worker::{self, Event, Flags};
use outpeek_app::{paths, PRODUCT_NAME, VERSION};
use outpeek_overlay::ui::OverlayWindow;
use slint::{ComponentHandle as _, ModelRc, VecModel};

/// The narrowest window the widest screen still fits, in logical pixels. Kept
/// beside the Slint `min-width`/`min-height` so a restored size can never
/// undercut the layout.
const MIN_WINDOW_WIDTH: u32 = 1280;
const MIN_WINDOW_HEIGHT: u32 = 720;

/// Persisted HUD toggle. Missing file = on: this app's own HUD is the point.
/// Build profile the Settings page reports. Compile-time fact, not runtime env.
const BUILD_PROFILE: &str = if cfg!(debug_assertions) {
    "debug"
} else {
    "release"
};
/// Target triple emitted by build.rs from the compiler's own TARGET. Never a
/// guessed host string.
const BUILD_TARGET: &str = match option_env!("OUTPEEK_TARGET") {
    Some(target) => target,
    None => "unknown target",
};

fn main() {
    // Diagnostic path: `--trn-probe [HANDLE]` reads one Tracker Network
    // profile through this app's own transport and exits. It runs before any
    // window exists, so it can be driven over SSH on the PC — that is how the
    // Windows fingerprint (Schannel + HTTP/1.1 = 403) was proven.
    if let Some(handle) = trn_probe_arg() {
        run_trn_probe(&handle);
        return;
    }
    // Arguments first: `parse_args` fills SLINT_MCP_PORT, and the backend reads
    // it exactly once when the platform is created. Font registration below
    // touches Slint's font collection, which creates that platform — so a flag
    // parsed after it never starts the MCP server.
    let initial_tab = parse_args();
    outpeek_app::boot::mark("args");
    // Pin the one proven renderer before any window exists.
    outpeek_overlay::force_femtovg();
    // Embedded OFL fonts: without them Windows falls back to Segoe UI and the
    // whole design's metrics change. Must run before the first window.
    let faces = outpeek_app::fonts::register();
    outpeek_app::boot::mark(&format!("fonts ({faces} faces)"));
    eprintln!("[Outpeek] fonts registered: {faces} face(s)");
    // Copy-only migrations, exactly once per launch (same rules as the Tauri app).
    paths::migrate_previous_app_data();
    paths::migrate_legacy_data();
    outpeek_app::boot::mark("migrations");
    // Plain-text flags are retired: fold overlay_renderer.txt into settings.json once.
    let _ = outpeek_app::settings::migrate_legacy_default();
    outpeek_app::boot::mark("settings");

    let app = AppWindow::new().expect("outpeek-app: app window");
    outpeek_app::boot::mark("app-window");
    // Open where the last session was left: window size, shell view and tab.
    // `--tab` (debug/CI) still wins over the remembered tab, and a stored value
    // this build does not understand falls back instead of opening a dead page.
    let stored = outpeek_app::settings::load_default();
    if let Some((width, height)) = stored.window_size() {
        // A remembered size below the shell's own minimum would clip the
        // Matches table, so the restore honours the floor, not the file.
        let width = width.max(MIN_WINDOW_WIDTH);
        let height = height.max(MIN_WINDOW_HEIGHT);
        app.global::<Tracker>().set_window_width(width as f32);
        app.global::<Tracker>().set_window_height(height as f32);
        // The layout sizes the window when it is created, so the resize has to
        // land after the first loop turn; setting it before `run` is silently
        // replaced by the layout's preferred size.
        let weak = app.as_weak();
        let _ = slint::invoke_from_event_loop(move || {
            outpeek_app::boot::mark("first-event");
            if let Some(app) = weak.upgrade() {
                app.window()
                    .set_size(slint::LogicalSize::new(width as f32, height as f32));
            }
        });
    }
    let boot_tab = initial_tab
        .clone()
        .unwrap_or_else(|| stored.start_tab().to_string());
    app.global::<Tracker>().set_tab(boot_tab.into());
    if initial_tab.is_none() {
        app.global::<Tracker>().set_view(stored.start_view().into());
    }
    // Restore the playlist before the worker starts so the first network frame
    // already shows it; the cached boot frame stays competitive until then.
    let boot_playlist = load_last_playlist();
    app.global::<Tracker>()
        .set_playlist(boot_playlist.clone().into());
    app.global::<Tracker>().set_playlist_label(
        outpeek_app::trn_profile::playlist_label(&boot_playlist)
            .unwrap_or("Competitive")
            .into(),
    );
    apply_playlist_options(&app);
    // Frame styling is best-effort and logged: a refused DWM attribute costs a
    // cosmetic detail, never the app. The HWND can be absent before the event
    // loop starts, so the first tick retries once.
    // The HUD window is created hidden here; a failure is a notice in the
    // tracker, never a startup failure (the tracker must still work).
    outpeek_app::boot::mark("overlay-window:start");
    let (overlay, overlay_error) = match OverlayWindow::new() {
        Ok(overlay) => {
            let _ = overlay.hide();
            (Some(overlay), None)
        }
        Err(e) => (None, Some(e.to_string())),
    };
    // The HUD's native recipe lands now, on the still-hidden window: anything
    // that shows it later maps a HUD, never winit's default decorated/topmost
    // frame. Slint creates the HWND from the event loop, so the pass retries
    // there and logs the applied style once (`[outpeek-overlay] style_hidden`).
    if let Some(overlay) = &overlay {
        outpeek_overlay::hud::style_hidden(overlay, outpeek_overlay::roles::WindowRole::Overlay);
    }
    outpeek_app::boot::mark("overlay-window:done");
    // PRESENT defaults to 1 for the standalone binary's first-frame contract;
    // the app owns the gate, and it starts closed. Without this the first
    // facts frame would show the HUD before the foreground check ran.
    outpeek_overlay::set_present(false);

    let enabled = Arc::new(AtomicBool::new(load_enabled()));
    let foreground = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let flags = Arc::new(Flags::default());
    if let Ok(mut slot) = flags.playlist.lock() {
        *slot = boot_playlist;
    }

    // Boot the chrome from the persisted toggle so the first frame is honest.
    let t = app.global::<Tracker>();
    t.set_version(VERSION.into());
    // The drawn titlebar and the Win32 hit/region maths read the same numbers.
    t.set_titlebar_height(app_window::TITLEBAR_HEIGHT);
    t.set_control_width(app_window::CONTROL_WIDTH);
    let platform_note = if !PLATFORM_SUPPORTS_HUD {
        "HUD window styling is Win32-only; the overlay stays hidden on this host.".to_string()
    } else if let Some(error) = &overlay_error {
        format!("HUD window could not open: {error}")
    } else {
        String::new()
    };
    if let Some(error) = &overlay_error {
        eprintln!("[{PRODUCT_NAME}] overlay window failed to open: {error}");
    }
    t.set_platform_note(platform_note.into());
    apply_hud(&app, enabled.load(Ordering::Relaxed), false);
    apply_settings(&app);
    // Honest first frame: the glyph reflects the real window, not a default.
    apply_window_state(&app);

    // Overlay close does not kill the app; app close kills the process.
    {
        // Remember the size before the window goes: the last honest read, not
        // a value cached at startup.
        let app_weak_close = app.as_weak();
        app.window().on_close_requested(move || {
            if let Some(app) = app_weak_close.upgrade() {
                save_window_size(&app);
            }
            let _ = slint::quit_event_loop();
            slint::CloseRequestResponse::HideWindow
        });
    }

    // What the Win32 chrome pass learned, so a resize or a maximise can never
    // leave a stale corner clip or a wrong glyph behind.
    let chrome = Rc::new(ChromeState::default());

    wire_callbacks(&app, &overlay, &enabled, &foreground, &flags, &chrome);
    outpeek_app::boot::mark("callbacks");

    // Both background threads start on the first loop iteration:
    // `invoke_from_event_loop` is only accepted once the loop is running.
    {
        let app_weak = app.as_weak();
        let overlay_weak = overlay.as_ref().map(|o| o.as_weak());
        let enabled = enabled.clone();
        let foreground = foreground.clone();
        let stop = stop.clone();
        let flags = flags.clone();
        let chrome = chrome.clone();
        // The window is laid out only after the loop runs; log its real size
        // there, once, as the startup proof that the frame exists.
        {
            let app_weak = app.as_weak();
            slint::Timer::single_shot(Duration::from_millis(250), move || {
                if let Some(app) = app_weak.upgrade() {
                    let size = app.window().size();
                    eprintln!(
                        "[{PRODUCT_NAME}] app window {}x{} physical",
                        size.width, size.height
                    );
                }
            });
        }
        slint::Timer::single_shot(Duration::ZERO, move || {
            start_worker(app_weak.clone(), overlay_weak.clone(), &flags);
            if let Some(app) = app_weak.upgrade() {
                apply_window_chrome_retrying(&app, &chrome);
                // The maximise glyph and any corner clip follow the window:
                // snap layouts and taskbar restores change them with no click
                // of ours, so this is a read-back, not a state machine.
                poll_window_state(app.as_weak(), chrome.clone());
                spawn_foreground_watch(
                    stop.clone(),
                    enabled,
                    foreground,
                    app.as_weak(),
                    overlay_weak,
                );
            }
            if let Some(app) = app_weak.upgrade() {
                hud::spawn_loop_probe(stop.clone());
                maybe_run_repaint_bench(&app);
            }
            // CI/dev smoke hook: close the app through the same path a user
            // close takes, so shutdown (HUD hide, poller stop) is exercised.
            if let Some(ms) = std::env::var("OUTPEEK_SMOKE_EXIT_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
            {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(ms));
                    let _ = slint::quit_event_loop();
                });
            }
        });
    }

    outpeek_app::boot::mark("run");
    let run = app.run();
    // Shutdown: the HUD is the first thing that must go.
    stop.store(true, Ordering::Relaxed);
    flags.stop.store(true, Ordering::Relaxed);
    outpeek_overlay::set_present(false);
    if let Some(overlay) = &overlay {
        let _ = overlay.hide();
    }
    if let Err(e) = run {
        eprintln!("[{PRODUCT_NAME}] event loop error: {e}");
    }
}

/// `--trn-probe [HANDLE]` → the handle to read (the remembered account when
/// the flag carries none), or `None` when the flag is absent.
fn trn_probe_arg() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let at = args.iter().position(|a| a == "--trn-probe")?;
    Some(
        args.get(at + 1)
            .filter(|a| !a.starts_with("--"))
            .cloned()
            .or_else(|| outpeek_app::account::load_default().map(|a| a.handle))
            .unwrap_or_default(),
    )
}

/// One profile read, printed and exited. `TRN OK …` / `TRN FAILED …` is the
/// whole contract: it is a probe, not a feature.
fn run_trn_probe(handle: &str) {
    if handle.trim().is_empty() {
        eprintln!("outpeek-app: --trn-probe needs a handle or a remembered account");
        std::process::exit(2);
    }
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("outpeek-app: --trn-probe could not start: {e}");
            std::process::exit(2);
        }
    };
    match rt.block_on(outpeek_app::trn::fetch_profile(handle)) {
        Ok(json) => {
            let bytes = serde_json::to_string(&json).map(|s| s.len()).unwrap_or(0);
            match outpeek_app::trn_profile::parse(&json) {
                Ok(profile) => println!(
                    "TRN OK {bytes} bytes · {} · {} · {} matches",
                    profile.handle,
                    profile.season_short,
                    profile
                        .season
                        .matches
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "—".to_string())
                ),
                Err(e) => println!("TRN OK {bytes} bytes · parse failed: {e}"),
            }
        }
        Err(e) => {
            println!("TRN FAILED {e}");
            std::process::exit(1);
        }
    }
}

/// `--mcp-port PORT` (and `--help`). Slint reads `SLINT_MCP_PORT` at backend
/// init; the flag only fills it when the environment did not. A port the app
/// cannot use is rejected here instead of being handed to Slint, which would
/// print its own error and leave MCP silently off.
fn parse_args() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let mut tab: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--mcp-port" => {
                i += 1;
                let Some(port) = args.get(i) else {
                    eprintln!("outpeek-app: --mcp-port needs a value (1–65535)");
                    std::process::exit(2);
                };
                match settings_model::parse_mcp_port(Some(port)) {
                    McpPort::Set(_) => {
                        if std::env::var_os("SLINT_MCP_PORT").is_none() {
                            // SAFETY: single-threaded at startup, before any
                            // backend or event loop exists.
                            unsafe { std::env::set_var("SLINT_MCP_PORT", port) };
                        }
                    }
                    _ => {
                        eprintln!(
                            "outpeek-app: --mcp-port '{port}' is not a usable port (1–65535)"
                        );
                        std::process::exit(2);
                    }
                }
            }
            "--tab" => {
                i += 1;
                let Some(value) = args.get(i) else {
                    eprintln!(
                        "outpeek-app: --tab needs a value (overview|matches|agents|maps|live)"
                    );
                    std::process::exit(2);
                };
                match value.as_str() {
                    "overview" | "matches" | "agents" | "maps" | "live" => {
                        tab = Some(value.clone())
                    }
                    other => {
                        eprintln!(
                            "outpeek-app: --tab '{other}' is not ported \
                             (overview|matches|agents|maps|live)"
                        );
                        std::process::exit(2);
                    }
                }
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: outpeek-app [--mcp-port PORT]\n\
                     \n\
                     One process hosts the tracker window and the HUD overlay window.\n\
                     OUTPEEK_LOOP_PROBE=1  logs max event-loop dispatch latency every 5 s\n\
                     OUTPEEK_BENCH_REPAINTS=1  runs the 200-update loop bench once"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("outpeek-app: unknown argument {other} (try --help)");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    tab
}

fn load_enabled() -> bool {
    // One JSON store; the old overlay_renderer.txt is migrated once at startup.
    outpeek_app::settings::load_default().hud_enabled()
}

/// Persist the shell view. Cheap (a few hundred bytes) and only on a real
/// change: a click that lands on the same view must not rewrite the file.
fn save_last_view(view: &str) {
    let keep_playlist = load_last_playlist_raw();
    let mut settings = outpeek_app::settings::load_default();
    if settings.last_view == view {
        return;
    }
    settings.last_view = view.to_string();
    if let Err(e) = outpeek_app::settings::save_default(&settings) {
        eprintln!("[{PRODUCT_NAME}] could not save the last view: {e}");
    }
    restore_playlist_key(keep_playlist);
}

/// Persist the tracker tab, same rule.
fn save_last_tab(tab: &str) {
    let keep_playlist = load_last_playlist_raw();
    let mut settings = outpeek_app::settings::load_default();
    if settings.last_tab == tab {
        return;
    }
    settings.last_tab = tab.to_string();
    if let Err(e) = outpeek_app::settings::save_default(&settings) {
        eprintln!("[{PRODUCT_NAME}] could not save the last tab: {e}");
    }
    restore_playlist_key(keep_playlist);
}

/// Playlist persistence: one `last_playlist` key inside settings.json, the
/// same store as `last_tab`/`last_view`. `settings.rs` is owned outside this
/// pass, so this merges the key into the existing JSON instead of extending
/// `AppSettings` — and the tab/view savers above restore it across their
/// rewrites (they serialize the struct, which would otherwise drop it).
fn load_last_playlist_raw() -> Option<String> {
    let dir = outpeek_app::paths::app_data_dir()?;
    let bytes = std::fs::read(dir.join("settings.json")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("last_playlist")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// The playlist to open: the remembered one when it is one of the four the
/// payload carries, Competitive otherwise.
fn load_last_playlist() -> String {
    load_last_playlist_raw()
        .filter(|p| outpeek_app::trn_profile::is_known_playlist(p))
        .unwrap_or_else(|| outpeek_app::trn_profile::DEFAULT_PLAYLIST.to_string())
}

fn save_last_playlist(playlist: &str) {
    let Some(dir) = outpeek_app::paths::app_data_dir() else {
        return;
    };
    let path = dir.join("settings.json");
    let mut value: serde_json::Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "last_playlist".to_string(),
            serde_json::Value::from(playlist),
        );
    }
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
    if let Err(e) = outpeek_app::account::write_atomic(&path, text.as_bytes()) {
        eprintln!("[{PRODUCT_NAME}] could not save the last playlist: {e}");
    }
}

/// Re-merge a playlist key a struct-serializing save just dropped.
fn restore_playlist_key(keep: Option<String>) {
    if let Some(playlist) = keep {
        save_last_playlist(&playlist);
    }
}

/// Persist the window size in logical pixels. A maximised window reports the
/// work area — that is still the size the user left it at, and restoring it
/// un-maximised is the honest reading of what was recorded.
fn save_window_size(app: &AppWindow) {
    let physical = app.window().size();
    let scale = app.window().scale_factor();
    if scale <= 0.0 {
        return;
    }
    let width = (f64::from(physical.width) / f64::from(scale)).round() as u32;
    let height = (f64::from(physical.height) / f64::from(scale)).round() as u32;
    let mut settings = outpeek_app::settings::load_default();
    settings.set_window_size(width, height);
    if let Err(e) = outpeek_app::settings::save_default(&settings) {
        eprintln!("[{PRODUCT_NAME}] could not save the window size: {e}");
    }
}

fn save_enabled(enabled: bool) {
    let mut s = outpeek_app::settings::load_default();
    s.set_hud_enabled(enabled);
    let _ = outpeek_app::settings::save_default(&s);
}

fn setting(enabled: bool) -> OverlayRendererSetting {
    if enabled {
        OverlayRendererSetting::Native
    } else {
        OverlayRendererSetting::Ts
    }
}

fn wire_callbacks(
    app: &AppWindow,
    overlay: &Option<OverlayWindow>,
    enabled: &Arc<AtomicBool>,
    foreground: &Arc<AtomicBool>,
    flags: &Arc<Flags>,
    chrome: &Rc<ChromeState>,
) {
    // Sidebar navigation: Rust owns the view state, so the header, the active
    // row and the window title can never disagree with the content area.
    {
        let app_weak = app.as_weak();
        app.on_navigate(move |view| {
            if let Some(app) = app_weak.upgrade() {
                app.global::<Tracker>().set_view(view.clone());
            }
            save_last_view(&view);
            #[cfg(debug_assertions)]
            eprintln!("[{PRODUCT_NAME}] nav -> {view}");
        });

        // Tracker tabs. Only the four ported tabs can be selected; the header
        // marks the rest SOON, so a click there never reaches this handler.
        let app_weak_tab = app.as_weak();
        app.on_select_tab(move |tab| {
            let tab = match tab.as_str() {
                "matches" => "matches",
                "agents" => "agents",
                "maps" => "maps",
                "live" => "live",
                _ => "overview",
            };
            if let Some(app) = app_weak_tab.upgrade() {
                app.global::<Tracker>().set_tab(tab.into());
            }
            save_last_tab(tab);
            #[cfg(debug_assertions)]
            eprintln!("[{PRODUCT_NAME}] tab -> {tab}");
        });
    }

    // Kill switch. Persist first, then apply to the live window on this thread
    // so a hitch is isolated by one click instead of an app restart.
    {
        let app_weak = app.as_weak();
        let enabled = enabled.clone();
        let foreground = foreground.clone();
        let overlay_handle: Option<OverlayWindow> = overlay.as_ref().map(|o| o.clone_strong());
        app.on_toggle_hud(move || {
            let now = !enabled.load(Ordering::Relaxed);
            enabled.store(now, Ordering::Relaxed);
            save_enabled(now);
            let fg = foreground.load(Ordering::Relaxed);
            outpeek_overlay::set_present(model::should_show_overlay(
                now,
                setting(now),
                fg,
                PLATFORM_SUPPORTS_HUD,
            ));
            if let Some(overlay) = &overlay_handle {
                outpeek_overlay::hud::sync_visibility(overlay, None);
            }
            if let Some(app) = app_weak.upgrade() {
                apply_hud(&app, now, fg);
            }
        });
    }

    {
        let flags = flags.clone();
        app.on_refresh_rank(move || {
            flags.refresh_rank.store(true, Ordering::Relaxed);
        });
    }

    // Match detail: a Matches row click opens the scoreboard in place.
    // Rust owns the selection like select-tab; the worker fetches on demand
    // (memory-only, no disk) and publishes the frame. Back clears the
    // selection and the table returns — there is no separate tab.
    {
        let app_weak = app.as_weak();
        let flags = flags.clone();
        app.global::<Tracker>().on_select_match(move |id| {
            let id = id.to_string();
            if id.trim().is_empty() {
                return;
            }
            if let Ok(mut slot) = flags.match_request.lock() {
                *slot = Some(id.clone());
            }
            flags.match_pending.store(true, Ordering::Relaxed);
            // Optimistic: the detail view opens at once with its loading
            // line; the worker's event lands the rows on its next tick.
            if let Some(app) = app_weak.upgrade() {
                let t = app.global::<Tracker>();
                t.set_match_id(id.into());
                t.set_match_state("loading".into());
                t.set_match_note("Reading match…".into());
            }
        });
    }
    {
        let app_weak = app.as_weak();
        app.global::<Tracker>().on_close_match_detail(move || {
            if let Some(app) = app_weak.upgrade() {
                let t = app.global::<Tracker>();
                t.set_match_id("".into());
                t.set_match_state("empty".into());
                t.set_match_note("".into());
            }
        });
    }

    // PLAYLIST dropdown: Rust owns the value like select-tab. Unknown ids fall
    // back to competitive; the worker re-parses the last body in memory (no
    // refetch) and the choice persists beside last_tab/last_view.
    {
        let app_weak = app.as_weak();
        let flags = flags.clone();
        app.global::<Tracker>().on_select_playlist(move |choice| {
            let id = choice.as_str();
            let id = if outpeek_app::trn_profile::is_known_playlist(id) {
                id.to_string()
            } else {
                outpeek_app::trn_profile::DEFAULT_PLAYLIST.to_string()
            };
            if let Ok(mut slot) = flags.playlist.lock() {
                *slot = id.clone();
            }
            flags.playlist_pending.store(true, Ordering::Relaxed);
            save_last_playlist(&id);
            if let Some(app) = app_weak.upgrade() {
                // Optimistic: the box label follows the click at once; the
                // worker republish lands the numbers on its next tick.
                app.global::<Tracker>().set_playlist(id.clone().into());
                app.global::<Tracker>().set_playlist_label(
                    outpeek_app::trn_profile::playlist_label(&id)
                        .unwrap_or("Competitive")
                        .into(),
                );
            }
        });
    }

    // Window controls. One place runs the command, logs what the window did,
    // and re-reads the state, so the glyph can never describe a click that the
    // window system refused.
    {
        let app_weak = app.as_weak();
        let chrome = chrome.clone();
        app.on_minimize_window(move || {
            if let Some(app) = app_weak.upgrade() {
                run_window_command(&app, WindowCommand::Minimize, &chrome);
            }
        });
    }
    {
        let app_weak = app.as_weak();
        let chrome = chrome.clone();
        app.on_toggle_maximize(move || {
            if let Some(app) = app_weak.upgrade() {
                run_window_command(&app, WindowCommand::ToggleMaximize, &chrome);
            }
        });
    }
    {
        let app_weak = app.as_weak();
        let chrome = chrome.clone();
        app.on_close_window(move || {
            if let Some(app) = app_weak.upgrade() {
                run_window_command(&app, WindowCommand::Close, &chrome);
            }
        });
    }
}

/// What the Win32 chrome pass learned, kept for the resize/maximise poll.
///
/// Only the Windows path reads these fields; the type stays compiled on every
/// host so one code path serves every `cfg`.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Default)]
struct ChromeState {
    /// How the corners are produced, decided once from DWM's HRESULT.
    corner: Cell<Option<app_window::CornerMode>>,
    /// The region currently on the window, so a resize can tell whether the
    /// clip it needs is already applied.
    region: Cell<Option<app_window::Region>>,
    /// The clip Windows refused at the last attempt, so a cosmetic miss logs
    /// once instead of on every poll tick.
    refused: Cell<Option<app_window::Region>>,
}

/// Push the window's real maximised state and its button glyph.
fn apply_window_state(ui: &AppWindow) {
    let control = app_window::max_control(app_is_maximized(ui));
    let t = ui.global::<Tracker>();
    t.set_max_glyph(control.glyph.into());
    t.set_max_label(control.label.into());
}

/// Read the maximised state from the window, once per poll tick. Cheap and
/// exact; there is no Slint event for "the window manager changed it".
#[cfg(windows)]
fn app_is_maximized(ui: &AppWindow) -> bool {
    app_hwnd(ui).map(app_window::is_maximized).unwrap_or(false)
}

#[cfg(not(windows))]
fn app_is_maximized(_ui: &AppWindow) -> bool {
    false
}

/// The window's native handle, once Slint's event loop has created it.
#[cfg(windows)]
fn app_hwnd(ui: &AppWindow) -> Option<isize> {
    use raw_window_handle::HasWindowHandle as _;

    let handle = ui.window().window_handle();
    match handle.window_handle().ok()?.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(hwnd) => Some(hwnd.hwnd.get() as isize),
        _ => None,
    }
}

/// Run one titlebar command, log the state it produced, refresh the glyph and
/// any corner clip. A refused command is one log line, never a panic.
#[cfg(windows)]
fn run_window_command(ui: &AppWindow, command: WindowCommand, chrome: &ChromeState) {
    let Some(hwnd) = app_hwnd(ui) else {
        eprintln!("[{PRODUCT_NAME}] app-window command {command:?}: no HWND yet");
        return;
    };
    let report = app_window::apply_window_command(hwnd, command);
    eprintln!("[{PRODUCT_NAME}] {}", report.log_line());
    apply_window_state(ui);
    refresh_corners(ui, chrome);
}

/// Non-Windows: there is no Win32 window to command. Close still quits the one
/// event loop (the same path the window's close request takes); the other two
/// are honest no-ops.
#[cfg(not(windows))]
fn run_window_command(_ui: &AppWindow, command: WindowCommand, _chrome: &ChromeState) {
    if command == WindowCommand::Close {
        let _ = slint::quit_event_loop();
    } else {
        #[cfg(debug_assertions)]
        eprintln!("[{PRODUCT_NAME}] app-window command {command:?}: no-op on this host");
    }
}

/// Keep the window's corner clip in step with its size and its maximised state.
///
/// Only the region fallback does anything; when DWM rounds the window this
/// returns immediately, so the 300 ms poll costs one comparison.
#[cfg(windows)]
fn refresh_corners(ui: &AppWindow, chrome: &ChromeState) {
    let Some(mode) = chrome.corner.get() else {
        return;
    };
    let Some(hwnd) = app_hwnd(ui) else {
        return;
    };
    let size = ui.window().size();
    let scale = ui.window().scale_factor();
    let wanted = app_window::wanted_region(
        mode,
        app_window::is_maximized(hwnd),
        size.width as i32,
        size.height as i32,
        scale,
    );
    if !app_window::should_apply_region(chrome.region.get(), wanted, chrome.refused.get()) {
        return;
    }
    let applied = app_window::apply_region(hwnd, wanted);
    if applied {
        chrome.region.set(wanted);
        chrome.refused.set(None);
    } else if wanted.is_some() {
        chrome.refused.set(wanted);
    }
    eprintln!(
        "[{PRODUCT_NAME}] {}",
        app_window::corner_log(
            mode,
            if applied { wanted } else { chrome.region.get() },
            scale
        )
    );
}

#[cfg(not(windows))]
fn refresh_corners(_ui: &AppWindow, _chrome: &ChromeState) {}

/// Re-read the window state on a slow tick and re-apply the corner clip when
/// the window changed size or was maximised/restored. 300 ms is invisible to a
/// user and one Win32 query per tick.
fn poll_window_state(ui: slint::Weak<AppWindow>, chrome: Rc<ChromeState>) {
    slint::Timer::single_shot(Duration::from_millis(300), move || {
        let Some(ui) = ui.upgrade() else {
            return;
        };
        apply_window_state(&ui);
        refresh_corners(&ui, &chrome);
        poll_window_state(ui.as_weak(), chrome.clone());
    });
}

fn apply_hud(ui: &AppWindow, enabled: bool, game_foreground: bool) {
    let status = model::hud_status(
        enabled,
        setting(enabled),
        game_foreground,
        PLATFORM_SUPPORTS_HUD,
    );
    let t = ui.global::<Tracker>();
    t.set_hud_enabled(enabled);
    t.set_hud_status(status.text.into());
    t.set_hud_tone(status.tone.as_str().into());
}

/// Fill the Settings page once at boot. Every value is resolved on this
/// machine; a missing base becomes the reason, never an invented path.
fn apply_settings(ui: &AppWindow) {
    // The real data directory first (it creates it, like every other read);
    // the pure fallback only supplies the honest message when it cannot.
    let data = paths::app_data_dir()
        .map(PathState::Resolved)
        .unwrap_or_else(|| settings_model::data_dir(None));
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from);
    let roaming = std::env::var_os("APPDATA").map(std::path::PathBuf::from);
    let log = settings_model::log_dir(
        local_app_data.as_deref(),
        roaming.as_deref(),
        &std::env::temp_dir(),
        settings_model::LOG_IDENTIFIER,
    );
    // Read after `parse_args`: the flag fills this before the backend starts.
    let mcp_port = std::env::var("SLINT_MCP_PORT").ok();
    let view = settings_model::resolve(
        data,
        log,
        mcp_port.as_deref(),
        PRODUCT_NAME,
        VERSION,
        BUILD_PROFILE,
        BUILD_TARGET,
    );

    let s = ui.global::<Settings>();
    s.set_data_path(view.data_path.display().into());
    s.set_data_resolved(view.data_path.is_resolved());
    s.set_log_path(view.log_path.display().into());
    s.set_log_resolved(view.log_path.is_resolved());
    s.set_hud_file(view.hud_file.into());
    s.set_hud_rule(view.hud_rule.into());
    s.set_theme_line(view.theme_line.into());
    s.set_mcp_line(settings_model::mcp_line(&view.mcp).into());
    s.set_mcp_tone(settings_model::mcp_tone(&view.mcp).into());
    s.set_version_line(view.version_line.into());
    s.set_build_line(view.build_line.into());

    let rows: Vec<SettingsItem> = view
        .not_ported
        .iter()
        .map(|row| SettingsItem {
            title: row.title.into(),
            detail: row.detail.into(),
        })
        .collect();
    s.set_not_ported(ModelRc::new(VecModel::from(rows)));
}

/// Apply the app-window's DWM chrome and decide how its corners are produced.
///
/// App-window only: the HUD keeps the overlay recipe (DONOTROUND, no frame) and
/// is never passed here. Every attribute result is logged; a refused attribute
/// is cosmetic and never fatal.
///
/// The window is frameless (`no-frame: true`), so there is no caption to paint
/// and no `DWMWA_CAPTION_COLOR` here: the pale band the user saw *was* DWM's
/// title-bar surface, and only removing the caption removes it.
///
/// One single_shot attempt was not enough on the real machine: the log showed
/// "no Win32 HWND yet" twice and the chrome never landed. Slint only
/// materializes the HWND from the event loop, so poll for it: 40 x 50 ms = 2 s,
/// then say so once and keep the system frame rather than spinning forever.
fn apply_window_chrome_retrying(ui: &AppWindow, chrome: &Rc<ChromeState>) {
    fn attempt(weak: slint::Weak<AppWindow>, tries: Rc<Cell<u32>>, chrome: Rc<ChromeState>) {
        slint::Timer::single_shot(Duration::from_millis(50), move || {
            let Some(ui) = weak.upgrade() else { return };
            if apply_window_chrome(&ui, &chrome) {
                return;
            }
            let n = tries.get() + 1;
            tries.set(n);
            if n >= 40 {
                eprintln!(
                    "[{PRODUCT_NAME}] app-window chrome: no Win32 HWND after 2s; the system frame stays"
                );
                return;
            }
            attempt(ui.as_weak(), tries.clone(), chrome.clone());
        });
    }
    attempt(ui.as_weak(), Rc::new(Cell::new(0)), chrome.clone());
}

#[cfg(windows)]
fn apply_window_chrome(ui: &AppWindow, chrome: &ChromeState) -> bool {
    let Some(hwnd) = app_hwnd(ui) else {
        // Slint creates the HWND from the event loop, so a miss here is normal
        // on the first tick. The caller retries with a bounded budget and is
        // the only place that logs the outcome.
        return false;
    };
    let report = app_window::style_app_window(hwnd);
    eprintln!("[{PRODUCT_NAME}] {}", report.log_line());
    if !report.all_ok() {
        eprintln!(
            "[{PRODUCT_NAME}] app-window chrome: a DWM attribute was refused; \
             corners fall back to a window region"
        );
    }
    let mode = app_window::corner_mode(report.corner);
    chrome.corner.set(Some(mode));
    if mode == app_window::CornerMode::Dwm {
        // The one line that proves the corners: DWM owns them, no clip needed.
        eprintln!(
            "[{PRODUCT_NAME}] {}",
            app_window::corner_log(mode, None, ui.window().scale_factor())
        );
    }
    // Region mode: apply (and log) the clip now, not on the next poll tick.
    refresh_corners(ui, chrome);
    true
}

#[cfg(not(windows))]
fn apply_window_chrome(ui: &AppWindow, _chrome: &ChromeState) -> bool {
    // No DWM on this host; there is nothing to apply or to clip.
    let _ = ui;
    eprintln!(
        "[{PRODUCT_NAME}] app-window chrome: frameless, no Win32 chrome to apply on this host"
    );
    true
}

fn apply_snapshot(ui: &AppWindow, snap: &TrackerSnapshot) {
    let t = ui.global::<Tracker>();
    t.set_player_display(snap.player_display().into());
    t.set_player_initial(snap.player_initial().into());
    t.set_phase_title(snap.phase_title().into());
    t.set_phase_chip(snap.phase_chip().into());
    t.set_map_name(snap.map.clone().into());
    t.set_mode(snap.mode.clone().into());
    t.set_blue(snap.blue);
    t.set_red(snap.red);
    t.set_live_state(snap.live_state().into());
    t.set_live_message(snap.live_message().into());

    let (line, tone) = match &snap.client {
        ClientState::Starting => ("Connecting to the Riot Client…", "warn"),
        ClientState::Missing => ("Riot Client not running", "warn"),
        ClientState::Ready => ("Riot Client connected", "ok"),
        ClientState::Error(_) => ("Riot Client error", "error"),
    };
    t.set_client_line(line.into());
    t.set_client_tone(tone.into());
}

fn apply_rank(ui: &AppWindow, view: &RankView) {
    let t = ui.global::<Tracker>();
    t.set_rank_state(view.state.as_str().into());
    t.set_rank_label(view.label.clone().into());
    t.set_rank_rr(view.rr as i32);
    t.set_rank_rr_known(view.rr_known);
    t.set_rank_message(view.message.clone().into());
    t.set_peak_label(view.peak_label.clone().into());
}

/// The PLAYLIST dropdown's fixed options, from the parser's own table so Rust
/// and the view can never disagree about what is offered.
fn apply_playlist_options(ui: &AppWindow) {
    let t = ui.global::<Tracker>();
    let ids: Vec<slint::SharedString> = outpeek_app::trn_profile::PLAYLISTS
        .iter()
        .map(|(id, _)| slint::SharedString::from(*id))
        .collect();
    let labels: Vec<slint::SharedString> = outpeek_app::trn_profile::PLAYLISTS
        .iter()
        .map(|(_, label)| slint::SharedString::from(*label))
        .collect();
    t.set_playlists(ModelRc::new(VecModel::from(ids)));
    t.set_playlist_labels(ModelRc::new(VecModel::from(labels)));
}

/// Push one Tracker Network profile frame into every Overview slot.
///
/// The provider (worker) already formatted the strings and left `""` where a
/// stat was not reported; this pass never invents a value or a zero.
fn apply_profile(ui: &AppWindow, view: &ProfileView) {
    let t = ui.global::<Tracker>();
    t.set_profile_state(view.state.as_str().into());
    t.set_profile_message(view.message.clone().into());
    t.set_profile_handle(view.handle.clone().into());
    t.set_profile_season(view.season.clone().into());
    t.set_playlist(view.playlist.clone().into());
    t.set_playlist_label(
        outpeek_app::trn_profile::playlist_label(&view.playlist)
            .unwrap_or("Competitive")
            .into(),
    );
    apply_playlist_options(ui);
    t.set_profile_matches(view.matches.clone().into());
    t.set_win_pct(view.win_pct.clone().into());
    t.set_kd(view.kd.clone().into());
    t.set_hs_pct(view.hs_pct.clone().into());
    t.set_dmg_round(view.dmg_round.clone().into());
    t.set_metric_wins(view.wins.clone().into());
    t.set_metric_losses(view.losses.clone().into());
    t.set_metric_kills(view.kills.clone().into());
    t.set_metric_deaths(view.deaths.clone().into());
    t.set_metric_assists(view.assists.clone().into());
    t.set_metric_headshots(view.headshots.clone().into());
    t.set_metric_flawless(view.flawless.clone().into());
    t.set_metric_clutches(view.clutches.clone().into());
    t.set_best_kills(view.best_kills.clone().into());
    t.set_first_blood_deaths(view.first_blood_deaths.clone().into());
    t.set_aces(view.aces.clone().into());
    t.set_acc_head(view.acc_head.clone().into());
    t.set_acc_head_hits(view.acc_head_hits.clone().into());
    t.set_acc_body(view.acc_body.clone().into());
    t.set_acc_body_hits(view.acc_body_hits.clone().into());
    t.set_acc_legs(view.acc_legs.clone().into());
    t.set_acc_legs_hits(view.acc_legs_hits.clone().into());
    t.set_agent_name(view.agent_name.clone().into());
    t.set_agent_role(view.agent_role.clone().into());
    t.set_agent_line(view.agent_line.clone().into());
    t.set_agent_win_pct(view.agent_win_pct.clone().into());
    t.set_agent_kd(view.agent_kd.clone().into());
    t.set_agent_adr(view.agent_adr.clone().into());
    t.set_agent_acs(view.agent_acs.clone().into());
    t.set_act1_label(view.act1_label.clone().into());
    t.set_act1_value(view.act1_value.clone().into());
    t.set_act2_label(view.act2_label.clone().into());
    t.set_act2_value(view.act2_value.clone().into());
    t.set_act3_label(view.act3_label.clone().into());
    t.set_act3_value(view.act3_value.clone().into());
    t.set_score_value(view.score.clone().into());
    t.set_score_badge(view.score_badge.clone().into());
    t.set_score_round_win_pct(view.round_win_pct.clone().into());
    t.set_score_kast(view.kast.clone().into());
    t.set_score_acs(view.acs.clone().into());
    t.set_score_dda(view.dda_round.clone().into());
    t.set_win_pctile(view.win_pctile.clone().into());
    t.set_kd_pctile(view.kd_pctile.clone().into());
    t.set_hs_pctile(view.hs_pctile.clone().into());
    t.set_dpr_pctile(view.dpr_pctile.clone().into());
    t.set_round_win_pctile(view.round_win_pctile.clone().into());
    t.set_kast_pctile(view.kast_pctile.clone().into());
    t.set_acs_pctile(view.acs_pctile.clone().into());
    t.set_dda_pctile(view.dda_pctile.clone().into());
    // The header says where the numbers came from while a profile is loaded.
    let subtitle = match view.state {
        ProfileState::Ready | ProfileState::Cached if !view.handle.is_empty() => {
            view.handle.clone()
        }
        _ => String::new(),
    };
    t.set_profile_subtitle(subtitle.into());
}

/// Push one scoreboard frame into the Matches detail view.
///
/// A stale event (Back was pressed, or another row was clicked while this
/// fetch was in flight) is dropped: the open selection owns the screen.
fn apply_match_detail(ui: &AppWindow, view: &outpeek_app::worker::MatchDetailView) {
    use outpeek_app::trn_match_detail::{DetailPlayer, DetailRound};
    let t = ui.global::<Tracker>();
    if t.get_match_id().as_str() != view.id {
        return;
    }
    t.set_match_state(view.state.into());
    t.set_match_note(view.message.clone().into());
    let Some(d) = view.detail.as_ref() else {
        t.set_match_title("".into());
        t.set_match_score("".into());
        t.set_match_blue_won(false);
        t.set_match_red_won(false);
        t.set_match_blue(ModelRc::new(VecModel::from(Vec::<MatchPlayerUi>::new())));
        t.set_match_red(ModelRc::new(VecModel::from(Vec::<MatchPlayerUi>::new())));
        t.set_match_rounds(ModelRc::new(VecModel::from(Vec::<MatchRoundUi>::new())));
        return;
    };
    let dash = |s: &str| {
        if s.is_empty() {
            "—".to_string()
        } else {
            s.to_string()
        }
    };
    let title = [
        dash(&d.map),
        dash(&d.mode),
        dash(&d.date),
        dash(&d.duration),
    ]
    .join(" · ");
    t.set_match_title(title.into());
    t.set_match_score(d.score_line().into());
    t.set_match_blue_won(d.blue_won);
    t.set_match_red_won(d.red_won);
    let dir = outpeek_app::assets::art_dir();
    let load = |url: &str| -> slint::Image {
        match dir.as_deref() {
            Some(dir) if !url.is_empty() => {
                outpeek_overlay::image_cache::load_from_scaled(dir, url, 96)
            }
            _ => slint::Image::default(),
        }
    };
    let rows = |players: &[DetailPlayer]| -> Vec<MatchPlayerUi> {
        players
            .iter()
            .map(|p| MatchPlayerUi {
                name: p.name.clone().into(),
                agent: dash(&p.agent).into(),
                art: load(&p.art_url),
                rank: dash(&p.rank).into(),
                acs: p.acs.clone().into(),
                kda: p.kda.clone().into(),
                kd: p.kd.clone().into(),
                hs: p.hs.clone().into(),
                kast: p.kast.clone().into(),
                dd: p.delta.clone().into(),
            })
            .collect()
    };
    t.set_match_blue(ModelRc::new(VecModel::from(rows(&d.blue))));
    t.set_match_red(ModelRc::new(VecModel::from(rows(&d.red))));
    let rounds: Vec<MatchRoundUi> = d
        .rounds
        .iter()
        .map(|r: &DetailRound| MatchRoundUi {
            label: r.n.to_string().into(),
            blue: r.winner == "Blue",
            result: r.result.clone().into(),
            tip: format!("Round {} — {}, {}", r.n, r.winner, dash(&r.result)).into(),
        })
        .collect();
    t.set_match_rounds(ModelRc::new(VecModel::from(rounds)));
}

/// Push the recent-match rows plus the website strips into the Matches tab.
/// The rows arrive already formatted; an absent stat is an em dash from
/// `trn_matches`, never a zero. Day headers, form cells and agent cards are
/// pre-formatted the same way — this pass only decodes art and binds models.
fn apply_matches(ui: &AppWindow, view: &outpeek_app::worker::MatchesView) {
    use outpeek_app::trn_matches::MatchRow;
    let t = ui.global::<Tracker>();
    let now = outpeek_app::worker::now_ms();
    let any_art = view.rows.iter().any(|r| !r.art_url.is_empty())
        || view.agents.iter().any(|a| !a.art_url.is_empty());
    let dir = if any_art {
        outpeek_app::assets::art_dir()
    } else {
        None
    };
    let load = |url: &str, edge: u32| -> slint::Image {
        match dir.as_deref() {
            Some(dir) => outpeek_overlay::image_cache::load_from_scaled(dir, url, edge),
            None => slint::Image::default(),
        }
    };
    let row = |r: &MatchRow| MatchRowUi {
        id: r.id.clone().into(),
        map: r.map.clone().into(),
        age: r.age_line(now).into(),
        score: r.rounds().into(),
        score_won: r.rounds_won_label().into(),
        score_lost: r.rounds_lost_label().into(),
        placement: r.placement_label().into(),
        rank_art: load(&r.rank_url, 64),
        rank_name: r.rank_name.clone().into(),
        won: r.won(),
        agent: r.agent.clone().unwrap_or_else(|| "—".to_string()).into(),
        trs: r.trs.map(|v| v.to_string()).unwrap_or_default().into(),
        // The same banding the Tracker Score card uses, applied to this
        // match's own performance score.
        trs_grade: outpeek_app::badges::badge_key("", r.trs.map(|v| v as i32)).into(),
        // A sub-1.0 ratio reads in the client's muted rose.
        kd_low: matches!((r.kills, r.deaths), (Some(k), Some(d)) if d > 0 && k < d),
        clutch: r.clutch.clone().unwrap_or_default().into(),
        clutch_bad: r.clutch_bad,
        art: load(&r.art_url, 96),
        kda: r.kda().into(),
        kd: r.kd_ratio().into(),
        hs: r.hs_pct().into(),
        dd: r.damage_delta_label().into(),
        acs: r.acs().into(),
    };
    let rows: Vec<MatchRowUi> = view.rows.iter().map(&row).collect();
    t.set_matches(ModelRc::new(VecModel::from(rows)));
    let summary = &view.summary;
    t.set_match_summary(MatchSummaryUi {
        record: summary.record.clone().into(),
        kd: summary.kd.clone().into(),
        adr: summary.adr.clone().into(),
        win_share: summary.win_share,
    });
    let agents: Vec<MatchAgentUi> = view
        .agents
        .iter()
        .map(|a| MatchAgentUi {
            name: a.name.clone().into(),
            record: a.record.clone().into(),
            kd: a.kd.clone().into(),
            win_share: a.win_share,
            art: load(&a.art_url, 96),
        })
        .collect();
    t.set_match_agents(ModelRc::new(VecModel::from(agents)));
    let days: Vec<MatchDayUi> = view
        .days
        .iter()
        .map(|d| MatchDayUi {
            title: d.title.clone().into(),
            count_label: d.count_label.clone().into(),
            wins_label: d.wins_label.clone().into(),
            losses_label: d.losses_label.clone().into(),
            kd: d.kd.clone().into(),
            kda: d.kda.clone().into(),
        kda_sub: d.kda_sub.clone().into(),
            dda: d.dda.clone().into(),
            hs: d.hs.clone().into(),
            acs: d.acs.clone().into(),
        })
        .collect();
    // One row model per day, index-aligned with `match-days`: the view loops
    // `match-day-rows[i]` under `match-days[i]`.
    let day_rows: Vec<ModelRc<MatchRowUi>> = view
        .days
        .iter()
        .map(|d| ModelRc::new(VecModel::from(d.rows.iter().map(&row).collect::<Vec<_>>())))
        .collect();
    t.set_match_days(ModelRc::new(VecModel::from(days)));
    t.set_match_day_rows(ModelRc::new(VecModel::from(day_rows)));
    t.set_matches_state(view.state.into());
    t.set_matches_note(view.message.clone().into());
}

/// Push the agent and map rows into the Agents/Maps tabs. Rows arrive already
/// formatted and already sharing the Overview's season; only the art needs
/// decoding here, at the size the views actually draw it (128 px portraits,
/// 320 px map banners) instead of the full-size files.
fn apply_breakdown(ui: &AppWindow, view: &outpeek_app::worker::BreakdownView) {
    use outpeek_app::trn_breakdown::{AgentRow, MapRow};
    let t = ui.global::<Tracker>();
    let any_art = view.agents.iter().any(|r| !r.art_url.is_empty())
        || view.maps.iter().any(|r| !r.art_url.is_empty());
    // No art to load means no directory to create: an empty frame must not
    // touch the disk.
    let dir = if any_art {
        outpeek_app::assets::art_dir()
    } else {
        None
    };
    let load = |url: &str, edge: u32| -> slint::Image {
        match dir.as_deref() {
            Some(dir) => outpeek_overlay::image_cache::load_from_scaled(dir, url, edge),
            None => slint::Image::default(),
        }
    };
    let agents: Vec<AgentRowUi> = view
        .agents
        .iter()
        .map(|r: &AgentRow| AgentRowUi {
            name: r.name.clone().into(),
            role: r.role.clone().into(),
            matches: r.matches.clone().into(),
            win: r.win.clone().into(),
            kd: r.kd.clone().into(),
            acs: r.acs.clone().into(),
            adr: r.adr.clone().into(),
            hs: r.hs.clone().into(),
            pick: r.pick.clone().into(),
            win_share: r.win_share,
            art: load(&r.art_url, 128),
        })
        .collect();
    let maps: Vec<MapRowUi> = view
        .maps
        .iter()
        .map(|r: &MapRow| MapRowUi {
            name: r.name.clone().into(),
            matches: r.matches.clone().into(),
            win: r.win.clone().into(),
            wins: r.wins.clone().into(),
            losses: r.losses.clone().into(),
            dd: r.dd.clone().into(),
            kd: r.kd.clone().into(),
            kd_low: r.kd_low,
            acs: r.acs.clone().into(),
            adr: r.adr.clone().into(),
            hs: r.hs.clone().into(),
            win_share: r.win_share,
            agents: ModelRc::new(VecModel::from(
                r.agents
                    .iter()
                    .map(|a| MapAgentUi {
                        win: a.win.clone().into(),
                        win_share: a.win_share,
                        art: load(&a.art_url, 96),
                    })
                    .collect::<Vec<MapAgentUi>>(),
            )),
            art: load(&r.art_url, 320),
        })
        .collect();
    t.set_agents(ModelRc::new(VecModel::from(agents)));
    t.set_map_rows(ModelRc::new(VecModel::from(maps)));
    t.set_breakdown_state(view.state.into());
    t.set_breakdown_note(view.message.clone().into());
}

/// Push one live-lobby frame into the Live Match tab.
///
/// The worker already formatted every cell — including the honest rank states
/// (`RANK HIDDEN` / `UNRATED` / `NOT READ`) — so this pass only decodes art at
/// the size the rows draw it.
fn apply_live(ui: &AppWindow, view: &LiveView) {
    let t = ui.global::<Tracker>();
    let any_art = view
        .blue
        .iter()
        .chain(view.red.iter())
        .any(|row| !row.agent_art.is_empty() || !row.crest.is_empty());
    let dir = if any_art {
        outpeek_app::assets::art_dir()
    } else {
        None
    };
    let load = |url: &str, edge: u32| -> slint::Image {
        match dir.as_deref() {
            Some(dir) => outpeek_overlay::image_cache::load_from_scaled(dir, url, edge),
            None => slint::Image::default(),
        }
    };
    let rows = |players: &[LivePlayerRow]| -> Vec<LivePlayerUi> {
        players
            .iter()
            .map(|row| LivePlayerUi {
                name: row.name.clone().into(),
                agent: row.agent.clone().into(),
                agent_art: load(&row.agent_art, 96),
                level: row.level.clone().into(),
                rank: row.rank.clone().into(),
                rr: row.rr.clone().into(),
                crest: load(&row.crest, 64),
                party: row.party.clone().into(),
                me: row.me,
                incognito: row.incognito,
            })
            .collect()
    };
    t.set_live_blue_team(ModelRc::new(VecModel::from(rows(&view.blue))));
    t.set_live_red_team(ModelRc::new(VecModel::from(rows(&view.red))));
    t.set_live_roster_state(view.state.into());
    t.set_live_note(view.message.clone().into());
    // The board's strip has its own slots: the snapshot clears its copies on an
    // idle tick, which would blank a board that is still on screen.
    t.set_live_map(view.map.clone().into());
    t.set_live_mode(view.mode.clone().into());
    t.set_live_blue_score(view.blue_score as i32);
    t.set_live_red_score(view.red_score as i32);
}

/// Decode the cached art this event names into the window's image slots.
///
/// The worker cannot send `slint::Image` (it is not `Send`), so it publishes the
/// URLs it has already written to the shared disk cache and this — the UI
/// thread — builds the images. A URL that is empty, or whose file vanished
/// between the download and this call, decodes to an empty image: the UI then
/// draws its fallback, exactly as it did before any art existed.
fn apply_art(ui: &AppWindow, art: &ArtView) {
    let t = ui.global::<Tracker>();
    let dir = if art.rank_crest.is_empty()
        && art.peak_crest.is_empty()
        && art.agent_icon.is_empty()
        && art.banner.is_empty()
    {
        // Nothing to load: clearing the slots must not create directories.
        None
    } else {
        outpeek_app::assets::art_dir()
    };
    let load = |url: &str| -> slint::Image {
        match dir.as_deref() {
            Some(dir) => outpeek_overlay::image_cache::load_from(dir, url),
            None => slint::Image::default(),
        }
    };
    t.set_rank_crest(load(&art.rank_crest));
    t.set_peak_crest(load(&art.peak_crest));
    t.set_agent_icon(load(&art.agent_icon));
    t.set_player_banner(load(&art.banner));
    // The catalogue's name/role beat the payload's hints, but an empty art
    // frame (no network, agent not in the catalogue) must not erase the names
    // the profile frame already set.
    if !art.agent_name.is_empty() {
        t.set_agent_name(art.agent_name.clone().into());
        t.set_agent_role(art.agent_role.clone().into());
    }
}

fn start_worker(
    app: slint::Weak<AppWindow>,
    overlay: Option<slint::Weak<OverlayWindow>>,
    flags: &Arc<Flags>,
) {
    worker::spawn(
        move |event: Event| {
            let app = app.clone();
            let overlay = overlay.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(app) = app.upgrade() else {
                    return;
                };
                match event {
                    Event::Snapshot(snap) => {
                        apply_snapshot(&app, &snap);
                        if let Some(overlay) = overlay.as_ref().and_then(|weak| weak.upgrade()) {
                            // Same facts, same event loop: properties, not a pipe.
                            outpeek_overlay::hud::apply_facts(&overlay, snap.facts());
                            outpeek_overlay::hud::sync_visibility(&overlay, None);
                        }
                    }
                    Event::Rank(view) => apply_rank(&app, &view),
                    Event::Art(art) => apply_art(&app, &art),
                    Event::Profile(view) => apply_profile(&app, &view),
                    Event::Matches(view) => apply_matches(&app, &view),
                    Event::MatchDetail(view) => apply_match_detail(&app, &view),
                    Event::Breakdown(view) => apply_breakdown(&app, &view),
                    Event::Live(view) => apply_live(&app, &view),
                }
            });
        },
        flags.clone(),
    );
}

fn spawn_foreground_watch(
    stop: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
    foreground: Arc<AtomicBool>,
    app: slint::Weak<AppWindow>,
    overlay: Option<slint::Weak<OverlayWindow>>,
) {
    let _ = std::thread::Builder::new()
        .name("outpeek-foreground".to_string())
        .spawn(move || {
            // Force the first frame: the tracker's HUD status must be right
            // even before the first foreground change.
            let mut last_fg = !hud::game_foreground();
            let mut last_on = !enabled.load(Ordering::Relaxed);
            let mut last_sync = Instant::now();
            loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let fg = hud::game_foreground();
                let on = enabled.load(Ordering::Relaxed);
                foreground.store(fg, Ordering::Relaxed);
                let show = model::should_show_overlay(on, setting(on), fg, PLATFORM_SUPPORTS_HUD);
                // Re-assert the HUD every 2 s while it is visible (the cadence
                // the Tauri daemon used for the sidecar): follows a resolution
                // or window move and heals a lost topmost/click-through bit.
                // Transitions are immediate; the periodic pass runs only while
                // shown.
                let resync = show && last_sync.elapsed() >= Duration::from_secs(2);
                if fg != last_fg || on != last_on || resync {
                    last_fg = fg;
                    last_on = on;
                    if resync {
                        last_sync = Instant::now();
                    }
                    let app = app.clone();
                    let overlay = overlay.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(app) = app.upgrade() {
                            apply_hud(&app, on, fg);
                        }
                        if let Some(overlay) = overlay.as_ref().and_then(|weak| weak.upgrade()) {
                            outpeek_overlay::set_present(show);
                            outpeek_overlay::hud::sync_visibility(&overlay, None);
                        }
                    });
                }
                std::thread::sleep(Duration::from_millis(1_000));
            }
        });
}

/// Debug instrumentation for the shared-event-loop risk. OFF by default.
///
/// Pushes 200 tracker property updates through `invoke_from_event_loop` and
/// reports total wall time plus the worst UI-thread dispatch latency. That is
/// the cost of both windows sharing one loop; it is **not** GPU frame time.
/// Run with the HUD visible to make the comparison meaningful.
fn maybe_run_repaint_bench(app: &AppWindow) {
    if std::env::var_os("OUTPEEK_BENCH_REPAINTS").is_none() {
        return;
    }
    let weak = app.as_weak();
    let _ = std::thread::Builder::new()
        .name("outpeek-bench".to_string())
        .spawn(move || {
            // Let the first frame + poller settle, then warm the loop so the
            // measurement is steady state, not renderer startup.
            std::thread::sleep(Duration::from_millis(1_500));
            let (warm_tx, warm_rx) = std::sync::mpsc::channel();
            for _ in 0..5 {
                let tx = warm_tx.clone();
                if slint::invoke_from_event_loop(move || {
                    let _ = tx.send(());
                })
                .is_err()
                {
                    return;
                }
            }
            drop(warm_tx);
            for _ in warm_rx.iter().take(5) {}

            let (tx, rx) = std::sync::mpsc::channel();
            let started = Instant::now();
            let mut posted = 0usize;
            for i in 0..200i32 {
                let at = Instant::now();
                let weak = weak.clone();
                let tx = tx.clone();
                let ok = slint::invoke_from_event_loop(move || {
                    if let Some(app) = weak.upgrade() {
                        app.global::<Tracker>().set_rank_rr(i % 100);
                    }
                    let _ = tx.send(at.elapsed());
                });
                if ok.is_err() {
                    break;
                }
                posted += 1;
            }
            drop(tx);
            let mut max = Duration::ZERO;
            let mut completed = 0usize;
            for waited in rx.iter() {
                max = max.max(waited);
                completed += 1;
            }
            eprintln!(
                "[bench] {completed}/{posted} tracker updates: total={}ms max_dispatch={}ms \
                 (event-loop queue latency, not GPU frame time)",
                started.elapsed().as_millis(),
                max.as_millis()
            );
        });
}
