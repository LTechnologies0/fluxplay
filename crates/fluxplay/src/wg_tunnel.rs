//! Userspace WireGuard tunnel scoped to FluxPlay (no system routes).
//!
//! Desktop + Android: `wg-socks` builds an in-process WireGuard datapath and
//! exposes a local SOCKS5 proxy. HTTP clients + mpv are pointed at that proxy
//! so only FluxPlay traffic exits through the tunnel (no VpnService required).
//!
//! DNS policy:
//! - Profile `DNS=` resolves the peer **Endpoint** only (bootstrap, clearnet).
//! - App day-to-day DNS is the user's Custom / DoH / DoT (queries tunnelled via SOCKS).

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use tracing::{info, warn};

static SOCKS_URL: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static PROXY: OnceLock<Mutex<Option<wg_socks::WgSocksProxy>>> = OnceLock::new();
static START_IN_FLIGHT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn socks_lock() -> &'static Mutex<Option<String>> {
    SOCKS_URL.get_or_init(|| Mutex::new(None))
}

pub fn tunnel_start_in_flight() -> bool {
    START_IN_FLIGHT.load(std::sync::atomic::Ordering::SeqCst)
}

/// Current SOCKS5 URL for app HTTP / player, if the tunnel is up.
pub fn socks_proxy_url() -> Option<String> {
    socks_lock().lock().ok().and_then(|g| g.clone())
}

pub fn set_socks_proxy_url(url: Option<String>) {
    if let Ok(mut g) = socks_lock().lock() {
        *g = url.clone();
    }
    fluxplay_providers::set_socks_proxy(url);
}

async fn reserve_local_socks_addr() -> Result<std::net::SocketAddr, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("bind SOCKS local: {e}"))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {e}"))?;
    drop(listener);
    Ok(addr)
}

/// Rewrite `Endpoint = hostname:port` → IP using bootstrap DNS (profile `DNS=`).
async fn prepare_conf_with_bootstrap(conf: &str, bootstrap_dns: &str) -> Result<String, String> {
    let mut out = String::with_capacity(conf.len() + 32);
    for line in conf.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let Some((key, val)) = trimmed.split_once('=') else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("endpoint") {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let endpoint = val.trim();
        if endpoint.parse::<std::net::SocketAddr>().is_ok() {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let (host, port) = split_endpoint_host_port(endpoint)
            .ok_or_else(|| format!("Endpoint invalide: {endpoint}"))?;
        if host.parse::<std::net::IpAddr>().is_ok() {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let ips = if bootstrap_dns.trim().is_empty() {
            tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|e| format!("bootstrap Endpoint (DNS système) {host}: {e}"))?
                .map(|sa| sa.ip())
                .collect::<Vec<_>>()
        } else {
            fluxplay_providers::bootstrap_lookup_ip(bootstrap_dns, &host)
                .await
                .map_err(|e| format!("bootstrap Endpoint ({bootstrap_dns}) {host}: {e}"))?
        };
        let ip = ips
            .into_iter()
            .next()
            .ok_or_else(|| format!("bootstrap Endpoint: aucune IP pour {host}"))?;
        let rewritten = format!("{ip}:{port}");
        info!(%host, %rewritten, bootstrap = %bootstrap_dns, "WG Endpoint rewritten via bootstrap DNS");
        out.push_str("Endpoint = ");
        out.push_str(&rewritten);
        out.push('\n');
    }
    Ok(out)
}

fn split_endpoint_host_port(endpoint: &str) -> Option<(String, u16)> {
    let endpoint = endpoint.trim();
    if let Some(rest) = endpoint.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        let port = rest.trim_start_matches(':').parse().ok()?;
        return Some((host.to_string(), port));
    }
    let (host, port) = endpoint.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    Some((host.to_string(), port))
}

fn bootstrap_from_conf_or_settings(conf: &str, settings_bootstrap: &str) -> String {
    if !settings_bootstrap.trim().is_empty() {
        return settings_bootstrap.trim().to_string();
    }
    crate::network::parse_wg_dns(conf).unwrap_or_default()
}

/// Start userspace WG from a `.conf` file. Returns `socks5h://127.0.0.1:port`.
pub async fn start_tunnel_from_file(path: &Path, bootstrap_dns: &str) -> Result<String, String> {
    START_IN_FLIGHT.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = async {
        stop_tunnel();
        let raw = std::fs::read_to_string(path).map_err(|e| format!("lecture profil: {e}"))?;
        let bootstrap = bootstrap_from_conf_or_settings(&raw, bootstrap_dns);
        let conf = prepare_conf_with_bootstrap(&raw, &bootstrap).await?;
        start_tunnel_from_prepared(&conf).await
    }
    .await;
    START_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
    result
}

#[allow(dead_code)]
pub async fn start_tunnel_from_str(conf: &str, bootstrap_dns: &str) -> Result<String, String> {
    stop_tunnel();
    let bootstrap = bootstrap_from_conf_or_settings(conf, bootstrap_dns);
    let conf = prepare_conf_with_bootstrap(conf, &bootstrap).await?;
    start_tunnel_from_prepared(&conf).await
}

async fn start_tunnel_from_prepared(conf: &str) -> Result<String, String> {
    let mut last_err = String::new();
    for _ in 0..5 {
        let bind = match reserve_local_socks_addr().await {
            Ok(a) => a,
            Err(e) => {
                last_err = e;
                continue;
            }
        };
        match wg_socks::WgSocksProxy::start_from_str(conf, bind).await {
            Ok(proxy) => {
                // socks5h = remote DNS through the proxy (avoids clearnet DNS leak).
                let url = format!("socks5h://{bind}");
                info!(%url, "WireGuard userspace SOCKS proxy up (app-only, remote DNS)");
                if let Ok(mut g) = PROXY.get_or_init(|| Mutex::new(None)).lock() {
                    *g = Some(proxy);
                }
                set_socks_proxy_url(Some(url.clone()));
                return Ok(url);
            }
            Err(e) => {
                last_err = format!("{e:#}");
                warn!(error = %last_err, %bind, "WG SOCKS bind retry");
            }
        }
    }
    Err(format!("démarrage tunnel WireGuard: {last_err}"))
}

pub fn stop_tunnel() {
    if let Some(lock) = PROXY.get() {
        if let Ok(mut g) = lock.lock() {
            if let Some(proxy) = g.take() {
                proxy.shutdown();
                info!("WireGuard userspace proxy stopped");
            }
        }
    }
    set_socks_proxy_url(None);
}

pub fn tunnel_is_up() -> bool {
    socks_proxy_url().is_some()
}
