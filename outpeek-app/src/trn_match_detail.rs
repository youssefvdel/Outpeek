//! One match's scoreboard, TRN-website shaped.
//!
//! Route: `GET /api/v2/valorant/standard/matches/{matchId}?source=overwolf-2`
//! (same Overwolf UA transport as the profile). The live body is ~1.5 MB for a
//! 30-round game — almost all of it per-round damage/kill/loadout segments this
//! view never reads. Only `player-summary` x10, `team-summary` x2 and
//! `round-summary` xN are parsed; everything else is ignored. The trimmed test
//! fixture keeps exactly that.
//!
//! Cache: memory-only. A body that large is never written to disk; every row
//! click refetches (15 s timeout, same error strings as the profile path).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One scoreboard row, pre-formatted. Empty string = not reported = em dash in
/// the view, never a zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetailPlayer {
    pub name: String,
    pub agent: String,
    /// Catalogue key for the portrait (the `agentKey` uuid). Empty = draw glyph.
    pub agent_id: String,
    pub art_url: String,
    pub team: String,
    pub rank: String,
    pub acs: String,
    pub kda: String,
    pub kd: String,
    pub hs: String,
    pub kast: String,
    /// Signed damage delta per round (`-36` / `+12`), empty when absent.
    pub delta: String,
}

/// One round cell: the winner colours it, the result names it for screen
/// readers and tooltips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetailRound {
    pub n: u32,
    /// `Blue` | `Red` (as sent).
    pub winner: String,
    /// `Elimination` | `Defuse` | `Detonate` … (as sent).
    pub result: String,
}

/// The whole scoreboard, already flattened. `blue_score`/`red_score` are
/// counted from the round cells, so the header and the strip can never
/// disagree with each other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MatchDetail {
    pub id: String,
    pub map: String,
    pub mode: String,
    pub date: String,
    pub duration: String,
    pub playlist: String,
    pub blue_score: u32,
    pub red_score: u32,
    pub blue_won: bool,
    pub red_won: bool,
    pub blue: Vec<DetailPlayer>,
    pub red: Vec<DetailPlayer>,
    pub rounds: Vec<DetailRound>,
}

impl MatchDetail {
    /// `Blue 14 – 16 Red` for the header strip.
    pub fn score_line(&self) -> String {
        format!("Blue {} – {} Red", self.blue_score, self.red_score)
    }
}

fn num(stats: &Value, key: &str) -> Option<f64> {
    let v = stats.get(key)?.get("value")?;
    v.as_f64().or_else(|| v.as_u64().map(|n| n as f64))
}

fn display(stats: &Value, key: &str) -> String {
    stats
        .get(key)
        .and_then(|s| s.get("displayValue"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn opt_text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// `2026-11-14T09:58:…` → `Nov 14`. Anything unexpected comes back unchanged.
fn date_label(started: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let date = started.get(0..10).unwrap_or("");
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return started.to_string();
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

/// Milliseconds → `48m 27s`.
fn duration_label(ms: u64) -> String {
    format!("{}m {:02}s", ms / 60_000, (ms % 60_000) / 1_000)
}

fn part(v: Option<String>) -> String {
    v.filter(|s| !s.is_empty())
        .unwrap_or_else(|| "—".to_string())
}

fn fmt_int(v: f64) -> String {
    format!("{}", v.round() as i64)
}

fn fmt_signed(v: f64) -> String {
    let n = v.round() as i64;
    if n < 0 {
        format!("-{n}", n = n.unsigned_abs())
    } else {
        format!("+{n}")
    }
}

fn player_row(seg: &Value, rounds: u64) -> Option<DetailPlayer> {
    let meta = seg.get("metadata")?;
    let stats = seg.get("stats").cloned().unwrap_or(Value::Null);
    let team = opt_text(meta, "teamId");
    if team.is_empty() {
        return None;
    }
    let kills = num(&stats, "kills").map(fmt_int);
    let deaths = num(&stats, "deaths").map(fmt_int);
    let assists = num(&stats, "assists").map(fmt_int);
    let score = num(&stats, "score");
    let acs = match (score, rounds) {
        (Some(s), r) if r > 0 => ((s / r as f64).round() as i64).to_string(),
        _ => String::new(),
    };
    let kd = {
        let d = display(&stats, "kdRatio");
        if d.is_empty() {
            match (num(&stats, "kills"), num(&stats, "deaths")) {
                (Some(k), Some(dd)) if dd > 0.0 => format!("{:.2}", k / dd),
                _ => String::new(),
            }
        } else {
            d
        }
    };
    // Reported strings win; computed values are the fallback, never both.
    let hs = {
        let d = display(&stats, "hsAccuracy");
        if d.is_empty() {
            match (num(&stats, "headshots"), num(&stats, "kills")) {
                (Some(h), Some(k)) if k > 0.0 => format!("{:.1}%", h * 100.0 / k),
                _ => String::new(),
            }
        } else {
            d
        }
    };
    let kast = display(&stats, "kast");
    let delta = {
        let d = display(&stats, "damageDeltaPerRound");
        if d.is_empty() {
            num(&stats, "damageDeltaPerRound")
                .map(fmt_signed)
                .unwrap_or_default()
        } else {
            // Display reads `-36`; normalise a bare number to a signed one.
            let t = d.trim();
            if t.starts_with(['-', '+']) {
                t.to_string()
            } else if t.parse::<f64>().is_ok() {
                let v: f64 = t.parse().unwrap_or(0.0);
                fmt_signed(v)
            } else {
                t.to_string()
            }
        }
    };
    let rank = {
        let curr = meta
            .get("currRank")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if curr.is_empty() {
            display(&stats, "currRank")
        } else {
            curr
        }
    };
    Some(DetailPlayer {
        name: meta
            .get("platformInfo")
            .and_then(|p| p.get("platformUserHandle"))
            .and_then(|h| h.as_str())
            .unwrap_or("")
            .to_string(),
        agent: opt_text(meta, "agentName"),
        agent_id: opt_text(meta, "agentKey"),
        art_url: String::new(),
        team: team.clone(),
        rank,
        acs,
        kda: format!("{} / {} / {}", part(kills), part(deaths), part(assists)),
        kd,
        hs,
        kast,
        delta,
    })
}

/// Parse the match payload. `None` = not the shape we understand — the caller
/// keeps its previous state instead of showing a blank scoreboard.
pub fn parse(payload: &Value) -> Option<MatchDetail> {
    let data = payload.get("data")?;
    let id = data.get("attributes")?.get("id")?.as_str()?.to_string();
    let meta = data.get("metadata")?;
    let segments = data.get("segments")?.as_array()?;

    let map = opt_text(meta, "mapName");
    let mode = opt_text(meta, "modeName");
    let playlist = opt_text(meta, "playlist");
    let date = date_label(&opt_text(meta, "dateStarted"));
    let duration = meta
        .get("duration")
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
        .map(duration_label)
        .unwrap_or_default();
    let total_rounds = meta
        .get("rounds")
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
        .unwrap_or(0);

    let mut blue = Vec::new();
    let mut red = Vec::new();
    let mut rounds: Vec<DetailRound> = Vec::new();
    let mut blue_won = false;
    let mut red_won = false;
    for seg in segments {
        match seg.get("type").and_then(|t| t.as_str()) {
            Some("player-summary") => {
                if let Some(row) = player_row(seg, total_rounds) {
                    if row.team == "Blue" {
                        blue.push(row);
                    } else {
                        red.push(row);
                    }
                }
            }
            Some("team-summary") => {
                let name = seg
                    .get("metadata")
                    .and_then(|m| m.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                let won = seg
                    .get("metadata")
                    .and_then(|m| m.get("hasWon"))
                    .and_then(|w| w.as_bool())
                    .unwrap_or(false);
                if name == "Blue" {
                    blue_won = won;
                } else if name == "Red" {
                    red_won = won;
                }
            }
            Some("round-summary") => {
                let stats = seg.get("stats").cloned().unwrap_or(Value::Null);
                let winner = stats
                    .get("winningTeam")
                    .and_then(|w| w.get("value"))
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        stats
                            .get("winningTeam")
                            .and_then(|w| w.get("displayValue"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or("")
                    .to_string();
                let result = stats
                    .get("roundResult")
                    .and_then(|r| r.get("displayValue"))
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        stats
                            .get("roundResult")
                            .and_then(|r| r.get("value"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or("")
                    .to_string();
                if winner.is_empty() {
                    continue;
                }
                let n = seg
                    .get("attributes")
                    .and_then(|a| a.get("round"))
                    .and_then(|r| r.as_u64().or_else(|| r.as_f64().map(|f| f as u64)))
                    .unwrap_or((rounds.len() + 1) as u64) as u32;
                rounds.push(DetailRound { n, winner, result });
            }
            _ => {}
        }
    }
    // The payload does not promise segment order; the strip reads 1..N only
    // when the cells are sorted by the round TRN numbered them.
    rounds.sort_by_key(|r| r.n);
    if blue.is_empty() && red.is_empty() {
        return None;
    }
    rounds.sort_by_key(|r| r.n);
    // Team score is counted from the rounds, never copied: the strip and the
    // header read the same cells.
    let blue_score = rounds.iter().filter(|r| r.winner == "Blue").count() as u32;
    let red_score = rounds.iter().filter(|r| r.winner == "Red").count() as u32;
    Some(MatchDetail {
        id,
        map,
        mode,
        date,
        duration,
        playlist,
        blue_score,
        red_score,
        blue_won,
        red_won,
        blue,
        red,
        rounds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/trn-match-detail.json");

    fn detail() -> MatchDetail {
        let payload: Value = serde_json::from_str(FIXTURE).expect("fixture parses");
        parse(&payload).expect("fixture parses to a detail")
    }

    #[test]
    fn the_fixture_yields_ten_players_two_sides_thirty_rounds() {
        let d = detail();
        assert_eq!(d.id, "cadcc206-9d07-4695-b792-4215ec6ef341");
        assert_eq!(d.blue.len() + d.red.len(), 10);
        assert_eq!(d.blue.len(), 5);
        assert_eq!(d.red.len(), 5);
        assert_eq!(d.rounds.len(), 30);
        assert_eq!(d.map, "Bind");
        assert_eq!(d.mode, "Competitive");
    }

    #[test]
    fn the_team_score_is_counted_from_the_rounds() {
        let d = detail();
        let blue = d.rounds.iter().filter(|r| r.winner == "Blue").count() as u32;
        let red = d.rounds.iter().filter(|r| r.winner == "Red").count() as u32;
        assert_eq!((blue, red), (14, 16));
        assert_eq!((d.blue_score, d.red_score), (14, 16));
        assert_eq!(d.score_line(), "Blue 14 – 16 Red");
        assert!(!d.blue_won && d.red_won);
    }

    #[test]
    fn the_known_row_is_present_with_its_reported_numbers() {
        let d = detail();
        let sage = d
            .blue
            .iter()
            .chain(d.red.iter())
            .find(|p| p.name == "lil ga7ed#zngr")
            .expect("lil ga7ed row");
        assert_eq!(sage.agent, "Sage");
        assert_eq!(sage.kda, "14 / 23 / 7");
        assert_eq!(sage.rank, "Diamond 3");
        // 4,375 over 30 rounds.
        assert_eq!(sage.acs, "146");
        assert!(!sage.agent_id.is_empty());
    }

    #[test]
    fn rounds_carry_their_winner_and_result() {
        let d = detail();
        assert_eq!(d.rounds[0].n, 1);
        assert!(!d.rounds[0].result.is_empty());
        assert!(d
            .rounds
            .iter()
            .all(|r| r.winner == "Blue" || r.winner == "Red"));
        assert!(d.rounds.windows(2).all(|w| w[0].n <= w[1].n));
    }

    #[test]
    fn header_fields_come_from_the_metadata() {
        let d = detail();
        assert_eq!(d.date, "Nov 14");
        assert_eq!(d.duration, "48m 27s");
    }

    #[test]
    fn a_payload_of_the_wrong_shape_is_not_a_blank_scoreboard() {
        assert!(parse(&serde_json::json!({})).is_none());
        assert!(parse(&serde_json::json!({"data": {}})).is_none());
        assert!(parse(&serde_json::json!({"data": {"attributes": {"id": "x"}, "metadata": {}, "segments": []}})).is_none());
    }
}
