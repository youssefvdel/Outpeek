//! Live lobby: the probe → match payload read that fills the Live Match tab.
//!
//! Shape of the read (mirrors `src/sources/riot.ts` + `docs/v2/live-fixture-contract.md`):
//!
//! 1. the regional GLZ host answers `/core-game/v1/players/{puuid}` with a
//!    `MatchID`, or 404 when no match is running. `/pregame/...` is the second
//!    probe (agent select);
//! 2. `/core-game/v1/matches/{id}` (or `/pregame/...`) carries the roster:
//!    `Players[]` with `Subject`, `TeamID`, `CharacterID` and `PlayerIdentity`
//!    (`AccountLevel`, `Incognito`, `partyId`);
//! 3. names come from the Riot Client's own account service (loopback POST
//!    `/player-account/lookup/v2/namesets-for-puuids`), with the presences as
//!    the fallback — a name that neither source proves is rendered as
//!    `Player <last 4 of puuid>`, never invented;
//! 4. ranks come from `/mmr/v1/players/{puuid}` per player (public tier + RR;
//!    `IsActRankBadgeHidden` is Riot's own "rank hidden" flag).
//!
//! **Honesty.** The wire values below were never captured in-game (no match ran
//! while logging existed — see the fixture contract §"Honesty note"). Field
//! names come from the parser that consumes them; the tests use the documented
//! contract, and the app logs the endpoint + status of every miss so a wrong
//! shape is visible instead of silent.

use serde_json::Value;

/// `glz-<shard>-1.<shard>.a.pvp.net` — the live-match host. Same table as
/// `glzHostFor` in `src/sources/riot.ts`; an unknown region falls back to `eu`
/// rather than addressing a host that never exists.
pub fn glz_host(region: &str) -> String {
    let shard = match region.trim().to_lowercase().as_str() {
        "na" => "na",
        "ap" => "ap",
        "kr" => "kr",
        "latam" => "latam",
        "br" => "br",
        _ => "eu",
    };
    format!("glz-{shard}-1.{shard}.a.pvp.net")
}

/// The probe path for a phase. `pregame` is agent select, `coregame` is a match.
pub fn probe_path(phase: &str, puuid: &str) -> String {
    if phase == "pregame" {
        format!("/pregame/v1/players/{puuid}")
    } else {
        format!("/core-game/v1/players/{puuid}")
    }
}

/// The match payload path for a phase.
pub fn match_path(phase: &str, match_id: &str) -> String {
    if phase == "pregame" {
        format!("/pregame/v1/matches/{match_id}")
    } else {
        format!("/core-game/v1/matches/{match_id}")
    }
}

/// `MatchID` from a probe body. `None` for a 404 body (Riot's "no match") and
/// for any other envelope without an id: "nothing here" is a state, not an
/// error, and never a guessed id.
pub fn parse_probe(payload: &Value) -> Option<String> {
    payload
        .get("MatchID")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// One `/chat/v4/presences` entry: the Riot ID and the party the client itself
/// attaches. Never an agent or a rank — those come from the match payload and
/// `/mmr`, and a presence must not be able to fake them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresenceRow {
    pub puuid: String,
    pub name: String,
    pub tag: String,
    pub party_id: String,
}

/// Party id from a decoded presence blob: the flat `partyId` first, then the
/// nested `partyPresenceData.partyId` the client also ships.
fn party_from_blob(blob: &Value) -> String {
    let direct = text(blob.get("partyId"));
    if !direct.is_empty() {
        return direct;
    }
    text(blob.pointer("/partyPresenceData/partyId"))
}

/// Every presence that names a puuid, for the lobby's names and parties.
pub fn presence_rows(root: &Value) -> Vec<PresenceRow> {
    let Some(list) = root.get("presences").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|presence| {
            let puuid = crate::live_facts::presence_puuid(presence);
            if puuid.is_empty() {
                return None;
            }
            let name = {
                let n = text(presence.get("game_name"));
                if n.is_empty() {
                    text(presence.get("gameName"))
                } else {
                    n
                }
            };
            let tag = {
                let t = text(presence.get("game_tag"));
                if t.is_empty() {
                    text(presence.get("gameTag"))
                } else {
                    t
                }
            };
            let party_id = crate::live_facts::presence_blob(presence)
                .map(|blob| party_from_blob(&blob))
                .unwrap_or_default();
            Some(PresenceRow {
                puuid,
                name,
                tag,
                party_id,
            })
        })
        .collect()
}

/// One player of the live lobby, as the payload reports them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LivePlayer {
    pub puuid: String,
    /// `"Blue"` / `"Red"`; empty when the payload names no team.
    pub team: String,
    /// Agent uuid (valorant-api `uuid`), empty during agent select.
    pub agent_id: String,
    pub account_level: i64,
    pub incognito: bool,
    /// Party the payload itself attaches, empty when it carries none.
    pub party_id: String,
}

/// The parsed match: what the tab needs from the payload alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveMatch {
    pub match_id: String,
    /// `"coregame"` | `"pregame"`.
    pub phase: String,
    /// Codename (`ascent`) from `MapID`; the catalogue resolves the display
    /// name and the card art, exactly like the Maps tab.
    pub map_id: String,
    /// `queueId`/`QueueID` when the payload names one (`competitive`).
    pub queue_id: String,
    /// `ModeID` tail (`BombGameMode`), for the mode label.
    pub mode_id: String,
    pub players: Vec<LivePlayer>,
}

impl LiveMatch {
    /// Only the two phases the app can draw. Anything else is not a match.
    pub fn is_live(&self) -> bool {
        matches!(self.phase.as_str(), "coregame" | "pregame") && !self.match_id.is_empty()
    }

    /// A payload with no players is not a roster: the tab keeps its empty state
    /// rather than drawing two empty teams.
    pub fn has_roster(&self) -> bool {
        !self.players.is_empty()
    }
}

/// `MapID` (`/Game/Maps/Ascent/Ascent`) → codename (`ascent`). Same
/// normalisation the map catalogue uses, so `mapIcon` resolves.
fn map_codename(map_id: &str) -> String {
    let tail = map_id
        .rsplit('/')
        .find(|part| !part.trim().is_empty())
        .unwrap_or("");
    tail.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// `ModeID` tail, e.g. `BombGameMode` from
/// `/Game/GameModes/Bomb/BombGameMode.BombGameMode_C`.
fn mode_tail(mode_id: &str) -> String {
    let last = mode_id.rsplit('/').next().unwrap_or(mode_id);
    last.split('.').next().unwrap_or(last).trim().to_string()
}

fn text(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn number(value: Option<&Value>) -> i64 {
    value.and_then(Value::as_i64).unwrap_or(0)
}

/// One `Players[]` entry. `coregame` carries a flat `Players[]`; `pregame`
/// carries `Teams[].Players[]` (and, for custom games, `AllyTeam`/`EnemyTeam`),
/// so the caller hands in the player value plus the team name it came from.
fn parse_player(entry: &Value, team_from_container: &str) -> Option<LivePlayer> {
    let puuid = text(entry.get("Subject"));
    if puuid.is_empty() {
        return None;
    }
    let identity = entry.get("PlayerIdentity").cloned().unwrap_or(Value::Null);
    let team = {
        let from_row = text(entry.get("TeamID"));
        if !from_row.is_empty() {
            from_row
        } else {
            team_from_container.to_string()
        }
    };
    Some(LivePlayer {
        puuid,
        team,
        agent_id: text(entry.get("CharacterID")).to_lowercase(),
        account_level: number(identity.get("AccountLevel")),
        incognito: identity
            .get("Incognito")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        party_id: text(identity.get("partyId")),
    })
}

/// Parse one coregame/pregame match payload. An error envelope (`httpStatus`)
/// is an error: it must never parse into an empty roster, because "could not
/// read" and "nobody here" are different sentences on the page.
pub fn parse_match(payload: &Value, phase: &str) -> Result<LiveMatch, String> {
    if !payload.is_object() {
        return Err("Unreadable live match payload.".to_string());
    }
    if let Some(status) = payload.get("httpStatus").and_then(Value::as_i64) {
        return Err(format!("Riot returned HTTP {status} for the live match."));
    }
    let match_id = text(payload.get("MatchID"));
    if match_id.is_empty() {
        return Err("Live match payload had no MatchID.".to_string());
    }

    let mut players = Vec::new();
    if let Some(list) = payload.get("Players").and_then(Value::as_array) {
        for entry in list {
            if let Some(player) = parse_player(entry, "") {
                players.push(player);
            }
        }
    }
    if players.is_empty() {
        // Pregame: Teams[] → Players[], plus the two flat team containers custom
        // games fill.
        if let Some(teams) = payload.get("Teams").and_then(Value::as_array) {
            for team in teams {
                let team_id = text(team.get("TeamID"));
                if let Some(list) = team.get("Players").and_then(Value::as_array) {
                    for entry in list {
                        if let Some(player) = parse_player(entry, &team_id) {
                            players.push(player);
                        }
                    }
                }
            }
        }
        for (key, team_id) in [("AllyTeam", "Blue"), ("EnemyTeam", "Red")] {
            if let Some(team) = payload.get(key) {
                if let Some(list) = team.get("Players").and_then(Value::as_array) {
                    for entry in list {
                        if let Some(player) = parse_player(entry, team_id) {
                            players.push(player);
                        }
                    }
                }
            }
        }
    }
    // A repeated puuid (custom-game containers can overlap) is one player.
    players.dedup_by(|a, b| a.puuid.eq_ignore_ascii_case(&b.puuid));

    let queue_id = {
        let from_row = text(payload.get("QueueID")).to_lowercase();
        if from_row.is_empty() {
            text(payload.get("queueId")).to_lowercase()
        } else {
            from_row
        }
    };
    Ok(LiveMatch {
        match_id,
        phase: phase.to_string(),
        map_id: map_codename(&text(payload.get("MapID"))),
        queue_id,
        mode_id: mode_tail(&text(payload.get("ModeID"))),
        players,
    })
}

/// Human queue/mode label for the strip. Riot's `queueId` is the fact; the mode
/// id is only the fallback, and an unknown queue prints as itself rather than
/// as a guessed "Competitive".
pub fn mode_label(queue_id: &str, mode_id: &str) -> String {
    let queue = queue_id.trim().to_lowercase();
    let label = match queue.as_str() {
        "competitive" => "Competitive",
        "unrated" => "Unrated",
        "swiftplay" => "Swiftplay",
        "spikerush" => "Spike Rush",
        "deathmatch" => "Deathmatch",
        "hurm" => "Team Deathmatch",
        "custom" => "Custom",
        "premier" => "Premier",
        _ => "",
    };
    if !label.is_empty() {
        return label.to_string();
    }
    if mode_id.contains("Deathmatch") {
        return "Deathmatch".to_string();
    }
    if queue.is_empty() {
        return String::new();
    }
    queue
}

/// One row of `/player-account/lookup/v2/namesets-for-puuids`. `error` is the
/// service's own per-PUUID marker ("Nameset V2 not found…"), which is how a
/// missing name stays distinguishable from a failed call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NameRow {
    pub puuid: String,
    pub name: String,
    pub tag: String,
    pub error: String,
}

/// Parse the loopback nameset answer. Tolerates both the `{namesets: [...]}`
/// envelope and a bare array (the Tauri command returns the array).
pub fn parse_namesets(payload: &Value) -> Vec<NameRow> {
    let items = payload
        .get("namesets")
        .and_then(Value::as_array)
        .or_else(|| payload.as_array());
    let Some(items) = items else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let puuid = {
                let direct = text(item.get("puuid"));
                if direct.is_empty() {
                    text(item.get("Subject"))
                } else {
                    direct
                }
            };
            if puuid.is_empty() {
                return None;
            }
            let alias = item.get("alias").cloned().unwrap_or(Value::Null);
            let name = {
                let n = text(alias.get("gameName"));
                if n.is_empty() {
                    text(alias.get("game_name"))
                } else {
                    n
                }
            };
            let tag = {
                let t = text(alias.get("tagLine"));
                if t.is_empty() {
                    text(alias.get("tag_line"))
                } else {
                    t
                }
            };
            Some(NameRow {
                puuid,
                name,
                tag,
                error: text(item.get("error")),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The documented coregame shape (`docs/v2/live-fixture-contract.md` §5):
    /// two players, one incognito, one with a party.
    fn coregame_payload() -> Value {
        json!({
            "MatchID": "b2f0c1d2-0000-4000-8000-000000000001",
            "MapID": "/Game/Maps/Ascent/Ascent",
            "ModeID": "/Game/GameModes/Bomb/BombGameMode.BombGameMode_C",
            "QueueID": "competitive",
            "Players": [
                {
                    "Subject": "11111111-2222-3333-4444-555555555555",
                    "TeamID": "Blue",
                    "CharacterID": "E370FA57-4757-3604-3648-499E1F642D3F",
                    "PlayerIdentity": {
                        "AccountLevel": 214,
                        "Incognito": false,
                        "partyId": "party-1"
                    }
                },
                {
                    "Subject": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                    "TeamID": "Red",
                    "CharacterID": "JETT-UUID",
                    "PlayerIdentity": {
                        "AccountLevel": 0,
                        "Incognito": true
                    }
                }
            ]
        })
    }

    #[test]
    fn a_coregame_payload_parses_the_roster_and_the_map_codename() {
        let m = parse_match(&coregame_payload(), "coregame").unwrap();
        assert_eq!(m.phase, "coregame");
        assert_eq!(m.map_id, "ascent");
        assert_eq!(m.queue_id, "competitive");
        assert_eq!(m.mode_id, "BombGameMode");
        assert!(m.is_live());
        assert!(m.has_roster());
        assert_eq!(m.players.len(), 2);
        let me = &m.players[0];
        assert_eq!(me.team, "Blue");
        assert_eq!(me.agent_id, "e370fa57-4757-3604-3648-499e1f642d3f");
        assert_eq!(me.account_level, 214);
        assert_eq!(me.party_id, "party-1");
        assert!(!me.incognito);
        // Riot's own incognito flag, not inferred from a missing name.
        assert!(m.players[1].incognito);
        assert_eq!(m.players[1].team, "Red");
    }

    #[test]
    fn a_pregame_payload_reads_teams_and_the_flat_custom_containers() {
        let payload = json!({
            "MatchID": "pg-1",
            "MapID": "/Game/Maps/Lotus/Lotus",
            "ModeID": "/Game/GameModes/Bomb/BombGameMode.BombGameMode_C",
            "QueueID": "custom",
            "Teams": [
                {"TeamID": "Blue", "Players": [
                    {"Subject": "p1", "CharacterID": "", "PlayerIdentity": {"AccountLevel": 7}}
                ]},
                {"TeamID": "Red", "Players": [
                    {"Subject": "p2", "CharacterID": "", "PlayerIdentity": {"AccountLevel": 9}}
                ]}
            ]
        });
        let m = parse_match(&payload, "pregame").unwrap();
        assert_eq!(m.map_id, "lotus");
        assert_eq!(m.players.len(), 2);
        assert_eq!(m.players[0].team, "Blue");
        assert_eq!(m.players[1].team, "Red");
        assert_eq!(m.players[1].account_level, 9);
        // Agent select: no character yet, so no agent art may be guessed.
        assert!(m.players.iter().all(|p| p.agent_id.is_empty()));
    }

    #[test]
    fn an_error_envelope_is_an_error_not_an_empty_roster() {
        let err = json!({"httpStatus": 404, "errorCode": "RESOURCE_NOT_FOUND"});
        assert!(parse_match(&err, "coregame").is_err());
        assert!(parse_match(&json!({}), "coregame").is_err());
        assert!(parse_match(&json!("nope"), "coregame").is_err());
    }

    #[test]
    fn a_probe_404_is_no_match_and_a_body_without_an_id_is_not_a_match_either() {
        assert_eq!(
            parse_probe(&json!({"httpStatus": 404})),
            None,
            "Riot's 404 means no match, not an error"
        );
        assert_eq!(parse_probe(&json!({"MatchID": "  "})), None);
        assert_eq!(parse_probe(&json!({})), None);
        assert_eq!(
            parse_probe(&json!({"MatchID": "m-1"})).as_deref(),
            Some("m-1")
        );
    }

    #[test]
    fn namesets_keep_the_per_puuid_error_and_the_alias() {
        let payload = json!({"namesets": [
            {"puuid": "a", "alias": {"gameName": "Shadow", "tagLine": "EUW"}},
            {"puuid": "b", "alias": {"gameName": "", "tagLine": ""},
             "error": "Nameset V2 not found for puuid."}
        ]});
        let rows = parse_namesets(&payload);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Shadow");
        assert_eq!(rows[0].tag, "EUW");
        assert!(rows[0].error.is_empty());
        assert_eq!(rows[1].name, "");
        assert!(rows[1].error.contains("Nameset V2 not found"));
        // A bare array (the Tauri command's own return shape) parses too.
        assert_eq!(
            parse_namesets(&json!([{"puuid": "c", "alias": {"gameName": "X"}}])).len(),
            1
        );
        assert!(parse_namesets(&json!("nope")).is_empty());
    }

    #[test]
    fn presences_give_names_and_parties_but_never_an_agent_or_rank() {
        let root = json!({"presences": [
            {"puuid": "p1", "game_name": "Shadow", "game_tag": "EUW",
             "private": "eyJwYXJ0eUlkIjoicGFydHktMSJ9"},
            {"puuid": "p2", "game_name": "Solo", "game_tag": "NA1", "private": ""},
            {"puuid": "", "game_name": "NoId"},
            // The nested shape the client also ships.
            {"puuid": "p3", "game_name": "Nested", "game_tag": "EUW",
             "private": "eyJwYXJ0eVByZXNlbmNlRGF0YSI6eyJwYXJ0eUlkIjoicGFydHktOSJ9fQ=="}
        ]});
        let rows = presence_rows(&root);
        assert_eq!(rows.len(), 3, "an entry with no puuid is not a player");
        assert_eq!(rows[0].name, "Shadow");
        assert_eq!(rows[0].tag, "EUW");
        assert_eq!(rows[0].party_id, "party-1");
        assert!(rows[1].party_id.is_empty(), "no blob, no party");
        assert_eq!(rows[2].party_id, "party-9");
    }

    #[test]
    fn probe_and_match_paths_use_the_phase_spelling_riot_expects() {
        assert_eq!(probe_path("coregame", "p1"), "/core-game/v1/players/p1");
        assert_eq!(probe_path("pregame", "p1"), "/pregame/v1/players/p1");
        assert_eq!(match_path("coregame", "m1"), "/core-game/v1/matches/m1");
        assert_eq!(match_path("pregame", "m1"), "/pregame/v1/matches/m1");
        // Unknown regions fall back to eu, never to a host that does not exist.
        assert_eq!(glz_host("EU"), "glz-eu-1.eu.a.pvp.net");
        assert_eq!(glz_host("na"), "glz-na-1.na.a.pvp.net");
        assert_eq!(glz_host("mars"), "glz-eu-1.eu.a.pvp.net");
    }

    #[test]
    fn the_mode_label_prefers_the_queue_and_never_guesses_competitive() {
        assert_eq!(mode_label("competitive", ""), "Competitive");
        assert_eq!(mode_label("", "DeathmatchGameMode"), "Deathmatch");
        assert_eq!(mode_label("", ""), "");
        assert_eq!(mode_label("replication", ""), "replication");
    }
}
