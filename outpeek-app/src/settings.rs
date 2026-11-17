//! Persisted app settings — one structured file, not a pile of `.txt` flags.
//!
//! Everything the app remembers between launches lives in
//! `%APPDATA%\Outpeek\settings.json` as JSON. A file per boolean is how you end
//! up with `overlay_renderer.txt`, `auto_borderless.txt`, `hotkey.txt` and no
//! idea which one is authoritative — so this module is the single store, and
//! new settings are fields here, never new files.
//!
//! Migration: builds before this one wrote `overlay_renderer.txt` (plain text,
//! `native` or `ts`). [`migrate_legacy`] reads it once, writes the JSON, and
//! renames the old file to `overlay_renderer.txt.migrated` — it is never
//! deleted, because losing a user's choice to tidy a filename is not a trade
//! worth making.
//!
//! Corruption is not fatal: an unreadable or malformed `settings.json` yields
//! [`AppSettings::default`], which is the same state a fresh install has.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name under the app data directory.
pub const SETTINGS_FILE: &str = "settings.json";
/// Where the pre-JSON build kept the renderer flag.
pub const LEGACY_RENDERER_FILE: &str = "overlay_renderer.txt";
/// The renamed legacy file, kept for the user's records.
pub const LEGACY_RENDERER_MIGRATED: &str = "overlay_renderer.txt.migrated";

/// Tracker tabs this build can open. A remembered tab outside this list is
/// ignored rather than trusted: an old file must not open a dead page.
pub const TABS: [&str; 5] = ["overview", "matches", "agents", "maps", "live"];

/// The shell's own size bounds, mirroring `ui/app.slint`'s min/max. Used to
/// clamp a restored window size, so a hand-edited `40x40` cannot open an
/// unusable window.
pub const MIN_WINDOW: (u32, u32) = (1280, 720);
pub const MAX_WINDOW: (u32, u32) = (3840, 2160);

/// Persisted settings. Every field needs a `default` so an older or newer file
/// still loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    /// `native` (HUD drawn by this app) or `ts` (hidden; the Tauri build's
    /// webview owns the HUD). Unknown values resolve to `native` at read time.
    #[serde(default = "renderer_default")]
    pub overlay_renderer: String,
    /// Last shell view ("tracker" | "settings"); empty = never recorded.
    #[serde(default)]
    pub last_view: String,
    /// Last tracker tab; empty or unknown = Overview.
    #[serde(default)]
    pub last_tab: String,
    /// Window size in logical pixels; 0 = never recorded.
    #[serde(default)]
    pub window_width: u32,
    #[serde(default)]
    pub window_height: u32,
}

fn renderer_default() -> String {
    "native".to_string()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            overlay_renderer: renderer_default(),
            last_view: String::new(),
            last_tab: String::new(),
            window_width: 0,
            window_height: 0,
        }
    }
}

impl AppSettings {
    /// True unless the file explicitly says `ts` — the same polarity the old
    /// plain-text flag had, so migration cannot change behaviour.
    pub fn hud_enabled(&self) -> bool {
        self.overlay_renderer.trim() != "ts"
    }

    pub fn set_hud_enabled(&mut self, enabled: bool) {
        self.overlay_renderer = if enabled { "native" } else { "ts" }.to_string();
    }

    /// The view to open: the remembered one when it is one this build has.
    pub fn start_view(&self) -> &str {
        match self.last_view.trim() {
            "settings" => "settings",
            _ => "tracker",
        }
    }

    /// The tab to open: the remembered one when it is a tab this build has,
    /// Overview otherwise.
    pub fn start_tab(&self) -> &str {
        let tab = self.last_tab.trim();
        if TABS.contains(&tab) {
            tab
        } else {
            "overview"
        }
    }

    /// The remembered window size, clamped to the shell's bounds. `None` when
    /// nothing was recorded (the layout's own size then applies).
    pub fn window_size(&self) -> Option<(u32, u32)> {
        if self.window_width == 0 || self.window_height == 0 {
            return None;
        }
        Some((
            self.window_width.clamp(MIN_WINDOW.0, MAX_WINDOW.0),
            self.window_height.clamp(MIN_WINDOW.1, MAX_WINDOW.1),
        ))
    }

    pub fn set_window_size(&mut self, width: u32, height: u32) {
        self.window_width = width.clamp(MIN_WINDOW.0, MAX_WINDOW.0);
        self.window_height = height.clamp(MIN_WINDOW.1, MAX_WINDOW.1);
    }
}

/// Path of the settings file inside `dir`.
pub fn settings_path(dir: &Path) -> PathBuf {
    dir.join(SETTINGS_FILE)
}

/// Parse settings from bytes. Never fails: anything unreadable is the default.
pub fn parse(bytes: &[u8]) -> AppSettings {
    serde_json::from_slice(bytes).unwrap_or_default()
}

/// Serialise settings as pretty JSON (one trailing newline).
pub fn to_json(settings: &AppSettings) -> String {
    let mut s = serde_json::to_string_pretty(settings).unwrap_or_else(|_| "{}".to_string());
    s.push('\n');
    s
}

/// Load from `dir`, or default when the file is missing or unreadable.
pub fn load(dir: &Path) -> AppSettings {
    match std::fs::read(settings_path(dir)) {
        Ok(bytes) => parse(&bytes),
        Err(_) => AppSettings::default(),
    }
}

/// Load from the app data directory (`%APPDATA%\Outpeek` on Windows).
pub fn load_default() -> AppSettings {
    match crate::paths::app_data_dir() {
        Some(dir) => load(&dir),
        None => AppSettings::default(),
    }
}

/// Save into `dir` atomically: write a temp file in the same directory, then
/// rename over the target, so a crash mid-write cannot truncate the settings.
pub fn save(dir: &Path, settings: &AppSettings) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let target = settings_path(dir);
    let tmp = dir.join(format!(
        ".{SETTINGS_FILE}.tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp, to_json(settings)).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {} -> {}: {e}", tmp.display(), target.display())
    })
}

pub fn save_default(settings: &AppSettings) -> Result<(), String> {
    match crate::paths::app_data_dir() {
        Some(dir) => save(&dir, settings),
        None => Err("no app data directory on this host".to_string()),
    }
}

/// Migrate the pre-JSON plain-text renderer flag exactly once.
///
/// Returns `Some(settings)` when a migration happened. The legacy file is
/// renamed, never deleted, and a failure leaves it in place for the next launch.
pub fn migrate_legacy(dir: &Path) -> Option<AppSettings> {
    let target = settings_path(dir);
    if target.exists() {
        return None;
    }
    let legacy = dir.join(LEGACY_RENDERER_FILE);
    let value = std::fs::read_to_string(&legacy).ok()?;
    let mut settings = AppSettings::default();
    settings.set_hud_enabled(value.trim() != "ts");
    save(dir, &settings).ok()?;
    let _ = std::fs::rename(&legacy, dir.join(LEGACY_RENDERER_MIGRATED));
    Some(settings)
}

/// `migrate_legacy` against the real app data directory.
pub fn migrate_legacy_default() -> Option<AppSettings> {
    crate::paths::app_data_dir().and_then(|dir| migrate_legacy(&dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "outpeek-settings-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn round_trips_through_json() {
        let dir = temp_dir("roundtrip");
        let mut s = AppSettings::default();
        s.set_hud_enabled(false);
        save(&dir, &s).unwrap();
        let raw = std::fs::read_to_string(settings_path(&dir)).unwrap();
        assert!(raw.contains("\"overlay_renderer\": \"ts\""));
        assert_eq!(load(&dir), s);
        assert!(!load(&dir).hud_enabled());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_from_before_this_version_still_loads_and_opens_overview() {
        // The pre-window-state shape: only the renderer.
        let raw = br#"{"overlay_renderer":"native"}"#;
        let s = parse(raw);
        assert_eq!(s.start_tab(), "overview");
        assert_eq!(s.start_view(), "tracker");
        assert_eq!(s.window_size(), None, "nothing recorded, nothing restored");
        assert!(s.hud_enabled());
    }

    #[test]
    fn a_remembered_tab_and_size_are_restored_within_bounds() {
        let raw = br#"{"last_view":"settings","last_tab":"maps",
                       "window_width":1300,"window_height":900}"#;
        let s = parse(raw);
        assert_eq!(s.start_view(), "settings");
        assert_eq!(s.start_tab(), "maps");
        assert_eq!(s.window_size(), Some((1300, 900)));

        // A tab this build does not have falls back to Overview instead of
        // opening a dead page; a size outside the shell's bounds is clamped.
        let odd = parse(
            br#"{"last_tab":"live-match-v2","last_view":"store",
                 "window_width":40,"window_height":9000}"#,
        );
        assert_eq!(odd.start_tab(), "overview");
        assert_eq!(odd.start_view(), "tracker");
        assert_eq!(odd.window_size(), Some((MIN_WINDOW.0, MAX_WINDOW.1)));
    }

    #[test]
    fn last_view_and_size_round_trip() {
        let dir = temp_dir("window-state");
        let mut s = AppSettings::default();
        s.last_tab = "live".to_string();
        s.last_view = "tracker".to_string();
        s.set_window_size(1440, 1000);
        save(&dir, &s).unwrap();
        let back = load(&dir);
        assert_eq!(back.start_tab(), "live");
        assert_eq!(back.window_size(), Some((1440, 1000)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_file_is_the_default_state() {
        let dir = temp_dir("missing");
        assert_eq!(load(&dir), AppSettings::default());
        assert!(load(&dir).hud_enabled());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_json_never_stops_launch() {
        let dir = temp_dir("corrupt");
        std::fs::write(settings_path(&dir), b"{ not json").unwrap();
        assert_eq!(load(&dir), AppSettings::default());
        std::fs::write(settings_path(&dir), b"").unwrap();
        assert_eq!(load(&dir), AppSettings::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unknown_field_or_value_does_not_lose_the_file() {
        let dir = temp_dir("unknown");
        std::fs::write(
            settings_path(&dir),
            br#"{ "overlay_renderer": "something-new", "future": 1 }"#,
        )
        .unwrap();
        let s = load(&dir);
        // Unknown value keeps the "not ts" polarity rather than hiding the HUD.
        assert!(s.hud_enabled());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_plain_text_is_migrated_once_and_kept() {
        let dir = temp_dir("legacy");
        std::fs::write(dir.join(LEGACY_RENDERER_FILE), "ts").unwrap();
        let migrated = migrate_legacy(&dir).expect("migration");
        assert!(!migrated.hud_enabled());
        assert!(settings_path(&dir).exists());
        assert!(!dir.join(LEGACY_RENDERER_FILE).exists());
        assert!(dir.join(LEGACY_RENDERER_MIGRATED).exists());
        // Second run is a no-op: the JSON already exists.
        assert!(migrate_legacy(&dir).is_none());
        assert!(!load(&dir).hud_enabled());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn existing_json_wins_over_a_stale_legacy_file() {
        let dir = temp_dir("wins");
        let mut s = AppSettings::default();
        s.set_hud_enabled(true);
        save(&dir, &s).unwrap();
        std::fs::write(dir.join(LEGACY_RENDERER_FILE), "ts").unwrap();
        assert!(migrate_legacy(&dir).is_none());
        assert!(load(&dir).hud_enabled(), "JSON must not be overwritten");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_leaves_no_temp_files_behind() {
        let dir = temp_dir("atomic");
        save(&dir, &AppSettings::default()).unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![SETTINGS_FILE.to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
