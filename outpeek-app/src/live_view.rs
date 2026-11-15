//! Compose the Live Match tab from one lobby read.
//!
//! The payload, the presences and `/mmr` each answer a different question, and
//! this module is where they meet:
//!
//! * roster, agent id, level, incognito, party → the match payload;
//! * Riot ID → presences, then the account service, then `Player <last4>`;
//! * rank → `/mmr` (public tier + RR), cached per player for
//!   [`RANK_TTL`]. A hidden badge is Riot's own flag, not "Unrated".
//!
//! Nothing here guesses: a value no source proved renders as an empty string
//! and the view draws its own honest placeholder.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use crate::assets::{Art, Fetcher};
use crate::live_match::LiveMatch;
use crate::model::tier_label;
use crate::rank::RankRow;

/// A rank read is per player, not per match: keep it for a quarter of an hour.
pub const RANK_TTL: Duration = Duration::from_secs(15 * 60);
/// A failed read is retried sooner than a good one, but not on every poll.
pub const RANK_RETRY: Duration = Duration::from_secs(2 * 60);

/// One row of the Live Match board. Every cell is a display string; the view
/// never formats a number.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LivePlayerUi {
    /// `Shadow#EUW`, or `Player 3f2a` when no source proved a Riot ID.
    pub name: String,
    pub agent: String,
    /// Cached agent icon URL; empty when the catalogue does not list the agent.
    pub agent_art: String,
    /// `Lv 214`, empty when Riot reported no level.
    pub level: String,
    /// `Diamond 2` | `UNRATED` | `RANK HIDDEN` | `NOT READ`.
    pub rank: String,
    /// `47 RR`, empty when there is no public RR to show.
    pub rr: String,
    /// Cached rank crest URL.
    pub crest: String,
    /// `2` when this player queued with someone else, empty solo.
    pub party: String,
    pub me: bool,
    pub incognito: bool,
}

/// The tab's whole state: the strip facts plus the two teams.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveView {
    /// `idle` | `pregame` | `coregame` | `error`.
    pub state: &'static str,
    /// The reason line: why there is no board, or which read failed.
    pub message: String,
    pub map: String,
    pub mode: String,
    pub blue_score: i64,
    pub red_score: i64,
    pub blue: Vec<LivePlayerUi>,
    pub red: Vec<LivePlayerUi>,
}

impl LiveView {
    pub fn idle(message: impl Into<String>) -> Self {
        Self {
            state: "idle",
            message: message.into(),
            map: String::new(),
            mode: String::new(),
            blue_score: 0,
            red_score: 0,
            blue: Vec::new(),
            red: Vec::new(),
        }
    }

    /// A failed read never wipes a board that is still on screen: the caller
    /// keeps the rows and swaps the label.
    pub fn error(mut self, message: impl Into<String>) -> Self {
        self.state = "error";
        self.message = message.into();
        self
    }

    pub fn is_empty(&self) -> bool {
        self.blue.is_empty() && self.red.is_empty()
    }
}

/// Names and ranks outlive one lobby: both are per player, and re-reading them
/// on every poll would be ten remote calls every 2.5 s.
#[derive(Debug, Default)]
pub struct LiveCache {
    names: HashMap<String, (String, String)>,
    ranks: HashMap<String, (Instant, Option<RankRow>)>,
}

impl LiveCache {
    pub fn name(&self, puuid: &str) -> Option<&(String, String)> {
        self.names.get(&puuid.to_lowercase())
    }

    pub fn set_name(&mut self, puuid: &str, name: &str, tag: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.names.insert(
            puuid.to_lowercase(),
            (name.to_string(), tag.trim().to_string()),
        );
    }

    /// Cached rank for a player, read through `read` when the entry is missing
    /// or stale. A failed read is cached negative for [`RANK_RETRY`].
    pub fn rank(
        &mut self,
        puuid: &str,
        now: Instant,
        read: impl FnOnce() -> Option<RankRow>,
    ) -> Option<RankRow> {
        let key = puuid.to_lowercase();
        if let Some((at, value)) = self.ranks.get(&key) {
            let ttl = if value.is_some() {
                RANK_TTL
            } else {
                RANK_RETRY
            };
            if now.duration_since(*at) < ttl {
                return *value;
            }
        }
        let value = read();
        self.ranks.insert(key, (now, value));
        value
    }

    /// Drop entries for players who are no longer in the lobby: a long session
    /// would otherwise keep every PUUID it ever saw.
    pub fn retain(&mut self, puuids: &[String]) {
        let wanted: Vec<String> = puuids.iter().map(|p| p.to_lowercase()).collect();
        self.ranks.retain(|puuid, _| wanted.contains(puuid));
    }
}

/// Party size per party id across both teams, so a bracket is a fact from the
/// payload and not a guess from slot order.
fn party_sizes(match_: &LiveMatch) -> HashMap<String, i64> {
    let mut sizes: HashMap<String, i64> = HashMap::new();
    for player in &match_.players {
        if !player.party_id.is_empty() {
            *sizes.entry(player.party_id.clone()).or_insert(0) += 1;
        }
    }
    sizes
}

/// `Player 3f2a` — the honest placeholder for a PUUID no name source answered
/// (same rule as the React board).
fn name_placeholder(puuid: &str) -> String {
    let tail: String = puuid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .rev()
        .take(4)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    if tail.is_empty() {
        "Player".to_string()
    } else {
        format!("Player {tail}")
    }
}

/// The rank cell: Riot's hidden flag first, then the public tier, then the
/// honest "not read". Unrated is only ever shown after a successful read.
fn rank_cells(rank: Option<RankRow>) -> (String, String) {
    match rank {
        Some(row) if row.hidden => ("RANK HIDDEN".to_string(), String::new()),
        Some(row) if row.tier >= 3 => (
            tier_label(row.tier),
            if row.rr > 0 {
                format!("{} RR", row.rr)
            } else {
                String::new()
            },
        ),
        Some(_) => ("UNRATED".to_string(), String::new()),
        None => ("NOT READ".to_string(), String::new()),
    }
}

/// Build one frame. `rank_for` is the cached reader; art is best-effort.
pub async fn build(
    match_: &LiveMatch,
    local_puuid: &str,
    map: &str,
    mode: &str,
    blue_score: i64,
    red_score: i64,
    cache: &mut LiveCache,
    rank_for: &mut dyn FnMut(&str) -> Option<RankRow>,
    art: &Art,
    fetcher: std::sync::Arc<dyn Fetcher>,
) -> LiveView {
    let sizes = party_sizes(match_);
    let now = SystemTime::now();
    let mut blue = Vec::new();
    let mut red = Vec::new();
    for player in &match_.players {
        let (name, tag) = match cache.name(&player.puuid) {
            Some((name, tag)) => (name.clone(), tag.clone()),
            None => (name_placeholder(&player.puuid), String::new()),
        };
        let display = if tag.is_empty() {
            name
        } else {
            format!("{name}#{tag}")
        };
        let agent = crate::assets::agent_for(art.agents(), &player.agent_id);
        let agent_name = agent.map(|a| a.name.clone()).unwrap_or_default();
        let agent_art = match agent {
            Some(agent) => art
                .ensure_url(fetcher.clone(), &agent.icon, now)
                .await
                .unwrap_or_default(),
            None => String::new(),
        };
        let rank = cache.rank(&player.puuid, Instant::now(), || rank_for(&player.puuid));
        let (rank_text, rr) = rank_cells(rank);
        // The crest is the tier's own emblem; a hidden or unread rank has no
        // crest to draw, because there is no proven tier.
        let crest = match rank {
            Some(row) if !row.hidden && row.tier >= 3 => art
                .tier_icon(fetcher.clone(), row.tier, now)
                .await
                .unwrap_or_default(),
            _ => String::new(),
        };
        let party = if player.party_id.is_empty() {
            String::new()
        } else {
            match sizes.get(&player.party_id).copied().unwrap_or(0) {
                size if size >= 2 => size.to_string(),
                _ => String::new(),
            }
        };
        let row = LivePlayerUi {
            name: display,
            agent: agent_name,
            agent_art,
            level: if player.account_level > 0 {
                format!("Lv {}", player.account_level)
            } else {
                String::new()
            },
            rank: rank_text,
            rr,
            crest,
            party,
            me: player.puuid.eq_ignore_ascii_case(local_puuid),
            incognito: player.incognito,
        };
        if player.team.eq_ignore_ascii_case("red") {
            red.push(row);
        } else {
            blue.push(row);
        }
    }
    LiveView {
        state: phase_state(&match_.phase),
        message: String::new(),
        map: map.to_string(),
        mode: mode.to_string(),
        blue_score,
        red_score,
        blue,
        red,
    }
}

/// The `&'static str` the Slint state property carries.
fn phase_state(phase: &str) -> &'static str {
    match phase {
        "pregame" => "pregame",
        "coregame" => "coregame",
        _ => "idle",
    }
}
