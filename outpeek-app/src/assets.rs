//! Real Riot game art: rank crests and agent portraits, fetched at runtime.
//!
//! Nothing Riot owns is committed to this repository. The art is read from
//! valorant-api.com (the same public mirror the React app used), cached in the
//! user's own app-data directory, and never re-sold or bundled.
//!
//! Two layers, deliberately separate:
//!
//! 1. [`Index`] — the two JSON catalogues (`competitivetiers`, `agents`). One
//!    request per family, kept in memory for [`META_TTL_MS`] so a 2.5 s poll
//!    never re-downloads a catalogue. Pure parsers, no clock of their own.
//! 2. [`ArtCache`] — the PNG bytes, keyed by URL hash, 30-day TTL, 64 MB cap
//!    with oldest-first eviction. Writes are atomic (temp + rename), and a
//!    fetched body must actually be a PNG before it is stored: an HTML error
//!    page cached under `.png` would poison the entry for a month.
//!
//! Fail-soft contract: no art is a *state*, never an error. [`ArtView`] carries
//! an empty URL when the bytes are not on disk, and the UI draws its own
//! fallback glyph. A request failure is logged as URL + status only — never a
//! body, which is untrusted remote content.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::riot_http::{self, Request};

/// Competitive tier sets. The newest set carries the tier ids Riot still uses.
pub const TIERS_URL: &str = "https://valorant-api.com/v1/competitivetiers";
/// Playable agents only: the URL filter and the `isPlayableCharacter` field are
/// both checked, so a non-playable entry can never reach the roster.
pub const AGENTS_URL: &str = "https://valorant-api.com/v1/agents?isPlayableCharacter=true";
/// Map catalogue, used only for `listViewIcon` row art. Tracker Network's own
/// map URL is a 3840×2160 splash (Lotus 443 KB JPEG, Summit 4.4 MB PNG) — past
/// the art budget and the wrong aspect for a row — so the small official card
/// art fills the Maps page instead.
pub const MAPS_URL: &str = "https://valorant-api.com/v1/maps";

/// The only hosts this app will fetch row art from. Agent portraits travel with
/// the TRN payload: `titles.trackercdn.com` is its valorant-api mirror, and
/// `trackercdn.com` carries the `_thumb` art for an agent the mirror does not
/// list yet (Hush today). A URL outside this list is refused before any
/// request: the payload is remote data and must not be able to name a host.
pub const ART_HOSTS: &[&str] = &[
    "media.valorant-api.com",
    "titles.trackercdn.com",
    "trackercdn.com",
];

/// How long a cached art file stays usable. Media responses carry
/// `cache-control: public, max-age=1209600` (14 days); 30 days is the loose
/// upper bound this cache allows itself, so an act boundary cannot leave the
/// client re-downloading the same crests on every launch.
pub const ART_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Catalogue JSON is re-read at most once a day, per family.
pub const META_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// Total on-disk budget for crests + portraits.
pub const CACHE_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// A crest or portrait is tens of KB; anything this large is not art.
pub const MAX_ART_BYTES: usize = 4 * 1024 * 1024;
/// Connect/read budget for one art request. The catalogue JSON is ~190 KB in
/// total, so a slow link still finishes well inside this.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// valorant-api.com serves a public CDN; it needs no credentials. Sent anyway so
/// the request identifies itself rather than looking like a scanner.
const USER_AGENT: &str = concat!("Outpeek/", env!("CARGO_PKG_VERSION"));

// ---------------------------------------------------------------------------
// Catalogue shapes (pure)
// ---------------------------------------------------------------------------

/// One competitive tier from the newest tier set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tier {
    pub tier: i64,
    /// `divisionName` as the API spells it (`IRON`, `RADIANT`, `UNRANKED`).
    pub division: String,
    /// Empty when the tier set has no art for it (`Unused1`, `Unused2`).
    pub small_icon: String,
    pub large_icon: String,
}

/// One playable agent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentArt {
    /// Lowercased: Riot sends mixed case across endpoints and match payloads.
    pub uuid: String,
    pub name: String,
    /// `role.displayName` (`Duelist`, …); empty when the payload has no role.
    pub role: String,
    /// `displayIcon`, empty when the agent has no portrait.
    pub icon: String,
}

/// Parse `/v1/competitivetiers`, keeping only the newest tier set.
///
/// "Newest" is decided by content, not array order: the set whose highest tier
/// id is greatest wins, ties going to the later entry. Today that is the 28-tier
/// set (0–27) the React app picked as `sets[length - 1]`; deriving it means a
/// reordered response cannot silently downgrade every crest.
pub fn parse_tiers(json: &str) -> Result<Vec<Tier>, String> {
    let root: Value =
        serde_json::from_str(json).map_err(|_| "Unreadable tier payload.".to_string())?;
    let sets = root
        .get("data")
        .and_then(Value::as_array)
        .ok_or("Tier payload has no data array.")?;
    let mut best: Option<(i64, &Vec<Value>)> = None;
    for set in sets {
        let Some(tiers) = set.get("tiers").and_then(Value::as_array) else {
            continue;
        };
        let highest = tiers
            .iter()
            .filter_map(|t| t.get("tier").and_then(Value::as_i64))
            .max()
            .unwrap_or(-1);
        // `>=` keeps the later set on a tie: later = newer.
        if best.map_or(true, |(seen, _)| highest >= seen) {
            best = Some((highest, tiers));
        }
    }
    let (_, tiers) = best.ok_or("Tier payload had no tier set.")?;
    Ok(tiers
        .iter()
        .filter_map(|t| {
            let tier = t.get("tier").and_then(Value::as_i64)?;
            let text = |key: &str| {
                t.get(key)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };
            Some(Tier {
                tier,
                division: text("divisionName"),
                small_icon: text("smallIcon"),
                large_icon: text("largeIcon"),
            })
        })
        .collect())
}

/// One map from `/v1/maps`, reduced to what row art needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MapArt {
    /// Normalised display name (`lotus`, `split`, `summit`): the TRN segment key
    /// is the same codename, so a match needs no act-specific table.
    pub key: String,
    pub name: String,
    /// `listViewIcon` (456×100, ~62 KB), empty when the map has no card art.
    pub icon: String,
}

/// Lowercase, alphanumerics only. "Split" and "split" are one map; a stored
/// key with punctuation cannot miss because of a space.
fn normalize_map_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Parse `/v1/maps`. Rows without a name or card art are dropped: an entry the
/// page cannot draw is not a map row.
pub fn parse_maps(json: &str) -> Result<Vec<MapArt>, String> {
    let root: Value =
        serde_json::from_str(json).map_err(|_| "Unreadable map payload.".to_string())?;
    let rows = root
        .get("data")
        .and_then(Value::as_array)
        .ok_or("Map payload has no data array.")?;
    Ok(rows
        .iter()
        .filter_map(|m| {
            let name = m
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let icon = m
                .get("listViewIcon")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let key = normalize_map_key(&name);
            if name.is_empty() || key.is_empty() || icon.is_empty() {
                return None;
            }
            Some(MapArt { key, name, icon })
        })
        .collect())
}

/// Card art for a map by its codename or display name ("lotus"/"Lotus").
pub fn map_icon_url<'a>(maps: &'a [MapArt], name_or_key: &str) -> Option<&'a str> {
    let wanted = normalize_map_key(name_or_key);
    if wanted.is_empty() {
        return None;
    }
    maps.iter()
        .find(|m| m.key == wanted)
        .map(|m| m.icon.as_str())
        .filter(|url| !url.is_empty())
}

/// HTTPS and a host this module named itself. Anything else is refused before
/// a request is attempted, so a payload cannot point the cache at a new host.
pub fn art_url_allowed(url: &str) -> bool {
    split_https_url(url)
        .map(|(host, _)| ART_HOSTS.contains(&host))
        .unwrap_or(false)
}

/// Parse `/v1/agents?isPlayableCharacter=true`. Entries that are not playable,
/// unnamed, or have no uuid are dropped rather than half-rendered.
pub fn parse_agents(json: &str) -> Result<Vec<AgentArt>, String> {
    let root: Value =
        serde_json::from_str(json).map_err(|_| "Unreadable agent payload.".to_string())?;
    let rows = root
        .get("data")
        .and_then(Value::as_array)
        .ok_or("Agent payload has no data array.")?;
    Ok(rows
        .iter()
        .filter_map(|a| {
            // Missing field: the URL already filtered, so trust the row.
            let playable = a
                .get("isPlayableCharacter")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if !playable {
                return None;
            }
            let uuid = a.get("uuid").and_then(Value::as_str)?.trim().to_lowercase();
            let name = a
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if uuid.is_empty() || name.is_empty() {
                return None;
            }
            Some(AgentArt {
                uuid,
                name,
                role: a
                    .pointer("/role/displayName")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                icon: a
                    .get("displayIcon")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            })
        })
        .collect())
}

/// Crest URL for a tier id, or `None` when that tier has no art (Unused1/2,
/// or an id newer than the cached set).
pub fn tier_icon_url(tiers: &[Tier], tier: i64) -> Option<&str> {
    let row = tiers.iter().find(|t| t.tier == tier)?;
    // `largeIcon` is what the React app rendered; `smallIcon` is the same art at
    // a lower resolution, used only when the large one is missing.
    if !row.large_icon.is_empty() {
        Some(&row.large_icon)
    } else if !row.small_icon.is_empty() {
        Some(&row.small_icon)
    } else {
        None
    }
}

/// Portrait URL for an agent uuid (case-insensitive), or `None` when the uuid is
/// not in the cached catalogue.
pub fn agent_icon_url<'a>(agents: &'a [AgentArt], uuid: &str) -> Option<&'a str> {
    let wanted = uuid.trim().to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    agents
        .iter()
        .find(|a| a.uuid == wanted)
        .map(|a| a.icon.as_str())
        .filter(|url| !url.is_empty())
}

/// The agent row for a uuid, for the name and role chip that go with the art.
pub fn agent_for<'a>(agents: &'a [AgentArt], uuid: &str) -> Option<&'a AgentArt> {
    let wanted = uuid.trim().to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    agents.iter().find(|a| a.uuid == wanted)
}

/// `https://host/path?query` → (`host`, `/path?query`). Pure and strict: the art
/// cache only ever speaks HTTPS to a host it named itself, so a malformed URL
/// must fail here instead of reaching the transport.
pub fn split_https_url(url: &str) -> Option<(&str, String)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        // A bare host is a request for `/`.
        None => (rest, "/"),
    };
    if host.is_empty() || host.contains([' ', '\r', '\n']) {
        return None;
    }
    Some((host, path.to_string()))
}

// ---------------------------------------------------------------------------
// Transport (injectable)
// ---------------------------------------------------------------------------

/// The one thing this module needs from the network. A trait so the tests can
/// drive a cache without a socket, and so the app keeps exactly one HTTP
/// transport (`riot_http`).
pub trait Fetcher: Send + Sync {
    /// Whole body of `url`, or a message with the URL/status only — never a
    /// body, which is untrusted remote content.
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// HTTPS through [`riot_http`]'s native-tls transport (Schannel on Windows).
pub struct HttpFetcher;

impl Fetcher for HttpFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let Some((host, path)) = split_https_url(url) else {
            return Err(format!("not an https URL: {url}"));
        };
        let headers = [("User-Agent".to_string(), USER_AGENT.to_string())];
        let resp = riot_http::send_bytes(&Request {
            host,
            port: 443,
            path: &path,
            method: "GET",
            headers: &headers,
            body: None,
            basic_auth: None,
            accept_invalid_certs: false,
            connect_timeout: CONNECT_TIMEOUT,
            max_time: REQUEST_TIMEOUT,
        })?;
        if resp.status != 200 {
            return Err(format!("HTTP {}", resp.status));
        }
        if resp.body.len() > MAX_ART_BYTES {
            return Err(format!("body too large ({} bytes)", resp.body.len()));
        }
        Ok(resp.body)
    }
}

/// Run one blocking fetch off the poller's runtime thread.
async fn fetch(fetcher: Arc<dyn Fetcher>, url: String) -> Result<Vec<u8>, String> {
    match tokio::task::spawn_blocking(move || fetcher.get(&url)).await {
        Ok(result) => result,
        Err(_) => Err("fetch task failed".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Catalogue index (24 h in-memory TTL)
// ---------------------------------------------------------------------------

/// What one [`Index::ensure`] call did, for the log line that proves it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetaReport {
    pub tiers: usize,
    pub agents: usize,
    pub maps: usize,
    /// Families actually fetched this call (0 or 1 each): 0 means the in-memory
    /// copy was still inside its TTL, i.e. a poll did not re-download.
    pub fetched: u8,
    /// First failure, or empty. A failure never clears a good catalogue.
    pub error: String,
}

#[derive(Debug, Clone)]
struct Timed<T> {
    at_ms: u64,
    value: T,
}

impl<T> Timed<T> {
    fn fresh(&self, now_ms: u64, ttl_ms: u64) -> bool {
        now_ms.saturating_sub(self.at_ms) < ttl_ms
    }
}

/// The in-memory catalogues. Owns no clock: every call takes `now_ms`, which is
/// what makes the TTL testable without sleeping.
#[derive(Debug, Default)]
pub struct Index {
    ttl_ms: u64,
    tiers: Option<Timed<Vec<Tier>>>,
    agents: Option<Timed<Vec<AgentArt>>>,
    maps: Option<Timed<Vec<MapArt>>>,
}

impl Index {
    pub fn new() -> Self {
        Self {
            ttl_ms: META_TTL_MS,
            tiers: None,
            agents: None,
            maps: None,
        }
    }

    /// A copy with a different TTL, for tests and for a future shorter refresh.
    pub fn with_ttl(ttl_ms: u64) -> Self {
        Self {
            ttl_ms,
            ..Self::default()
        }
    }

    pub fn tiers(&self) -> &[Tier] {
        self.tiers
            .as_ref()
            .map(|t| t.value.as_slice())
            .unwrap_or(&[])
    }

    pub fn agents(&self) -> &[AgentArt] {
        self.agents
            .as_ref()
            .map(|t| t.value.as_slice())
            .unwrap_or(&[])
    }

    pub fn maps(&self) -> &[MapArt] {
        self.maps
            .as_ref()
            .map(|t| t.value.as_slice())
            .unwrap_or(&[])
    }

    pub fn has_tiers(&self) -> bool {
        !self.tiers().is_empty()
    }

    /// Fetch whichever family is missing or older than the TTL. Never fails:
    /// a dead network leaves the previous catalogue in place and reports it.
    pub async fn ensure(&mut self, fetcher: Arc<dyn Fetcher>, now_ms: u64) -> MetaReport {
        let mut report = MetaReport::default();
        if !self
            .tiers
            .as_ref()
            .is_some_and(|t| t.fresh(now_ms, self.ttl_ms))
        {
            match fetch(fetcher.clone(), TIERS_URL.to_string()).await {
                Ok(body) => match parse_tiers(&String::from_utf8_lossy(&body)) {
                    Ok(tiers) if !tiers.is_empty() => {
                        self.tiers = Some(Timed {
                            at_ms: now_ms,
                            value: tiers,
                        });
                        report.fetched += 1;
                    }
                    Ok(_) => report.error = "tier payload was empty".to_string(),
                    Err(e) => report.error = e,
                },
                Err(e) => {
                    report.error = format!("tiers: {e}");
                }
            }
        }
        if !self
            .agents
            .as_ref()
            .is_some_and(|a| a.fresh(now_ms, self.ttl_ms))
        {
            match fetch(fetcher.clone(), AGENTS_URL.to_string()).await {
                Ok(body) => match parse_agents(&String::from_utf8_lossy(&body)) {
                    Ok(agents) if !agents.is_empty() => {
                        self.agents = Some(Timed {
                            at_ms: now_ms,
                            value: agents,
                        });
                        report.fetched += 1;
                    }
                    Ok(_) => report.error = "agent payload was empty".to_string(),
                    Err(e) => report.error = e,
                },
                Err(e) => {
                    report.error = format!("agents: {e}");
                }
            }
        }
        if !self
            .maps
            .as_ref()
            .is_some_and(|m| m.fresh(now_ms, self.ttl_ms))
        {
            match fetch(fetcher, MAPS_URL.to_string()).await {
                Ok(body) => match parse_maps(&String::from_utf8_lossy(&body)) {
                    Ok(maps) if !maps.is_empty() => {
                        self.maps = Some(Timed {
                            at_ms: now_ms,
                            value: maps,
                        });
                        report.fetched += 1;
                    }
                    Ok(_) => report.error = "map payload was empty".to_string(),
                    Err(e) => report.error = e,
                },
                Err(e) => {
                    report.error = format!("maps: {e}");
                }
            }
        }
        report.tiers = self.tiers().len();
        report.agents = self.agents().len();
        report.maps = self.maps().len();
        report
    }
}

// ---------------------------------------------------------------------------
// Art bytes cache (30-day TTL, 64 MB, atomic writes)
// ---------------------------------------------------------------------------

/// Where a URL's bytes stand after [`ArtCache::ensure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheState {
    /// A fresh copy was already on disk: no request was made.
    Hit,
    /// Downloaded this call and stored atomically.
    Fetched,
    /// The copy is past its TTL and the refetch failed — still shown, because a
    /// crest does not change under the same URL.
    Stale,
    /// Nothing usable on disk.
    Failed,
}

impl CacheState {
    /// True when the UI has something to draw.
    pub fn usable(self) -> bool {
        matches!(
            self,
            CacheState::Hit | CacheState::Fetched | CacheState::Stale
        )
    }
}

/// Is a file modified at `modified` still inside `ttl` at `now`?
/// A timestamp in the future counts as fresh: clock skew must not cause a
/// re-download loop.
pub fn is_fresh(modified: SystemTime, now: SystemTime, ttl: Duration) -> bool {
    match now.duration_since(modified) {
        Ok(age) => age < ttl,
        Err(_) => true,
    }
}

/// The art bytes, one file per URL, in one directory.
#[derive(Debug, Clone)]
pub struct ArtCache {
    dir: PathBuf,
}

impl ArtCache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `(files, bytes)` currently on disk.
    pub fn usage(&self) -> (usize, u64) {
        outpeek_overlay::image_cache::usage_in(&self.dir)
    }

    /// A fresh cached copy, or `None` when missing/expired/unreadable.
    pub fn fresh(&self, url: &str, now: SystemTime) -> bool {
        if url.is_empty() {
            return false;
        }
        let path = outpeek_overlay::image_cache::path_for_url_in(&self.dir, url);
        match std::fs::metadata(&path) {
            Ok(meta) => {
                let modified = meta.modified().unwrap_or(now);
                is_fresh(modified, now, ART_TTL)
            }
            Err(_) => false,
        }
    }

    /// Make sure `url`'s bytes are on disk. Downloads only when there is no
    /// fresh copy, so a poll that keeps the same rank makes zero requests.
    pub async fn ensure(
        &self,
        fetcher: Arc<dyn Fetcher>,
        url: &str,
        now: SystemTime,
    ) -> CacheState {
        if url.is_empty() {
            return CacheState::Failed;
        }
        if self.fresh(url, now) {
            return CacheState::Hit;
        }
        let path = outpeek_overlay::image_cache::path_for_url_in(&self.dir, url);
        let had_copy = path.is_file();
        match fetch(fetcher, url.to_string()).await {
            Ok(bytes) if is_png(&bytes) => {
                match outpeek_overlay::image_cache::store_bytes_in(&self.dir, url, &bytes) {
                    Ok(_) => {
                        self.cap();
                        CacheState::Fetched
                    }
                    Err(e) => {
                        eprintln!("[art] store failed for {url}: {e}");
                        if had_copy {
                            CacheState::Stale
                        } else {
                            CacheState::Failed
                        }
                    }
                }
            }
            Ok(bytes) => {
                // Not art: an HTML error page or a CDN interstitial. Storing it
                // would make the entry unusable for the whole TTL.
                eprintln!("[art] refused {url}: {} bytes, not a PNG", bytes.len());
                if had_copy {
                    CacheState::Stale
                } else {
                    CacheState::Failed
                }
            }
            Err(e) => {
                // URL + status only. The body is remote, untrusted content.
                eprintln!("[art] GET {url} failed: {e}");
                if had_copy {
                    CacheState::Stale
                } else {
                    CacheState::Failed
                }
            }
        }
    }

    /// Evict oldest-first until the directory fits [`CACHE_MAX_BYTES`].
    pub fn cap(&self) -> u64 {
        outpeek_overlay::image_cache::evict_lru_in(&self.dir, CACHE_MAX_BYTES)
    }
}

/// A PNG starts with this signature. Anything else is not art.
fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() > 8
}

// ---------------------------------------------------------------------------
// Worker-facing facade
// ---------------------------------------------------------------------------

/// One frame of art for the tracker UI. URLs, not images: `slint::Image` is not
/// `Send`, so it can only be built on the UI thread from files this module has
/// already cached. An empty URL means "no art" and the UI draws its fallback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtView {
    pub rank_crest: String,
    pub peak_crest: String,
    pub agent_icon: String,
    pub agent_name: String,
    pub agent_role: String,
}

/// What the worker asks the cache for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtRequest {
    /// Current competitive tier (`/mmr` TierAfterUpdate). 0 = none.
    pub tier: i64,
    /// Best tier across the acts Riot reports. 0 = none.
    pub peak_tier: i64,
    /// Agent uuid, when a read produced one.
    pub agent_uuid: Option<String>,
}

/// The catalogue plus the bytes cache: everything the poller needs for art.
#[derive(Debug)]
pub struct Art {
    dir: Option<PathBuf>,
    cache: Option<ArtCache>,
    index: Index,
}

impl Art {
    /// `dir` is `%APPDATA%\Outpeek\cache\art`; `None` disables art entirely
    /// (no app-data directory on this host), which is a supported state.
    pub fn new(dir: Option<PathBuf>) -> Self {
        let cache = dir.clone().map(ArtCache::new);
        Self {
            dir,
            cache,
            index: Index::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.cache.is_some()
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Enforce the on-disk budget once (called at poller start, so a previous
    /// run that died over budget cannot leave it there). Returns bytes freed.
    pub fn cap(&self) -> u64 {
        self.cache.as_ref().map(ArtCache::cap).unwrap_or(0)
    }

    /// `(files, bytes)` on disk, for the startup log line.
    pub fn usage(&self) -> (usize, u64) {
        self.cache.as_ref().map(ArtCache::usage).unwrap_or((0, 0))
    }

    /// The catalogue, refreshed when stale. Reported for one log line.
    pub async fn ensure_catalogue(&mut self, fetcher: Arc<dyn Fetcher>, now_ms: u64) -> MetaReport {
        self.index.ensure(fetcher, now_ms).await
    }

    /// Cache one row-art URL and return it when the bytes are on disk. The URL
    /// may come from the TRN payload, so the host allowlist is enforced here —
    /// before a request, not after. Missing art is `None`, never an error.
    pub async fn ensure_url(
        &self,
        fetcher: Arc<dyn Fetcher>,
        url: &str,
        now: SystemTime,
    ) -> Option<String> {
        let cache = self.cache.as_ref()?;
        if url.is_empty() {
            return None;
        }
        if !art_url_allowed(url) {
            eprintln!("[art] refused host for row art: {url}");
            return None;
        }
        if cache.ensure(fetcher, url, now).await.usable() {
            Some(url.to_string())
        } else {
            None
        }
    }

    /// Map card art for a codename (`lotus`), resolved from the catalogue.
    pub async fn map_icon(
        &self,
        fetcher: Arc<dyn Fetcher>,
        key: &str,
        now: SystemTime,
    ) -> Option<String> {
        let url = map_icon_url(self.index.maps(), key)?.to_string();
        self.ensure_url(fetcher, &url, now).await
    }

    /// Agent portrait for a uuid, resolved from the catalogue. The mirror is the
    /// fallback for rows whose TRN art is absent; most portraits arrive with
    /// the payload and never reach here.
    pub async fn agent_icon(
        &self,
        fetcher: Arc<dyn Fetcher>,
        uuid: &str,
        now: SystemTime,
    ) -> Option<String> {
        let url = agent_icon_url(self.index.agents(), uuid)?.to_string();
        self.ensure_url(fetcher, &url, now).await
    }

    /// Resolve one frame of art. Every step fails soft: a missing catalogue or a
    /// dead network yields an empty URL for that slot and nothing else.
    pub async fn view(
        &self,
        fetcher: Arc<dyn Fetcher>,
        req: &ArtRequest,
        now: SystemTime,
    ) -> ArtView {
        let Some(cache) = &self.cache else {
            return ArtView::default();
        };
        let mut view = ArtView::default();
        // The catalogue may still be empty (its fetch failed): then no slot can
        // be resolved and the UI keeps its drawn fallbacks.
        let rank_url = tier_icon_url(self.index.tiers(), req.tier).map(str::to_string);
        // Peak art only above the Unrated rows: `peak_tier == 0` means "no act
        // reported a rank", and tier 0's own UNRANKED emblem would then draw a
        // crest the empty peak label contradicts.
        let peak_url = if req.peak_tier < 3 {
            None
        } else if req.peak_tier == req.tier {
            rank_url.clone()
        } else {
            tier_icon_url(self.index.tiers(), req.peak_tier).map(str::to_string)
        };
        if let Some(url) = rank_url {
            if cache.ensure(fetcher.clone(), &url, now).await.usable() {
                view.rank_crest = url;
            }
        }
        if let Some(url) = peak_url {
            if cache.ensure(fetcher.clone(), &url, now).await.usable() {
                view.peak_crest = url;
            }
        }
        if let Some(uuid) = req.agent_uuid.as_deref() {
            if let Some(agent) = agent_for(self.index.agents(), uuid) {
                let icon = agent_icon_url(self.index.agents(), uuid).map(str::to_string);
                if let Some(url) = icon {
                    if cache.ensure(fetcher, &url, now).await.usable() {
                        view.agent_icon = url;
                        view.agent_name = agent.name.clone();
                        view.agent_role = agent.role.clone();
                    }
                }
            }
        }
        view
    }
}

/// `%APPDATA%\Outpeek\cache\art`, created on demand. `None` when this host has
/// no app-data directory — art is then simply unavailable, never an error.
pub fn art_dir() -> Option<PathBuf> {
    let dir = crate::paths::app_data_dir()?.join("cache").join("art");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Two real tier sets, trimmed: the older 25-tier set and the current
    /// 28-tier one, with the icon-less `Unused` rows left in.
    const TIERS_JSON: &str = r#"{"status":200,"data":[
        {"uuid":"old","assetObjectName":"Episode4_CompetitiveTierDataTable","tiers":[
            {"tier":0,"divisionName":"UNRANKED","smallIcon":"https://media/o/0/s.png","largeIcon":"https://media/o/0/l.png"},
            {"tier":24,"divisionName":"IMMORTAL 3","smallIcon":"https://media/o/24/s.png","largeIcon":"https://media/o/24/l.png"}
        ]},
        {"uuid":"new","assetObjectName":"Episode5_CompetitiveTierDataTable","tiers":[
            {"tier":0,"divisionName":"UNRANKED","smallIcon":"https://media/n/0/s.png","largeIcon":"https://media/n/0/l.png"},
            {"tier":1,"divisionName":"Unused1","smallIcon":null,"largeIcon":null},
            {"tier":3,"divisionName":"IRON","smallIcon":"https://media/n/3/s.png","largeIcon":"https://media/n/3/l.png"},
            {"tier":27,"divisionName":"RADIANT","smallIcon":"https://media/n/27/s.png","largeIcon":"https://media/n/27/l.png"}
        ]}
    ]}"#;

    const AGENTS_JSON: &str = r#"{"status":200,"data":[
        {"uuid":"E370FA57-4757-3604-3648-499E1F642D3F","displayName":"Gekko","displayIcon":"https://media/a/gekko.png","isPlayableCharacter":true,"role":{"displayName":"Initiator"}},
        {"uuid":"dade69b4-4f5a-8528-247b-219e5a1facd6","displayName":"Fade","displayIcon":"https://media/a/fade.png","isPlayableCharacter":true,"role":{"displayName":"Initiator"}},
        {"uuid":"00000000-0000-0000-0000-000000000000","displayName":"Training Bot","displayIcon":"","isPlayableCharacter":false,"role":null}
    ]}"#;

    /// The real `/v1/maps` shape, trimmed: card art for every map TRN reports,
    /// plus rows without art (which must be dropped, not half-rendered).
    const MAPS_JSON: &str = r#"{"status":200,"data":[
        {"uuid":"m1","displayName":"Lotus","listViewIcon":"https://media.valorant-api.com/maps/m1/listviewicon.png"},
        {"uuid":"m2","displayName":"Summit","listViewIcon":"https://media.valorant-api.com/maps/m2/listviewicon.png"},
        {"uuid":"m3","displayName":"Skirmish A","listViewIcon":"https://media.valorant-api.com/maps/m3/listviewicon.png"},
        {"uuid":"m4","displayName":"Range","listViewIcon":""}
    ]}"#;

    fn png() -> Vec<u8> {
        use image::ImageEncoder;
        let mut out = Vec::new();
        let img = image::RgbaImage::from_fn(8, 8, |x, _| image::Rgba([x as u8 * 8, 40, 90, 255]));
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), 8, 8, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    /// A fetcher that answers from a table and counts calls per URL.
    struct FakeFetcher {
        bodies: Vec<(String, Result<Vec<u8>, String>)>,
        calls: AtomicUsize,
    }

    impl FakeFetcher {
        fn ok(pairs: &[(&str, Vec<u8>)]) -> Arc<Self> {
            Arc::new(Self {
                bodies: pairs
                    .iter()
                    .map(|(u, b)| (u.to_string(), Ok(b.clone())))
                    .collect(),
                calls: AtomicUsize::new(0),
            })
        }
        /// Every URL answers with a transport failure.
        fn offline() -> Arc<Self> {
            Arc::new(Self {
                bodies: Vec::new(),
                calls: AtomicUsize::new(0),
            })
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    impl Fetcher for FakeFetcher {
        fn get(&self, url: &str) -> Result<Vec<u8>, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.bodies
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, b)| b.clone())
                .unwrap_or_else(|| Err("offline".to_string()))
        }
    }

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("outpeek-art-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // --- URL builders -----------------------------------------------------

    #[test]
    fn the_newest_tier_set_wins_by_content_not_array_order() {
        let tiers = parse_tiers(TIERS_JSON).unwrap();
        // The 28-tier set (0..27) is newer than the 25-tier one.
        assert_eq!(tiers.iter().map(|t| t.tier).max(), Some(27));
        assert_eq!(tier_icon_url(&tiers, 3), Some("https://media/n/3/l.png"));
        assert_eq!(tier_icon_url(&tiers, 27), Some("https://media/n/27/l.png"));
        // An id only the older set had does not leak through.
        assert_eq!(tier_icon_url(&tiers, 24), None);
        // A reordered payload still picks the set with the highest tier id.
        let mut root: Value = serde_json::from_str(TIERS_JSON).unwrap();
        root["data"].as_array_mut().unwrap().reverse();
        let reordered = serde_json::to_string(&root).unwrap();
        let tiers = parse_tiers(&reordered).unwrap();
        assert_eq!(tier_icon_url(&tiers, 3), Some("https://media/n/3/l.png"));
        assert_eq!(tier_icon_url(&tiers, 24), None);
    }

    #[test]
    fn a_tier_without_art_has_no_url() {
        let tiers = parse_tiers(TIERS_JSON).unwrap();
        // Unused1: both icons are null in the live payload.
        assert_eq!(tier_icon_url(&tiers, 1), None);
        // Unknown tier ids are absent, not guessed.
        assert_eq!(tier_icon_url(&tiers, 99), None);
        // smallIcon stands in when the large one is missing.
        let mut rows = tiers.clone();
        rows.iter_mut()
            .find(|t| t.tier == 3)
            .unwrap()
            .large_icon
            .clear();
        assert_eq!(tier_icon_url(&rows, 3), Some("https://media/n/3/s.png"));
        rows.iter_mut()
            .find(|t| t.tier == 3)
            .unwrap()
            .small_icon
            .clear();
        assert_eq!(tier_icon_url(&rows, 3), None);
    }

    #[test]
    fn agent_urls_are_case_insensitive_and_playable_only() {
        let agents = parse_agents(AGENTS_JSON).unwrap();
        assert_eq!(agents.len(), 2, "the non-playable row must be dropped");
        assert_eq!(
            agent_icon_url(&agents, "E370FA57-4757-3604-3648-499E1F642D3F"),
            Some("https://media/a/gekko.png")
        );
        assert_eq!(
            agent_icon_url(&agents, "e370fa57-4757-3604-3648-499e1f642d3f"),
            Some("https://media/a/gekko.png")
        );
        assert_eq!(agent_icon_url(&agents, ""), None);
        assert_eq!(agent_icon_url(&agents, "not-an-agent"), None);
        let gekko = agent_for(&agents, "E370FA57-4757-3604-3648-499E1F642D3F").unwrap();
        assert_eq!(gekko.name, "Gekko");
        assert_eq!(gekko.role, "Initiator");
        // The uuid is normalised on parse, so later lookups cannot disagree.
        assert_eq!(gekko.uuid, "e370fa57-4757-3604-3648-499e1f642d3f");
    }

    #[test]
    fn a_map_resolves_by_codename_and_rows_without_art_are_dropped() {
        let maps = parse_maps(MAPS_JSON).unwrap();
        assert_eq!(maps.len(), 3, "the art-less Range row is not a card");
        // The TRN segment key is the codename; both spellings must resolve.
        assert_eq!(
            map_icon_url(&maps, "lotus"),
            Some("https://media.valorant-api.com/maps/m1/listviewicon.png")
        );
        assert_eq!(
            map_icon_url(&maps, "Lotus"),
            Some("https://media.valorant-api.com/maps/m1/listviewicon.png")
        );
        assert_eq!(
            map_icon_url(&maps, "SUMMIT"),
            Some("https://media.valorant-api.com/maps/m2/listviewicon.png")
        );
        assert_eq!(
            map_icon_url(&maps, "skirmish a"),
            Some("https://media.valorant-api.com/maps/m3/listviewicon.png")
        );
        assert_eq!(map_icon_url(&maps, "range"), None, "no art, no URL");
        assert_eq!(map_icon_url(&maps, ""), None);
        assert_eq!(map_icon_url(&maps, "not-a-map"), None);
    }

    #[test]
    fn only_the_three_art_hosts_are_ever_fetched_from() {
        assert!(art_url_allowed(
            "https://titles.trackercdn.com/valorant-api/agents/abc/displayicon.png"
        ));
        assert!(art_url_allowed(
            "https://trackercdn.com/cdn/tracker.gg/valorant/db/agents/hush_thumb.png"
        ));
        assert!(art_url_allowed(
            "https://media.valorant-api.com/maps/x/listviewicon.png"
        ));
        // A payload naming any other host is refused, not requested.
        assert!(!art_url_allowed("https://evil.example/agent.png"));
        assert!(!art_url_allowed(
            "https://media.valorant-api.com.evil.example/maps/x/listviewicon.png"
        ));
        assert!(!art_url_allowed(
            "http://media.valorant-api.com/maps/x/listviewicon.png"
        ));
        assert!(!art_url_allowed(""));
    }

    #[test]
    fn unreadable_payloads_are_errors_or_empty_never_panics() {
        assert!(parse_tiers("not json").is_err());
        assert!(parse_tiers(r#"{"status":200}"#).is_err());
        assert!(parse_tiers(r#"{"data":[]}"#).is_err());
        assert!(parse_agents("not json").is_err());
        assert!(parse_agents(r#"{"status":200}"#).is_err());
        assert!(parse_maps("not json").is_err());
        assert!(parse_maps(r#"{"status":200}"#).is_err());
        // A tier row without an id is dropped, not defaulted to tier 0.
        let rows = parse_tiers(r#"{"data":[{"tiers":[{"divisionName":"X"}]}]}"#).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn https_urls_split_into_host_and_request_path() {
        assert_eq!(
            split_https_url("https://valorant-api.com/v1/competitivetiers"),
            Some(("valorant-api.com", "/v1/competitivetiers".to_string()))
        );
        // The query is part of the request path, never the host.
        assert_eq!(
            split_https_url("https://valorant-api.com/v1/agents?isPlayableCharacter=true"),
            Some((
                "valorant-api.com",
                "/v1/agents?isPlayableCharacter=true".to_string()
            ))
        );
        assert_eq!(
            split_https_url("https://media.valorant-api.com/agents/x/displayicon.png"),
            Some((
                "media.valorant-api.com",
                "/agents/x/displayicon.png".to_string()
            ))
        );
        assert_eq!(split_https_url("http://insecure.example/x.png"), None);
        assert_eq!(split_https_url("https:///nohost"), None);
        assert_eq!(split_https_url("not a url"), None);
    }

    // --- cache policy -----------------------------------------------------

    #[test]
    fn freshness_follows_the_ttl_and_never_loops_on_clock_skew() {
        let now = SystemTime::now();
        assert!(is_fresh(now, now, ART_TTL));
        let almost = now - (ART_TTL - Duration::from_secs(60));
        assert!(is_fresh(almost, now, ART_TTL));
        let past = now - (ART_TTL + Duration::from_secs(60));
        assert!(!is_fresh(past, now, ART_TTL));
        // A file stamped in the future (mtime restored, DST/UTC skew) counts as
        // fresh: re-downloading in a loop is worse than trusting it.
        let future = now + Duration::from_secs(3600);
        assert!(is_fresh(future, now, ART_TTL));
    }

    #[tokio::test]
    async fn a_cached_url_is_served_without_a_second_request() {
        let tmp = TmpDir::new("hit");
        let cache = ArtCache::new(tmp.path().to_path_buf());
        let url = "https://media/a/crest.png";
        let fetcher = FakeFetcher::ok(&[(url, png())]);
        let now = SystemTime::now();
        assert_eq!(
            cache.ensure(fetcher.clone(), url, now).await,
            CacheState::Fetched
        );
        assert_eq!(fetcher.calls(), 1);
        assert_eq!(
            cache.ensure(fetcher.clone(), url, now).await,
            CacheState::Hit
        );
        assert_eq!(fetcher.calls(), 1, "a cache hit must not re-download");
        // Past the TTL it refetches once. The extra hour keeps the assertion
        // clear of filesystem timestamp granularity (the exact boundary is
        // covered by `freshness_follows_the_ttl...` with synthetic times).
        let later = now + ART_TTL + Duration::from_secs(3600);
        assert_eq!(
            cache.ensure(fetcher.clone(), url, later).await,
            CacheState::Fetched
        );
        assert_eq!(fetcher.calls(), 2);
    }

    #[tokio::test]
    async fn an_offline_fetch_fails_soft_and_keeps_the_stale_copy() {
        let tmp = TmpDir::new("offline");
        let cache = ArtCache::new(tmp.path().to_path_buf());
        let url = "https://media/a/crest.png";
        let now = SystemTime::now();
        // Nothing cached, no network: Failed, and nothing written.
        assert_eq!(
            cache.ensure(FakeFetcher::offline(), url, now).await,
            CacheState::Failed
        );
        assert_eq!(cache.usage(), (0, 0));
        assert!(!cache.fresh(url, now));
        // A copy on disk survives a dead network: stale, still usable.
        cache
            .ensure(FakeFetcher::ok(&[(url, png())]), url, now)
            .await;
        let later = now + ART_TTL + Duration::from_secs(3600);
        assert_eq!(
            cache.ensure(FakeFetcher::offline(), url, later).await,
            CacheState::Stale
        );
        assert!(CacheState::Stale.usable());
        assert_eq!(cache.usage().0, 1);
        // An empty URL never touches the network.
        let fetcher = FakeFetcher::offline();
        assert_eq!(
            cache.ensure(fetcher.clone(), "", now).await,
            CacheState::Failed
        );
        assert_eq!(fetcher.calls(), 0);
    }

    #[tokio::test]
    async fn a_non_png_body_is_refused_instead_of_poisoning_the_entry() {
        let tmp = TmpDir::new("html");
        let cache = ArtCache::new(tmp.path().to_path_buf());
        let url = "https://media/a/crest.png";
        let fetcher = FakeFetcher::ok(&[(url, b"<html>503</html>".to_vec())]);
        assert_eq!(
            cache.ensure(fetcher, url, SystemTime::now()).await,
            CacheState::Failed
        );
        assert_eq!(cache.usage(), (0, 0), "an error page must not be stored");
    }

    #[tokio::test]
    async fn the_cache_cap_evicts_oldest_first() {
        let tmp = TmpDir::new("cap");
        let cache = ArtCache::new(tmp.path().to_path_buf());
        let now = SystemTime::now();
        // Two files, then force the cap: evict_lru_in is the same call `cap`
        // makes, with a budget this test can actually reach.
        for name in ["a", "b"] {
            let url = format!("https://media/{name}.png");
            cache
                .ensure(FakeFetcher::ok(&[(&url, png())]), &url, now)
                .await;
        }
        let (files, bytes) = cache.usage();
        assert_eq!(files, 2);
        let freed = outpeek_overlay::image_cache::evict_lru_in(&cache.dir, bytes - 1);
        assert_eq!(freed, bytes / 2, "one file, oldest first");
        assert_eq!(cache.usage().0, 1);
    }

    #[test]
    fn the_meta_ttl_keeps_a_catalogue_in_memory_for_a_day() {
        let mut index = Index::with_ttl(META_TTL_MS);
        let fetcher = FakeFetcher::ok(&[
            (TIERS_URL, TIERS_JSON.into()),
            (AGENTS_URL, AGENTS_JSON.into()),
            (MAPS_URL, MAPS_JSON.into()),
        ]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let first = rt.block_on(index.ensure(fetcher.clone(), 1_000));
        assert_eq!(first.fetched, 3);
        assert_eq!(first.tiers, 4);
        assert_eq!(first.agents, 2);
        assert_eq!(first.maps, 3);
        assert!(first.error.is_empty());
        // Inside the TTL: no request at all, same catalogue.
        let second = rt.block_on(index.ensure(fetcher.clone(), 1_000 + META_TTL_MS - 1));
        assert_eq!(second.fetched, 0);
        assert_eq!(second.tiers, 4);
        assert_eq!(fetcher.calls(), 3);
        // Past it: one refetch per family.
        let third = rt.block_on(index.ensure(fetcher.clone(), 1_000 + META_TTL_MS));
        assert_eq!(third.fetched, 3);
        assert_eq!(fetcher.calls(), 6);
    }

    #[test]
    fn a_failed_catalogue_fetch_keeps_the_previous_one() {
        let mut index = Index::with_ttl(META_TTL_MS);
        let good = FakeFetcher::ok(&[
            (TIERS_URL, TIERS_JSON.into()),
            (AGENTS_URL, AGENTS_JSON.into()),
            (MAPS_URL, MAPS_JSON.into()),
        ]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(index.ensure(good, 1_000));
        assert!(index.has_tiers());
        let report = rt.block_on(index.ensure(FakeFetcher::offline(), 1_000 + META_TTL_MS));
        assert_eq!(report.tiers, 4, "the previous catalogue must survive");
        assert!(!report.error.is_empty(), "the failure must be reported");
        assert_eq!(report.fetched, 0);
    }

    // --- end-to-end facade (what the worker calls) ------------------------

    #[tokio::test]
    async fn the_view_fills_every_slot_it_can_and_leaves_the_rest_empty() {
        let tmp = TmpDir::new("view");
        let mut art = Art::new(Some(tmp.path().to_path_buf()));
        assert!(art.enabled());
        let fetcher = FakeFetcher::ok(&[
            (TIERS_URL, TIERS_JSON.into()),
            (AGENTS_URL, AGENTS_JSON.into()),
            (MAPS_URL, MAPS_JSON.into()),
            ("https://media/n/3/l.png", png()),
            ("https://media/n/27/l.png", png()),
            ("https://media/a/gekko.png", png()),
        ]);
        let report = art.ensure_catalogue(fetcher.clone(), 1_000).await;
        assert_eq!((report.tiers, report.agents), (4, 2));
        let view = art
            .view(
                fetcher,
                &ArtRequest {
                    tier: 3,
                    peak_tier: 27,
                    agent_uuid: Some("E370FA57-4757-3604-3648-499E1F642D3F".into()),
                },
                SystemTime::now(),
            )
            .await;
        assert_eq!(view.rank_crest, "https://media/n/3/l.png");
        assert_eq!(view.peak_crest, "https://media/n/27/l.png");
        assert_eq!(view.agent_icon, "https://media/a/gekko.png");
        assert_eq!(view.agent_name, "Gekko");
        assert_eq!(view.agent_role, "Initiator");
        // Peak equal to the current tier reuses one file.
        let same = art
            .view(
                FakeFetcher::offline(),
                &ArtRequest {
                    tier: 3,
                    peak_tier: 3,
                    agent_uuid: None,
                },
                SystemTime::now(),
            )
            .await;
        assert_eq!(same.rank_crest, "https://media/n/3/l.png");
        assert_eq!(same.peak_crest, "https://media/n/3/l.png");
        // Unknown tier / no agent: empty, never a guessed crest.
        let none = art
            .view(
                FakeFetcher::offline(),
                &ArtRequest {
                    tier: 99,
                    peak_tier: 1,
                    agent_uuid: None,
                },
                SystemTime::now(),
            )
            .await;
        assert_eq!(none, ArtView::default());
        // No peak read (`peak_tier == 0`) means no peak crest, even though the
        // tier set does carry art for tier 0 (the UNRANKED emblem).
        let no_peak = art
            .view(
                FakeFetcher::offline(),
                &ArtRequest {
                    tier: 3,
                    peak_tier: 0,
                    agent_uuid: None,
                },
                SystemTime::now(),
            )
            .await;
        assert_eq!(no_peak.rank_crest, "https://media/n/3/l.png");
        assert_eq!(no_peak.peak_crest, "");
    }

    #[tokio::test]
    async fn row_art_is_cached_only_from_an_allowed_host() {
        let tmp = TmpDir::new("rowart");
        let mut art = Art::new(Some(tmp.path().to_path_buf()));
        let portrait = "https://titles.trackercdn.com/valorant-api/agents/x/displayicon.png";
        let thumb = "https://trackercdn.com/cdn/tracker.gg/valorant/db/agents/hush_thumb.png";
        let map = "https://media.valorant-api.com/maps/m1/listviewicon.png";
        let evil = "https://evil.example/agent.png";
        let fetcher = FakeFetcher::ok(&[
            (TIERS_URL, TIERS_JSON.into()),
            (AGENTS_URL, AGENTS_JSON.into()),
            (MAPS_URL, MAPS_JSON.into()),
            (portrait, png()),
            (thumb, png()),
            (map, png()),
            (evil, png()),
        ]);
        let report = art.ensure_catalogue(fetcher.clone(), 1_000).await;
        assert_eq!((report.agents, report.maps), (2, 3));
        let now = SystemTime::now();
        assert_eq!(
            art.ensure_url(fetcher.clone(), portrait, now)
                .await
                .as_deref(),
            Some(portrait)
        );
        // Hush: only TRN's `_thumb` carries this agent's art today.
        assert_eq!(
            art.ensure_url(fetcher.clone(), thumb, now).await.as_deref(),
            Some(thumb)
        );
        // A host the module did not name is refused without a request, even
        // though this fetcher would happily answer it.
        let calls_before = fetcher.calls();
        assert_eq!(art.ensure_url(fetcher.clone(), evil, now).await, None);
        assert_eq!(
            fetcher.calls(),
            calls_before,
            "no request for a refused host"
        );
        // Map art resolves through the catalogue by codename.
        assert_eq!(
            art.map_icon(fetcher.clone(), "lotus", now).await.as_deref(),
            Some(map)
        );
        // Cached now: a second frame makes zero requests.
        let calls_before = fetcher.calls();
        assert!(art
            .ensure_url(fetcher.clone(), portrait, now)
            .await
            .is_some());
        assert!(art.map_icon(fetcher.clone(), "Lotus", now).await.is_some());
        assert_eq!(fetcher.calls(), calls_before);
        // A map the catalogue does not know is empty, not a guessed URL.
        assert_eq!(art.map_icon(fetcher, "Haven", now).await, None);
    }

    #[tokio::test]
    async fn an_unreachable_catalogue_leaves_every_slot_empty_and_does_not_panic() {
        let tmp = TmpDir::new("coldstart");
        let mut art = Art::new(Some(tmp.path().to_path_buf()));
        let report = art.ensure_catalogue(FakeFetcher::offline(), 1_000).await;
        assert_eq!((report.tiers, report.agents, report.fetched), (0, 0, 0));
        assert!(report.error.contains("offline"));
        let view = art
            .view(
                FakeFetcher::offline(),
                &ArtRequest {
                    tier: 21,
                    peak_tier: 24,
                    agent_uuid: Some("e370fa57-4757-3604-3648-499e1f642d3f".into()),
                },
                SystemTime::now(),
            )
            .await;
        assert_eq!(view, ArtView::default());
    }

    #[test]
    fn art_without_a_data_dir_is_disabled_not_broken() {
        let art = Art::new(None);
        assert!(!art.enabled());
        assert_eq!(art.dir(), None);
    }

    // --- UI contract ------------------------------------------------------

    /// The crest and portrait slots must scale with `contain`, or a 256x256
    /// square PNG is stretched into whatever box the layout gives it — and the
    /// slot must keep the explicit square the task fixes (48 / 56 logical px).
    #[test]
    fn the_ui_binds_image_fit_contain_for_both_slots() {
        fn assert_slot(src: &str, source: &str, size: &str, name: &str) {
            let lines: Vec<&str> = src.lines().collect();
            let at = lines
                .iter()
                .position(|l| l.trim() == source)
                .unwrap_or_else(|| panic!("{name}: no `{source}` binding"));
            let block = &lines[at..(at + 6).min(lines.len())];
            let fits = block.iter().any(|l| l.trim() == "image-fit: contain;");
            assert!(fits, "{name}: image-fit: contain is not bound to {source}");
            let w = format!("width: {size};");
            let h = format!("height: {size};");
            assert!(block.iter().any(|l| l.trim() == w), "{name}: no {w}");
            assert!(block.iter().any(|l| l.trim() == h), "{name}: no {h}");
        }

        let sidebar = include_str!("../ui/components/sidebar.slint");
        let cards = include_str!("../ui/components/cards.slint");
        assert_slot(sidebar, "source: root.crest;", "48px", "player-card crest");
        assert_slot(
            cards,
            "source: Tracker.agent-icon;",
            "56px",
            "top-agent portrait",
        );
        // And the rank cells are actually fed the two crest slots.
        assert!(sidebar.contains("crest: Tracker.rank-crest;"));
        assert!(sidebar.contains("crest: Tracker.peak-crest;"));
    }
}
