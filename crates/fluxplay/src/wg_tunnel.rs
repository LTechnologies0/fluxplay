//! Userspace WireGuard tunnel scoped to FluxPlay (no system routes).
//!
//! Desktop + Android: [`crate::wg_proxy`] runs the WireGuard datapath in-process
//! and exposes one authenticated loopback port (SOCKS5 for HTTP clients, HTTP
//! proxy for mpv / FFmpeg), so only FluxPlay traffic exits through the tunnel
//! (no VpnService required).
//!
//! DNS policy:
//! - Profile `DNS=` resolves the peer **Endpoint** (bootstrap, clearnet) and is a
//!   fallback resolver inside the tunnel.
//! - Every app / player hostname is resolved by the proxy, inside the tunnel,
//!   with the user's Custom / DoH / DoT provider first.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use tracing::info;

use crate::wg_proxy::{parse_profile, WgProxy};

static PROXY: OnceLock<Mutex<Option<WgProxy>>> = OnceLock::new();
static START_IN_FLIGHT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn proxy_lock() -> &'static Mutex<Option<WgProxy>> {
    PROXY.get_or_init(|| Mutex::new(None))
}

fn with_proxy<R>(f: impl FnOnce(&WgProxy) -> R) -> Option<R> {
    proxy_lock().lock().ok().and_then(|g| g.as_ref().map(f))
}

pub fn tunnel_start_in_flight() -> bool {
    START_IN_FLIGHT.load(std::sync::atomic::Ordering::SeqCst)
}

/// `http://` URL (with credentials) for mpv / FFmpeg, which ignore SOCKS proxies.
pub fn player_proxy_url() -> Option<String> {
    with_proxy(WgProxy::http_url)
}

/// `127.0.0.1:port` — safe to show (no credentials).
pub fn proxy_display() -> Option<String> {
    with_proxy(|p| p.addr().to_string())
}

/// Settings "Test DNS": through the tunnel resolver when up, else the app resolver.
pub async fn probe_dns(host: &str) -> String {
    let Some((servers, lookup)) = with_proxy(|p| {
        let servers = p.dns_servers();
        let host = host.to_string();
        let dns = p.resolver();
        (servers, async move { dns.resolve(&host).await })
    }) else {
        return fluxplay_providers::probe_dns(host).await;
    };
    let first = servers.first().map(|s| s.ip().to_string()).unwrap_or_else(|| "-".into());
    match lookup.await {
        Ok(ips) => {
            let list: Vec<_> = ips.iter().map(|ip| ip.to_string()).collect();
            format!("{host} → {} (dans le tunnel, résolveur {first})", list.join(", "))
        }
        Err(e) => format!("échec DNS dans le tunnel pour {host} : {e}"),
    }
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

/// Start userspace WG from a `.conf` file. Returns the loopback proxy address.
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
    let profile = parse_profile(conf).map_err(|e| format!("profil WireGuard : {e}"))?;
    let proxy = WgProxy::start(&profile)
        .await
        .map_err(|e| format!("démarrage tunnel WireGuard : {e}"))?;
    let local_addr = proxy.addr().to_string();
    let socks = proxy.socks_url();
    if let Ok(mut g) = proxy_lock().lock() {
        if let Some(old) = g.replace(proxy) {
            old.shutdown();
        }
    }
    fluxplay_providers::set_socks_proxy(Some(socks));
    info!(proxy = %local_addr, mtu = profile.mtu(), "WireGuard app proxy up (SOCKS5 + HTTP, tunnel DNS)");
    Ok(local_addr)
}

pub fn stop_tunnel() {
    let old = proxy_lock().lock().ok().and_then(|mut g| g.take());
    fluxplay_providers::set_socks_proxy(None);
    if let Some(proxy) = old {
        proxy.shutdown();
        info!("WireGuard userspace proxy stopped");
    }
}

pub fn tunnel_is_up() -> bool {
    proxy_lock().lock().map(|g| g.is_some()).unwrap_or(false)
}
