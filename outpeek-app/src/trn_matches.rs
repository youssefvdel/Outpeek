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
    /// Cached agent portrait URL. Seeded at parse from the payload's own
    /// `agentImageUrl` (non-empty only); the worker downloads it through the
    /// allowlisted art pipeline, and the catalogue fills in rows whose payload
    /// carried none. Empty = catalogue missed; the row draws the glyph.
    /// `default` so caches written before this field still load.
    #[serde(default)]
    pub art_url: String,
    /// TRN performance score for this match (`trnPerformanceScore` value).
    /// `default` so caches written before this field still load.
    #[serde(default)]
    pub trs: Option<u64>,
    /// First `tags[]` name (`1v1 Lost`), if the payload attached any.
    /// `default` so caches written before this field still load.
    #[serde(default)]
    pub clutch: Option<String>,
    /// The first tag's tone was `Negative` — the chip draws coral, else mint.
    /// `default` so caches written before this field still load.
    #[serde(default)]
    pub clutch_bad: bool,
    /// `placement` value (9 renders as `9th` under the map). A placement fact
    /// the payload reports for the mode; absent more often than not.
    /// `default` so caches written before this field still load.
    #[serde(default)]
    pub placement: Option<u64>,
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

    /// `0.61` — kills ÷ deaths for this match only. Either side missing, or no
    /// deaths recorded, is an em dash: a ratio needs both numbers.
    pub fn kd_ratio(&self) -> String {
        match (self.kills, self.deaths) {
            (Some(k), Some(d)) if d > 0 => format!("{:.2}", k as f64 / d as f64),
            _ => String::new(),
        }
    }

    /// `29%` — headshots ÷ kills for this match only (the desktop client's own
    /// per-match column). Missing on either side is empty, never 0%.
    pub fn hs_pct(&self) -> String {
        match (self.headshots, self.kills) {
            (Some(head), Some(kills)) if kills > 0 => {
                format!("{:.0}%", head as f64 * 100.0 / kills as f64)
            }
            _ => String::new(),
        }
    }

    /// Per-match combat score ÷ rounds played, rounded — the desktop client's
    /// ACS column (a 4,375 total over 30 rounds reads 146). Either side missing
    /// is empty.
    pub fn acs(&self) -> String {
        let rounds = self.rounds_won.unwrap_or(0) + self.rounds_lost.unwrap_or(0);
        match (self.score, rounds) {
            (Some(score), r) if r > 0 => (score as f64 / r as f64).round().to_string(),
            _ => String::new(),
        }
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

use crate::trn_breakdown::BreakdownRow;

// --- TRN-website match-history layout --------------------------------------
// The Matches tab mirrors the website order: a top-3 agent strip, a
// recent-form strip, then one group per day. Rust owns every string below;
// `.slint` only renders.
//
// OUT OF SCOPE, stated once so no reader hunts for it:
// * Per-match TRS, clutch badges, and placement come from the payload's own
//   overview segment (`trnPerformanceScore`, `tags[]`, `placement`), so the
//   rows show them. Weapon stats still have no source: TOP WEAPONS stays out.

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
/// Lets `rel_age` and the day grouping share UTC calendar days with no date
/// crate.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse an RFC 3339 timestamp (`2026-11-14T09:58:26.274+00:00`) to epoch
/// milliseconds. `None` means "not understood" — callers render empty, never
/// a guessed date.
fn rfc3339_ms(ts: &str) -> Option<i64> {
    let (date, time) = ts.split_once('T').or_else(|| ts.split_once('t'))?;
    let d = date.as_bytes();
    if d.len() != 10 || d[4] != b'-' || d[7] != b'-' {
        return None;
    }
    let year: i64 = date[0..4].parse().ok()?;
    let month: i64 = date[5..7].parse().ok()?;
    let day: i64 = date[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let t = time.as_bytes();
    if t.len() < 8 || t[2] != b':' || t[5] != b':' {
        return None;
    }
    let hour: i64 = time[0..2].parse().ok()?;
    let minute: i64 = time[3..5].parse().ok()?;
    let second: i64 = time[6..8].parse().ok()?;
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut rest = &time[8..];
    let mut millis: i64 = 0;
    if let Some(after_dot) = rest.strip_prefix('.') {
        let end = after_dot
            .find(|c: char| c == 'Z' || c == 'z' || c == '+' || c == '-')
            .unwrap_or(after_dot.len());
        let frac = &after_dot[..end];
        if frac.is_empty() || frac.len() > 9 || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut buf = frac.to_string();
        while buf.len() < 3 {
            buf.push('0');
        }
        buf.truncate(3);
        millis = buf.parse().ok()?;
        rest = &after_dot[end..];
    }
    let offset: i64 = if rest == "Z" || rest == "z" {
        0
    } else {
        let sign = match rest.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let digits: String = rest[1..]
            .bytes()
            .filter(|b| *b != b':')
            .map(|b| b as char)
            .collect();
        if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let oh: i64 = digits[0..2].parse().ok()?;
        let om: i64 = digits[2..4].parse().ok()?;
        if oh > 23 || om > 59 {
            return None;
        }
        sign * (oh * 3600 + om * 60)
    };
    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000 + millis - offset * 1000)
}

/// `Today` / `1d ago` / `Nd ago` across UTC midnight boundaries. A future
/// timestamp clamps to `Today` (clock skew is not a match played tomorrow);
/// an unparsable one is empty, never a guessed age.
pub fn rel_age(timestamp: &str, now_ms: u64) -> String {
    const DAY_MS: i64 = 86_400_000;
    let ts_ms = match rfc3339_ms(timestamp) {
        Some(v) => v,
        None => return String::new(),
    };
    let diff = (now_ms as i64).div_euclid(DAY_MS) - ts_ms.div_euclid(DAY_MS);
    if diff <= 0 {
        "Today".to_string()
    } else if diff == 1 {
        "1d ago".to_string()
    } else {
        format!("{diff}d ago")
    }
}

impl MatchRow {
    /// `2d ago // Competitive` — the match row's first line. A missing half
    /// shortens the line; both missing is empty and the view draws its dash.
    pub fn age_line(&self, now_ms: u64) -> String {
        let age = rel_age(&self.timestamp, now_ms);
        match (age.as_str(), self.mode.trim()) {
            ("", "") => String::new(),
            (a, "") => a.to_string(),
            ("", m) => m.to_string(),
            (a, m) => format!("{a} // {m}"),
        }
    }

    /// `TRS 279` — the badge chip beside the agent. Empty when the payload
    /// carried no performance score, and the view hides the chip.
    pub fn trs_chip(&self) -> String {
        self.trs.map(|v| format!("TRS {v}")).unwrap_or_default()
    }

    /// `Bind 9th` — the map with the reported placement after it. The payload
    /// sends a bare number (`9`); the ordinal is presentation, not a new fact.
    /// Either half missing shortens the line.
    pub fn map_label(&self) -> String {
        let ord = self.placement.map(ordinal).unwrap_or_default();
        match (self.map.trim(), ord.as_str()) {
            ("", "") => String::new(),
            (m, "") => m.to_string(),
            ("", o) => o.to_string(),
            (m, o) => format!("{m} {o}"),
        }
    }
}

/// Comma-grouped count (`24876` -> `24,876`).
fn commas(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Signed grouped delta (`-1822` -> `-1,822`, `270` -> `+270`).
fn signed_commas(delta: i64) -> String {
    let sign = if delta < 0 { "-" } else { "+" };
    format!("{sign}{}", commas(delta.unsigned_abs()))
}

fn side(has: bool, value: u64) -> String {
    if has {
        commas(value)
    } else {
        "—".to_string()
    }
}

/// One form-strip cell: the last games at a glance. `score` is the rounds
/// score with the row's existing result colour (`won`); `kd` is the match's
/// own ratio string.
#[derive(Debug, Clone, PartialEq)]
pub struct FormCell {
    pub age: String,
    pub score: String,
    pub won: bool,
    pub kd: String,
}

/// Last 8 matches, newest first (the source rows already arrive that way).
pub fn form_strip(rows: &[MatchRow], now_ms: u64) -> Vec<FormCell> {
    rows.iter()
        .take(8)
        .map(|r| FormCell {
            age: rel_age(&r.timestamp, now_ms),
            score: r.rounds(),
            won: r.won(),
            kd: r.kd_ratio(),
        })
        .collect()
}

/// One agent of the top-3 strip: portrait, name, `7W - 9L (44%)`, K/D.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentStripRow {
    /// Breakdown key (uuid), so the worker can resolve art for the same row
    /// it formatted — never shown.
    pub key: String,
    pub name: String,
    /// Wins rounded from matches x win share. Empty unless both numbers were
    /// reported.
    pub record: String,
    /// The agent's own K/D, the same 2-decimal label as the Agents tab.
    pub kd: String,
    /// Share of that agent's matches won (0–1). Widths the strip's bar; the
    /// number beside it is TRN's reported percentage, never re-derived here.
    pub win_share: f32,
    /// Cached portrait URL, filled by the worker through the existing art
    /// pipeline. Empty = the catalogue missed and the strip draws its glyph.
    pub art_url: String,
}

/// Top 3 agents by matches played (the breakdown arrives most-played first;
/// this re-sorts so the strip never depends on input order).
pub fn top_agents(rows: &[BreakdownRow]) -> Vec<AgentStripRow> {
    let mut sorted: Vec<&BreakdownRow> = rows.iter().collect();
    sorted.sort_by(|a, b| b.matches.cmp(&a.matches).then_with(|| a.name.cmp(&b.name)));
    sorted
        .into_iter()
        .take(3)
        .map(|b| {
            let (record, win_share) = match (b.matches, b.win_pct) {
                (Some(m), Some(p)) if m >= 0 && p.is_finite() => {
                    let wins = ((m as f64) * p / 100.0).round().clamp(0.0, m as f64) as i64;
                    (
                        format!("{}W - {}L ({:.0}%)", wins, m - wins, p),
                        (p / 100.0).clamp(0.0, 1.0) as f32,
                    )
                }
                _ => (String::new(), 0.0),
            };
            AgentStripRow {
                key: b.key.clone(),
                name: b.name.clone(),
                record,
                kd: b.kd_label(),
                win_share,
                art_url: String::new(),
            }
        })
        .collect()
}

/// The season cell at the left of the strip: TRN prints the overall record on
/// the first line and `K/D | ADR` under it, with the same bar the agent cells
/// carry. Every part comes from the season segment TRN reported.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SeasonStrip {
    /// `8W - 12L (40%)`; empty when the wins or losses were not reported.
    pub record: String,
    /// `0.91 K/D`; empty when the payload carried no ratio.
    pub kd: String,
    /// `128 ADR`; empty when the payload carried no damage per round.
    pub adr: String,
    /// Season win share (0–1) for the bar.
    pub win_share: f32,
}

/// Build the strip's season cell from the already-parsed season stats.
pub fn season_strip(season: &crate::trn_profile::SeasonStats) -> SeasonStrip {
    let record = match (season.wins, season.losses) {
        (Some(w), Some(l)) if w >= 0 && l >= 0 => {
            let total = w + l;
            let pct = if total > 0 {
                format!(" ({:.0}%)", (w as f64) * 100.0 / (total as f64))
            } else {
                String::new()
            };
            format!("{w}W - {l}L{pct}")
        }
        _ => String::new(),
    };
    let win_share = match (season.wins, season.losses) {
        (Some(w), Some(l)) if w >= 0 && l >= 0 && w + l > 0 => {
            (w as f32) / ((w + l) as f32)
        }
        _ => 0.0,
    };
    SeasonStrip {
        record,
        kd: season
            .kd
            .filter(|v| v.is_finite())
            .map(|v| format!("{v:.2} K/D"))
            .unwrap_or_default(),
        adr: season
            .dmg_per_round
            .filter(|v| v.is_finite())
            .map(|v| format!("{v:.0} ADR"))
            .unwrap_or_default(),
        win_share,
    }
}

/// One day group: the header aggregates plus that day's rows, newest first.
#[derive(Debug, Clone, PartialEq)]
pub struct DayGroup {
    /// Payload calendar date (`2026-11-14`), the grouping key.
    pub key: String,
    /// `Nov 14 [3]` — the date plus the matches counted under it.
    pub title: String,
    /// `2 W // 1 L` — victories/defeats from the `result` field. Draws are
    /// counted in the title and in every aggregate below but own no slot in
    /// this line; anything else is not a result we understand.
    pub wl: String,
    /// Sum kills / sum deaths, 1 decimal. Empty unless both sides reported
    /// and deaths are non-zero.
    pub kd: String,
    /// `90 K // 95 D // 40 A` — comma-grouped sums, em dash per side when no
    /// match reported it.
    pub kda: String,
    /// Signed grouped damage-delta sum over reporting matches.
    pub dda: String,
    /// Mean of the reporting matches' headshot rates (whole %). A mean of
    /// per-match rates — not a share of shots — so a single-match day reads
    /// exactly its match's HS%.
    pub hs: String,
    /// Total score / total rounds, rounded — the same ratio as the per-match
    /// ACS, so a single-match day reads exactly its match's number.
    pub acs: String,
    pub count: usize,
    pub wins: u64,
    pub losses: u64,
    /// Newest first, like the source rows.
    pub rows: Vec<MatchRow>,
}

/// Payload calendar date (`2026-11-14`), or `unknown` when the timestamp has
/// no date shaped like one.
fn day_key(timestamp: &str) -> String {
    let b = timestamp.as_bytes();
    let digits = |r: std::ops::Range<usize>| {
        b.get(r)
            .map(|s| s.iter().all(|c| c.is_ascii_digit()))
            .unwrap_or(false)
    };
    if timestamp.len() >= 10
        && b[4] == b'-'
        && b[7] == b'-'
        && digits(0..4)
        && digits(5..7)
        && digits(8..10)
    {
        timestamp[0..10].to_string()
    } else {
        "unknown".to_string()
    }
}

/// Group rows by payload calendar date, newest day first (first-seen order:
/// the source rows already arrive newest first).
pub fn group_days(rows: &[MatchRow]) -> Vec<DayGroup> {
    let mut keys: Vec<String> = Vec::new();
    let mut buckets: Vec<Vec<&MatchRow>> = Vec::new();
    for row in rows {
        let key = day_key(&row.timestamp);
        match keys.iter().position(|k| *k == key) {
            Some(i) => buckets[i].push(row),
            None => {
                keys.push(key);
                buckets.push(vec![row]);
            }
        }
    }
    buckets
        .into_iter()
        .map(|bucket| {
            let first = bucket[0];
            let mut wins = 0u64;
            let mut losses = 0u64;
            let (mut kills, mut has_kills) = (0u64, false);
            let (mut deaths, mut has_deaths) = (0u64, false);
            let (mut assists, mut has_assists) = (0u64, false);
            let (mut score, mut has_score) = (0u64, false);
            let mut rounds = 0u64;
            let (mut damage, mut has_damage) = (0i64, false);
            let (mut hs_sum, mut hs_n) = (0.0f64, 0u32);
            for m in &bucket {
                if m.result.eq_ignore_ascii_case("victory") {
                    wins += 1;
                } else if m.result.eq_ignore_ascii_case("defeat") {
                    losses += 1;
                }
                if let Some(k) = m.kills {
                    kills += k;
                    has_kills = true;
                }
                if let Some(d) = m.deaths {
                    deaths += d;
                    has_deaths = true;
                }
                if let Some(a) = m.assists {
                    assists += a;
                    has_assists = true;
                }
                if let Some(s) = m.score {
                    score += s;
                    has_score = true;
                }
                if let (Some(w), Some(l)) = (m.rounds_won, m.rounds_lost) {
                    rounds += w + l;
                }
                if let Some(delta) = m.damage_delta {
                    damage += delta;
                    has_damage = true;
                }
                if let (Some(h), Some(k)) = (m.headshots, m.kills) {
                    if k > 0 {
                        hs_sum += h as f64 * 100.0 / k as f64;
                        hs_n += 1;
                    }
                }
            }
            let n = bucket.len();
            let key = day_key(&first.timestamp);
            let day = if key == "unknown" {
                "—".to_string()
            } else {
                first.date_label()
            };
            DayGroup {
                key,
                title: format!("{day} [{n}]"),
                wl: format!("{wins} W // {losses} L"),
                kd: if has_kills && has_deaths && deaths > 0 {
                    format!("{:.1}", kills as f64 / deaths as f64)
                } else {
                    String::new()
                },
                kda: format!(
                    "{} K // {} D // {} A",
                    side(has_kills, kills),
                    side(has_deaths, deaths),
                    side(has_assists, assists)
                ),
                dda: if has_damage {
                    signed_commas(damage)
                } else {
                    String::new()
                },
                hs: if hs_n > 0 {
                    format!("{:.0}%", hs_sum / hs_n as f64)
                } else {
                    String::new()
                },
                acs: if has_score && rounds > 0 {
                    ((score as f64) / (rounds as f64)).round().to_string()
                } else {
                    String::new()
                },
                count: n,
                wins,
                losses,
                rows: bucket.into_iter().cloned().collect(),
            }
        })
        .collect()
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

/// First `tags[]` entry's name, trimmed; `None` when the payload attached no
/// named tag.
fn first_tag_name(seg_meta: &Value) -> Option<String> {
    seg_meta
        .get("tags")?
        .as_array()?
        .first()?
        .get("name")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Whether the first tag's tone equals `want` (case-insensitive). No tag, or
/// no tone, is not `want`.
fn first_tag_tone_is(seg_meta: &Value, want: &str) -> bool {
    seg_meta
        .get("tags")
        .and_then(|t| t.as_array())
        .and_then(|t| t.first())
        .and_then(|tag| tag.get("tone"))
        .and_then(|tone| tone.as_str())
        .map(|tone| tone.eq_ignore_ascii_case(want))
        .unwrap_or(false)
}

/// `9` -> `9th`, `1` -> `1st` — 11/12/13 stay `th`.
fn ordinal(n: u64) -> String {
    let suffix = if (11..=13).contains(&(n % 100)) {
        "th"
    } else {
        match n % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{n}{suffix}")
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
                art_url: opt_text(&seg_meta, "agentImageUrl").unwrap_or_default(),
                trs: num(&stats, "trnPerformanceScore"),
                clutch: first_tag_name(&seg_meta),
                clutch_bad: first_tag_tone_is(&seg_meta, "Negative"),
                placement: num(&stats, "placement"),
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
    fn per_match_derived_columns_need_both_sides() {
        let base = rows().remove(0);
        // Bind: 14 / 23 / 7, 12 headshots, score 4,375 over 30 rounds.
        assert_eq!(base.kd_ratio(), "0.61");
        assert_eq!(base.hs_pct(), "86%");
        assert_eq!(base.acs(), "146");
        let missing = MatchRow {
            kills: None,
            deaths: Some(3),
            headshots: Some(1),
            score: None,
            rounds_won: None,
            rounds_lost: None,
            ..base.clone()
        };
        assert_eq!(missing.kd_ratio(), "");
        assert_eq!(missing.hs_pct(), "");
        assert_eq!(missing.acs(), "");
        // No deaths recorded is not a division by zero, it is no ratio.
        let no_deaths = MatchRow {
            kills: Some(5),
            deaths: Some(0),
            ..base
        };
        assert_eq!(no_deaths.kd_ratio(), "");
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

    /// 2026-11-16T12:00:00Z, fixed so the age math is exact, not "about".
    const NOW: u64 = 1_794_830_400_000;

    #[test]
    fn rel_age_counts_whole_days_across_midnights() {
        assert_eq!(rel_age("2026-11-16T00:30:00+00:00", NOW), "Today");
        // One minute past UTC midnight is already yesterday.
        assert_eq!(rel_age("2026-11-15T23:59:59+00:00", NOW), "1d ago");
        assert_eq!(rel_age("2026-11-11T06:44:25.388+00:00", NOW), "5d ago");
    }

    #[test]
    fn rel_age_honours_offsets_and_never_shows_tomorrow() {
        // 08:00 at +02:00 is 06:00Z — still today.
        assert_eq!(rel_age("2026-11-16T08:00:00+02:00", NOW), "Today");
        // 23:30 at -02:00 is 01:30Z the next day — today, not yesterday.
        assert_eq!(rel_age("2026-11-15T23:30:00-02:00", NOW), "Today");
        // Clock skew is not a match played tomorrow.
        assert_eq!(rel_age("2026-11-20T00:00:00Z", NOW), "Today");
    }

    #[test]
    fn rel_age_refuses_a_guess_when_the_shape_is_wrong() {
        assert_eq!(rel_age("not-a-time", NOW), "");
        assert_eq!(rel_age("", NOW), "");
        // A zone is required: a bare local time is not UTC.
        assert_eq!(rel_age("2026-11-16T12:00:00", NOW), "");
        assert_eq!(rel_age("2026-13-99T99:99:99Z", NOW), "");
    }

    #[test]
    fn the_age_line_pairs_age_with_mode() {
        let mut row = rows().remove(0);
        assert_eq!(row.age_line(NOW), "2d ago // Competitive");
        row.mode = String::new();
        assert_eq!(row.age_line(NOW), "2d ago");
        row.timestamp = "garbage".to_string();
        assert_eq!(row.age_line(NOW), "");
    }

    #[test]
    fn days_group_newest_first_with_counts_and_records() {
        let r = rows();
        let days = group_days(&r);
        let keys: Vec<&str> = days.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "2026-11-14",
                "2026-11-13",
                "2026-11-11",
                "2026-11-10",
                "2026-11-09",
                "2026-11-08",
                "2026-11-07",
                "2026-11-06"
            ]
        );
        let counts: Vec<usize> = days.iter().map(|d| d.count).collect();
        assert_eq!(counts, [1, 1, 6, 1, 4, 2, 1, 4]);
        assert_eq!(days.iter().map(|d| d.count).sum::<usize>(), 20);
        // The fixture is 7W / 13L with no draws.
        assert_eq!(days.iter().map(|d| d.wins).sum::<u64>(), 7);
        assert_eq!(days.iter().map(|d| d.losses).sum::<u64>(), 13);
        let nov9 = days.iter().find(|d| d.key == "2026-11-09").unwrap();
        assert_eq!(nov9.title, "Nov 9 [4]");
        assert_eq!(nov9.wl, "3 W // 1 L");
    }

    #[test]
    fn a_single_match_day_reads_exactly_its_match() {
        let r = rows();
        let m = &r[0];
        let days = group_days(&r);
        let g = days.iter().find(|d| d.key == "2026-11-14").unwrap();
        assert_eq!(g.count, 1);
        assert_eq!(g.wins, 0);
        assert_eq!(g.losses, 1);
        assert_eq!(g.title, "Nov 14 [1]");
        assert_eq!(g.wl, "0 W // 1 L");
        // Same sums the row itself reports: 14 / 23 / 7.
        assert_eq!(g.kda, "14 K // 23 D // 7 A");
        assert_eq!(g.kd, format!("{:.1}", 14.0 / 23.0));
        // Same derived strings, same formatting rules.
        assert_eq!(g.dda, m.damage_delta_label());
        assert_eq!(g.hs, m.hs_pct());
        assert_eq!(g.acs, m.acs());
        assert_eq!(g.rows, vec![m.clone()]);
    }

    #[test]
    fn a_busy_day_sums_and_means_its_matches() {
        let days = group_days(&rows());
        let g = days.iter().find(|d| d.key == "2026-11-11").unwrap();
        assert_eq!(g.count, 6);
        assert_eq!(g.wl, "1 W // 5 L");
        assert_eq!(g.kda, "90 K // 95 D // 40 A");
        assert_eq!(g.kd, "0.9");
        // Mean of the six per-match headshot rates, not a share of shots.
        assert_eq!(g.hs, "79%");
        assert_eq!(g.dda, "-1,822");
        assert_eq!(g.acs, "196");
    }

    #[test]
    fn draws_count_but_unknown_results_do_not_win_or_lose() {
        let base = rows().remove(0);
        let mystery = MatchRow {
            result: "mystery".to_string(),
            ..base.clone()
        };
        let draw = MatchRow {
            result: "draw".to_string(),
            ..base.clone()
        };
        let victory = MatchRow {
            result: "victory".to_string(),
            ..base
        };
        let days = group_days(&[victory, draw, mystery]);
        assert_eq!(days.len(), 1);
        let g = &days[0];
        assert_eq!(g.count, 3, "every match counts in the title");
        assert_eq!(g.wins, 1);
        assert_eq!(g.losses, 0, "draws and unknowns win and lose nothing");
        assert_eq!(g.wl, "1 W // 0 L");
    }

    #[test]
    fn a_day_without_numbers_keeps_dashes_not_zeroes() {
        let mut row = rows().remove(0);
        row.kills = None;
        row.deaths = None;
        row.assists = None;
        row.score = None;
        row.rounds_won = None;
        row.rounds_lost = None;
        row.headshots = None;
        row.damage_delta = None;
        let days = group_days(&[row]);
        let g = &days[0];
        assert_eq!(g.kda, "— K // — D // — A");
        assert_eq!(g.kd, "");
        assert_eq!(g.hs, "");
        assert_eq!(g.acs, "");
        assert_eq!(g.dda, "");
    }

    #[test]
    fn the_form_strip_is_the_last_eight_newest_first() {
        let r = rows();
        let form = form_strip(&r, NOW);
        assert_eq!(form.len(), 8);
        assert_eq!(form[0].age, "2d ago");
        assert_eq!(form[1].age, "3d ago");
        for cell in form.iter().skip(2) {
            assert_eq!(cell.age, "5d ago");
        }
        // Score and K/D reuse the row's own strings and colour rule.
        assert_eq!(form[0].score, r[0].rounds());
        assert_eq!(form[0].kd, r[0].kd_ratio());
        assert_eq!(form[0].won, r[0].won());
        assert!(!form[0].won, "the newest fixture row is a defeat");
    }

    fn brow(
        name: &str,
        matches: Option<i64>,
        win_pct: Option<f64>,
        kd: Option<f64>,
    ) -> BreakdownRow {
        BreakdownRow {
            name: name.to_string(),
            matches,
            win_pct,
            kd,
            ..BreakdownRow::default()
        }
    }

    /// The strip's season cell: TRN's three facts, and a bar share that is a
    /// proportion of the reported record rather than a re-derived percentage.
    #[test]
    fn the_season_cell_reports_record_kd_and_adr() {
        let season = crate::trn_profile::SeasonStats {
            wins: Some(8),
            losses: Some(12),
            kd: Some(0.91),
            dmg_per_round: Some(128.4),
            ..Default::default()
        };
        let cell = season_strip(&season);
        assert_eq!(cell.record, "8W - 12L (40%)");
        assert_eq!(cell.kd, "0.91 K/D");
        assert_eq!(cell.adr, "128 ADR");
        assert!((cell.win_share - 0.4).abs() < 0.001);
    }

    /// No record yet draws nothing and no bar fill: absence is never a zero
    /// record.
    #[test]
    fn the_season_cell_leaves_missing_facts_empty() {
        let cell = season_strip(&crate::trn_profile::SeasonStats::default());
        assert_eq!(cell.record, "");
        assert_eq!(cell.kd, "");
        assert_eq!(cell.adr, "");
        assert_eq!(cell.win_share, 0.0);
    }

    #[test]
    fn the_agent_strip_is_the_top_three_by_matches() {
        let rows = vec![
            brow("Sage", Some(9), Some(50.0), Some(1.1)),
            brow("Omen", Some(16), Some(43.8), Some(0.86)),
            brow("Jett", Some(3), Some(100.0), Some(2.0)),
            brow("Phoenix", None, None, None),
            brow("Reyna", Some(12), Some(25.0), Some(0.7)),
        ];
        let strip = top_agents(&rows);
        assert_eq!(
            strip.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["Omen", "Reyna", "Sage"]
        );
        assert_eq!(strip[0].record, "7W - 9L (44%)");
        assert_eq!(strip[0].kd, "0.86");
        assert_eq!(strip[1].record, "3W - 9L (25%)");
        assert!(
            strip.iter().all(|a| a.art_url.is_empty()),
            "art is the worker's job"
        );
    }

    #[test]
    fn an_agent_without_numbers_has_no_record() {
        let strip = top_agents(&[brow("Hush", Some(1), None, None)]);
        assert_eq!(strip.len(), 1);
        assert_eq!(strip[0].record, "");
        assert_eq!(strip[0].kd, "");
    }

    #[test]
    fn new_payload_keys_parse_into_row_fields() {
        let r = rows();
        // Match 0 (Bind): Sage's own portrait, TRS 279, a lost 1v1, 9th.
        assert_eq!(
            r[0].art_url,
            "https://titles.trackercdn.com/valorant-api/agents/\
             569fdd95-4d10-43ab-ca70-79becc718b46/displayicon.png"
        );
        assert_eq!(r[0].trs, Some(279));
        assert_eq!(r[0].trs_chip(), "TRS 279");
        assert_eq!(r[0].clutch.as_deref(), Some("1v1 Lost"));
        assert!(r[0].clutch_bad);
        assert_eq!(r[0].placement, Some(9));
        assert_eq!(r[0].map_label(), "Bind 9th");
        // Match 1 (Summit): a won 1v1 is not bad, 6th place, TRS 543.
        assert_eq!(r[1].art_url, r[0].art_url, "same Sage portrait");
        assert_eq!(r[1].clutch.as_deref(), Some("1v1 Clutch"));
        assert!(!r[1].clutch_bad);
        assert_eq!(r[1].trs_chip(), "TRS 543");
        assert_eq!(r[1].map_label(), "Summit 6th");
        // Match 2 (Lotus): the old shape — everything new is absent, and the
        // row reads exactly as before.
        assert_eq!(r[2].art_url, "");
        assert_eq!(r[2].trs, None);
        assert_eq!(r[2].trs_chip(), "");
        assert_eq!(r[2].clutch, None);
        assert!(!r[2].clutch_bad);
        assert_eq!(r[2].placement, None);
        assert_eq!(r[2].map_label(), "Lotus");
    }

    #[test]
    fn ordinals_keep_11_12_13_on_th() {
        assert_eq!(ordinal(1), "1st");
        assert_eq!(ordinal(2), "2nd");
        assert_eq!(ordinal(3), "3rd");
        assert_eq!(ordinal(4), "4th");
        assert_eq!(ordinal(9), "9th");
        assert_eq!(ordinal(11), "11th");
        assert_eq!(ordinal(12), "12th");
        assert_eq!(ordinal(13), "13th");
        assert_eq!(ordinal(21), "21st");
        assert_eq!(ordinal(22), "22nd");
        assert_eq!(ordinal(23), "23rd");
        assert_eq!(ordinal(111), "111th");
    }

    #[test]
    fn a_tag_without_a_name_is_no_chip() {
        let payload = serde_json::json!({"data": {"matches": [
            {"attributes": {"id": "m1"}, "metadata": {"result": "victory"},
             "segments": [{"type": "overview",
                "metadata": {"agentName": "Omen", "agentImageUrl": "  ",
                             "tags": [{"name": "   ", "tone": "Positive"},
                                      {"name": "3k", "tone": "Positive"}]},
                "stats": {"trnPerformanceScore": {"value": 300},
                          "placement": {"value": 1}}}]}
        ]}});
        let r = parse(&payload).expect("shape is valid");
        assert_eq!(r.len(), 1);
        // Only the first tag counts, even when it is blank: no fallback to
        // later tags, and a blank tone is not Negative.
        assert_eq!(r[0].clutch, None);
        assert!(!r[0].clutch_bad);
        assert_eq!(r[0].art_url, "", "a blank URL is not a portrait");
        assert_eq!(r[0].trs_chip(), "TRS 300");
        assert_eq!(r[0].map_label(), "1st");
    }
}
