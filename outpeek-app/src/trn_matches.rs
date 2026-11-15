//! Recent matches from the Tracker Network endpoint — the same keyless path as
//! the profile (`User-Agent: OverwolfClient`, `?source=overwolf-2`).
//!
//! One request returns the last 20 matches with the fields the Matches view
//! needs; nothing here invents a value. A row whose stat is absent renders an
//! em dash at the UI layer, and a payload that does not parse is "no matches",
//! never an empty-looking loss.
//!
//! The parsed rows are cached (`trn-matches.json`, 24 h) so a relaunch with no
//! network still lists the last read — the same rule the profile cache follows.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Cache file beside `trn-cache.json`.
pub const CACHE_FILE: &str = "trn-matches.json";
/// Same TTL as the profile: one act cannot change a played match.
pub const CACHE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
pub const CACHE_VERSION: u32 = 1;
/// The endpoint returns at most 20; the UI shows all of them.
pub const MAX_ROWS: usize = 20;

/// One played match, already flattened for rendering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchRow {
    pub id: String,
    /// RFC 3339 from the payload; the UI shortens it for display.
    pub timestamp: String,
    pub mode: String,
    pub map: String,
    pub season: String,
    /// `victory` | `defeat` | `draw` (lowercase, as sent).
    pub result: String,
    pub agent: Option<String>,
    pub agent_id: Option<String>,
    pub kills: Option<u64>,
    pub deaths: Option<u64>,
    pub assists: Option<u64>,
    pub score: Option<u64>,
    pub rounds_won: Option<u64>,
    pub rounds_lost: Option<u64>,
    pub headshots: Option<u64>,
    pub damage_delta: Option<i64>,
}

impl MatchRow {
    /// `Victory` / `Defeat` / `Draw`, or the raw payload word capitalised.
    pub fn result_label(&self) -> String {
        let mut chars = self.result.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => "—".to_string(),
        }
    }

    pub fn won(&self) -> bool {
        self.result.eq_ignore_ascii_case("victory")
    }

    /// `14 / 23 / 7` when all three are present; partial data keeps its dashes.
    pub fn kda(&self) -> String {
        let part = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_else(|| "—".to_string());
        format!(
            "{} / {} / {}",
            part(self.kills),
            part(self.deaths),
            part(self.assists)
        )
    }

    /// `-1,091` / `+1,234` — the damage delta the payload reports. Signed and
    /// grouped, empty when absent. This is NOT a headshot rate: TRN's list has
    /// no percentage, and headshots ÷ kills is a different metric that would
    /// contradict the profile's HS% (30.9% is share of hits).
    pub fn damage_delta_label(&self) -> String {
        match self.damage_delta {
            Some(delta) => {
                let sign = if delta < 0 { "-" } else { "+" };
                let digits = delta.unsigned_abs().to_string();
                let mut out = String::from(sign);
                for (i, ch) in digits.chars().enumerate() {
                    if i > 0 && (digits.len() - i) % 3 == 0 {
                        out.push(',');
                    }
                    out.push(ch);
                }
                out
            }
            None => String::new(),
        }
    }

    /// Combat score (`4,375`), grouped like every other count in the app.
    pub fn score_label(&self) -> String {
        match self.score {
            Some(score) => {
                let digits = score.to_string();
                let mut out = String::new();
                for (i, ch) in digits.chars().enumerate() {
                    if i > 0 && (digits.len() - i) % 3 == 0 {
                        out.push(',');
                    }
                    out.push(ch);
                }
                out
            }
            None => String::new(),
        }
    }

    /// `14–16` from the round totals.
    pub fn rounds(&self) -> String {
        match (self.rounds_won, self.rounds_lost) {
            (Some(w), Some(l)) => format!("{w}–{l}"),
            _ => "—".to_string(),
        }
    }

    /// `2026-11-14` → `Nov 14`. Anything unexpected is returned unchanged so a
    /// format change is visible rather than silently blank.
    pub fn date_label(&self) -> String {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let date = self.timestamp.get(0..10).unwrap_or("");
        let parts: Vec<&str> = date.split('-').collect();
        if parts.len() != 3 {
            return self.timestamp.clone();
        }
        let month = parts[1]
            .parse::<usize>()
            .ok()
            .and_then(|m| MONTHS.get(m.saturating_sub(1)));
        let day = parts[2].trim_start_matches('0');
        match month {
            Some(m) => format!("{m} {day}"),
            None => date.to_string(),
        }
    }
}

fn num(stats: &Value, key: &str) -> Option<u64> {
    stats.get(key)?.get("value")?.as_u64()
}

fn signed(stats: &Value, key: &str) -> Option<i64> {
    let v = stats.get(key)?.get("value")?;
    v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64))
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn opt_text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Parse the matches payload into rows, newest first as sent.
///
/// `None` means the payload is not the shape we understand (no `data.matches`
/// array) — the caller keeps its previous state instead of showing an empty
/// history as if the account had never played.
pub fn parse(payload: &Value) -> Option<Vec<MatchRow>> {
    let matches = payload.get("data")?.get("matches")?.as_array()?;
    let rows: Vec<MatchRow> = matches
        .iter()
        .filter_map(|m| {
            let id = m.get("attributes")?.get("id")?.as_str()?.to_string();
            let meta = m.get("metadata")?;
            let segment = m
                .get("segments")?
                .as_array()?
                .iter()
                .find(|s| s.get("type").and_then(|t| t.as_str()) == Some("overview"))?;
            let stats = segment.get("stats").cloned().unwrap_or(Value::Null);
            let seg_meta = segment.get("metadata").cloned().unwrap_or(Value::Null);
            Some(MatchRow {
                id,
                timestamp: text(meta, "timestamp"),
                mode: text(meta, "modeName"),
                map: text(meta, "mapName"),
                season: text(meta, "seasonName"),
                result: text(meta, "result"),
                agent: opt_text(&seg_meta, "agentName"),
                agent_id: opt_text(&seg_meta, "agent"),
                kills: num(&stats, "kills"),
                deaths: num(&stats, "deaths"),
                assists: num(&stats, "assists"),
                score: num(&stats, "score"),
                rounds_won: num(&stats, "roundsWon"),
                rounds_lost: num(&stats, "roundsLost"),
                headshots: num(&stats, "headshots"),
                damage_delta: signed(&stats, "damageDelta"),
            })
        })
        .take(MAX_ROWS)
        .collect();
    Some(rows)
}

/// Cache envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheFile {
    pub version: u32,
    pub saved_at: u64,
    pub handle: String,
    pub matches: Vec<MatchRow>,
}

/// `%APPDATA%\Outpeek\trn-matches.json`.
pub fn cache_path() -> Option<PathBuf> {
    crate::paths::data_file(CACHE_FILE)
}

/// Refuse an old schema, an old timestamp, or a file that does not parse.
pub fn load_cache(path: &Path, now_ms: u64, ttl_ms: u64) -> Option<CacheFile> {
    let text = std::fs::read_to_string(path).ok()?;
    let cache: CacheFile = serde_json::from_str(&text).ok()?;
    if cache.version != CACHE_VERSION {
        return None;
    }
    if !crate::trn_profile::cache_usable(cache.saved_at, now_ms, ttl_ms) {
        return None;
    }
    if cache.handle.is_empty() {
        return None;
    }
    Some(cache)
}

/// Persist rows atomically (never the raw body).
pub fn save_cache(path: &Path, handle: &str, rows: &[MatchRow], now_ms: u64) -> Result<(), String> {
    let cache = CacheFile {
        version: CACHE_VERSION,
        saved_at: now_ms,
        handle: handle.to_string(),
        matches: rows.to_vec(),
    };
    let mut json = serde_json::to_string(&cache)
        .map_err(|_| "Could not encode the matches cache.".to_string())?;
    json.push('\n');
    crate::account::write_atomic(path, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/trn-matches.json");

    fn rows() -> Vec<MatchRow> {
        let payload: Value = serde_json::from_str(FIXTURE).expect("fixture parses");
        parse(&payload).expect("fixture matches parse")
    }

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "outpeek-matches-{tag}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn the_fixture_yields_every_match_with_its_core_fields() {
        let r = rows();
        assert_eq!(r.len(), 20);
        assert_eq!(r[0].id, "cadcc206-9d07-4695-b792-4215ec6ef341");
        assert_eq!(r[0].map, "Bind");
        assert_eq!(r[0].mode, "Competitive");
        assert_eq!(r[0].result, "defeat");
        assert_eq!(r[0].agent.as_deref(), Some("Sage"));
        assert_eq!(r[0].kills, Some(14));
        assert_eq!(r[0].deaths, Some(23));
        assert_eq!(r[0].assists, Some(7));
        assert_eq!(r[0].rounds_won, Some(14));
        assert_eq!(r[0].rounds_lost, Some(16));
        assert_eq!(r[0].damage_delta, Some(-1091));
    }

    #[test]
    fn headshot_share_and_score_come_only_from_reported_numbers() {
        let base = rows().remove(0);
        let row = MatchRow {
            score: Some(4375),
            damage_delta: Some(1234),
            ..base.clone()
        };
        assert_eq!(row.score_label(), "4,375");
        assert_eq!(row.damage_delta_label(), "+1,234");
        let negative = MatchRow {
            damage_delta: Some(-1091),
            ..base.clone()
        };
        assert_eq!(negative.damage_delta_label(), "-1,091");
        let missing = MatchRow {
            damage_delta: None,
            score: None,
            ..base
        };
        assert_eq!(missing.damage_delta_label(), "");
        assert_eq!(missing.score_label(), "");
    }

    #[test]
    fn rows_render_without_inventing_numbers() {
        let r = rows();
        assert_eq!(r[0].kda(), "14 / 23 / 7");
        assert_eq!(r[0].rounds(), "14–16");
        assert_eq!(r[0].result_label(), "Defeat");
        assert!(!r[0].won());
        assert_eq!(r[0].date_label(), "Nov 14");

        let mut missing = r[0].clone();
        missing.kills = None;
        missing.assists = None;
        missing.rounds_lost = None;
        assert_eq!(missing.kda(), "— / 23 / —");
        assert_eq!(missing.rounds(), "—");
    }

    #[test]
    fn a_payload_of_the_wrong_shape_is_not_an_empty_history() {
        assert!(parse(&serde_json::json!({})).is_none());
        assert!(parse(&serde_json::json!({"data": {}})).is_none());
        assert!(parse(&serde_json::json!({"data": {"matches": "nope"}})).is_none());
        // An empty list is legitimately empty.
        assert_eq!(
            parse(&serde_json::json!({"data": {"matches": []}})),
            Some(vec![])
        );
    }

    #[test]
    fn a_match_without_the_overview_segment_is_skipped_not_faked() {
        let payload = serde_json::json!({"data": {"matches": [
            {"attributes": {"id": "a"}, "metadata": {}, "segments": []},
            {"attributes": {"id": "b"}, "metadata": {"result": "victory"},
             "segments": [{"type": "overview", "metadata": {"agentName": "Omen"}, "stats": {}}]}
        ]}});
        let r = parse(&payload).expect("shape is valid");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "b");
        assert_eq!(r[0].agent.as_deref(), Some("Omen"));
        assert_eq!(r[0].kills, None);
    }

    #[test]
    fn cache_round_trips_and_respects_its_ttl() {
        let path = temp_path("roundtrip");
        let r = rows();
        save_cache(&path, "lil ga7ed#zngr", &r, 1_000).unwrap();
        let loaded = load_cache(&path, 2_000, CACHE_TTL_MS).expect("fresh cache");
        assert_eq!(loaded.matches, r);
        assert_eq!(loaded.handle, "lil ga7ed#zngr");
        assert!(load_cache(&path, 1_000 + CACHE_TTL_MS + 1, CACHE_TTL_MS).is_none());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_corrupt_or_foreign_cache_is_no_cache() {
        let path = temp_path("corrupt");
        std::fs::write(&path, b"{ broken").unwrap();
        assert!(load_cache(&path, 1_000, CACHE_TTL_MS).is_none());
        std::fs::write(
            &path,
            br#"{"version": 99, "saved_at": 1, "handle": "x", "matches": []}"#,
        )
        .unwrap();
        assert!(load_cache(&path, 1_000, CACHE_TTL_MS).is_none());
        std::fs::remove_file(&path).ok();
    }
}
