//! Manual art-pipeline proof: fetch the real catalogue, resolve one tier crest
//! through the real cache, and print what landed on disk.
//!
//! ```text
//! APPDATA=/tmp/outpeek-appdata cargo run -p outpeek-app --example art_proof -- 21
//! ```
//!
//! It is not a test (the suite never touches the network) and it fakes nothing:
//! the tier id comes from the command line, and the file it prints is exactly
//! what the app would load for that tier. Use it to prove the transport, the
//! atomic write and the PNG guard on a machine with no Riot client, then load
//! the printed path in any Slint surface to prove the render half.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use outpeek_app::assets::{self, Art, ArtRequest, Fetcher, HttpFetcher};

fn main() {
    let tier: i64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(21);
    // Optional second argument: an agent uuid, so the portrait half of the
    // pipeline can be proved with the same run.
    let agent_uuid = std::env::args().nth(2).filter(|s| !s.trim().is_empty());
    let Some(dir) = assets::art_dir() else {
        eprintln!("art_proof: no app-data directory (APPDATA unset) — nothing to cache into");
        std::process::exit(2);
    };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("art_proof: runtime");
    runtime.block_on(async move {
        let fetcher: Arc<dyn Fetcher> = Arc::new(HttpFetcher);
        let mut art = Art::new(Some(dir.clone()));
        let report = art.ensure_catalogue(fetcher.clone(), now_ms).await;
        println!(
            "catalogue: {} tiers, {} agents, {} fetch(es), error={:?}",
            report.tiers, report.agents, report.fetched, report.error
        );
        let view = art
            .view(
                fetcher,
                &ArtRequest {
                    tier,
                    peak_tier: 0,
                    agent_uuid: agent_uuid.clone(),
                    banner_url: None,
                },
                SystemTime::now(),
            )
            .await;
        println!("requested tier {tier} -> crest {:#?}", view.rank_crest);
        if view.rank_crest.is_empty() {
            println!("no art resolved (no catalogue or the download failed)");
            std::process::exit(1);
        }
        let path = outpeek_overlay::image_cache::path_for_url_in(&dir, &view.rank_crest);
        match std::fs::metadata(&path) {
            Ok(meta) => println!("cached crest {} ({} bytes)", path.display(), meta.len()),
            Err(e) => {
                println!("cached crest missing at {}: {e}", path.display());
                std::process::exit(1);
            }
        }
        if !view.agent_icon.is_empty() {
            let path = outpeek_overlay::image_cache::path_for_url_in(&dir, &view.agent_icon);
            if let Ok(meta) = std::fs::metadata(&path) {
                println!(
                    "cached portrait {} ({} bytes) for {} ({})",
                    path.display(),
                    meta.len(),
                    view.agent_name,
                    view.agent_role
                );
            }
        }
        // Same decoder the app's UI thread uses, so a decode failure shows here.
        let image = outpeek_overlay::image_cache::load_from(&dir, &view.rank_crest);
        println!(
            "decoded {}x{} for slint",
            image.size().width,
            image.size().height
        );
        println!("dir {}", dir.display());
    });
}
