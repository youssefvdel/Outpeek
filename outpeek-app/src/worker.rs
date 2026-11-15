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
use crate::identity::{self, Identity};
use crate::live_facts::{derive_facts, find_local_presence};
use crate::model::{format_player_tag, RankState, RankView, TrackerSnapshot};
use crate::rank;
use crate::trn;
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
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_time()
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
        let mut view = cache.profile.to_view();
        view.state = ProfileState::Cached;
        view.message = format!(
            "Showing the cached profile from {}.",
            trn_profile::age_label(now_ms().saturating_sub(cache.saved_at))
        );
        publish(Event::Profile(view));
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
                "[art] catalogue: {} tiers, {} agents ({} fetch(es), dir {})",
                report.tiers,
                report.agents,
                report.fetched,
                art.dir()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default()
            );
        } else {
            eprintln!(
                "[art] catalogue: {} tiers, {} agents — {}",
                report.tiers, report.agents, report.error
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
                publish(Event::Profile(ProfileView::placeholder(
                    ProfileState::Empty,
                    "No account yet — open the Riot Client once and Outpeek will remember it.",
                )));
            } else {
                if profile.is_none() {
                    publish(Event::Profile(ProfileView::placeholder(
                        ProfileState::Loading,
                        "Reading Tracker Network…",
                    )));
                }
                match trn::fetch_profile(&profile_handle).and_then(|json| trn_profile::parse(&json))
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
                        // A failed read never wipes a shown profile: the cached
                        // copy stays, with the reason on the line beside it.
                        let view = match &profile {
                            Some(last) => {
                                let mut view = last.to_view();
                                view.state = ProfileState::Cached;
                                view.message = format!(
                                    "{e} — showing the profile saved {}.",
                                    trn_profile::age_label(
                                        now_ms().saturating_sub(profile_saved_at)
                                    )
                                );
                                view
                            }
                            None => ProfileView::placeholder(
                                ProfileState::Error,
                                format!("Tracker Network did not answer ({e})."),
                            ),
                        };
                        publish(Event::Profile(view));
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
                match trn::fetch_matches(&profile_handle).and_then(|json| {
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
