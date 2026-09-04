//! Requires env: `FLUXPLAY_XTREAM_URL`, `FLUXPLAY_XTREAM_USER`, `FLUXPLAY_XTREAM_PASS`.
use fluxplay_core::models::{MediaSource, SourceKind};
use fluxplay_providers::{check_xtream_portal, format_health};

#[tokio::main]
async fn main() {
    let endpoint = std::env::var("FLUXPLAY_XTREAM_URL").expect("FLUXPLAY_XTREAM_URL");
    let user = std::env::var("FLUXPLAY_XTREAM_USER").expect("FLUXPLAY_XTREAM_USER");
    let pass = std::env::var("FLUXPLAY_XTREAM_PASS").expect("FLUXPLAY_XTREAM_PASS");
    let extra = std::env::var("FLUXPLAY_XTREAM_URL_2").ok();

    let mut portals = vec![("primary", endpoint)];
    if let Some(u2) = extra {
        portals.push(("secondary", u2));
    }

    for (name, base) in portals {
        println!("=== {name} {base} ===");
        match check_xtream_portal(&base, &user, &pass).await {
            Ok(h) => println!("{}", format_health(&h)),
            Err(e) => eprintln!("error: {e}"),
        }
        let _ = MediaSource::new(name, SourceKind::Xtream, &base);
    }
}
