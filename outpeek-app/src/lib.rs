//! Outpeek app library: the one-process native app.
//!
//! Two windows, one Slint event loop:
//!
//! * the tracker window — frameless with a drawn titlebar, resizable,
//!   taskbar-visible;
//! * the HUD overlay — transparent, topmost, click-through, toolwindow,
//!   created hidden at startup and shown only while VALORANT is focused and
//!   the renderer setting says native.
//!
//! Facts live in memory. One poller reads the Riot Client's loopback API and
//! publishes a [`model::TrackerSnapshot`]; the same event drives both windows
//! through Slint properties, so there is no IPC and no second process.
//!
//! Business logic that already exists is compiled **by path** from
//! `legacy/src-tauri/src` (`paths`, `fs_migrate`, `live_facts`, `riot_http`,
//! `overlay_renderer`), exactly like `legacy/src-tauri/pure`.
//!
//! Safety: read-only. Loopback GETs with the lockfile credentials; the only
//! remote call is one `/mmr` read on demand (rank is not exposed locally), and
//! it is never on the 2.5 s live tick.

#[path = "../../legacy/src-tauri/src/fs_migrate.rs"]
pub mod fs_migrate;
#[path = "../../legacy/src-tauri/src/live_facts.rs"]
#[allow(dead_code)]
pub mod live_facts;
#[path = "../../legacy/src-tauri/src/overlay_renderer.rs"]
pub mod overlay_renderer;
#[path = "../../legacy/src-tauri/src/paths.rs"]
pub mod paths;
#[path = "../../legacy/src-tauri/src/riot_http.rs"]
pub mod riot_http;

pub mod account;
pub mod app_window;
pub mod assets;
pub mod badges;
pub mod boot;
pub mod fonts;
pub mod hud;
pub mod identity;
pub mod live_api;
pub mod live_match;
pub mod live_view;
pub mod model;
pub mod rank;
pub mod settings;
pub mod settings_model;
pub mod trn;
pub mod trn_breakdown;
pub mod trn_matches;
pub mod trn_profile;
pub mod worker;

/// Product name shown in the window title and sidebar.
pub const PRODUCT_NAME: &str = "Outpeek";
/// Version the shell reports (crate version; `bun run bump` keeps it in sync).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
