//! One live poller for the whole process.
//!
//! Two independent sources, one loop:
//!
//! * **Riot Client** (local): 2.5 s while something is happening, backing off to
//!   30 s after 30 s of an idle phase, and a cheap lockfile `stat` decides
//!   whether the client is even running. Rank is the exception: `/mmr` is
//!   remote, so it is read once when the account resolves, again when it changes,
//!   and on explicit user request — never on the live tick.
//! * **Tracker Network** (remote, keyless): the profile for the remembered
//!   account, fetched on start, on an account change and on explicit refresh.
//!   This read is deliberately **not** gated on the Riot Client: the Overview
//!   must show the last account's real numbers when Riot is closed. A parsed
//!   profile is cached on disk (24 h TTL) so even a dead network leaves the last
//!   read on screen.
//!
//! Merge rule: a successful `/mmr` read owns tier + RR; TRN owns everything else
//! (peak, season stats, agents, score). With no client, TRN's tier fills the
//! rank cell and the card says NO RR instead of inventing one.
//!
//! Every publish is a partial state change; the UI thread decides what to
//! repaint. The loop never blocks the UI thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::account::{self, Account};
use crate::assets::{Art, ArtRequest, ArtView, Fetcher, HttpFetcher};
use crate::identity::{self, Entitlements, Identity};
use crate::live_api::{self, Probe};
use crate::live_facts::{derive_facts, find_local_presence};
use crate::live_match;
use crate::live_view::{self, LiveCache, LiveView};
use crate::model::{format_player_tag, RankState, RankView, TrackerSnapshot};
use crate::rank;
use crate::trn;
use crate::trn_breakdown::{self, AgentRow, MapRow};
use crate::trn_matches::{self, MatchRow};
use crate::trn_profile::{self, Profile, ProfileState, ProfileView, CACHE_TTL_MS};

/// Agent select is short and round transitions are quick.
pub const FAST_POLL: Duration = Duration::from_millis(2_500);
/// Menu cadence after [`IDLE_AFTER`] of idle phase.
pub const IDLE_POLL: Duration = Duration::from_secs(30);
/// How long the phase must have been idle before the slow cadence applies.
pub const IDLE_AFTER: Duration = Duration::from_secs(30);
/// Consecutive presence failures before the UI is told the client is gone.
pub const FAIL_STREAK_MAX: u32 = 8;
/// Retry pace while the client is running but not answering identity calls.
const IDENTITY_RETRY: Duration = Duration::from_secs(5);
/// Sleep granularity so a shutdown is never stuck behind a 30 s backoff.
const SLEEP_STEP: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Snapshot(TrackerSnapshot),
    Rank(RankView),
    /// Game art resolved for the current rank. URLs, not images: `slint::Image`
    /// is not `Send`, so the UI thread builds it from the cached files.
    Art(ArtView),
    /// Tracker Network profile for the remembered account. Independent of the
    /// Riot Client: pushed even when no lockfile exists.
    Profile(ProfileView),
    /// Recent matches from the same keyless endpoint. `state` names what the
    /// rows are (`ready`, `cached`, `loading`, `empty`, `error`) so the view can
    /// say "showing the saved list" instead of pretending it is live.
    Matches(MatchesView),
    /// Agent and map breakdowns from the same profile payload. Published with
    /// every profile frame; rows carry cached art URLs, never raw TRN ones.
    Breakdown(BreakdownView),
    /// The live lobby: two teams, names, agents, ranks. Only published on a
    /// change; a failed read keeps the last board instead of blanking it.
    Live(LiveView),
}

/// Rows plus the honesty label that belongs beside them.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchesView {
    pub rows: Vec<MatchRow>,
    pub state: &'static str,
    pub message: String,
}

impl MatchesView {
    fn placeholder(state: &'static str, message: impl Into<String>) -> Self {
        Self {
            rows: Vec::new(),
            state,
            message: message.into(),
        }
    }
}

/// Agent + map rows with the same honesty label as [`MatchesView`]. `state` is
/// ready|cached|loading|empty|error and `message` carries the reason/age line.
#[derive(Debug, Clone, PartialEq)]
pub struct BreakdownView {
    pub agents: Vec<AgentRow>,
    pub maps: Vec<MapRow>,
    pub state: &'static str,
    pub message: String,
}

impl BreakdownView {
    fn placeholder(state: &'static str, message: impl Into<String>) -> Self {
        Self {
            agents: Vec::new(),
            maps: Vec::new(),
            state,
            message: message.into(),
        }
    }
}

/// Cross-thread switches owned by the app (UI) and read by the poller.
#[derive(Debug, Default)]
pub struct Flags {
    pub stop: AtomicBool,
    /// Set by the "Refresh" button; consumed by the poller. Refreshes both
    /// sources: the TRN profile and the local `/mmr` read.
    pub refresh_rank: AtomicBool,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Start the single poller on its own thread with a current-thread runtime.
/// Slint owns the main thread; this thread only blocks on network/`sleep`.
pub fn spawn<F>(publish: F, flags: Arc<Flags>)
where
    F: Fn(Event) + Send + 'static,
{
    let _ = std::thread::Builder::new()
        .name("outpeek-live-poll".to_string())
        .spawn(move || {
            // `enable_all`: the TRN transport is async reqwest (rustls + h2)
            // and needs the IO driver, not just the timer.
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(run(publish, flags));
        });
}

/// The rank cell's best available answer: a local `/mmr` row wins (tier + RR),
/// otherwise the TRN profile's tier + peak, otherwise an honest unknown.
fn merged_rank(profile: Option<&Profile>, local: Option<&rank::RankRow>) -> RankView {
    match local {
        Some(row) => {
            RankView::from_rank_with_peak(row.tier, row.rr, row.peak_tier, trn_peak(profile))
        }
        None => RankView::from_profile(trn_tier(profile), trn_peak(profile)),
    }
}

fn trn_tier(profile: Option<&Profile>) -> Option<i64> {
    profile.and_then(|p| p.rank.as_ref()).and_then(|r| r.tier)
}

fn trn_peak(profile: Option<&Profile>) -> Option<i64> {
    profile.and_then(|p| p.peak.as_ref()).and_then(|r| r.tier)
}

/// Crest inputs: local tier when Riot answered, else TRN's; peak from either
/// (TRN's read is the one that survives a closed client).
fn art_tiers(profile: Option<&Profile>, local: Option<&rank::RankRow>) -> (i64, i64) {
    let local_tier = local.map(|r| r.tier).filter(|t| *t >= 3).unwrap_or(0);
    let local_peak = local.map(|r| r.peak_tier).unwrap_or(0);
    let tier = if local_tier > 0 {
        local_tier
    } else {
        trn_tier(profile).unwrap_or(0)
    };
    (tier, trn_peak(profile).unwrap_or(0).max(local_peak))
}

fn trn_agent(profile: Option<&Profile>) -> Option<String> {
    profile
        .and_then(|p| p.top_agent.as_ref())
        .map(|a| a.uuid.clone())
        .filter(|u| !u.is_empty())
}

/// Resolve one frame of breakdown art: agent portraits come straight from the
/// TRN row URLs (allowlist-gated inside `ensure_url`), map art comes from the
/// valorant-api catalogue. Every miss is an empty URL; the views draw their
/// own fallbacks.
async fn breakdown_frame(
    art: &Art,
    fetcher: Arc<dyn Fetcher>,
    profile: &Profile,
) -> (Vec<AgentRow>, Vec<MapRow>) {
    let mut agents = trn_breakdown::agent_rows(&profile.agents);
    for (row, breakdown) in agents.iter_mut().zip(profile.agents.iter()) {
        // TRN's portrait wins: it is the only source that names an agent the
        // mirror does not list yet (Hush today). The catalogue fills in the
        // uuid rows whose payload art is absent.
        let url = if !breakdown.image_url.is_empty() {
            art.ensure_url(fetcher.clone(), &breakdown.image_url, SystemTime::now())
                .await
        } else {
            None
        };
        let url = match url {
            Some(url) => Some(url),
            None => {
                art.agent_icon(fetcher.clone(), &breakdown.key, SystemTime::now())
                    .await
            }
        };
        row.art_url = url.unwrap_or_default();
    }
    let mut maps = trn_breakdown::map_rows(&profile.maps);
    for (row, breakdown) in maps.iter_mut().zip(profile.maps.iter()) {
        row.art_url = art
            .map_icon(fetcher.clone(), &breakdown.key, SystemTime::now())
            .await
            .unwrap_or_default();
    }
    (agents, maps)
}

/// Build one breakdown event for the current profile. Rows come from the same
/// parsed payload as the Overview, so the two can never disagree; only the art
/// URLs need the network.
///
/// `state`/`message` are the profile frame's own honesty label: a failed
/// refresh that keeps the cached profile must also keep its rows on screen
/// (`cached` + the reason), never blank them.
async fn breakdown_event(
    art: &Art,
    fetcher: Arc<dyn Fetcher>,
    profile: Option<&Profile>,
    state: &'static str,
    message: impl Into<String>,
) -> Event {
    let message = message.into();
    let Some(profile) = profile else {
        return Event::Breakdown(BreakdownView::placeholder(state, message));
    };
    if profile.agents.is_empty() && profile.maps.is_empty() {
        return Event::Breakdown(BreakdownView::placeholder(
            state,
            if message.is_empty() {
                "Tracker Network reported no agent or map rows for this season.".to_string()
            } else {
                message
            },
        ));
    }
    let (agents, maps) = breakdown_frame(art, fetcher, profile).await;
    eprintln!(
        "[trn] breakdown: {} agent row(s), {} map row(s)",
        agents.len(),
        maps.len()
    );
    Event::Breakdown(BreakdownView {
        agents,
        maps,
        state,
        message,
    })
}

/// One live-lobby frame.
///
/// Order matters: presences name players and carry their parties, the probe
/// finds the match, the payload carries the roster, `/mmr` answers ranks and
/// the catalogue supplies agent art. Every step fails soft — a failed read
/// returns a labelled view, never a board with invented cells.
/// Debug-only board for hosts without VALORANT: `OUTPEEK_FAKE_LIVE=1` renders
/// the Live Match page with the documented awkward cases (you, a party, an
/// incognito player, an unresolved name, a hidden rank, an unrated player, a
/// failed read). Never compiled into a release build.
#[cfg(debug_assertions)]
fn fake_live_frame() -> Option<LiveView> {
    if std::env::var_os("OUTPEEK_FAKE_LIVE").is_none() {
        return None;
    }
    let row = |name: &str,
               agent: &str,
               level: &str,
               rank: &str,
               rr: &str,
               party: &str,
               me: bool,
               incognito: bool| live_view::LivePlayerUi {
        name: name.to_string(),
        agent: agent.to_string(),
        agent_art: String::new(),
        level: level.to_string(),
        rank: rank.to_string(),
        rr: rr.to_string(),
        crest: String::new(),
        party: party.to_string(),
        me,
        incognito,
    };
    Some(LiveView {
        state: "coregame",
        message: String::new(),
        map: "Ascent".to_string(),
        mode: "Competitive".to_string(),
        blue_score: 6,
        red_score: 4,
        blue: vec![
            row(
                "Youssef#EUW",
                "Jett",
                "Lv 214",
                "Diamond 3",
                "26 RR",
                "3",
                true,
                false,
            ),
            row(
                "Shadow#EUW",
                "Omen",
                "Lv 187",
                "Diamond 1",
                "12 RR",
                "3",
                false,
                false,
            ),
            row(
                "Incognito#EUW",
                "Raze",
                "Lv 92",
                "Platinum 3",
                "74 RR",
                "3",
                false,
                true,
            ),
            row(
                "Player 4bad",
                "Cypher",
                "",
                "NOT READ",
                "",
                "",
                false,
                false,
            ),
            row(
                "Hidden#EUW",
                "Sova",
                "Lv 44",
                "RANK HIDDEN",
                "",
                "",
                false,
                false,
            ),
        ],
        red: vec![
            row(
                "Enemy 1#EUW",
                "Reyna",
                "Lv 310",
                "Ascendant 1",
                "40 RR",
                "2",
                false,
                false,
            ),
            row(
                "Enemy 2#EUW",
                "Chamber",
                "Lv 140",
                "Platinum 1",
                "8 RR",
                "2",
                false,
                false,
            ),
            row("Enemy 3#EUW", "Viper", "", "UNRATED", "", "", false, false),
            row(
                "Enemy 4#NA1",
                "Breach",
                "Lv 61",
                "Gold 2",
                "55 RR",
                "",
                false,
                false,
            ),
            row(
                "Enemy 5#EUW",
                "Skye",
                "Lv 25",
                "ERROR",
                "",
                "",
                false,
                false,
            ),
        ],
    })
}

async fn live_frame(
    art: &Art,
    fetcher: Arc<dyn Fetcher>,
    identity: Option<&Identity>,
    snap: &TrackerSnapshot,
    presences: &serde_json::Value,
    ent_cache: &mut Option<(Entitlements, Instant)>,
    cache: &mut LiveCache,
) -> LiveView {
    let Some(me) = identity else {
        return LiveView::offline("No Riot Client — the live lobby needs one.");
    };
    // Names first: they are also the fallback for a lobby where the account
    // service has nothing.
    let presence_rows = live_match::presence_rows(presences);
    for row in &presence_rows {
        cache.set_name(&row.puuid, &row.name, &row.tag);
    }

    let now = Instant::now();
    let ent = match ent_cache {
        Some((ent, at)) if now.duration_since(*at) < Duration::from_secs(300) => ent.clone(),
        _ => match identity::read_lockfile().and_then(|auth| identity::fetch_entitlements(&auth)) {
            Ok(ent) => {
                *ent_cache = Some((ent.clone(), now));
                ent
            }
            Err(e) => return LiveView::idle(e),
        },
    };
    let region =
        rank::region_from_access_token(&ent.access_token).unwrap_or_else(|| "eu".to_string());

    let matched = match live_api::probe(&ent, &region, &me.puuid) {
        Ok(Probe::Idle) => return LiveView::idle("In menus — no live match."),
        Ok(Probe::Match { phase, match_id }) => {
            match live_api::match_by_id(&ent, &region, phase, &match_id) {
                Ok(Some(matched)) => matched,
                Ok(None) => return LiveView::idle("The match ended — back to the menu."),
                Err(e) => {
                    eprintln!("[live] roster: {e}");
                    return LiveView::idle(e).error("Live lobby read failed.");
                }
            }
        }
        Err(e) => {
            eprintln!("[live] probe: {e}");
            return LiveView::idle(e).error("Live lobby read failed.");
        }
    };
    if !matched.has_roster() {
        return LiveView::idle("The match has no roster yet.");
    }

    // Parties the payload omits come from the presences (the client puts the
    // local party there); a party no source names stays a solo slot.
    let mut matched = matched;
    for player in &mut matched.players {
        if player.party_id.is_empty() {
            if let Some(row) = presence_rows
                .iter()
                .find(|r| r.puuid.eq_ignore_ascii_case(&player.puuid))
            {
                player.party_id = row.party_id.clone();
            }
        }
    }

    // Only PUUIDs no name source has answered go to the account service.
    let missing: Vec<String> = matched
        .players
        .iter()
        .map(|p| p.puuid.clone())
        .filter(|puuid| cache.name(puuid).is_none())
        .collect();
    if !missing.is_empty() {
        if let Ok(auth) = identity::read_lockfile() {
            match live_api::namesets(&auth, &missing) {
                Ok(rows) => {
                    for row in rows {
                        if row.error.is_empty() {
                            cache.set_name(&row.puuid, &row.name, &row.tag);
                        }
                    }
                }
                Err(e) => eprintln!("[live] names: {e}"),
            }
        }
    }
    cache.retain(
        &matched
            .players
            .iter()
            .map(|p| p.puuid.clone())
            .collect::<Vec<_>>(),
    );

    let mode = if snap.mode.is_empty() {
        live_match::mode_label(&matched.queue_id, &matched.mode_id)
    } else {
        snap.mode.clone()
    };
    let mut rank_for = |puuid: &str| match live_api::player_rank(&ent, &region, puuid) {
        Ok(row) => Some(row),
        Err(e) => {
            eprintln!("[live] rank {puuid}: {e}");
            None
        }
    };
    live_view::build(
        &matched,
        &me.puuid,
        &snap.map,
        &mode,
        i64::from(snap.blue),
        i64::from(snap.red),
        cache,
        &mut rank_for,
        art,
        fetcher,
    )
    .await
}

/// Resolve one frame of art for the current tier/peak/agent. Every step fails
/// soft, so an offline host keeps the drawn fallbacks.
async fn art_frame(
    art: &Art,
    fetcher: Arc<dyn Fetcher>,
    profile: Option<&Profile>,
    local: Option<&rank::RankRow>,
) -> ArtView {
    if !art.enabled() {
        return ArtView::default();
    }
    let (tier, peak_tier) = art_tiers(profile, local);
    art.view(
        fetcher,
        &ArtRequest {
            tier,
            peak_tier,
            agent_uuid: trn_agent(profile),
        },
        SystemTime::now(),
    )
    .await
}

async fn run<F>(publish: F, flags: Arc<Flags>)
where
    F: Fn(Event) + Send + 'static,
{
    let mut snap = TrackerSnapshot::starting();
    let mut rank_view = RankView::waiting();
    let mut identity: Option<Identity> = None;
    let mut local_rank: Option<rank::RankRow> = None;
    let mut rank_pending = true;
    let mut idle_since: Option<Instant> = Some(Instant::now());
    let mut fail_streak: u32 = 0;
    // Profile state, independent of the Riot Client.
    let mut profile: Option<Profile> = None;
    // The cached frame's note, reused when the catalogue lands and the
    // breakdown art is re-resolved: the rows are still that saved read, so the
    // chip must keep saying SAVED.
    let mut cached_note = String::new();
    let mut profile_handle = String::new();
    let mut profile_saved_at: u64 = 0;
    let mut profile_pending = true;
    let mut matches_pending = true;
    let mut matches: Option<Vec<MatchRow>> = None;
    let mut matches_saved_at: u64 = 0;
    // The cache read is a first-frame read, not a per-tick one: repeating it
    // would overwrite a fresh `ready` frame with `cached` on every poll.
    let mut matches_cache_loaded = false;
    // Game art: one catalogue in memory, one cache directory. A host without an
    // app-data directory gets `None`, and every art slot then stays empty —
    // a supported state, not an error.
    let mut art = Art::new(crate::assets::art_dir());
    let fetcher: Arc<dyn Fetcher> = Arc::new(HttpFetcher);
    // Live lobby state. Names and ranks are per player and cached; the board is
    // only republished when it actually changed.
    let mut live_cache = LiveCache::default();
    let mut live_last: Option<LiveView> = None;
    let mut live_ent: Option<(Entitlements, Instant)> = None;
    // Debug board for a host without VALORANT: the poller never probes, so the
    // fixture is published here and the live path is skipped entirely.
    let fake_live: Option<LiveView> = {
        #[cfg(debug_assertions)]
        {
            fake_live_frame()
        }
        #[cfg(not(debug_assertions))]
        {
            None
        }
    };
    if let Some(view) = &fake_live {
        live_last = Some(view.clone());
        publish(Event::Live(view.clone()));
    }
    if art.enabled() {
        let freed = art.cap();
        let (files, bytes) = art.usage();
        eprintln!(
            "[art] cache: {files} file(s), {} KiB{}",
            bytes / 1024,
            if freed > 0 {
                format!(" — evicted {} KiB to fit the cap", freed / 1024)
            } else {
                String::new()
            }
        );
    }

    publish(Event::Snapshot(snap.clone()));
    publish(Event::Rank(rank_view.clone()));

    // The remembered account first: it names the player card and keys the TRN
    // profile, so the Overview is useful with the Riot Client closed.
    if let Some(account) = account::load_default() {
        profile_handle = account.handle.clone();
        let next = snap.with_account_handle(&account.handle);
        if next != snap {
            snap = next;
            publish(Event::Snapshot(snap.clone()));
        }
    }

    // A cached profile is the first frame's data; the network replaces it in the
    // loop. Loading the parsed cache (not the body) is a few milliseconds.
    if let Some(cache) = trn_profile::cache_path()
        .and_then(|path| trn_profile::load_cache(&path, now_ms(), CACHE_TTL_MS))
    {
        if profile_handle.is_empty() {
            profile_handle = cache.handle.clone();
            let next = snap.with_account_handle(&cache.handle);
            if next != snap {
                snap = next;
                publish(Event::Snapshot(snap.clone()));
            }
        }
        profile_saved_at = cache.saved_at;
        let message = format!(
            "Showing the cached profile from {}.",
            trn_profile::age_label(now_ms().saturating_sub(cache.saved_at))
        );
        let mut view = cache.profile.to_view();
        view.state = ProfileState::Cached;
        view.message = message.clone();
        publish(Event::Profile(view));
        cached_note = message.clone();
        publish(
            breakdown_event(
                &art,
                fetcher.clone(),
                Some(&cache.profile),
                "cached",
                message,
            )
            .await,
        );
        profile = Some(cache.profile);
        let next = merged_rank(profile.as_ref(), local_rank.as_ref());
        if next != rank_view {
            rank_view = next;
            publish(Event::Rank(rank_view.clone()));
        }
        // Crests resolve from the same cache: with no network this is a soft
        // miss and the UI keeps its drawn fallbacks.
        publish(Event::Art(
            art_frame(&art, fetcher.clone(), profile.as_ref(), local_rank.as_ref()).await,
        ));
    }

    // The catalogues are what every art slot is resolved against, so they are
    // read once here rather than inside the rank path: a slow or dead mirror
    // must never delay the rank label. One request per family, then 24 h of
    // memory TTL — a poll never re-downloads them.
    if art.enabled() {
        let report = art.ensure_catalogue(fetcher.clone(), now_ms()).await;
        if report.error.is_empty() {
            eprintln!(
                "[art] catalogue: {} tiers, {} agents, {} maps ({} fetch(es), dir {})",
                report.tiers,
                report.agents,
                report.maps,
                report.fetched,
                art.dir()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default()
            );
            // The first breakdown frame resolved art before the catalogues
            // landed; re-resolve so map cards fill in without waiting for a
            // profile refresh.
            if profile.is_some() {
                publish(
                    breakdown_event(
                        &art,
                        fetcher.clone(),
                        profile.as_ref(),
                        "cached",
                        cached_note.clone(),
                    )
                    .await,
                );
                // Crests and the top-agent portrait resolve against the same
                // catalogue: the first frame ran before it was loaded, so
                // without this a cold offline start keeps its drawn fallbacks
                // even though every URL is now known.
                publish(Event::Art(
                    art_frame(&art, fetcher.clone(), profile.as_ref(), local_rank.as_ref()).await,
                ));
            }
        } else {
            eprintln!(
                "[art] catalogue: {} tiers, {} agents, {} maps — {}",
                report.tiers, report.agents, report.maps, report.error
            );
        }
    }

    loop {
        if flags.stop.load(Ordering::Relaxed) {
            return;
        }

        // A user refresh means both sources: the TRN profile and the local
        // `/mmr` read.
        if flags.refresh_rank.swap(false, Ordering::Relaxed) {
            profile_pending = true;
            matches_pending = true;
            rank_pending = true;
        }

        // TRN profile: start, account change, refresh — never gated on the Riot
        // Client. This is the read that keeps the Overview populated offline.
        if profile_pending {
            profile_pending = false;
            if profile_handle.is_empty() {
                let note =
                    "No account yet — open the Riot Client once and Outpeek will remember it.";
                publish(Event::Profile(ProfileView::placeholder(
                    ProfileState::Empty,
                    note,
                )));
                publish(Event::Breakdown(BreakdownView::placeholder("empty", note)));
            } else {
                if profile.is_none() {
                    publish(Event::Profile(ProfileView::placeholder(
                        ProfileState::Loading,
                        "Reading Tracker Network…",
                    )));
                    publish(Event::Breakdown(BreakdownView::placeholder(
                        "loading",
                        "Reading Tracker Network…",
                    )));
                }
                match trn::fetch_profile(&profile_handle)
                    .await
                    .and_then(|json| trn_profile::parse(&json))
                {
                    Ok(fresh) => {
                        profile_saved_at = now_ms();
                        if let Some(path) = trn_profile::cache_path() {
                            if let Err(e) = trn_profile::save_cache(&path, &fresh, profile_saved_at)
                            {
                                eprintln!("[trn] profile cache write failed: {e}");
                            }
                        }
                        eprintln!(
                            "[trn] profile: {} · {} · {} matches ({} bytes cached profile)",
                            fresh.handle,
                            fresh.season_short,
                            fresh
                                .season
                                .matches
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "—".to_string()),
                            serde_json::to_string(&fresh).map(|s| s.len()).unwrap_or(0)
                        );
                        profile = Some(fresh);
                        profile_handle = profile
                            .as_ref()
                            .map(|p| p.handle.clone())
                            .unwrap_or_default();
                        let view = profile
                            .as_ref()
                            .map(Profile::to_view)
                            .unwrap_or_else(|| ProfileView::default());
                        publish(Event::Profile(view));
                        publish(
                            breakdown_event(&art, fetcher.clone(), profile.as_ref(), "ready", "")
                                .await,
                        );
                        if local_rank.is_none() {
                            let next = merged_rank(profile.as_ref(), None);
                            if next != rank_view {
                                rank_view = next;
                                publish(Event::Rank(rank_view.clone()));
                            }
                        }
                        publish(Event::Art(
                            art_frame(&art, fetcher.clone(), profile.as_ref(), local_rank.as_ref())
                                .await,
                        ));
                    }
                    Err(e) => {
                        eprintln!("[trn] profile read failed: {e}");
                        // A failed read never wipes a shown profile: the cached
                        // copy stays, with the reason on the line beside it. The
                        // agent/map rows come from that same copy, so they stay
                        // too — blanking them would lose data a failure did not
                        // touch.
                        let (view, state, note) = match &profile {
                            Some(last) => {
                                let note = format!(
                                    "{e} — showing the profile saved {}.",
                                    trn_profile::age_label(
                                        now_ms().saturating_sub(profile_saved_at)
                                    )
                                );
                                let mut view = last.to_view();
                                view.state = ProfileState::Cached;
                                view.message = note.clone();
                                (view, "cached", note)
                            }
                            None => {
                                let note = format!("Tracker Network did not answer ({e}).");
                                (
                                    ProfileView::placeholder(ProfileState::Error, note.clone()),
                                    "error",
                                    note,
                                )
                            }
                        };
                        publish(Event::Profile(view));
                        publish(
                            breakdown_event(&art, fetcher.clone(), profile.as_ref(), state, note)
                                .await,
                        );
                    }
                }
            }
        }

        // Recent matches: same trigger as the profile, same keyless path. A
        // failure keeps the shown rows and says why; it never blanks the list.
        if matches_pending {
            matches_pending = false;
            if !profile_handle.is_empty() {
                if matches.is_none() {
                    publish(Event::Matches(MatchesView::placeholder(
                        "loading",
                        "Reading recent matches…",
                    )));
                }
                match trn::fetch_matches(&profile_handle).await.and_then(|json| {
                    trn_matches::parse(&json).ok_or_else(|| "TRN_HTTP_BAD_BODY".to_string())
                }) {
                    Ok(rows) => {
                        matches_saved_at = now_ms();
                        if let Some(path) = trn_matches::cache_path() {
                            if let Err(e) = trn_matches::save_cache(
                                &path,
                                &profile_handle,
                                &rows,
                                matches_saved_at,
                            ) {
                                eprintln!("[trn] matches cache write failed: {e}");
                            }
                        }
                        eprintln!("[trn] matches: {} · {} row(s)", profile_handle, rows.len());
                        matches = Some(rows.clone());
                        publish(Event::Matches(MatchesView {
                            rows,
                            state: "ready",
                            message: String::new(),
                        }));
                    }
                    Err(e) => {
                        let (rows, state, message) = match &matches {
                            Some(last) => (
                                last.clone(),
                                "cached",
                                format!(
                                    "{e} — showing the list saved {}.",
                                    trn_profile::age_label(
                                        now_ms().saturating_sub(matches_saved_at)
                                    )
                                ),
                            ),
                            None => (
                                Vec::new(),
                                "error",
                                format!("Tracker Network did not answer ({e})."),
                            ),
                        };
                        publish(Event::Matches(MatchesView {
                            rows,
                            state,
                            message,
                        }));
                    }
                }
            }
        }

        // Matches cache: the last read is the first frame, like the profile.
        // Loaded once; the network replaces it below.
        if !matches_cache_loaded && matches.is_none() {
            matches_cache_loaded = true;
            if let Some(cache) = trn_matches::cache_path().and_then(|path| {
                trn_matches::load_cache(&path, now_ms(), trn_matches::CACHE_TTL_MS)
            }) {
                matches_saved_at = cache.saved_at;
                matches = Some(cache.matches.clone());
                publish(Event::Matches(MatchesView {
                    rows: cache.matches,
                    state: "cached",
                    message: format!(
                        "Showing the saved list from {}.",
                        trn_profile::age_label(now_ms().saturating_sub(matches_saved_at))
                    ),
                }));
            }
        }

        // Cheap gate first: no lockfile means no Riot client, so there is
        // nothing to ask. A client that closed mid-match cannot still be in
        // one — reset the live half rather than keep a stale score on screen.
        // The profile half stays exactly as it was: that is the point.
        if !identity::lockfile_present() {
            identity = None;
            local_rank = None;
            rank_pending = true;
            let next = TrackerSnapshot::no_client();
            let next = if profile_handle.is_empty() {
                next
            } else {
                next.with_account_handle(&profile_handle)
            };
            if next != snap {
                snap = next;
                publish(Event::Snapshot(snap.clone()));
            }
            // No client, no lobby: say so once, not on every idle tick.
            if fake_live.is_none() && live_last.as_ref().map(|v| v.state) != Some("offline") {
                let view = LiveView::offline("No Riot Client — the live lobby needs one.");
                live_last = Some(view.clone());
                publish(Event::Live(view));
            }
            // Rank falls back to the profile instead of WAITING.
            let next_rank = merged_rank(profile.as_ref(), None);
            if next_rank != rank_view {
                rank_view = next_rank;
                publish(Event::Rank(rank_view.clone()));
                if profile.is_none() {
                    // No rank on screen means no crest: art follows the label.
                    publish(Event::Art(ArtView::default()));
                }
            }
            idle_since.get_or_insert_with(Instant::now);
            sleep_or_stop(&flags, IDLE_POLL).await;
            continue;
        }

        // Identity is fetched once per session (startup or account change).
        if identity.is_none() {
            match identity::read_lockfile().and_then(|auth| identity::fetch_identity(&auth)) {
                Ok(id) => {
                    identity = Some(id.clone());
                    snap = snap.with_identity(&id.game_name, &id.tag_line);
                    publish(Event::Snapshot(snap.clone()));
                    rank_pending = true;
                    fail_streak = 0;
                    // Remember the account for the next (possibly Riot-less)
                    // launch, and re-key the profile when it changed.
                    let handle = format_player_tag(&id.game_name, &id.tag_line);
                    if !handle.is_empty() && handle != profile_handle {
                        match Account::new(&handle, account::PLATFORM_RIOT, now_ms()) {
                            Ok(account) => {
                                if let Err(e) = account::save_default(&account) {
                                    eprintln!("[account] save failed: {e}");
                                }
                                profile_handle = handle;
                                profile_pending = true;
                                matches_pending = true;
                            }
                            Err(e) => eprintln!("[account] not stored: {e}"),
                        }
                    }
                }
                Err(e) => {
                    let next = snap.with_error(e);
                    if next != snap {
                        snap = next;
                        publish(Event::Snapshot(snap.clone()));
                    }
                    sleep_or_stop(&flags, IDENTITY_RETRY).await;
                    continue;
                }
            }
        }

        let Some(me) = identity.clone() else {
            continue;
        };

        // Presence poll: the only thing the live tick owns.
        let polled = identity::read_lockfile().and_then(|auth| identity::fetch_presences(&auth));
        match polled {
            Ok(root) => {
                fail_streak = 0;
                let facts = derive_facts(&root, &me.puuid, now_ms());
                let next = snap.with_facts(&facts);
                if next != snap {
                    snap = next;
                    publish(Event::Snapshot(snap.clone()));
                }
                // A real match is a reason to stay fast; a stable idle set is
                // what earns the slow cadence.
                if snap.phase == "idle" {
                    idle_since.get_or_insert_with(Instant::now);
                } else {
                    idle_since = None;
                }
                // A different account (client relaunch or sign-in switch) stops
                // matching: re-resolve identity and rank next tick instead of
                // staying blind on the old PUUID.
                if find_local_presence(&root, &me.puuid).is_none() {
                    identity = None;
                    local_rank = None;
                    rank_pending = true;
                } else if fake_live.is_none() {
                    // The live lobby rides the same poll and the same presences:
                    // one read feeds the strip, the names and the parties.
                    let view = live_frame(
                        &art,
                        fetcher.clone(),
                        identity.as_ref(),
                        &snap,
                        &root,
                        &mut live_ent,
                        &mut live_cache,
                    )
                    .await;
                    if live_last.as_ref() != Some(&view) {
                        live_last = Some(view.clone());
                        publish(Event::Live(view));
                    }
                }
            }
            Err(e) => {
                fail_streak += 1;
                let message = if fail_streak >= FAIL_STREAK_MAX {
                    "Lost the Riot Client — waiting for it to answer.".to_string()
                } else {
                    e
                };
                let next = snap.with_error(message);
                if next != snap {
                    snap = next;
                    publish(Event::Snapshot(snap.clone()));
                }
                idle_since.get_or_insert_with(Instant::now);
            }
        }

        // On-demand rank read (startup, account change, explicit refresh).
        // Never on the live tick: `/mmr` is a remote call.
        if identity.is_some() && rank_pending {
            rank_pending = false;
            publish(Event::Rank(RankView::loading()));
            let resolved = identity::read_lockfile().and_then(|auth| {
                let ent = identity::fetch_entitlements(&auth)?;
                if ent.subject.is_empty() {
                    return Err("Riot Client session has no PUUID.".to_string());
                }
                let region = rank::region_from_access_token(&ent.access_token)
                    .unwrap_or_else(|| "eu".to_string());
                rank::fetch_rank(&me.puuid, &ent, &region)
            });
            match resolved {
                Ok(row) => {
                    local_rank = Some(row);
                    let next = merged_rank(profile.as_ref(), local_rank.as_ref());
                    // The label goes first: a slow crest download must never
                    // delay the rank the user is waiting for.
                    rank_view = next;
                    publish(Event::Rank(rank_view.clone()));
                    publish(Event::Art(
                        art_frame(&art, fetcher.clone(), profile.as_ref(), local_rank.as_ref())
                            .await,
                    ));
                }
                Err(e) => {
                    // With a profile on hand the panel keeps real TRN numbers;
                    // only a profile-less failure shows the error message.
                    let next = merged_rank(profile.as_ref(), None);
                    rank_view = if next.state == RankState::Unknown {
                        RankView::error(e)
                    } else {
                        next
                    };
                    publish(Event::Rank(rank_view.clone()));
                    if profile.is_none() {
                        publish(Event::Art(ArtView::default()));
                    }
                }
            }
        }

        let cadence = if idle_since.map_or(false, |t| t.elapsed() >= IDLE_AFTER) {
            IDLE_POLL
        } else {
            FAST_POLL
        };
        sleep_or_stop(&flags, cadence).await;
    }
}

/// Sleep `total`, waking often enough that `stop` is honoured promptly.
async fn sleep_or_stop(flags: &Flags, total: Duration) {
    let mut slept = Duration::ZERO;
    while slept < total {
        if flags.stop.load(Ordering::Relaxed) {
            return;
        }
        let chunk = SLEEP_STEP.min(total - slept);
        tokio::time::sleep(chunk).await;
        slept += chunk;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trn_breakdown::BreakdownRow;

    /// One agent row, shaped like the live profile. The poller's own tests are
    /// end-to-end (fake client); these cover the decision the fail-soft path
    /// makes between "keep the rows" and "say why there are none".
    fn profile_with_one_agent() -> Profile {
        Profile {
            handle: "lil ga7ed#zngr".to_string(),
            agents: vec![BreakdownRow {
                key: "601dbbe7-43ce-be57-2a40-4abd24953621".to_string(),
                name: "KAY/O".to_string(),
                role: "Initiator".to_string(),
                matches: Some(48),
                win_pct: Some(43.8),
                ..BreakdownRow::default()
            }],
            ..Profile::default()
        }
    }

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    /// No cache directory on this host: every art slot is a soft miss, so the
    /// event is decided by the profile alone and no request is attempted.
    fn art_off() -> (Art, Arc<dyn Fetcher>) {
        (Art::new(None), Arc::new(HttpFetcher))
    }

    fn as_breakdown(event: Event) -> BreakdownView {
        match event {
            Event::Breakdown(view) => view,
            other => panic!("expected a breakdown event, got {other:?}"),
        }
    }

    #[test]
    fn a_failed_refresh_keeps_the_rows_it_did_not_lose() {
        let (art, fetcher) = art_off();
        let profile = profile_with_one_agent();
        let note = "TRN_TIMEOUT — showing the profile saved 2 hours ago.";
        let view = as_breakdown(block_on(breakdown_event(
            &art,
            fetcher,
            Some(&profile),
            "cached",
            note,
        )));
        assert_eq!(view.state, "cached");
        assert_eq!(
            view.agents.len(),
            1,
            "a network failure does not blank rows"
        );
        assert_eq!(view.agents[0].name, "KAY/O");
        assert_eq!(view.agents[0].matches, "48 matches");
        assert_eq!(view.message, note, "the reason travels with the rows");
    }

    #[test]
    fn with_no_profile_the_placeholder_carries_the_reason_not_a_blank_table() {
        let (art, fetcher) = art_off();
        let view = as_breakdown(block_on(breakdown_event(
            &art,
            fetcher,
            None,
            "error",
            "Tracker Network did not answer (TRN_HTTP_503).",
        )));
        assert!(view.agents.is_empty(), "no profile, no invented rows");
        assert!(view.maps.is_empty());
        assert_eq!(view.state, "error");
        assert!(view.message.contains("TRN_HTTP_503"));
    }

    #[test]
    fn a_ready_profile_with_no_rows_says_so_instead_of_an_empty_success() {
        let (art, fetcher) = art_off();
        let profile = Profile {
            handle: "lil ga7ed#zngr".to_string(),
            ..Profile::default()
        };
        let view = as_breakdown(block_on(breakdown_event(
            &art,
            fetcher,
            Some(&profile),
            "ready",
            "",
        )));
        assert_eq!(view.state, "ready");
        assert!(view.agents.is_empty());
        assert!(view.message.contains("no agent or map rows"));
    }
}
