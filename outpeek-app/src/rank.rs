//! Public rank (tier + RR) for the local account.
//!
//! Rank is the one fact Riot does **not** expose on the loopback API: the
//! shipping app reads `/mmr/v1/players/{puuid}` from the regional shard with
//! the entitlement triple the local client hands out. This module mirrors that
//! read (`legacy/src-tauri/src/tracker.rs` is canonical) and keeps it **off the live
//! tick**: one read at startup / account change / explicit refresh, never on
//! the 2.5 s presence poll.
//!
//! Wording rule from AGENTS.md: these are PUBLIC rank + RR. Riot does not
//! expose raw/hidden MMR, and nothing here may claim otherwise.

use std::time::Duration;

use serde_json::Value;

use crate::identity::Entitlements;
use crate::riot_http::{self, Request};

/// `X-Riot-ClientPlatform` blob Riot's remote APIs expect. Protocol data, not
/// logic; identical to the value in `legacy/src-tauri/src/tracker.rs`.
const CLIENT_PLATFORM: &str = "ew0KCSJwbGF0Zm9ybVR5cGUiOiAiUEMiLA0KCSJwbGF0Zm9ybU9TIjogIldpbmRvd3MiLA0KCSJwbGF0Zm9ybU9TVmVyc2lvbiI6ICIxMC4wLjE5MDQyLjEuMjU2LjY0Yml0IiwNCgkicGxhdGZvcm1DaGlwc2V0IjogIlVua25vd24iDQp9";
/// Used when the game log is not available; the same value the Tauri app
/// falls back to. Riot tolerates a slightly stale version on `/mmr`.
const FALLBACK_CLIENT_VERSION: &str = "release-13.05-shipping-11-5350494";

/// Public rank for one account: current tier + ranked rating, plus the best
/// tier Riot's per-act rows report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankRow {
    pub tier: i64,
    pub rr: i64,
    /// Highest tier across the acts in `SeasonalInfoBySeasonID`. 0 when Riot
    /// reported no act. Mirrors `parseLocalRank`'s `peakTier` in
    /// `src/sources/riot.ts` — a read of Riot's own act history, not a guess.
    pub peak_tier: i64,
    /// Riot's own `IsActRankBadgeHidden`: the player hid their act rank. A
    /// hidden rank is a fact to show as hidden, never as Unrated.
    pub hidden: bool,
}

/// Decode the region claim out of the entitlements access token (JWT). Claim
/// `pp.c` is the shard Riot itself used; `c` is the older form. No extra
/// endpoint, no guess from locale.
pub fn region_from_access_token(access_token: &str) -> Option<String> {
    let payload = access_token.split('.').nth(1)?;
    let bytes = crate::live_facts::decode_base64(payload)?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.pointer("/pp/c")
        .or_else(|| v.get("c"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
}

/// Region → shard host. Unknown regions fall back to `eu`, exactly like
/// `shardFor` in `src/sources/riot.ts`: a rank read that fails honestly is
/// better than one that addresses a host that never exists.
pub fn shard_for(region: &str) -> String {
    let host = match region.trim().to_lowercase().as_str() {
        "na" => "pd.na.a.pvp.net",
        "ap" => "pd.ap.a.pvp.net",
        "br" => "pd.br.a.pvp.net",
        "latam" => "pd.latam.a.pvp.net",
        "kr" => "pd.kr.a.pvp.net",
        _ => "pd.eu.a.pvp.net",
    };
    host.to_string()
}

/// Parse one `/mmr/v1/players/{puuid}` body.
///
/// Rules (mirroring `parseLocalRank` in `src/sources/riot.ts`):
/// * an error payload is an error — it must never parse into tier 0, because
///   tier 0 renders as "Unrated" and that would be a fabricated rank;
/// * the current act's `RankedRating` is the live RR; LatestCompetitiveUpdate
///   only carries RR as of the last ranked game.
pub fn parse_rank(payload: &Value) -> Result<RankRow, String> {
    if !payload.is_object() {
        return Err("Unreadable rank payload.".to_string());
    }
    if payload.get("httpStatus").is_some()
        || payload.get("statusCode").is_some()
        || payload.get("errorCode").is_some()
    {
        let status = payload
            .get("httpStatus")
            .or_else(|| payload.get("statusCode"))
            .and_then(Value::as_i64);
        return Err(match status {
            Some(code) => format!("Riot refused the rank read (HTTP {code})."),
            None => "Riot refused the rank read.".to_string(),
        });
    }
    if payload.get("QueueSkills").is_none() && payload.get("LatestCompetitiveUpdate").is_none() {
        return Err("Rank payload had no competitive data.".to_string());
    }

    let latest = payload
        .get("LatestCompetitiveUpdate")
        .filter(|v| v.is_object());
    let tier = latest
        .and_then(|l| l.get("TierAfterUpdate"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let season_id = latest
        .and_then(|l| l.get("SeasonID"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let mut rr = latest
        .and_then(|l| l.get("RankedRatingAfterUpdate"))
        .and_then(Value::as_i64)
        .unwrap_or(0);

    let mut peak_tier = 0;
    if let Some(seasons) = payload
        .pointer("/QueueSkills/competitive/SeasonalInfoBySeasonID")
        .and_then(Value::as_object)
    {
        for row in seasons.values() {
            peak_tier = peak_tier.max(season_peak_tier(row));
        }
        if let Some(row) = seasons
            .iter()
            .find(|(id, _)| id.to_lowercase() == season_id)
            .map(|(_, row)| row)
        {
            if let Some(live) = row.get("RankedRating").and_then(Value::as_i64) {
                rr = live;
            }
        }
    }
    // The current tier is itself part of the peak: an act Riot has not written
    // into the season rows yet must not hide a new career best.
    Ok(RankRow {
        tier,
        rr,
        peak_tier: peak_tier.max(tier),
        hidden: payload
            .get("IsActRankBadgeHidden")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// Best tier one act row proves: the end-of-act tier, the act badge rank, or the
/// highest tier that act recorded a win in. Same three-way max as
/// `seasonPeakTier` in `src/sources/riot.ts`; any of them can be the high-water
/// mark for that act.
fn season_peak_tier(row: &Value) -> i64 {
    let end_tier = row
        .get("CompetitiveTier")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let act_rank = row.get("Rank").and_then(Value::as_i64).unwrap_or(0);
    let win_tier = row
        .get("WinsByTier")
        .and_then(Value::as_object)
        .map(|wins| {
            wins.iter()
                .filter(|(_, count)| count.as_i64().unwrap_or(0) > 0)
                .filter_map(|(tier, _)| tier.parse::<i64>().ok())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    end_tier.max(act_rank).max(win_tier)
}

/// Client version for `X-Riot-ClientVersion`, read from the newest game log.
/// Missing log → the fallback constant, never an empty header.
pub fn client_version() -> String {
    let Some(base) = std::env::var_os("LOCALAPPDATA") else {
        return FALLBACK_CLIENT_VERSION.to_string();
    };
    let log = std::path::PathBuf::from(base)
        .join("VALORANT")
        .join("Saved")
        .join("Logs")
        .join("ShooterGame.log");
    let Ok(text) = std::fs::read_to_string(log) else {
        return FALLBACK_CLIENT_VERSION.to_string();
    };
    for line in text.lines().rev() {
        if let Some(rest) = line.split("CI server version:").nth(1) {
            if let Some(version) = rest.split_whitespace().next() {
                if !version.is_empty() {
                    return version.to_string();
                }
            }
        }
    }
    FALLBACK_CLIENT_VERSION.to_string()
}

/// One authenticated GET against a Riot host with the five headers its remote
/// APIs expect. Returns `(status, body)`; `Err` means no response was read, so
/// a transport failure stays distinguishable from a 4xx body.
pub fn remote_get(host: &str, path: &str, ent: &Entitlements) -> Result<(u16, String), String> {
    let version = client_version();
    let headers = vec![
        (
            "Authorization".to_string(),
            format!("Bearer {}", ent.access_token),
        ),
        ("X-Riot-Entitlements-JWT".to_string(), ent.token.to_string()),
        (
            "X-Riot-ClientPlatform".to_string(),
            CLIENT_PLATFORM.to_string(),
        ),
        ("X-Riot-ClientVersion".to_string(), version.clone()),
        (
            "User-Agent".to_string(),
            format!("ShooterGame/{version} Windows/10.0.19042.1.256.64bit"),
        ),
    ];
    let resp = riot_http::send(&Request {
        host,
        port: 443,
        path,
        method: "GET",
        headers: &headers,
        body: None,
        basic_auth: None,
        accept_invalid_certs: false,
        connect_timeout: Duration::from_secs(2),
        max_time: Duration::from_secs(6),
    })
    .map_err(|_| "Could not reach Riot.".to_string())?;
    Ok((resp.status, resp.body))
}

/// One validated rank read. Remote by necessity; called once per launch and on
/// explicit request, never on a timer.
pub fn fetch_rank(puuid: &str, ent: &Entitlements, region: &str) -> Result<RankRow, String> {
    if puuid.trim().is_empty() {
        return Err("Rank read needs a PUUID.".to_string());
    }
    let host = shard_for(region);
    let path = format!("/mmr/v1/players/{puuid}");
    let (status, body) = remote_get(&host, &path, ent)?;
    if status == 401 || status == 403 || riot_http::is_auth_failure(&body) {
        return Err("Riot session expired — restart the Riot Client.".to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("Riot returned HTTP {status} for the rank read."));
    }
    let payload: Value =
        serde_json::from_str(&body).map_err(|_| "Unreadable rank payload.".to_string())?;
    parse_rank(&payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn region_comes_from_the_access_token_claim() {
        // {"pp":{"c":"eu"}} base64url, no padding.
        let token = format!("header.{}.signature", base64url(br#"{"pp":{"c":"eu"}}"#));
        assert_eq!(region_from_access_token(&token).as_deref(), Some("eu"));
        let legacy = format!("header.{}.sig", base64url(br#"{"c":"NA"}"#));
        assert_eq!(region_from_access_token(&legacy).as_deref(), Some("na"));
        assert_eq!(region_from_access_token("not-a-jwt"), None);
        assert_eq!(region_from_access_token(""), None);
    }

    #[test]
    fn every_known_region_maps_to_a_shard_and_unknown_falls_back_to_eu() {
        assert_eq!(shard_for("na"), "pd.na.a.pvp.net");
        assert_eq!(shard_for("EU"), "pd.eu.a.pvp.net");
        assert_eq!(shard_for("latam"), "pd.latam.a.pvp.net");
        assert_eq!(shard_for("nowhere"), "pd.eu.a.pvp.net");
    }

    #[test]
    fn rank_payload_reads_tier_and_the_live_act_rr() {
        let payload = json!({
            "LatestCompetitiveUpdate": {
                "TierAfterUpdate": 21,
                "RankedRatingAfterUpdate": 12,
                "SeasonID": "abc-act-3"
            },
            "QueueSkills": {
                "competitive": {
                    "SeasonalInfoBySeasonID": {
                        "abc-act-3": { "RankedRating": 64, "NumberOfWins": 9 }
                    }
                }
            }
        });
        let row = parse_rank(&payload).unwrap();
        assert_eq!(row.tier, 21);
        // The current act's live RR wins over the last-game snapshot.
        assert_eq!(row.rr, 64);
        // The current act's row (21) is the best one Riot reported here.
        assert_eq!(row.peak_tier, 21);
        assert!(!row.hidden, "the fixture does not hide the badge");
    }

    #[test]
    fn a_hidden_act_badge_is_a_flag_not_unrated() {
        // Riot's own flag: the tier is still reported, the player hid the
        // badge. The board must say HIDDEN, not "Unrated".
        let payload = json!({
            "IsActRankBadgeHidden": true,
            "LatestCompetitiveUpdate": { "TierAfterUpdate": 21, "SeasonID": "s" },
        });
        let row = parse_rank(&payload).unwrap();
        assert_eq!(row.tier, 21);
        assert!(row.hidden);
    }

    #[test]
    fn the_peak_is_the_best_act_riot_reports() {
        // An older act at Immortal 2 (24) outranks the current Diamond 1 (18);
        // a third act only shows its high-water mark through WinsByTier.
        let payload = json!({
            "LatestCompetitiveUpdate": {
                "TierAfterUpdate": 18,
                "RankedRatingAfterUpdate": 40,
                "SeasonID": "act-now"
            },
            "QueueSkills": {
                "competitive": {
                    "SeasonalInfoBySeasonID": {
                        "act-old": { "CompetitiveTier": 24, "RankedRating": 12 },
                        "act-mid": { "CompetitiveTier": 15, "WinsByTier": { "21": 3, "26": 0 } },
                        "act-now": { "CompetitiveTier": 18, "RankedRating": 40 }
                    }
                }
            }
        });
        let row = parse_rank(&payload).unwrap();
        assert_eq!(row.tier, 18);
        // 24 from the old act beats 18 now, and 21 from a won game beats that
        // act's end tier — the max of every evidence Riot reports.
        assert_eq!(row.peak_tier, 24);
        // A tier with zero wins in WinsByTier proves nothing.
        assert_eq!(season_peak_tier(&json!({ "WinsByTier": { "26": 0 } })), 0);
        // No act rows at all: the current tier is still the peak.
        let bare =
            parse_rank(&json!({ "LatestCompetitiveUpdate": { "TierAfterUpdate": 9 } })).unwrap();
        assert_eq!(bare.peak_tier, 9);
    }

    #[test]
    fn rank_error_payload_is_an_error_never_unrated() {
        let err = parse_rank(&json!({
            "httpStatus": 403,
            "errorCode": "FORBIDDEN",
            "message": "Forbidden"
        }))
        .unwrap_err();
        assert!(err.contains("403"));
        // No competitive data at all is also an error, not tier 0.
        assert!(parse_rank(&json!({})).is_err());
        assert!(parse_rank(&json!([])).is_err());
    }

    #[test]
    fn a_valid_never_ranked_payload_is_unrated_not_an_error() {
        let row = parse_rank(&json!({
            "LatestCompetitiveUpdate": {},
            "QueueSkills": { "competitive": null }
        }))
        .unwrap();
        assert_eq!(row.tier, 0);
        assert_eq!(row.rr, 0);
        assert_eq!(row.peak_tier, 0);
    }

    /// Minimal URL-safe base64 encoder for fixture tokens (test-only).
    fn base64url(input: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in input.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[(n >> 6) as usize & 63] as char);
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[n as usize & 63] as char);
            }
        }
        out
    }
}
