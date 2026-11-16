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
    /// `idle` | `offline` | `pregame` | `coregame` | `error`.
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

    /// No Riot Client: a different state from "in menus", because the fix is
    /// different (start the client, not queue).
    pub fn offline(message: impl Into<String>) -> Self {
        Self {
            state: "offline",
            message: message.into(),
            ..Self::idle("")
        }
    }

    /// A failed read. With rows on screen the caller keeps them and only the
    /// label changes; with none it is a failure with a reason.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rank(tier: i64, rr: i64) -> RankRow {
        RankRow {
            tier,
            rr,
            peak_tier: tier,
            hidden: false,
        }
    }

    #[test]
    fn a_rank_cell_is_a_fact_or_one_of_three_honest_states() {
        // A read tier plus RR.
        assert_eq!(
            rank_cells(Some(rank(21, 64))),
            ("Ascendant 1".to_string(), "64 RR".to_string())
        );
        // Riot's own hidden flag wins over the tier it also reported.
        let mut hidden = rank(21, 64);
        hidden.hidden = true;
        assert_eq!(
            rank_cells(Some(hidden)),
            ("RANK HIDDEN".to_string(), String::new())
        );
        // A successful read with tier 0 is Unrated — never "not read".
        assert_eq!(
            rank_cells(Some(rank(0, 0))),
            ("UNRATED".to_string(), String::new())
        );
        // No read at all is its own state.
        assert_eq!(rank_cells(None), ("NOT READ".to_string(), String::new()));
        // RR of zero is not printed as "0 RR" — there is nothing to show.
        assert_eq!(
            rank_cells(Some(rank(21, 0))),
            ("Ascendant 1".to_string(), String::new())
        );
    }

    #[test]
    fn an_unresolved_puuid_gets_the_short_placeholder_never_a_hex_string() {
        assert_eq!(
            name_placeholder("6f0a1d2e-1111-2222-3333-444444444444"),
            "Player 4444"
        );
        assert_eq!(name_placeholder("abc"), "Player abc");
        assert_eq!(name_placeholder(""), "Player");
    }

    #[test]
    fn party_sizes_count_the_players_the_payload_grouped() {
        let matched = LiveMatch {
            players: vec![
                crate::live_match::LivePlayer {
                    puuid: "a".into(),
                    party_id: "p1".into(),
                    ..Default::default()
                },
                crate::live_match::LivePlayer {
                    puuid: "b".into(),
                    party_id: "p1".into(),
                    ..Default::default()
                },
                crate::live_match::LivePlayer {
                    puuid: "c".into(),
                    party_id: "p2".into(),
                    ..Default::default()
                },
                crate::live_match::LivePlayer {
                    puuid: "d".into(),
                    party_id: String::new(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let sizes = party_sizes(&matched);
        assert_eq!(sizes.get("p1"), Some(&2));
        assert_eq!(
            sizes.get("p2"),
            Some(&1),
            "a solo party is counted but not shown"
        );
        assert!(!sizes.contains_key(""));
    }

    #[test]
    fn a_rank_read_is_cached_and_a_failure_is_retried_sooner() {
        let mut cache = LiveCache::default();
        let mut reads = 0;
        let now = Instant::now();
        let mut read = |_: &str| {
            reads += 1;
            Some(rank(21, 64))
        };
        let first = cache.rank("PUUID-A", now, || read("PUUID-A"));
        assert_eq!(first.map(|r| r.tier), Some(21));
        // Inside the TTL: no second read, and the key is case-insensitive.
        let second = cache.rank("puuid-a", now + Duration::from_secs(60), || read("x"));
        assert_eq!(second.map(|r| r.tier), Some(21));
        assert_eq!(reads, 1, "a cached rank is not re-read");

        // A failed read is negative-cached briefly, then retried.
        let mut failing = LiveCache::default();
        let mut attempts = 0;
        let miss = failing.rank("p", now, || {
            attempts += 1;
            None
        });
        assert!(miss.is_none());
        let _ = failing.rank("p", now + Duration::from_secs(30), || {
            attempts += 1;
            None
        });
        assert_eq!(
            attempts, 1,
            "a fresh failure is not retried on the next poll"
        );
        let _ = failing.rank("p", now + RANK_RETRY + Duration::from_secs(1), || {
            attempts += 1;
            None
        });
        assert_eq!(attempts, 2, "past the retry window it asks again");
    }

    #[test]
    fn names_outlive_a_lobby_but_the_cache_forgets_departed_players() {
        let mut cache = LiveCache::default();
        cache.set_name("ABC", "Shadow", "EUW");
        assert_eq!(
            cache.name("abc"),
            Some(&("Shadow".to_string(), "EUW".to_string())),
            "names are case-insensitive and survive a lobby change"
        );
        // An empty name is not a name: the placeholder must keep its slot.
        cache.set_name("DEF", "   ", "");
        assert!(cache.name("DEF").is_none());

        cache.set_name("abc", "Shadow", "EUW");
        cache.retain(&["abc".to_string()]);
        assert!(cache.name("abc").is_some());
        // Rank rows for players who left the lobby are dropped.
        let now = Instant::now();
        cache.rank("gone", now, || Some(rank(21, 10)));
        cache.retain(&["abc".to_string()]);
        assert!(cache.ranks.is_empty() || !cache.ranks.contains_key("gone"));
    }
}
