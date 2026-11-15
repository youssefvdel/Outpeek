//! Transport for the live lobby.
//!
//! Three reads, kept apart from the parsers in [`crate::live_match`] so the
//! shapes are testable without a network:
//!
//! * [`probe`] — `glz` `/core-game/v1/players/{puuid}`, then `/pregame/...`.
//!   A 404 on both is Riot saying "no match", not an error;
//! * [`match_by_id`] — the roster payload for that id;
//! * [`namesets`] — the Riot Client's own account service (loopback POST), the
//!   primary name source; presences are the fallback the caller merges in.
//!
//! Ranks are [`player_rank`], the same `/mmr` read as the local rank panel.

use std::time::Duration;

use serde_json::Value;

use crate::identity::{Entitlements, LockfileAuth};
use crate::live_match::{self, LiveMatch, NameRow};
use crate::rank::{self, RankRow};
use crate::riot_http::{self, Request};

/// What the two-phase probe found.
#[derive(Debug, Clone, PartialEq)]
pub enum Probe {
    /// Both routes answered 404: no match is running.
    Idle,
    /// A live match id, and the phase whose route answered it.
    Match {
        phase: &'static str,
        match_id: String,
    },
}

/// Poll both probe routes. `coregame` first: an agent-select lobby still has no
/// core-game entry, so a `pregame` answer only counts after the first 404.
pub fn probe(ent: &Entitlements, region: &str, puuid: &str) -> Result<Probe, String> {
    let host = live_match::glz_host(region);
    for phase in ["coregame", "pregame"] {
        let path = live_match::probe_path(phase, puuid);
        let (status, body) = rank::remote_get(&host, &path, ent)?;
        if status == 404 {
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(format!(
                "Riot returned HTTP {status} for the live probe ({phase})."
            ));
        }
        let payload: Value =
            serde_json::from_str(&body).map_err(|_| "Unreadable live probe.".to_string())?;
        if let Some(match_id) = live_match::parse_probe(&payload) {
            return Ok(Probe::Match { phase, match_id });
        }
    }
    Ok(Probe::Idle)
}

/// The roster payload for one live match. `Ok(None)` = Riot 404s the id
/// (the match ended between the probe and this read), which the caller treats
/// as "back to idle", not as a failure.
pub fn match_by_id(
    ent: &Entitlements,
    region: &str,
    phase: &str,
    match_id: &str,
) -> Result<Option<LiveMatch>, String> {
    let host = live_match::glz_host(region);
    let path = live_match::match_path(phase, match_id);
    let (status, body) = rank::remote_get(&host, &path, ent)?;
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(format!("Riot returned HTTP {status} for the live match."));
    }
    let payload: Value =
        serde_json::from_str(&body).map_err(|_| "Unreadable live match payload.".to_string())?;
    live_match::parse_match(&payload, phase).map(Some)
}

/// Names for a lobby, from the Riot Client's account service. Batched by the
/// caller; a per-PUUID miss keeps the service's own `error` string.
pub fn namesets(auth: &LockfileAuth, puuids: &[String]) -> Result<Vec<NameRow>, String> {
    if puuids.is_empty() {
        return Ok(Vec::new());
    }
    let port: u16 = auth
        .port
        .parse()
        .map_err(|_| "Riot Client lockfile has no usable port.".to_string())?;
    let body = serde_json::json!({ "puuids": puuids }).to_string();
    let headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    let resp = riot_http::send(&Request {
        host: "127.0.0.1",
        port,
        path: "/player-account/lookup/v2/namesets-for-puuids",
        method: "POST",
        headers: &headers,
        body: Some(&body),
        basic_auth: Some(format!("riot:{}", auth.password)),
        accept_invalid_certs: true,
        connect_timeout: Duration::from_secs(1),
        max_time: Duration::from_secs(5),
    })
    .map_err(|_| "Riot Client not responding — launch it and retry.".to_string())?;
    if !(200..300).contains(&resp.status) {
        return Err(format!(
            "The Riot Client refused the name lookup (HTTP {}).",
            resp.status
        ));
    }
    let payload: Value = serde_json::from_str(&resp.body)
        .map_err(|_| "Unexpected name-lookup response.".to_string())?;
    Ok(live_match::parse_namesets(&payload))
}

/// Public rank for one lobby player. The caller caches the answer: ten players
/// would otherwise be ten remote calls on every poll.
pub fn player_rank(ent: &Entitlements, region: &str, puuid: &str) -> Result<RankRow, String> {
    rank::fetch_rank(puuid, ent, region)
}
