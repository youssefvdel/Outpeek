//! Keyless Tracker Network client: the direct transport the overlay's RE proved
//! (`legacy/src-tauri/src/trn_http.rs` is the Tauri-side twin).
//!
//! What the RE established, and what this module relies on:
//! * `api.tracker.network` accepts the profile route with **no API key and no
//!   cookies** when the request carries `User-Agent: OverwolfClient` — every
//!   other UA is refused;
//! * the payload is selected by `?source=overwolf-2`, which is what makes TRN
//!   return the full statv2 profile (season + agent + map segments) instead of a
//!   trimmed page;
//! * the handle is a URL path segment, so `#` must arrive as `%23` and a space
//!   as `%20` — the handle itself is never put on the wire raw.
//!
//! Scope rules:
//! * one host, one path prefix (`/api/v2/valorant/standard/`). This transport
//!   cannot be pointed at another TRN surface by a caller, and it follows no
//!   redirects (a redirect would be a second, un-reviewed host);
//! * 15 s end-to-end timeout, 8 MB body cap, and no body ever logged — a
//!   profile body carries another player's data;
//! * error strings mirror `trn_http_rules::status_error` exactly, so the TS
//!   policy ladder and this app classify the same failure the same way.

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

/// The only host this transport may talk to.
pub const HOST: &str = "api.tracker.network";
/// The RE'd User-Agent. Without it TRN answers 403 to every profile route.
pub const USER_AGENT: &str = "OverwolfClient";
/// The query parameter that selects the full statv2 payload.
pub const SOURCE: &str = "overwolf-2";
/// Every path this client may request starts here.
pub const PROFILE_ROOT: &str = "/api/v2/valorant/standard/";
/// Platform segment for Riot accounts.
pub const PLATFORM: &str = "riot";
/// Same path cap the Tauri transport enforces.
const MAX_PATH: usize = 300;
/// Refuse to parse a body larger than this. The route's real payload is ~1.8 MB;
/// 8 MB leaves headroom for a much larger account without accepting a hostile
/// body into memory twice.
pub const MAX_BODY: usize = 8 * 1024 * 1024;
/// One profile read. The route answers in a second or two; 15 s is the ceiling
/// a user will wait before the poller moves on.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// RFC 3986 unreserved characters stay literal; everything else (space, `#`,
/// `/`, UTF-8 bytes) is percent-encoded. `lil ga7ed#zngr` → `lil%20ga7ed%23zngr`,
/// the exact segment the live probe used.
pub fn encode_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for byte in value.trim().as_bytes() {
        let b = *byte;
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// Append `source=overwolf-2` exactly once: an existing `source=` (any value) is
/// replaced, not duplicated, and other query parameters keep their order.
pub fn append_source(path: &str) -> String {
    let (base, query) = match path.split_once('?') {
        Some((base, query)) => (base, query),
        None => (path, ""),
    };
    let mut params: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty() && !p.starts_with("source="))
        .collect();
    let source = format!("source={SOURCE}");
    params.push(&source);
    format!("{base}?{}", params.join("&"))
}

/// The one profile path this client builds. Rejects an empty handle — a URL for
/// nobody is not a request worth sending.
pub fn profile_path(handle: &str) -> Result<String, String> {
    let handle = handle.trim();
    if handle.is_empty() {
        return Err("TRN_HTTP_BAD_PATH".to_string());
    }
    let path = format!(
        "{PROFILE_ROOT}profile/{PLATFORM}/{}",
        encode_segment(handle)
    );
    Ok(append_source(&path))
}

/// Strict path allowlist: root-relative, under [`PROFILE_ROOT`], no scheme, no
/// protocol-relative `//`, no dot segments, no whitespace, ≤300 bytes.
pub fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > MAX_PATH {
        return Err("TRN_HTTP_BAD_PATH".to_string());
    }
    let path_only = path.split_once('?').map(|(p, _)| p).unwrap_or(path);
    if !path_only.starts_with(PROFILE_ROOT) {
        return Err("TRN_HTTP_BAD_PATH".to_string());
    }
    if path_only.starts_with("//") || path.contains("://") {
        return Err("TRN_HTTP_BAD_PATH".to_string());
    }
    if path.contains([' ', '\n', '\r', '\t']) {
        return Err("TRN_HTTP_BAD_PATH".to_string());
    }
    if path.contains("..") || path.split('/').any(|seg| seg == ".") {
        return Err("TRN_HTTP_BAD_PATH".to_string());
    }
    Ok(())
}

/// Status → error surface, identical to the Tauri transport's contract
/// (`TRN_PRIVATE` 451, `TRN_NO_DATA` 404, `TRN_RATE_LIMITED` 429, `HTTP {n}`).
/// `None` means "there is a body to read".
pub fn status_error(status: u16) -> Option<String> {
    match status {
        200..=299 => None,
        451 => Some("TRN_PRIVATE".to_string()),
        404 => Some("TRN_NO_DATA".to_string()),
        429 => Some("TRN_RATE_LIMITED".to_string()),
        other => Some(format!("HTTP {other}")),
    }
}

/// One profile read, parsed. Remote by necessity; called on start, on an
/// account change, and on explicit refresh — never on the 2.5 s live tick.
///
/// A transport failure (no HTTP response at all) is `TRN_HTTP_ERROR`; a status
/// failure is the mapping above; a body that is not JSON is
/// `TRN_HTTP_BAD_BODY`. Bodies are never logged.
pub async fn fetch_profile(handle: &str) -> Result<Value, String> {
    let path = profile_path(handle)?;
    fetch_json(&path).await
}

/// Path of the recent-matches endpoint for `handle`.
pub fn matches_path(handle: &str) -> Result<String, String> {
    let path = append_source(&format!(
        "/api/v2/valorant/standard/matches/riot/{}",
        encode_segment(handle)
    ));
    validate_path(&path)?;
    Ok(path)
}

/// One matches read, parsed (limit 20 as sent by the endpoint).
pub async fn fetch_matches(handle: &str) -> Result<Value, String> {
    let path = matches_path(handle)?;
    fetch_json(&path).await
}

/// Path of one match's scoreboard. The id is a TRN uuid: hex + dashes only,
/// so anything else (a slash, `..`, whitespace) is a bad path, never a
/// request. It lives under [`PROFILE_ROOT`], so the allowlist below accepts
/// it with no new surface.
pub fn match_path(id: &str) -> Result<String, String> {
    let id = id.trim();
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("TRN_HTTP_BAD_PATH".to_string());
    }
    let path = append_source(&format!("{PROFILE_ROOT}matches/{id}"));
    validate_path(&path)?;
    Ok(path)
}

/// One scoreboard read, parsed by `trn_match_detail::parse`. Memory-only: the
/// body (~1.5 MB live) is never cached on disk; every open refetches.
pub async fn fetch_match(id: &str) -> Result<Value, String> {
    let path = match_path(id)?;
    fetch_json(&path).await
}

/// One client for the life of the process: rustls TLS, HTTP/2 via ALPN, the
/// OS-independent fingerprint TRN's edge accepts. Built once; a failure to
/// build is a transport error, never a panic.
fn client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(TIMEOUT)
                .connect_timeout(Duration::from_secs(5))
                // No redirects: a redirect would be a second, un-reviewed host.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| format!("TRN_HTTP_ERROR: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Shared GET for every TRN endpoint: allowlisted path, Overwolf UA, bounded
/// body, no logging of bodies.
async fn fetch_json(path: &str) -> Result<Value, String> {
    validate_path(path)?;
    let response = client()?
        .get(format!("https://{HOST}{path}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| {
            // reqwest's Display carries the URL and the transport detail. The
            // UI says one sentence; the log keeps the rest for a bug report.
            eprintln!("[trn] transport: {e}");
            "Tracker Network unreachable".to_string()
        })?;
    if let Some(code) = status_error(response.status().as_u16()) {
        return Err(code);
    }
    let body = response.bytes().await.map_err(|e| {
        eprintln!("[trn] body: {e}");
        "Tracker Network cut the response short".to_string()
    })?;
    if body.len() > MAX_BODY {
        return Err(format!(
            "TRN_HTTP_BODY_TOO_LARGE: {} bytes > {MAX_BODY}",
            body.len()
        ));
    }
    serde_json::from_slice(&body).map_err(|_| "TRN_HTTP_BAD_BODY".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handle_is_percent_encoded_for_the_path_segment() {
        assert_eq!(encode_segment("lil ga7ed#zngr"), "lil%20ga7ed%23zngr");
        assert_eq!(encode_segment("  lil ga7ed#zngr  "), "lil%20ga7ed%23zngr");
        assert_eq!(encode_segment("Plain-Name_1.2~"), "Plain-Name_1.2~");
        // UTF-8 bytes are encoded one at a time, not mangled as chars.
        assert_eq!(
            encode_segment("ناروتو#123"),
            "%D9%86%D8%A7%D8%B1%D9%88%D8%AA%D9%88%23123"
        );
        assert_eq!(encode_segment("a/b?c&d"), "a%2Fb%3Fc%26d");
    }

    #[test]
    fn the_proven_url_is_exactly_what_this_builder_produces() {
        assert_eq!(
            profile_path("lil ga7ed#zngr").unwrap(),
            "/api/v2/valorant/standard/profile/riot/lil%20ga7ed%23zngr?source=overwolf-2"
        );
        assert!(profile_path("   ").is_err());
    }

    #[test]
    fn source_is_appended_exactly_once() {
        assert_eq!(append_source("/x"), "/x?source=overwolf-2");
        assert_eq!(
            append_source("/x?source=overwolf-2"),
            "/x?source=overwolf-2"
        );
        // A different source value is replaced, not kept beside ours.
        assert_eq!(append_source("/x?source=browser"), "/x?source=overwolf-2");
        assert_eq!(
            append_source("/x?a=1&source=browser&b=2"),
            "/x?a=1&b=2&source=overwolf-2"
        );
        assert_eq!(append_source("/x?"), "/x?source=overwolf-2");
        let twice = append_source(&append_source("/x"));
        assert_eq!(twice.matches("source=").count(), 1);
    }

    #[test]
    fn the_path_allowlist_refuses_every_other_surface() {
        let good = profile_path("lil ga7ed#zngr").unwrap();
        assert!(validate_path(&good).is_ok());
        for bad in [
            "",
            "/api/v2/league-of-legends/standard/profile/riot/x",
            "/api/v1/valorant/standard/profile/riot/x",
            "https://api.tracker.network/api/v2/valorant/standard/profile/riot/x",
            "//api.tracker.network/api/v2/valorant/standard/profile/riot/x",
            "/api/v2/valorant/standard/profile/riot/../admin",
            "/api/v2/valorant/standard/profile/riot/a b",
            "/api/v2/valorant/standard/./profile",
        ] {
            assert!(validate_path(bad).is_err(), "must refuse {bad:?}");
        }
        // Length is capped before anything else looks at the string.
        let long = format!("{PROFILE_ROOT}{}", "a".repeat(MAX_PATH));
        assert!(validate_path(&long).is_err());
    }

    #[test]
    fn status_mapping_is_the_shared_contract() {
        assert_eq!(status_error(200), None);
        assert_eq!(status_error(204), None);
        assert_eq!(status_error(299), None);
        assert_eq!(status_error(451).as_deref(), Some("TRN_PRIVATE"));
        assert_eq!(status_error(404).as_deref(), Some("TRN_NO_DATA"));
        assert_eq!(status_error(429).as_deref(), Some("TRN_RATE_LIMITED"));
        assert_eq!(status_error(403).as_deref(), Some("HTTP 403"));
        assert_eq!(status_error(500).as_deref(), Some("HTTP 500"));
        assert_eq!(status_error(302).as_deref(), Some("HTTP 302"));
    }

    #[test]
    fn the_match_path_is_the_proven_scoreboard_route() {
        assert_eq!(
            match_path("cadcc206-9d07-4695-b792-4215ec6ef341").unwrap(),
            "/api/v2/valorant/standard/matches/cadcc206-9d07-4695-b792-4215ec6ef341?source=overwolf-2"
        );
        for bad in ["", "../admin", "a/b", "a b", "a?b", "x".repeat(65).as_str()] {
            assert!(match_path(bad).is_err(), "must refuse {bad:?}");
        }
    }

    #[test]
    fn the_transport_constants_are_the_re_findings() {
        assert_eq!(HOST, "api.tracker.network");
        assert_eq!(USER_AGENT, "OverwolfClient");
        assert_eq!(SOURCE, "overwolf-2");
        assert_eq!(TIMEOUT, Duration::from_secs(15));
        assert_eq!(MAX_BODY, 8 * 1024 * 1024);
    }
}
