//! Seed the offline caches from the checked-in TRN fixtures.
//!
//! ```text
//! APPDATA=/tmp/outpeek-appdata cargo run -p outpeek-app --example seed_offline
//! ```
//!
//! Parses `tests/fixtures/trn-profile.json` and `trn-matches.json` with the
//! REAL `trn_profile::parse` / `trn_matches::parse` and writes both caches with
//! the REAL `save_cache` functions — no hand-built JSON, so the schema can
//! never drift. `saved_at` is "now", so the 24 h TTL passes on first launch.
//! No network: fixtures only. Prints the exact paths written, then reads both
//! back with the REAL `load_cache` functions as proof.

use std::time::{SystemTime, UNIX_EPOCH};

use outpeek_app::{trn_matches, trn_profile};

const PROFILE_FIXTURE: &str = include_str!("../tests/fixtures/trn-profile.json");
const MATCHES_FIXTURE: &str = include_str!("../tests/fixtures/trn-matches.json");

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn main() {
    let now = now_ms();

    // --- profile: real parse, real save -------------------------------------
    let payload: serde_json::Value =
        serde_json::from_str(PROFILE_FIXTURE).expect("seed_offline: profile fixture parses");
    let profile = trn_profile::parse(&payload).expect("seed_offline: profile fixture parses");
    let profile_path = trn_profile::cache_path().unwrap_or_else(|| {
        eprintln!("seed_offline: no app-data directory (APPDATA unset) — nothing to write into");
        std::process::exit(2);
    });
    trn_profile::save_cache(&profile_path, &profile, now)
        .expect("seed_offline: profile save_cache");
    println!("profile cache: {}", profile_path.display());

    // --- matches: real parse, real save -------------------------------------
    let payload: serde_json::Value =
        serde_json::from_str(MATCHES_FIXTURE).expect("seed_offline: matches fixture parses");
    let rows = trn_matches::parse(&payload).expect("seed_offline: matches fixture parses");
    let matches_path = trn_matches::cache_path().unwrap_or_else(|| {
        eprintln!("seed_offline: no app-data directory (APPDATA unset) — nothing to write into");
        std::process::exit(2);
    });
    trn_matches::save_cache(&matches_path, &profile.handle, &rows, now)
        .expect("seed_offline: matches save_cache");
    println!("matches cache: {}", matches_path.display());

    // --- load-back proof: real load_cache, same TTL the worker uses ---------
    let cached = trn_profile::load_cache(&profile_path, now_ms(), trn_profile::CACHE_TTL_MS)
        .expect("seed_offline: profile load_cache reads back");
    println!(
        "profile load-back: handle={} season={} matches={} kd={} hs={} rank_tier={:?} score={}",
        cached.profile.handle,
        cached.profile.season_name,
        cached
            .profile
            .season
            .matches
            .map(|m| m.to_string())
            .unwrap_or_else(|| "—".to_string()),
        cached
            .profile
            .season
            .kd
            .map(|v| format!("{v:.2}"))
            .unwrap_or_else(|| "—".to_string()),
        cached
            .profile
            .season
            .hs_pct
            .map(|v| format!("{v:.1}%"))
            .unwrap_or_else(|| "—".to_string()),
        cached.profile.rank.as_ref().and_then(|r| r.tier),
        cached
            .profile
            .score
            .map(|s| s.to_string())
            .unwrap_or_else(|| "—".to_string()),
    );
    let mcached =
        trn_matches::load_cache(&matches_path, now_ms(), trn_matches::CACHE_TTL_MS)
            .expect("seed_offline: matches load_cache reads back");
    let first = mcached.matches.first();
    println!(
        "matches load-back: handle={} rows={} first={} {} {}",
        mcached.handle,
        mcached.matches.len(),
        first.map(|r| r.id.as_str()).unwrap_or("?"),
        first.map(|r| r.map.as_str()).unwrap_or("?"),
        first.map(|r| r.kda()).unwrap_or_default(),
    );
}
