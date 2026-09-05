use fluxplay_core::models::{MediaSource, SourceKind};
use tracing::{debug, info};

pub const DEMO_NAME: &str = "Démo FluxPlay";

/// Built-in demo when the user has no saved profile.
/// Uses public free-to-air M3U (iptv-org) + stable HLS samples — never pirate Xtream panels.
pub fn demo_source() -> MediaSource {
    // Prefer a compact public news slice so first sync stays respectful to hosts.
    let mut src = MediaSource::new(
        DEMO_NAME,
        SourceKind::M3uPlus,
        "https://iptv-org.github.io/iptv/categories/news.m3u",
    );
    src.enabled = true;
    info!("injecting public demo source (iptv-org news)");
    src
}

/// Extra public playlists offered in Sources when still on demo (FR / EN / culture).
pub fn public_demo_catalog() -> Vec<(String, String)> {
    vec![
        (
            "Démo · News (iptv-org)".into(),
            "https://iptv-org.github.io/iptv/categories/news.m3u".into(),
        ),
        (
            "Démo · France (iptv-org)".into(),
            "https://iptv-org.github.io/iptv/countries/fr.m3u".into(),
        ),
        (
            "Démo · Documentary (iptv-org)".into(),
            "https://iptv-org.github.io/iptv/categories/documentary.m3u".into(),
        ),
        (
            "Démo · HLS sample (Mux)".into(),
            "https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8".into(),
        ),
    ]
}

pub fn is_demo(src: &MediaSource) -> bool {
    src.name == DEMO_NAME || src.name.starts_with("Démo ·")
}

/// Drop built-in demo entries whenever at least one real source exists.
pub fn strip_demo_if_real(sources: &mut Vec<MediaSource>) {
    let has_real = sources.iter().any(|s| !is_demo(s));
    if has_real {
        let before = sources.len();
        sources.retain(|s| !is_demo(s));
        if sources.len() != before {
            info!(removed = before - sources.len(), "stripped demo source");
        }
    } else {
        debug!("no real sources — keeping demo if present");
    }
}
