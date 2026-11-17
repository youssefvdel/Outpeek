//! Agent and map breakdowns, read from the **same** keyless Tracker Network
//! profile payload that fills the Overview.
//!
//! Two reasons this is not a second request:
//!
//! 1. the payload already carries every `agent` and `map` segment for the
//!    default competitive season, so a second read would ask the same server
//!    for the same bytes;
//! 2. the breakdown must describe the same season as the headline numbers. Two
//!    reads could land on either side of an act boundary and disagree.
//!
//! Honesty rules, the same ones `trn_profile` follows:
//!
//! * an absent stat is `None` → the UI draws an em dash, never a zero;
//! * the endpoint caps its lists (today: 5 agents, 7 maps). These rows are what
//!   Tracker Network reports, not a full roster, and the view says so.
//!
//! `image_url` is the art URL TRN sent with the row. It is **not** fetched
//! here: `assets` owns the transport and the on-disk cache, and replaces this
//! field with a cached URL before the frame reaches the UI.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::trn_profile::{count_line, decimal, percent, stat_f64, stat_i64};

/// One agent or map row. Every number is as reported; formatting happens in
/// the `*_label` methods so the view never rounds or re-derives a value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BreakdownRow {
    /// Agent uuid, or the map codename (`lotus`). Lowercased on parse.
    pub key: String,
    pub name: String,
    /// Agent role (`Sentinel`); empty for maps.
    pub role: String,
    /// TRN's art URL for the row (portrait or map splash); empty when absent.
    pub image_url: String,
    pub matches: Option<i64>,
    /// Wins / losses as reported (`matchesWon`, `matchesLost`).
    pub wins: Option<i64>,
    pub losses: Option<i64>,
    pub win_pct: Option<f64>,
    pub kd: Option<f64>,
    pub acs: Option<f64>,
    pub dmg_per_round: Option<f64>,
    pub hs_pct: Option<f64>,
    /// Signed per-round damage delta (`damageDeltaPerRound`, rounded) — the
    /// website's DDΔ column. The match-total `damageDelta` is thousands deep
    /// and never what that column prints.
    pub dmg_delta: Option<i64>,
    /// Agents only: share of the accounted matches this agent was picked in.
    pub pick_rate: Option<f64>,
    /// Maps only: the two most-played agents on this map. Empty for agents.
    pub top_agents: Vec<MapAgent>,
}

impl BreakdownRow {
    pub fn matches_label(&self) -> String {
        count_line(self.matches, "match", "matches")
    }

    pub fn win_label(&self) -> String {
        percent(self.win_pct)
    }

    /// 0–1 for the win bar, clamped. An absent percentage is 0 width; the text
    /// beside it already reads as "not reported".
    pub fn win_share(&self) -> f32 {
        self.win_pct
            .filter(|v| v.is_finite())
            .map(|v| (v / 100.0).clamp(0.0, 1.0) as f32)
            .unwrap_or(0.0)
    }

    pub fn kd_label(&self) -> String {
        decimal(self.kd, 2)
    }

    pub fn acs_label(&self) -> String {
        decimal(self.acs, 0)
    }

    pub fn adr_label(&self) -> String {
        decimal(self.dmg_per_round, 1)
    }

    pub fn hs_label(&self) -> String {
        percent(self.hs_pct)
    }

    pub fn pick_label(&self) -> String {
        percent(self.pick_rate)
    }
}

/// `metadata.name`, or the codename with its first letter capitalised when TRN
/// omits the name. A uuid-shaped key has no readable fallback: the row is then
/// dropped rather than shown as a hex string.
fn display_name(metadata: &Value, key: &str) -> String {
    let name = metadata
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !name.is_empty() {
        return name.to_string();
    }
    if key.contains('-') {
        return String::new();
    }
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// One segment type (`agent` | `map`) for the current season, sorted for the
/// view. A segment naming another season is skipped: the page describes the
/// season the headline numbers belong to.
fn rows(segments: &[Value], kind: &str, season_id: &str) -> Vec<BreakdownRow> {
    segments
        .iter()
        .filter(|s| s.get("type").and_then(Value::as_str) == Some(kind))
        .filter(|s| {
            if season_id.is_empty() {
                return true;
            }
            s.pointer("/attributes/seasonId")
                .and_then(Value::as_str)
                .map(|id| id == season_id)
                .unwrap_or(true)
        })
        .filter_map(|s| {
            let key = s
                .pointer("/attributes/key")
                .and_then(Value::as_str)
                .map(|k| k.trim().to_lowercase())
                .filter(|k| !k.is_empty())?;
            let metadata = s.get("metadata").cloned().unwrap_or(Value::Null);
            let name = display_name(&metadata, &key);
            if name.is_empty() {
                return None;
            }
            let stats = s.get("stats").cloned().unwrap_or(Value::Null);
            Some(BreakdownRow {
                key,
                name,
                role: metadata
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                image_url: metadata
                    .get("imageUrl")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                matches: stat_i64(&stats, "matchesPlayed"),
                wins: stat_i64(&stats, "matchesWon"),
                losses: stat_i64(&stats, "matchesLost"),
                win_pct: stat_f64(&stats, "matchesWinPct"),
                kd: stat_f64(&stats, "kDRatio"),
                acs: stat_f64(&stats, "scorePerRound"),
                dmg_per_round: stat_f64(&stats, "damagePerRound"),
                hs_pct: stat_f64(&stats, "headshotsPercentage"),
                dmg_delta: stat_f64(&stats, "damageDeltaPerRound").map(|v| v.round() as i64),
                pick_rate: stat_f64(&stats, "pickRate"),
                top_agents: Vec::new(),
            })
        })
        .collect()
}

/// Lowercased alphanumeric only, so a payload codename (`kayo`) meets the
/// display name the agent row reports (`KAY/O`).
fn normalize_agent_key(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// `mapKey` → the two most-played agents there, most played first. TRN ships
/// these as standalone `map-agent` segments (one per map × agent, keyed
/// `{mapKey}_{agentUuid}`), so they are grouped here instead of being read
/// off the map segment.
///
/// A `map-agent` row carries no name or portrait of its own (`metadata` is
/// empty); both come from the already-parsed agent rows, matched by codename.
/// An agent the payload never names keeps its codename title-cased and no art
/// rather than being dropped or guessed.
fn top_agents(
    segments: &[Value],
    season_id: &str,
    agents: &HashMap<String, (String, String)>,
) -> HashMap<String, Vec<MapAgent>> {
    let mut by_map: HashMap<String, Vec<(i64, MapAgent)>> = HashMap::new();
    for s in segments {
        if s.get("type").and_then(Value::as_str) != Some("map-agent") {
            continue;
        }
        if !season_id.is_empty()
            && s.pointer("/attributes/seasonId")
                .and_then(Value::as_str)
                .map(|id| id != season_id)
                .unwrap_or(false)
        {
            continue;
        }
        let Some(map_key) = s
            .pointer("/attributes/mapKey")
            .and_then(Value::as_str)
            .map(|k| k.trim().to_lowercase())
            .filter(|k| !k.is_empty())
        else {
            continue;
        };
        let agent_key = s
            .pointer("/attributes/agentKey")
            .and_then(Value::as_str)
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());
        let Some(agent_key) = agent_key else {
            continue;
        };
        let stats = s.get("stats").cloned().unwrap_or(Value::Null);
        let played = stat_i64(&stats, "matchesPlayed").unwrap_or(0);
        // A map nobody played this agent on has no top agent.
        if played <= 0 {
            continue;
        }
        let win_pct = stat_f64(&stats, "matchesWinPct");
        // Codenames meet the agent rows stripped of punctuation (`KAY/O` →
        // `kayo`); a uuid-shaped key meets them verbatim instead.
        let (name, art_url) = agents
            .get(&normalize_agent_key(&agent_key))
            .or_else(|| agents.get(&agent_key.trim().to_lowercase()))
            .cloned()
            .unwrap_or_else(|| {
                let mut chars = agent_key.chars();
                let fallback = match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                };
                (fallback, String::new())
            });
        by_map.entry(map_key).or_default().push((
            played,
            MapAgent {
                name,
                // Whole percent like the website (`83%`, not `83.3%`) — the
                // map's own Win % keeps the decimal, the portraits do not.
                win: win_pct
                    .filter(|v| v.is_finite())
                    .map(|v| format!("{v:.0}%"))
                    .unwrap_or_default(),
                win_share: win_pct
                    .filter(|v| v.is_finite())
                    .map(|p| (p / 100.0) as f32)
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0),
                art_url,
            },
        ));
    }
    by_map
        .into_iter()
        .map(|(k, mut v)| {
            v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
            (k, v.into_iter().take(2).map(|(_, a)| a).collect())
        })
        .collect()
}

/// Most-played first; pick rate breaks a tie for agents, then the name keeps
/// the order stable across payloads. `(agents, maps)`.
pub fn parse(segments: &[Value], season_id: &str) -> (Vec<BreakdownRow>, Vec<BreakdownRow>) {
    let mut agents = rows(segments, "agent", season_id);
    let mut maps = rows(segments, "map", season_id);
    let by_play = |a: &BreakdownRow, b: &BreakdownRow| {
        b.matches
            .cmp(&a.matches)
            .then_with(|| match (a.pick_rate, b.pick_rate) {
                (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal),
                _ => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.name.cmp(&b.name))
    };
    // `map-agent` segments are keyed by map, not nested under it: attach
    // them before sorting so each map row carries its own two portraits.
    // Names and art come from the agent rows (a `map-agent` row carries
    // neither), matched by codename with the uuid as a fallback.
    let mut lookup: HashMap<String, (String, String)> = HashMap::new();
    for a in agents.iter() {
        let value = (a.name.clone(), a.image_url.clone());
        lookup
            .entry(normalize_agent_key(&a.name))
            .or_insert_with(|| value.clone());
        if !a.key.is_empty() {
            lookup.entry(a.key.clone()).or_insert(value);
        }
    }
    let top = top_agents(segments, season_id, &lookup);
    for row in maps.iter_mut() {
        if let Some(list) = top.get(&row.key) {
            row.top_agents = list.clone();
        }
    }
    agents.sort_by(by_play);
    maps.sort_by(by_play);
    (agents, maps)
}

/// One row of the Agents page. Every cell is already a display string; the
/// view never rounds a number itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentRow {
    pub name: String,
    pub role: String,
    pub matches: String,
    pub win: String,
    pub kd: String,
    /// True when the reported K/D is under 1.0.
    pub kd_low: bool,
    pub acs: String,
    pub adr: String,
    pub hs: String,
    pub pick: String,
    /// 0–1 for the win bar; 0 when no percentage was reported.
    pub win_share: f32,
    /// Cached portrait URL, filled by the worker from `assets`. Empty = no art
    /// on disk and the view draws its fallback glyph.
    pub art_url: String,
}

/// One agent in a map row's Top Agents cell: the portrait, its win share and
/// the same percentage pre-formatted. The payload's `map-agent` segments
/// carry the numbers; the name and portrait resolve against the agent rows.
/// A map with none keeps an empty vector and draws an empty cell.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MapAgent {
    pub name: String,
    pub win: String,
    pub win_share: f32,
    pub art_url: String,
}

/// One row of the Maps table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapRow {
    pub name: String,
    pub matches: String,
    pub win: String,
    pub wins: String,
    pub losses: String,
    pub kd: String,
    /// True when the reported K/D is under 1.0 — the muted rose tone.
    pub kd_low: bool,
    pub acs: String,
    pub adr: String,
    pub hs: String,
    /// Signed `-21` / `+12`, the table's last numeric column.
    pub dd: String,
    /// True when the reported per-round delta is negative.
    pub dd_negative: bool,
    /// The map's two most-played agents, most played first.
    pub agents: Vec<MapAgent>,
    pub win_share: f32,
    /// Cached map art URL (valorant-api `listViewIcon`), empty on a miss.
    pub art_url: String,
}

impl BreakdownRow {
    fn agent_row(&self) -> AgentRow {
        AgentRow {
            name: self.name.clone(),
            role: self.role.clone(),
            matches: self.matches_label(),
            win: self.win_label(),
            kd: self.kd_label(),
            kd_low: self.kd.map(|v| v < 1.0).unwrap_or(false),
            acs: self.acs_label(),
            adr: self.adr_label(),
            hs: self.hs_label(),
            pick: self.pick_label(),
            win_share: self.win_share(),
            art_url: String::new(),
        }
    }

    fn map_row(&self) -> MapRow {
        MapRow {
            name: self.name.clone(),
            matches: self.matches_label(),
            win: self.win_label(),
            wins: self.wins.map(|v| v.to_string()).unwrap_or_default(),
            losses: self.losses.map(|v| v.to_string()).unwrap_or_default(),
            kd: self.kd_label(),
            kd_low: self.kd.map(|v| v < 1.0).unwrap_or(false),
            acs: self.acs_label(),
            adr: self.adr_label(),
            hs: self.hs_label(),
            dd: self.dd_label(),
            dd_negative: self.dmg_delta.map(|v| v < 0).unwrap_or(false),
            agents: self.top_agents.clone(),
            win_share: self.win_share(),
            art_url: String::new(),
        }
    }

    /// `+12` / `-21` — the sign is the value's, never the layout's.
    fn dd_label(&self) -> String {
        match self.dmg_delta {
            Some(v) if v > 0 => format!("+{v}"),
            Some(v) => v.to_string(),
            None => String::new(),
        }
    }
}

/// Pre-formatted rows in the order `parse` produced (most played first).
pub fn agent_rows(rows: &[BreakdownRow]) -> Vec<AgentRow> {
    rows.iter().map(BreakdownRow::agent_row).collect()
}

pub fn map_rows(rows: &[BreakdownRow]) -> Vec<MapRow> {
    rows.iter().map(BreakdownRow::map_row).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/trn-profile.json");

    fn segments() -> Vec<Value> {
        let payload: Value = serde_json::from_str(FIXTURE).expect("fixture parses");
        payload["data"]["segments"]
            .as_array()
            .expect("segments")
            .clone()
    }

    fn parsed() -> (Vec<BreakdownRow>, Vec<BreakdownRow>) {
        parse(&segments(), "d816f426-48ea-f052-117f-9697a155b319")
    }

    #[test]
    fn the_live_fixture_yields_every_reported_agent_and_map() {
        let (agents, maps) = parsed();
        assert_eq!(agents.len(), 5, "TRN reports five agent rows here");
        assert_eq!(maps.len(), 7, "and seven map rows");

        // Most played first, ties by pick rate then name.
        assert_eq!(
            agents.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["KAY/O", "Sage", "Phoenix", "Hush", "Vyse"]
        );
        assert_eq!(
            maps.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["Summit", "Sunset", "Lotus", "Abyss", "Bind", "Corrode", "Haven"]
        );

        let kayo = &agents[0];
        assert_eq!(kayo.key, "601dbbe7-43ce-be57-2a40-4abd24953621");
        assert_eq!(kayo.role, "Initiator");
        assert_eq!(kayo.matches, Some(48));
        assert_eq!(
            kayo.image_url,
            "https://titles.trackercdn.com/valorant-api/agents/\
             601dbbe7-43ce-be57-2a40-4abd24953621/displayicon.png"
        );
        let lotus = maps.iter().find(|r| r.key == "lotus").expect("lotus");
        assert_eq!(lotus.name, "Lotus");
        assert_eq!(lotus.matches, Some(13));
        assert!(lotus.role.is_empty(), "a map has no role");
    }

    #[test]
    fn rows_are_pre_formatted_and_an_absent_stat_stays_empty() {
        let (agents, maps) = parsed();
        let kayo = &agents[0];
        assert_eq!(kayo.matches_label(), "48 matches");
        assert_eq!(kayo.win_label(), "43.8%");
        assert_eq!(kayo.kd_label(), "0.86");
        assert_eq!(kayo.acs_label(), "183");
        assert_eq!(kayo.adr_label(), "122.6");
        assert_eq!(kayo.hs_label(), "33.4%");
        assert_eq!(kayo.pick_label(), "56.5%");
        assert!((kayo.win_share() - 0.4375).abs() < 1e-6);

        // Maps publish no pick rate: the cell is empty, not "0.0%".
        let summit = &maps[0];
        assert_eq!(summit.pick_rate, None);
        assert_eq!(summit.pick_label(), "");

        let mut missing = kayo.clone();
        missing.win_pct = None;
        missing.kd = None;
        missing.matches = None;
        assert_eq!(missing.win_label(), "");
        assert_eq!(missing.kd_label(), "");
        assert_eq!(missing.matches_label(), "");
        assert_eq!(missing.win_share(), 0.0);
    }

    #[test]
    fn another_seasons_rows_are_not_shown_under_this_seasons_numbers() {
        let segments = serde_json::json!([
            { "type": "agent",
              "attributes": { "key": "a-uuid", "seasonId": "old" },
              "metadata": { "name": "Old Agent" },
              "stats": { "matchesPlayed": { "value": 99 } } },
            { "type": "agent",
              "attributes": { "key": "b-uuid", "seasonId": "new" },
              "metadata": { "name": "Current Agent" },
              "stats": { "matchesPlayed": { "value": 2 } } }
        ]);
        let (agents, maps) = parse(segments.as_array().unwrap(), "new");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "Current Agent");
        assert!(maps.is_empty());
        // With no season named by the payload every row is kept as reported.
        let (all, _) = parse(segments.as_array().unwrap(), "");
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn a_codename_names_itself_and_a_uuid_never_does() {
        let segments = serde_json::json!([
            { "type": "map", "attributes": { "key": "lotus" }, "stats": {} },
            { "type": "agent", "attributes": { "key": "569fdd95-4d10-43ab-ca70-79becc718b46" }, "stats": {} },
            { "type": "agent", "attributes": { "key": "" }, "metadata": { "name": "No Key" }, "stats": {} }
        ]);
        let (agents, maps) = parse(segments.as_array().unwrap(), "");
        assert_eq!(maps.len(), 1);
        assert_eq!(
            maps[0].name, "Lotus",
            "the codename is data, its title is formatting"
        );
        assert!(agents.is_empty(), "a uuid has no readable fallback name");
    }

    #[test]
    fn an_empty_segment_list_is_empty_not_an_error() {
        let (agents, maps) = parse(&[], "s1");
        assert!(agents.is_empty());
        assert!(maps.is_empty());
    }

    /// Wins, losses and the signed per-round delta come from the map segment's
    /// own stats; an absent value stays empty, never a zero.
    #[test]
    fn wins_losses_and_dd_parse_from_stats_with_a_signed_delta() {
        let segments = serde_json::json!([
            { "type": "map",
              "attributes": { "key": "lotus", "seasonId": "s1" },
              "metadata": { "name": "Lotus" },
              "stats": {
                "matchesWon": { "value": 9 },
                "matchesLost": { "value": 3 },
                "damageDeltaPerRound": { "value": 12.4 } } },
            { "type": "map",
              "attributes": { "key": "bind", "seasonId": "s1" },
              "metadata": { "name": "Bind" },
              "stats": {
                "matchesWon": { "value": 2 },
                "matchesLost": { "value": 5 },
                "damageDeltaPerRound": { "value": -21.2 } } },
            { "type": "map",
              "attributes": { "key": "haven", "seasonId": "s1" },
              "metadata": { "name": "Haven" },
              "stats": {} }
        ]);
        let (_, maps) = parse(segments.as_array().unwrap(), "s1");
        assert_eq!(maps.len(), 3);
        let view = map_rows(&maps);
        let lotus = view.iter().find(|r| r.name == "Lotus").expect("lotus");
        assert_eq!(lotus.wins, "9");
        assert_eq!(lotus.losses, "3");
        assert_eq!(lotus.dd, "+12", "a positive delta carries its sign");
        let bind = view.iter().find(|r| r.name == "Bind").expect("bind");
        assert_eq!(bind.wins, "2");
        assert_eq!(bind.losses, "5");
        assert_eq!(bind.dd, "-21");
        let haven = view.iter().find(|r| r.name == "Haven").expect("haven");
        assert_eq!(haven.wins, "");
        assert_eq!(haven.losses, "");
        assert_eq!(haven.dd, "", "an absent delta is empty, not a zero");
    }

    /// Top agents attach to the right map keyed by `mapKey`, capped at the
    /// two most played; names and art resolve against the agent rows.
    #[test]
    fn top_agents_attach_to_the_right_map_capped_at_two() {
        let agent = |uuid: &str, name: &str| {
            serde_json::json!({
                "type": "agent",
                "attributes": { "key": uuid, "seasonId": "s1" },
                "metadata": { "name": name, "imageUrl": format!("https://art/{uuid}.png") },
                "stats": { "matchesPlayed": { "value": 10 } }
            })
        };
        let pick = |map: &str, key: &str, played: i64, win: f64| {
            serde_json::json!({
                "type": "map-agent",
                "attributes": {
                    "key": format!("{map}_{key}"),
                    "seasonId": "s1",
                    "mapKey": map,
                    "agentKey": key
                },
                "stats": {
                    "matchesPlayed": { "value": played },
                    "matchesWinPct": { "value": win }
                }
            })
        };
        let uuid_k = "601dbbe7-43ce-be57-2a40-4abd24953621";
        let uuid_s = "569fdd95-4d10-43ab-ca70-79becc718b46";
        let uuid_p = "eb93336a-449b-9c1b-0a54-a891f7921d69";
        let segments = serde_json::json!([
            agent(uuid_k, "KAY/O"),
            agent(uuid_s, "Sage"),
            agent(uuid_p, "Phoenix"),
            { "type": "map",
              "attributes": { "key": "lotus", "seasonId": "s1" },
              "metadata": { "name": "Lotus" },
              "stats": { "matchesPlayed": { "value": 13 } } },
            { "type": "map",
              "attributes": { "key": "bind", "seasonId": "s1" },
              "metadata": { "name": "Bind" },
              "stats": { "matchesPlayed": { "value": 5 } } },
            pick("lotus", "kayo", 6, 83.33),
            pick("lotus", uuid_s, 4, 50.0),
            pick("lotus", uuid_p, 3, 66.67),
            pick("bind", "kayo", 5, 40.0)
        ]);
        let (_, maps) = parse(segments.as_array().unwrap(), "s1");
        let lotus = maps.iter().find(|r| r.key == "lotus").expect("lotus");
        assert_eq!(
            lotus
                .top_agents
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            ["KAY/O", "Sage"],
            "most played first, capped at two"
        );
        assert_eq!(lotus.top_agents[0].win, "83%", "whole percent, no decimal");
        assert_eq!(
            lotus.top_agents[0].art_url,
            format!("https://art/{uuid_k}.png"),
            "portraits resolve against the agent rows"
        );
        let bind = maps.iter().find(|r| r.key == "bind").expect("bind");
        assert_eq!(bind.top_agents.len(), 1);
        assert_eq!(bind.top_agents[0].name, "KAY/O");
        // A zero-play row is not a top agent.
        let segments = serde_json::json!([
            agent(uuid_k, "KAY/O"),
            { "type": "map",
              "attributes": { "key": "abyss", "seasonId": "s1" },
              "metadata": { "name": "Abyss" },
              "stats": { "matchesPlayed": { "value": 7 } } },
            pick("abyss", "kayo", 0, 0.0)
        ]);
        let (_, maps) = parse(segments.as_array().unwrap(), "s1");
        let abyss = maps.iter().find(|r| r.key == "abyss").expect("abyss");
        assert!(abyss.top_agents.is_empty());
    }

    /// A map with no `map-agent` segments keeps an empty agents vec.
    #[test]
    fn a_map_with_no_top_agent_segments_keeps_an_empty_agents_vec() {
        let segments = serde_json::json!([
            { "type": "map",
              "attributes": { "key": "haven", "seasonId": "s1" },
              "metadata": { "name": "Haven" },
              "stats": { "matchesPlayed": { "value": 11 } } }
        ]);
        let (_, maps) = parse(segments.as_array().unwrap(), "s1");
        assert_eq!(maps.len(), 1);
        assert!(maps[0].top_agents.is_empty());
        let view = map_rows(&maps);
        assert!(
            view[0].agents.is_empty(),
            "the view row draws an empty cell"
        );
    }

    #[test]
    fn the_view_rows_carry_the_same_numbers_as_the_cards() {
        let (agents, maps) = parsed();
        let rows = agent_rows(&agents);
        assert_eq!(rows.len(), 5);
        let kayo = &rows[0];
        assert_eq!(kayo.name, "KAY/O");
        assert_eq!(kayo.role, "Initiator");
        assert_eq!(kayo.matches, "48 matches");
        assert_eq!(kayo.win, "43.8%");
        assert_eq!(kayo.kd, "0.86");
        assert_eq!(kayo.acs, "183");
        assert_eq!(kayo.adr, "122.6");
        assert_eq!(kayo.hs, "33.4%");
        assert_eq!(kayo.pick, "56.5%");
        assert!(
            kayo.art_url.is_empty(),
            "art is the worker's job, not the parser's"
        );

        let cards = map_rows(&maps);
        assert_eq!(cards[0].name, "Summit");
        assert_eq!(cards[0].win, "28.6%");
        assert!(cards[0].art_url.is_empty());
        // The single-match agent still shows its real 0.0%: a zero is a value.
        let hush = rows.iter().find(|r| r.name == "Hush").expect("hush");
        assert_eq!(hush.win, "0.0%");
        assert_eq!(hush.win_share, 0.0);
    }
}
