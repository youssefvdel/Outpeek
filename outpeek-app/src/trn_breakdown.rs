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
    pub win_pct: Option<f64>,
    pub kd: Option<f64>,
    pub acs: Option<f64>,
    pub dmg_per_round: Option<f64>,
    pub hs_pct: Option<f64>,
    /// Agents only: share of the accounted matches this agent was picked in.
    pub pick_rate: Option<f64>,
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
                win_pct: stat_f64(&stats, "matchesWinPct"),
                kd: stat_f64(&stats, "kDRatio"),
                acs: stat_f64(&stats, "scorePerRound"),
                dmg_per_round: stat_f64(&stats, "damagePerRound"),
                hs_pct: stat_f64(&stats, "headshotsPercentage"),
                pick_rate: stat_f64(&stats, "pickRate"),
            })
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

/// One card of the Maps page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapRow {
    pub name: String,
    pub matches: String,
    pub win: String,
    pub kd: String,
    pub acs: String,
    pub adr: String,
    pub hs: String,
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
            kd: self.kd_label(),
            acs: self.acs_label(),
            adr: self.adr_label(),
            hs: self.hs_label(),
            win_share: self.win_share(),
            art_url: String::new(),
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
