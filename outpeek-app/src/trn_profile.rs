//! Pure parser for one Tracker Network statv2 profile → the strings the
//! Overview shows, plus the small on-disk cache that keeps the last profile in
//! front of the user when the network is gone.
//!
//! The parser keeps the honesty rule this app uses everywhere: **a missing
//! segment is `None`, never zero.** A real zero (0 aces, 0 flawless rounds) is
//! a value and renders as `0`; an absent stat renders as an empty string, which
//! the UI draws as an em dash.
//!
//! The fixture this is tested against is a live `lil ga7ed#zngr` payload (39
//! segments, trimmed to the fields the Overview renders), so every assertion
//! below is a number the live probe returned.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::trn_breakdown::{self, BreakdownRow};

/// Cache file under the app data directory. Holds the parsed profile, never the
/// ~1.8 MB response body: the body is a transport detail, the profile is data.
pub const CACHE_FILE: &str = "trn-cache.json";
/// A day. Refresh always tries the network first; this only bounds how old a
/// cached profile may be before the UI stops showing it as "the last read".
pub const CACHE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// Cache schema version; a future shape is ignored rather than misread.
pub const CACHE_VERSION: u32 = 1;

/// Playlists the Overview can show, as `(payload id, UI label)`. Exactly the
/// four season playlists the live payload carries (live probe 2026-11-16:
/// competitive 161 stats, deathmatch 158, skirmish2v2 158, swiftplay 158 — no
/// unrated segment). No dead options: the dropdown offers exactly these.
pub const PLAYLISTS: [(&str, &str); 4] = [
    ("competitive", "Competitive"),
    ("deathmatch", "Deathmatch"),
    ("swiftplay", "Swiftplay"),
    ("skirmish2v2", "2v2"),
];
/// The playlist [`parse`] reads: the competitive season the rank, peak, score
/// and breakdown tabs are built from.
pub const DEFAULT_PLAYLIST: &str = "competitive";

/// UI label for a playlist id, or `None` for an id this build does not offer.
pub fn playlist_label(id: &str) -> Option<&'static str> {
    PLAYLISTS
        .iter()
        .find(|(key, _)| *key == id)
        .map(|(_, label)| *label)
}

/// True for exactly the four ids in [`PLAYLISTS`].
pub fn is_known_playlist(id: &str) -> bool {
    playlist_label(id.trim()).is_some()
}

// ---------------------------------------------------------------------------
// Typed profile
// ---------------------------------------------------------------------------

/// A rank the payload reported, with the act it belongs to when TRN names one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankRef {
    /// Tier id from the icon URL (`.../tiersv2/20.png` → 20). `None` when the
    /// URL does not carry one — the crest then stays empty instead of guessing.
    pub tier: Option<i64>,
    /// TRN's own label ("Diamond 3"). Kept as reported, not re-derived.
    pub name: String,
    /// Act name for a peak row ("V26: ACT V"); empty for the current rank.
    pub act: String,
}

/// The competitive season's stats. Every field is optional for the same reason.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeasonStats {
    pub matches: Option<i64>,
    pub wins: Option<i64>,
    pub losses: Option<i64>,
    pub ties: Option<i64>,
    pub win_pct: Option<f64>,
    pub rounds: Option<i64>,
    pub rounds_win_pct: Option<f64>,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    pub assists: Option<i64>,
    pub kd: Option<f64>,
    pub dmg_per_round: Option<f64>,
    pub hs_pct: Option<f64>,
    pub headshots: Option<i64>,
    pub body: Option<i64>,
    pub legs: Option<i64>,
    pub flawless: Option<i64>,
    pub aces: Option<i64>,
    pub clutches: Option<i64>,
    pub most_kills: Option<i64>,
    pub first_bloods: Option<i64>,
    pub first_deaths: Option<i64>,
    pub kast: Option<f64>,
    pub acs: Option<f64>,
    pub dda_round: Option<f64>,
    /// Where this account sits among the players TRN tracks (0–100). The
    /// desktop client prints it under the headline numbers ("Bottom 10%",
    /// "Top 11%"); it is a rank among players, never a second value.
    pub win_pctile: Option<f64>,
    pub kd_pctile: Option<f64>,
    pub hs_pctile: Option<f64>,
    pub dpr_pctile: Option<f64>,
    pub rounds_win_pctile: Option<f64>,
    pub kast_pctile: Option<f64>,
    pub acs_pctile: Option<f64>,
    pub dda_pctile: Option<f64>,
}

/// "Top 10%" / "Bottom 15%" from a 0–100 percentile, the desktop client's own
/// words. At or above the middle it is "Top"; below, "Bottom". Absent or
/// non-finite stays empty: no percentile, no label.
pub(crate) fn percentile_label(value: Option<f64>) -> String {
    match value.filter(|v| v.is_finite()) {
        Some(p) if p >= 50.0 => format!("TOP {:.0}%", 100.0 - p),
        Some(p) => format!("BOTTOM {:.0}%", p),
        None => String::new(),
    }
}

/// One agent row. `uuid` is the segment key, which is what `assets` resolves a
/// portrait from; the name/role hints come from the live payload's segment
/// metadata (absent in the trimmed fixture) and are only a fallback.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentPick {
    pub uuid: String,
    pub name: String,
    pub role: String,
    pub matches: Option<i64>,
    pub win_pct: Option<f64>,
    pub kd: Option<f64>,
    pub dmg_per_round: Option<f64>,
    pub acs: Option<f64>,
    pub pick_rate: Option<f64>,
}

/// One account's competitive profile, as reported.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub handle: String,
    pub country: String,
    /// The account's playercard art (`platformInfo.avatarUrl`), fetched through
    /// the same allowlisted cache as crests and portraits. One URL, no data.
    #[serde(default)]
    pub avatar_url: String,
    pub season_id: String,
    /// Playlist this profile's season frame was read for. Breakdown rows and
    /// the matches list stay competitive — see [`parse_for_playlist`].
    #[serde(default)]
    pub playlist: String,
    /// Full season name from `metadata.seasons` ("Season 26 Act 6").
    pub season_name: String,
    /// Short act label ("E26: A6").
    pub season_short: String,
    /// Current rank for the default season.
    pub rank: Option<RankRef>,
    /// Best rank TRN reports, with the act it happened in.
    pub peak: Option<RankRef>,
    /// Peak rows from every `peak-rating` segment (today: the one act TRN
    /// publishes). Never padded with invented acts.
    pub acts: Vec<RankRef>,
    pub season: SeasonStats,
    pub top_agent: Option<AgentPick>,
    /// Every agent segment for this season, most played first. The Agents page
    /// lists these; `top_agent` is the pick-rate leader among the same rows, so
    /// the card and the list can never disagree.
    #[serde(default)]
    pub agents: Vec<BreakdownRow>,
    /// Every map segment for this season, most played first. Map art is
    /// resolved separately (`assets::map_icon_url`): TRN's own map URL is a
    /// 3840×2160 splash far past the art budget.
    #[serde(default)]
    pub maps: Vec<BreakdownRow>,
    pub score: Option<i64>,
}

impl Profile {
    /// The view the UI binds. Display strings only where a value exists; an
    /// absent value is `""` so the UI keeps its em dash.
    pub fn to_view(&self) -> ProfileView {
        let s = &self.season;
        let score = self.score;
        let score_badge = crate::badges::badge_key(
            "",
            score.map(|v| v.clamp(i32::MIN as i64, i32::MAX as i64) as i32),
        );
        let (acc_head, acc_body, acc_legs) = accuracy_shares(s.headshots, s.body, s.legs);
        let agent = self.top_agent.as_ref();
        let acts = self.act_rows();
        ProfileView {
            state: ProfileState::Ready,
            message: String::new(),
            handle: self.handle.clone(),
            season: self.season_name.clone(),
            season_short: self.season_short.clone(),
            playlist: self.playlist.clone(),
            matches: count_line(s.matches, "match", "matches"),
            win_pct: percent(s.win_pct),
            kd: decimal(s.kd, 2),
            hs_pct: percent(s.hs_pct),
            dmg_round: decimal(s.dmg_per_round, 1),
            wins: count(s.wins),
            losses: count(s.losses),
            kills: count(s.kills),
            deaths: count(s.deaths),
            assists: count(s.assists),
            headshots: count(s.headshots),
            flawless: count(s.flawless),
            clutches: count(s.clutches),
            best_kills: count(s.most_kills),
            first_blood_deaths: match (s.first_bloods, s.first_deaths) {
                (Some(fb), Some(fd)) => format!("{} / {}", with_commas(fb), with_commas(fd)),
                _ => String::new(),
            },
            aces: count(s.aces),
            acc_head,
            acc_body,
            acc_legs,
            acc_head_hits: count(s.headshots),
            acc_body_hits: count(s.body),
            acc_legs_hits: count(s.legs),
            agent_uuid: agent.map(|a| a.uuid.clone()).filter(|u| !u.is_empty()),
            agent_name: agent.map(|a| a.name.clone()).unwrap_or_default(),
            agent_role: agent.map(|a| a.role.clone()).unwrap_or_default(),
            agent_line: agent.map(agent_line).unwrap_or_default(),
            agent_win_pct: agent.map(|a| percent(a.win_pct)).unwrap_or_default(),
            agent_kd: agent.map(|a| decimal(a.kd, 2)).unwrap_or_default(),
            agent_adr: agent
                .map(|a| decimal(a.dmg_per_round, 1))
                .unwrap_or_default(),
            agent_acs: agent.map(|a| decimal(a.acs, 0)).unwrap_or_default(),
            act1_label: acts[0].0.clone(),
            act1_value: acts[0].1.clone(),
            act2_label: acts[1].0.clone(),
            act2_value: acts[1].1.clone(),
            act3_label: acts[2].0.clone(),
            act3_value: acts[2].1.clone(),
            score: score.map(|v| v.to_string()).unwrap_or_default(),
            score_badge: score_badge.to_string(),
            round_win_pct: percent(s.rounds_win_pct),
            kast: percent(s.kast),
            acs: decimal(s.acs, 0),
            dda_round: decimal(s.dda_round, 0),
            win_pctile: percentile_label(s.win_pctile),
            kd_pctile: percentile_label(s.kd_pctile),
            hs_pctile: percentile_label(s.hs_pctile),
            dpr_pctile: percentile_label(s.dpr_pctile),
            round_win_pctile: percentile_label(s.rounds_win_pctile),
            kast_pctile: percentile_label(s.kast_pctile),
            acs_pctile: percentile_label(s.acs_pctile),
            dda_pctile: percentile_label(s.dda_pctile),
            rank_tier: self.rank.as_ref().and_then(|r| r.tier),
            peak_tier: self.peak.as_ref().and_then(|r| r.tier),
        }
    }

    /// Three `(act label, rank label)` rows for the Previous Acts card: what TRN
    /// reported, then empty rows — the card keeps em dashes for acts nobody
    /// read. No row is ever a guess.
    fn act_rows(&self) -> [(String, String); 3] {
        let mut out = [
            (String::new(), String::new()),
            (String::new(), String::new()),
            (String::new(), String::new()),
        ];
        for (slot, act) in out.iter_mut().zip(self.acts.iter()) {
            slot.0 = act.act.clone();
            slot.1 = act.name.clone();
        }
        out
    }
}

/// One frame of profile data for the UI. `state` tells the page whether this is
/// a fresh read, the last cached copy, or an honest empty/error state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileState {
    Loading,
    Ready,
    Cached,
    Empty,
    Error,
}

impl ProfileState {
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileState::Loading => "loading",
            ProfileState::Ready => "ready",
            ProfileState::Cached => "cached",
            ProfileState::Empty => "empty",
            ProfileState::Error => "error",
        }
    }
}

impl Default for ProfileState {
    /// An empty view is the boot default: no profile has been read yet.
    fn default() -> Self {
        ProfileState::Empty
    }
}

/// Display strings for every bound slot. Empty == "not read".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProfileView {
    pub state: ProfileState,
    pub message: String,
    pub handle: String,
    pub season: String,
    pub season_short: String,
    pub playlist: String,
    pub matches: String,
    pub win_pct: String,
    pub kd: String,
    pub hs_pct: String,
    pub dmg_round: String,
    pub wins: String,
    pub losses: String,
    pub kills: String,
    pub deaths: String,
    pub assists: String,
    pub headshots: String,
    pub flawless: String,
    pub clutches: String,
    pub best_kills: String,
    pub first_blood_deaths: String,
    pub aces: String,
    pub acc_head: String,
    pub acc_body: String,
    pub acc_legs: String,
    pub acc_head_hits: String,
    pub acc_body_hits: String,
    pub acc_legs_hits: String,
    pub agent_uuid: Option<String>,
    pub agent_name: String,
    pub agent_role: String,
    pub agent_line: String,
    pub agent_win_pct: String,
    pub agent_kd: String,
    pub agent_adr: String,
    pub agent_acs: String,
    pub act1_label: String,
    pub act1_value: String,
    pub act2_label: String,
    pub act2_value: String,
    pub act3_label: String,
    pub act3_value: String,
    pub score: String,
    pub score_badge: String,
    pub round_win_pct: String,
    pub kast: String,
    pub acs: String,
    pub dda_round: String,
    pub win_pctile: String,
    pub kd_pctile: String,
    pub hs_pctile: String,
    pub dpr_pctile: String,
    pub round_win_pctile: String,
    pub kast_pctile: String,
    pub acs_pctile: String,
    pub dda_pctile: String,
    /// Tier ids the art pipeline resolves crests from.
    pub rank_tier: Option<i64>,
    pub peak_tier: Option<i64>,
}

impl ProfileView {
    /// A view with no data and a reason. Used before the first read, after a
    /// failed read with no cache, and when no account has ever been seen.
    pub fn placeholder(state: ProfileState, message: impl Into<String>) -> Self {
        Self {
            state,
            message: message.into(),
            ..Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse one `data` envelope from the profile route, competitive season.
pub fn parse(payload: &Value) -> Result<Profile, String> {
    parse_for_playlist(payload, DEFAULT_PLAYLIST)
}

/// Parse one `data` envelope for one playlist's season frame.
///
/// The switch moves the Overview season numbers only — headlines, strip,
/// highlights, accuracy, score, rank/peak cells. Agent/map rows, the top
/// agent and the matches list stay competitive: agent/map segments do carry a
/// `playlist` attribute in the live payload, but `trn_breakdown::parse` is
/// season-scoped only and this pass does not rewire it (nor the career-wide
/// matches endpoint). A playlist with no reported stats parses to honest
/// empties, never zeros. Unknown playlists fall back to competitive.
pub fn parse_for_playlist(payload: &Value, playlist: &str) -> Result<Profile, String> {
    let wanted = playlist.trim();
    let wanted = if is_known_playlist(wanted) {
        wanted
    } else {
        DEFAULT_PLAYLIST
    };
    let data = payload
        .get("data")
        .filter(|d| d.is_object())
        .ok_or_else(|| "TRN_NO_DATA".to_string())?;
    let segments = data
        .get("segments")
        .and_then(Value::as_array)
        .ok_or_else(|| "Unreadable TRN profile payload.".to_string())?;

    let handle = data
        .pointer("/platformInfo/platformUserHandle")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let avatar_url = data
        .pointer("/platformInfo/avatarUrl")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let country = data
        .pointer("/userInfo/countryCode")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    let default_season = data
        .pointer("/metadata/defaultSeason")
        .and_then(Value::as_str)
        .unwrap_or("");
    let season_segment = pick_season(segments, default_season, wanted);
    let season_id = season_segment
        .and_then(|s| s.pointer("/attributes/seasonId"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let (season_name, season_short) = season_labels(data, &season_id);
    let stats = season_segment
        .and_then(|s| s.get("stats"))
        .cloned()
        .unwrap_or(Value::Null);

    let rank = rank_stat(&stats, "rank");
    let peak = rank_stat(&stats, "peakRank");
    // Breakdowns stay on the competitive season (see the fn docs): the
    // season id below is the competitive segment's, not the picked one.
    let competitive_id = pick_season(segments, default_season, DEFAULT_PLAYLIST)
        .and_then(|s| s.pointer("/attributes/seasonId"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let (agents, maps) = trn_breakdown::parse(segments, competitive_id);
    let agent = top_agent(&agents);
    let mut acts = peak_rows(segments);
    if acts.is_empty() {
        if let Some(peak) = &peak {
            if !peak.name.is_empty() || peak.tier.is_some() {
                acts.push(peak.clone());
            }
        }
    }

    Ok(Profile {
        handle,
        avatar_url,
        country,
        season_id,
        playlist: wanted.to_string(),
        season_name,
        season_short,
        rank,
        peak,
        acts,
        season: season_stats(&stats),
        top_agent: agent,
        agents,
        maps,
        score: stat_i64(&stats, "trnPerformanceScore"),
    })
}

fn pick_season<'a>(
    segments: &'a [Value],
    default_season: &str,
    playlist: &str,
) -> Option<&'a Value> {
    let queued = |s: &&Value| {
        s.get("type").and_then(Value::as_str) == Some("season")
            && s.pointer("/attributes/playlist").and_then(Value::as_str) == Some(playlist)
    };
    if !default_season.is_empty() {
        if let Some(segment) = segments.iter().filter(queued).find(|s| {
            s.pointer("/attributes/seasonId").and_then(Value::as_str) == Some(default_season)
        }) {
            return Some(segment);
        }
    }
    segments.iter().filter(queued).next()
}

fn season_labels(data: &Value, season_id: &str) -> (String, String) {
    let Some(rows) = data.pointer("/metadata/seasons").and_then(Value::as_array) else {
        return (String::new(), String::new());
    };
    let Some(row) = rows.iter().find(|r| {
        r.get("id")
            .and_then(Value::as_str)
            .map(|id| id == season_id)
            .unwrap_or(false)
    }) else {
        return (String::new(), String::new());
    };
    let read = |key: &str| {
        row.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let name = read("name");
    let short = read("shortName");
    (name, short)
}

fn season_stats(stats: &Value) -> SeasonStats {
    SeasonStats {
        matches: stat_i64(stats, "matchesPlayed"),
        wins: stat_i64(stats, "matchesWon"),
        losses: stat_i64(stats, "matchesLost"),
        ties: stat_i64(stats, "matchesTied"),
        win_pct: stat_f64(stats, "matchesWinPct"),
        rounds: stat_i64(stats, "roundsPlayed"),
        rounds_win_pct: stat_f64(stats, "roundsWinPct"),
        kills: stat_i64(stats, "kills"),
        deaths: stat_i64(stats, "deaths"),
        assists: stat_i64(stats, "assists"),
        kd: stat_f64(stats, "kDRatio"),
        dmg_per_round: stat_f64(stats, "damagePerRound"),
        hs_pct: stat_f64(stats, "headshotsPercentage"),
        headshots: stat_i64(stats, "dealtHeadshots"),
        body: stat_i64(stats, "dealtBodyshots"),
        legs: stat_i64(stats, "dealtLegshots"),
        flawless: stat_i64(stats, "flawless"),
        aces: stat_i64(stats, "aces"),
        clutches: stat_i64(stats, "clutches"),
        most_kills: stat_i64(stats, "mostKillsInMatch"),
        first_bloods: stat_i64(stats, "firstBloods"),
        first_deaths: stat_i64(stats, "firstDeaths"),
        kast: stat_f64(stats, "kAST"),
        acs: stat_f64(stats, "scorePerRound"),
        dda_round: stat_f64(stats, "damageDeltaPerRound"),
        win_pctile: stat_pctile(stats, "matchesWinPct"),
        kd_pctile: stat_pctile(stats, "kDRatio"),
        hs_pctile: stat_pctile(stats, "headshotsPercentage"),
        dpr_pctile: stat_pctile(stats, "damagePerRound"),
        rounds_win_pctile: stat_pctile(stats, "roundsWinPct"),
        kast_pctile: stat_pctile(stats, "kAST"),
        acs_pctile: stat_pctile(stats, "scorePerRound"),
        dda_pctile: stat_pctile(stats, "damageDeltaPerRound"),
    }
}

/// The `percentile` sibling of a stat object. Missing or non-numeric is
/// `None`: some stats never carry one, and that is a state, not a zero.
fn stat_pctile(stats: &Value, key: &str) -> Option<f64> {
    stats.get(key)?.get("percentile")?.as_f64()
}

/// A rank stat object: `metadata.tierName` is TRN's label, and the tier id is
/// the trailing number in the icon URL (`.../tiersv2/20.png`). `value` is null
/// on every rank stat by design, so it is never read as a number.
fn rank_stat(stats: &Value, key: &str) -> Option<RankRef> {
    let stat = stats.get(key)?;
    let metadata = stat.get("metadata")?;
    let name = metadata
        .get("tierName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let icon = metadata
        .get("iconUrl")
        .and_then(Value::as_str)
        .unwrap_or("");
    let act = metadata
        .get("actName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let tier = tier_from_icon(icon);
    if name.is_empty() && tier.is_none() {
        return None;
    }
    Some(RankRef { tier, name, act })
}

/// Tier id from a tracker.gg crest URL. Pure and strict: only the
/// `.../tiersv2/<digits>.png` shape yields a number.
pub fn tier_from_icon(url: &str) -> Option<i64> {
    let rest = url.split("tiersv2/").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

fn peak_rows(segments: &[Value]) -> Vec<RankRef> {
    let mut rows: Vec<RankRef> = segments
        .iter()
        .filter(|s| s.get("type").and_then(Value::as_str) == Some("peak-rating"))
        .filter_map(|s| rank_stat(s.get("stats")?, "peakRating"))
        .collect();
    // One row per act: a payload that repeats the same act must not show it
    // twice in a three-column card.
    rows.dedup_by(|a, b| !a.act.is_empty() && a.act == b.act);
    rows
}

/// The pick-rate leader among the parsed agent rows: it is what the player
/// actually chose. Matches played breaks a tie, and the rows are already sorted
/// so the result is stable. `None` when no agent was reported.
fn top_agent(agents: &[BreakdownRow]) -> Option<AgentPick> {
    let best = agents.iter().reduce(|a, b| {
        let b_wins = match (a.pick_rate, b.pick_rate) {
            (Some(x), Some(y)) if x != y => y > x,
            _ => b.matches.unwrap_or(0) > a.matches.unwrap_or(0),
        };
        if b_wins {
            b
        } else {
            a
        }
    })?;
    Some(AgentPick {
        uuid: best.key.clone(),
        name: best.name.clone(),
        role: best.role.clone(),
        matches: best.matches,
        win_pct: best.win_pct,
        kd: best.kd,
        dmg_per_round: best.dmg_per_round,
        acs: best.acs,
        pick_rate: best.pick_rate,
    })
}

pub(crate) fn stat_f64(stats: &Value, key: &str) -> Option<f64> {
    stats.get(key)?.get("value")?.as_f64()
}

pub(crate) fn stat_i64(stats: &Value, key: &str) -> Option<i64> {
    let value = stats.get(key)?.get("value")?;
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|v| v.round() as i64))
}

// ---------------------------------------------------------------------------
// Display formatting (pure)
// ---------------------------------------------------------------------------

/// Integer with thousands separators, matching TRN's own `displayValue`.
fn with_commas(value: i64) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if negative {
        out.push('-');
    }
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn count(value: Option<i64>) -> String {
    value.map(with_commas).unwrap_or_default()
}

pub(crate) fn decimal(value: Option<f64>, places: usize) -> String {
    value
        .filter(|v| v.is_finite())
        .map(|v| format!("{v:.places$}"))
        .unwrap_or_default()
}

pub(crate) fn percent(value: Option<f64>) -> String {
    value
        .filter(|v| v.is_finite())
        .map(|v| format!("{v:.1}%"))
        .unwrap_or_default()
}

pub(crate) fn count_line(value: Option<i64>, one: &str, many: &str) -> String {
    value
        .map(|n| format!("{} {}", with_commas(n), if n == 1 { one } else { many }))
        .unwrap_or_default()
}

fn accuracy_shares(
    head: Option<i64>,
    body: Option<i64>,
    legs: Option<i64>,
) -> (String, String, String) {
    let (Some(head), Some(body), Some(legs)) = (head, body, legs) else {
        return (String::new(), String::new(), String::new());
    };
    let total = head + body + legs;
    if total <= 0 {
        return (String::new(), String::new(), String::new());
    }
    let share = |n: i64| format!("{:.1}%", n as f64 * 100.0 / total as f64);
    (share(head), share(body), share(legs))
}

fn agent_line(agent: &AgentPick) -> String {
    let matches = count_line(agent.matches, "match", "matches");
    match (matches.is_empty(), agent.pick_rate) {
        (false, Some(rate)) if rate.is_finite() => {
            format!("{matches} · {rate:.1}% pick rate")
        }
        (false, _) => matches,
        (true, Some(rate)) if rate.is_finite() => format!("{rate:.1}% pick rate"),
        (true, _) => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Disk cache (parsed profile only)
// ---------------------------------------------------------------------------

/// The cache file's shape. `version` gates a future schema; `saved_at` is epoch
/// ms and only bounds display age.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheFile {
    pub version: u32,
    pub saved_at: u64,
    pub handle: String,
    pub profile: Profile,
}

/// A cached profile may be shown while `now - saved_at <= ttl`. A timestamp in
/// the future (clock moved back) counts as just-saved, the same rule the art
/// cache uses.
pub fn cache_usable(saved_at: u64, now_ms: u64, ttl_ms: u64) -> bool {
    now_ms.saturating_sub(saved_at) <= ttl_ms
}

/// Coarse age for the line beside the numbers: "just now", "3 min ago",
/// "5 h ago", "2 days ago". Deliberately not a timestamp: the user needs to
/// know how stale the read is, not the wall clock of a previous session.
pub fn age_label(ms: u64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        0 => "just now".to_string(),
        1 => "1 min ago".to_string(),
        2..=59 => format!("{minutes} min ago"),
        _ => {
            let hours = minutes / 60;
            if hours < 48 {
                format!("{hours} h ago")
            } else {
                format!("{} days ago", hours / 24)
            }
        }
    }
}

/// `%APPDATA%\Outpeek\trn-cache.json`, when this host has an app data directory.
pub fn cache_path() -> Option<PathBuf> {
    crate::paths::data_file(CACHE_FILE)
}

/// Read the cache at `path`, refusing an old schema, an old timestamp, or a
/// file that does not parse. Any refusal is "no cache", not an error.
pub fn load_cache(path: &Path, now_ms: u64, ttl_ms: u64) -> Option<CacheFile> {
    let text = std::fs::read_to_string(path).ok()?;
    let cache: CacheFile = serde_json::from_str(&text).ok()?;
    if cache.version != CACHE_VERSION || !cache_usable(cache.saved_at, now_ms, ttl_ms) {
        return None;
    }
    if cache.profile.handle.is_empty() {
        return None;
    }
    Some(cache)
}

/// Persist the parsed profile (never the body), atomically.
pub fn save_cache(path: &Path, profile: &Profile, now_ms: u64) -> Result<(), String> {
    let cache = CacheFile {
        version: CACHE_VERSION,
        saved_at: now_ms,
        handle: profile.handle.clone(),
        profile: profile.clone(),
    };
    let mut json = serde_json::to_string(&cache)
        .map_err(|_| "Could not encode the profile cache.".to_string())?;
    json.push('\n');
    crate::account::write_atomic(path, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live `lil ga7ed#zngr` payload, trimmed to what the Overview renders.
    const FIXTURE: &str = include_str!("../tests/fixtures/trn-profile.json");

    fn profile() -> Profile {
        let payload: Value = serde_json::from_str(FIXTURE).expect("fixture parses");
        parse(&payload).expect("fixture profile parses")
    }

    fn close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("value present");
        assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
    }

    #[test]
    fn the_live_fixture_parses_to_the_numbers_the_probe_returned() {
        let profile = profile();
        assert_eq!(profile.handle, "lil ga7ed#zngr");
        assert_eq!(profile.country, "EG");
        assert_eq!(profile.season_id, "d816f426-48ea-f052-117f-9697a155b319");
        assert_eq!(profile.season_name, "Season 26 Act 6");

        let season = &profile.season;
        assert_eq!(season.matches, Some(85));
        assert_eq!(season.wins, Some(35));
        assert_eq!(season.losses, Some(48));
        close(season.win_pct, 41.176_470_588_235_29);
        close(season.kd, 0.811_292_719_167_904_9);
        close(season.hs_pct, 30.904_933_028_422_082);
        close(season.dmg_per_round, 117.671_857_619_577_31);
        assert_eq!(season.kills, Some(1_092));
        assert_eq!(season.deaths, Some(1_346));
        assert_eq!(season.assists, Some(611));
        assert_eq!(season.headshots, Some(946));
        assert_eq!(season.body, Some(1_962));
        assert_eq!(season.legs, Some(153));
        assert_eq!(season.flawless, Some(54));
        assert_eq!(season.clutches, Some(30));
        assert_eq!(season.aces, Some(0), "a real zero is a value");
        assert_eq!(season.most_kills, Some(27));
        assert_eq!(season.first_bloods, Some(123));
        assert_eq!(season.first_deaths, Some(162));

        let rank = profile.rank.as_ref().expect("rank segment");
        assert_eq!(rank.tier, Some(20));
        assert_eq!(rank.name, "Diamond 3");
        let peak = profile.peak.as_ref().expect("peak segment");
        assert_eq!(peak.tier, Some(23));
        assert_eq!(peak.name, "Ascendant 3");
        assert_eq!(peak.act, "V26: ACT V");
        assert_eq!(profile.score, Some(148));
    }

    #[test]
    fn percentiles_read_as_the_desktop_clients_top_bottom_words() {
        assert_eq!(percentile_label(Some(15.0)), "BOTTOM 15%");
        assert_eq!(percentile_label(Some(90.0)), "TOP 10%");
        assert_eq!(percentile_label(Some(50.0)), "TOP 50%");
        assert_eq!(percentile_label(None), "");
        assert_eq!(percentile_label(Some(f64::NAN)), "");
        // The live fixture carries the same percentiles the desktop client
        // prints under its headline numbers.
        let view = profile().to_view();
        assert_eq!(view.win_pctile, "BOTTOM 15%");
        assert_eq!(view.kd_pctile, "BOTTOM 11%");
        assert_eq!(view.hs_pctile, "TOP 10%");
        assert_eq!(view.dpr_pctile, "BOTTOM 10%");
        assert_eq!(view.round_win_pctile, "BOTTOM 27%");
        assert_eq!(view.kast_pctile, "BOTTOM 15%");
        assert_eq!(view.acs_pctile, "BOTTOM 9%");
        assert_eq!(view.dda_pctile, "BOTTOM 11%");
    }

    #[test]
    fn the_view_renders_the_same_read_without_inventing_zeros() {
        let view = profile().to_view();
        assert_eq!(view.state, ProfileState::Ready);
        assert_eq!(view.handle, "lil ga7ed#zngr");
        // The playercard art travels with the profile; the banner has one URL.
        let avatar = profile().avatar_url;
        assert!(avatar.ends_with("/displayicon.png"));
        assert!(avatar.contains("/playercards/"));
        assert_eq!(view.season, "Season 26 Act 6");
        assert_eq!(view.matches, "85 matches");
        assert_eq!(view.win_pct, "41.2%");
        assert_eq!(view.kd, "0.81");
        assert_eq!(view.hs_pct, "30.9%");
        assert_eq!(view.dmg_round, "117.7");
        assert_eq!(view.wins, "35");
        assert_eq!(view.losses, "48");
        assert_eq!(view.kills, "1,092");
        assert_eq!(view.deaths, "1,346");
        assert_eq!(view.assists, "611");
        assert_eq!(view.headshots, "946");
        assert_eq!(view.flawless, "54");
        assert_eq!(view.clutches, "30");
        assert_eq!(view.best_kills, "27");
        assert_eq!(view.first_blood_deaths, "123 / 162");
        // A real zero is shown as zero, never suppressed into an em dash.
        assert_eq!(view.aces, "0");
        // Accuracy: 946 / 3,061 = 30.9%, 1,962 = 64.1%, 153 = 5.0%.
        assert_eq!(view.acc_head, "30.9%");
        assert_eq!(view.acc_body, "64.1%");
        assert_eq!(view.acc_legs, "5.0%");
        assert_eq!(view.acc_head_hits, "946");
        assert_eq!(view.acc_body_hits, "1,962");
        assert_eq!(view.acc_legs_hits, "153");
        // The fixture has one peak act; the other two columns stay empty.
        assert_eq!(view.act1_label, "V26: ACT V");
        assert_eq!(view.act1_value, "Ascendant 3");
        assert_eq!(view.act2_label, "");
        assert_eq!(view.act3_value, "");
        // Score 148 falls in the D band (React's SCORE_TIERS).
        assert_eq!(view.score, "148");
        assert_eq!(view.score_badge, "D");
        assert_eq!(view.rank_tier, Some(20));
        assert_eq!(view.peak_tier, Some(23));
        // The four score inputs the Tracker Score card shows.
        assert_eq!(view.round_win_pct, "48.7%");
        assert_eq!(view.kast, "66.4%");
        assert_eq!(view.acs, "176");
        assert_eq!(view.dda_round, "-23");
    }

    #[test]
    fn the_top_agent_is_the_pick_rate_leader_and_carries_its_uuid() {
        let profile = profile();
        let agent = profile.top_agent.as_ref().expect("agent segment");
        assert_eq!(agent.uuid, "601dbbe7-43ce-be57-2a40-4abd24953621");
        assert_eq!(agent.matches, Some(48));
        close(agent.win_pct, 43.75);
        close(agent.kd, 0.863_697_705_802_969);
        close(agent.dmg_per_round, 122.629_301_868_239_92);
        close(agent.pick_rate, 56.5);
        // The live payload's own name/role travel as a fallback; the card's
        // portrait still resolves through `assets` from the uuid.
        assert_eq!(agent.name, "KAY/O");
        assert_eq!(agent.role, "Initiator");
        close(agent.acs, 183.387_413_962_635_2);

        let view = profile.to_view();
        assert_eq!(
            view.agent_uuid.as_deref(),
            Some("601dbbe7-43ce-be57-2a40-4abd24953621")
        );
        assert_eq!(view.agent_line, "48 matches · 56.5% pick rate");
        assert_eq!(view.agent_win_pct, "43.8%");
        assert_eq!(view.agent_kd, "0.86");
        assert_eq!(view.agent_adr, "122.6");
        assert_eq!(view.agent_acs, "183");
    }

    /// Two season segments, one season, two playlists — the shape the live
    /// payload carries (verified 2026-11-16: competitive/deathmatch/
    /// skirmish2v2/swiftplay, no unrated). Fixture only, no network.
    fn two_playlist_payload() -> Value {
        serde_json::json!({
            "data": {
                "platformInfo": { "platformUserHandle": "someone#tag" },
                "metadata": { "defaultSeason": "s1" },
                "segments": [
                    { "type": "season",
                      "attributes": { "seasonId": "s1", "playlist": "competitive" },
                      "stats": {
                          "matchesPlayed": { "value": 85 },
                          "kills": { "value": 1092 },
                          "rank": { "metadata": { "tierName": "Diamond 3" }, "value": null }
                      } },
                    { "type": "season",
                      "attributes": { "seasonId": "s1", "playlist": "deathmatch" },
                      "stats": {
                          "matchesPlayed": { "value": 12 },
                          "kills": { "value": 300 }
                      } },
                    { "type": "agent",
                      "attributes": { "key": "601dbbe7-43ce-be57-2a40-4abd24953621",
                                     "seasonId": "s1", "playlist": "competitive" },
                      "metadata": { "name": "KAY/O", "role": "Initiator" },
                      "stats": { "matchesPlayed": { "value": 48 } } }
                ]
            }
        })
    }

    #[test]
    fn parse_stays_competitive_and_names_its_playlist() {
        let payload = two_playlist_payload();
        let profile = parse(&payload).expect("competitive parses");
        assert_eq!(profile.playlist, "competitive");
        assert_eq!(profile.season.matches, Some(85));
        assert_eq!(profile.to_view().playlist, "competitive");
        // Delegation, not duplication: explicit competitive == default.
        let explicit = parse_for_playlist(&payload, "competitive").expect("explicit parses");
        assert_eq!(explicit.season, profile.season);
    }

    #[test]
    fn deathmatch_parses_different_numbers_with_honest_empties() {
        let payload = two_playlist_payload();
        let profile = parse_for_playlist(&payload, "deathmatch").expect("deathmatch parses");
        assert_eq!(profile.playlist, "deathmatch");
        assert_eq!(profile.season.matches, Some(12));
        assert_eq!(profile.season.kills, Some(300));
        // No rank on a deathmatch frame: em dash, never a zero or a copy.
        assert_eq!(profile.rank, None);
        assert_eq!(profile.season.wins, None);
        let view = profile.to_view();
        assert_eq!(view.playlist, "deathmatch");
        assert_eq!(view.matches, "12 matches");
        assert_eq!(view.wins, "");
        assert_eq!(view.kills, "300");
        // Breakdowns stay competitive this pass (see `parse_for_playlist`).
        assert_eq!(profile.agents.len(), 1);
        assert_eq!(
            profile.top_agent.as_ref().map(|a| a.name.as_str()),
            Some("KAY/O")
        );
    }

    #[test]
    fn an_unknown_playlist_falls_back_to_competitive() {
        let payload = two_playlist_payload();
        let profile = parse_for_playlist(&payload, "ranked").expect("fallback parses");
        assert_eq!(profile.playlist, "competitive");
        assert_eq!(profile.season.matches, Some(85));
    }

    #[test]
    fn a_playlist_with_no_segment_is_all_empties_never_zeros() {
        let payload = two_playlist_payload();
        let profile = parse_for_playlist(&payload, "skirmish2v2").expect("missing parses");
        assert_eq!(profile.playlist, "skirmish2v2");
        assert_eq!(profile.season.matches, None);
        let view = profile.to_view();
        assert_eq!(view.matches, "");
        assert_eq!(view.kills, "");
        assert_eq!(view.win_pct, "");
    }

    #[test]
    fn a_missing_segment_is_none_never_zero() {
        let payload = serde_json::json!({
            "data": {
                "platformInfo": { "platformUserHandle": "someone#tag" },
                "metadata": { "defaultSeason": "s1" },
                "segments": [
                    { "type": "season", "attributes": { "seasonId": "s1", "playlist": "competitive" },
                      "stats": { "matchesPlayed": { "value": 1 } } }
                ]
            }
        });
        let profile = parse(&payload).expect("minimal payload parses");
        assert_eq!(profile.season.matches, Some(1));
        assert_eq!(profile.season.wins, None);
        assert_eq!(profile.rank, None);
        assert_eq!(profile.peak, None);
        assert!(profile.acts.is_empty());
        assert_eq!(profile.top_agent, None);
        assert_eq!(profile.score, None);
        let view = profile.to_view();
        assert_eq!(view.wins, "");
        assert_eq!(view.aces, "");
        assert_eq!(view.score_badge, "");
        assert_eq!(view.agent_line, "");
        assert_eq!(view.acc_head, "");
        assert_eq!(view.act1_label, "");
        assert_eq!(view.rank_tier, None);
    }

    #[test]
    fn a_rank_with_no_icon_keeps_its_name_but_has_no_tier() {
        let payload = serde_json::json!({
            "data": {
                "platformInfo": { "platformUserHandle": "x#y" },
                "metadata": { "defaultSeason": "s1" },
                "segments": [
                    { "type": "season",
                      "attributes": { "seasonId": "s1", "playlist": "competitive" },
                      "stats": {
                          "rank": { "metadata": { "tierName": "Diamond 3" }, "value": null }
                      } }
                ]
            }
        });
        let profile = parse(&payload).unwrap();
        let rank = profile.rank.as_ref().expect("rank name present");
        assert_eq!(rank.tier, None, "no crest URL, no tier id");
        assert_eq!(rank.name, "Diamond 3");
        assert_eq!(profile.to_view().rank_tier, None);
    }

    #[test]
    fn an_error_envelope_has_no_profile() {
        assert!(parse(&serde_json::json!({ "errors": [{ "code": "NotFound" }] })).is_err());
        assert!(parse(&serde_json::json!({ "data": {} })).is_err());
        assert!(parse(&serde_json::json!(null)).is_err());
    }

    #[test]
    fn the_crest_tier_is_read_only_from_the_tiersv2_shape() {
        assert_eq!(
            tier_from_icon("https://trackercdn.com/cdn/tracker.gg/valorant/icons/tiersv2/20.png"),
            Some(20)
        );
        assert_eq!(
            tier_from_icon("https://trackercdn.com/cdn/tracker.gg/valorant/icons/tiersv2/23.png"),
            Some(23)
        );
        assert_eq!(
            tier_from_icon("https://trackercdn.com/x/tiersv2/.png"),
            None
        );
        assert_eq!(
            tier_from_icon("https://trackercdn.com/x/other/20.png"),
            None
        );
        assert_eq!(tier_from_icon(""), None);
    }

    #[test]
    fn the_age_line_reads_as_an_age_not_a_clock() {
        assert_eq!(age_label(0), "just now");
        assert_eq!(age_label(59_000), "just now");
        assert_eq!(age_label(60_000), "1 min ago");
        assert_eq!(age_label(3 * 60_000), "3 min ago");
        assert_eq!(age_label(59 * 60_000), "59 min ago");
        assert_eq!(age_label(5 * 3_600_000), "5 h ago");
        assert_eq!(age_label(3 * 24 * 3_600_000), "3 days ago");
    }

    #[test]
    fn the_cache_round_trips_the_parsed_profile_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE);
        let profile = profile();
        save_cache(&path, &profile, 1_000).unwrap();
        let loaded = load_cache(&path, 2_000, CACHE_TTL_MS).expect("fresh cache");
        assert_eq!(loaded.handle, "lil ga7ed#zngr");
        assert_eq!(loaded.profile, profile);
        // The body is not on disk: the file is the profile object, not the 1.8 MB
        // response (the whole fixture is the trimmed body; the cache must be far
        // smaller than what TRN sent).
        let bytes = std::fs::metadata(&path).unwrap().len();
        assert!(
            bytes < FIXTURE.len() as u64,
            "cache {bytes} bytes must be smaller than the payload"
        );
        // No temp litter beside it.
        let litter = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(litter, 0);
    }

    #[test]
    fn the_cache_expires_at_the_ttl_and_ignores_a_future_stamp() {
        assert!(cache_usable(1_000, 1_000, 100));
        assert!(cache_usable(1_000, 1_100, 100));
        assert!(!cache_usable(1_000, 1_101, 100));
        // A clock that moved backwards must not make a stamp valid forever.
        assert!(cache_usable(2_000, 1_000, 100));
        assert_eq!(CACHE_TTL_MS, 24 * 60 * 60 * 1000);
    }

    #[test]
    fn a_stale_or_foreign_cache_is_not_shown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE);
        let profile = profile();
        save_cache(&path, &profile, 1_000).unwrap();
        assert!(load_cache(&path, 1_000 + CACHE_TTL_MS + 1, CACHE_TTL_MS).is_none());
        // Wrong schema: ignored, never silently reinterpreted.
        let text = std::fs::read_to_string(&path).unwrap();
        let mut value: Value = serde_json::from_str(&text).unwrap();
        value["version"] = Value::from(99);
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
        assert!(load_cache(&path, 1_000, CACHE_TTL_MS).is_none());
        // Corrupt file: no cache.
        std::fs::write(&path, "{oops").unwrap();
        assert!(load_cache(&path, 1_000, CACHE_TTL_MS).is_none());
    }
}
