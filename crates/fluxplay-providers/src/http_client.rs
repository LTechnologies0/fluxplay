//! Shared reqwest clients with optional custom DNS (UDP / DoH / DoT).
//!
//! When the app-scoped WireGuard proxy is active, traffic uses `socks5h://`:
//! hostnames are resolved by the proxy inside the tunnel (see fluxplay `wg_proxy`),
//! so no local resolver is attached. While WireGuard is enabled but not up yet,
//! requests are pointed at a dead proxy instead of leaking to the clearnet.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use fluxplay_core::{DnsMode, NetworkSettings};
use hickory_resolver::config::{NameServerConfigGroup, ResolverConfig, ResolverOpts};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::TokioResolver;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tracing::{debug, info, warn};

use crate::{ProviderError, Result};

static NET: OnceLock<Mutex<NetworkSettings>> = OnceLock::new();
static PORTAL: OnceLock<Mutex<Option<reqwest::Client>>> = OnceLock::new();
/// Cached generic client keyed by `user_agent|timeout_secs`.
static GENERIC: OnceLock<Mutex<Option<(String, reqwest::Client)>>> = OnceLock::new();
static SOCKS: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn net_lock() -> &'static Mutex<NetworkSettings> {
    NET.get_or_init(|| Mutex::new(NetworkSettings::default()))
}

fn portal_lock() -> &'static Mutex<Option<reqwest::Client>> {
    PORTAL.get_or_init(|| Mutex::new(None))
}

fn generic_lock() -> &'static Mutex<Option<(String, reqwest::Client)>> {
    GENERIC.get_or_init(|| Mutex::new(None))
}

fn socks_lock() -> &'static Mutex<Option<String>> {
    SOCKS.get_or_init(|| Mutex::new(None))
}

/// Point HTTP clients at the app-scoped WireGuard SOCKS proxy (`socks5://…`).
pub fn set_socks_proxy(url: Option<String>) {
    if let Ok(mut g) = socks_lock().lock() {
        *g = url;
    }
    // Force rebuild so the next request uses / drops the proxy.
    if let Ok(mut g) = portal_lock().lock() {
        *g = None;
    }
    if let Ok(mut g) = generic_lock().lock() {
        *g = None;
    }
}

pub fn socks_proxy() -> Option<String> {
    socks_lock().lock().ok().and_then(|g| g.clone())
}

/// Discard port: connections are refused, so nothing leaves the machine.
const DEAD_PROXY: &str = "socks5h://127.0.0.1:9";

/// Proxy every app request must use: the tunnel, or a dead end while it is
/// enabled but not established (fail closed).
pub fn required_proxy() -> Option<String> {
    socks_proxy().or_else(|| {
        current_network_settings()
            .wireguard_enabled
            .then(|| DEAD_PROXY.to_string())
    })
}

/// Apply DNS / network prefs and drop cached HTTP clients (next call rebuilds).
pub fn apply_network_settings(settings: &NetworkSettings) {
    if let Ok(mut g) = net_lock().lock() {
        *g = settings.clone();
    }
    if let Ok(mut g) = portal_lock().lock() {
        *g = None;
    }
    if let Ok(mut g) = generic_lock().lock() {
        *g = None;
    }
    info!(
        dns = settings.dns_mode.label(),
        wg = settings.wireguard_enabled,
        tunnel = socks_proxy().is_some(),
        "network settings applied (HTTP clients reset)"
    );
}

pub fn current_network_settings() -> NetworkSettings {
    net_lock()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Portal / Xtream client (Smarters UA, no env proxy).
pub fn shared_http() -> Result<reqwest::Client> {
    let mut slot = portal_lock()
        .lock()
        .map_err(|_| ProviderError::Message("http client lock poisoned".into()))?;
    if let Some(c) = slot.as_ref() {
        return Ok(c.clone());
    }
    let net = current_network_settings();
    let client = build_client(
        crate::xtream::SMARTERS_UA,
        Duration::from_secs(120),
        Duration::from_secs(20),
        true,
        &net,
    )?;
    debug!("shared portal HTTP client initialized");
    *slot = Some(client.clone());
    Ok(client)
}

/// Generic app HTTP (images / metadata) — same DNS prefs, shorter timeouts.
///
/// `user_agent` is honored (IPTV logo CDNs often require a Smarters-like UA).
/// Clients are cached per `(user_agent, timeout_secs)` and reset on network/SOCKS changes.
pub fn app_http(user_agent: &str, timeout_secs: u64) -> Result<reqwest::Client> {
    let timeout_secs = timeout_secs.max(8);
    let cache_key = format!("{user_agent}|{timeout_secs}");
    let mut slot = generic_lock()
        .lock()
        .map_err(|_| ProviderError::Message("http client lock poisoned".into()))?;
    if let Some((key, c)) = slot.as_ref() {
        if key == &cache_key {
            return Ok(c.clone());
        }
    }
    let net = current_network_settings();
    let client = build_client(
        user_agent,
        Duration::from_secs(timeout_secs),
        Duration::from_secs(8),
        true,
        &net,
    )?;
    debug!(ua = user_agent, timeout_secs, "shared app HTTP client initialized");
    *slot = Some((cache_key, client.clone()));
    Ok(client)
}

/// Last-resort HTTP client that still respects WireGuard fail-closed.
///
/// Prefer [`app_http`] / [`shared_http`]. This path exists so `unwrap_or_else`
/// callers never fall through to a clearnet builder while the tunnel is required.
pub fn emergency_http(user_agent: &str, timeout_secs: u64) -> reqwest::Client {
    match app_http(user_agent, timeout_secs) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "app_http failed — building fail-closed emergency client");
            let net = current_network_settings();
            match build_client(
                user_agent,
                Duration::from_secs(timeout_secs.max(8)),
                Duration::from_secs(6),
                true,
                &net,
            ) {
                Ok(c) => c,
                Err(_) => {
                    // Absolute last resort: still never clearnet when WG is on.
                    let mut b = reqwest::Client::builder()
                        .user_agent(user_agent)
                        .timeout(Duration::from_secs(timeout_secs.max(8)))
                        .connect_timeout(Duration::from_secs(6))
                        .redirect(reqwest::redirect::Policy::limited(8))
                        .referer(false);
                    if let Some(px) = required_proxy() {
                        match reqwest::Proxy::all(&px) {
                            Ok(proxy) => b = b.proxy(proxy),
                            Err(_) => {
                                b = b.proxy(
                                    reqwest::Proxy::all(DEAD_PROXY)
                                        .expect("DEAD_PROXY is a valid socks URL"),
                                );
                            }
                        }
                    } else {
                        b = b.no_proxy();
                    }
                    b.build().expect("emergency http client")
                }
            }
        }
    }
}

fn build_client(
    user_agent: &str,
    timeout: Duration,
    connect_timeout: Duration,
    no_proxy: bool,
    net: &NetworkSettings,
) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(timeout)
        .connect_timeout(connect_timeout)
        .redirect(reqwest::redirect::Policy::limited(8))
        // A redirect must not forward `?username=&password=` to the next host as Referer.
        .referer(false)
        .gzip(true)
        .pool_max_idle_per_host(4);

    let socks = required_proxy();

    // App-scoped WireGuard proxy takes priority over "no env proxy".
    if let Some(ref socks) = socks {
        let proxy = reqwest::Proxy::all(socks).map_err(|e| {
            ProviderError::Message(
                fluxplay_core::FpCode::NetProxyRefuse
                    .ensure_coded(format!("tunnel proxy URL: {e}")),
            )
        })?;
        b = b.proxy(proxy);
        debug!("HTTP client routed via WireGuard proxy (tunnel DNS)");
    } else {
        if no_proxy {
            // Portals / CDNs often reject Tor exit IPs — never inherit HTTP(S)_PROXY.
            b = b.no_proxy();
        }
        if let Some(resolver) = build_dns_resolver(net) {
            b = b.dns_resolver(resolver);
            debug!(mode = net.dns_mode.label(), "custom DNS resolver attached");
        }
    }

    b.build().map_err(|e| {
        ProviderError::Message(
            fluxplay_core::FpCode::NetTcpConnect
                .ensure_coded(format!("http client build failed: {e}")),
        )
    })
}

/// Read a response body with a hard byte budget (CWE-400).
/// Rejects oversized `Content-Length` before buffering; streams otherwise.
pub async fn read_body_capped(
    mut resp: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    if let Some(len) = resp.content_length() {
        if len as usize > max_bytes {
            return Err(ProviderError::Message(
                fluxplay_core::FpCode::NetHttpBody.ensure_coded(format!(
                    "réponse trop volumineuse ({len} o, max {max_bytes})"
                )),
            ));
        }
    }
    let mut out = Vec::with_capacity(max_bytes.min(64 * 1024));
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if out.len().saturating_add(chunk.len()) > max_bytes {
                    return Err(ProviderError::Message(
                        fluxplay_core::FpCode::NetHttpBody.ensure_coded(format!(
                            "réponse trop volumineuse (max {max_bytes} o)"
                        )),
                    ));
                }
                out.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(out)
}

struct HickoryResolve {
    resolver: Arc<TokioResolver>,
}

impl Resolve for HickoryResolve {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().trim_end_matches('.').to_string();
        let resolver = Arc::clone(&self.resolver);
        Box::pin(async move {
            let lookup = resolver.lookup_ip(host).await.map_err(|e| {
                std::io::Error::other(e.to_string())
            })?;
            let addrs: Addrs = Box::new(lookup.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

fn build_dns_resolver(net: &NetworkSettings) -> Option<Arc<HickoryResolve>> {
    if matches!(net.dns_mode, DnsMode::System) {
        return None;
    }
    // Non-System modes must never silently fall back to OS DNS (UI would lie).
    // Incomplete Custom/DoH/DoT → Cloudflare preset matching the selected mode.
    let config = match resolver_config(net) {
        Some(c) => c,
        None => {
            let fallback = match net.dns_mode {
                DnsMode::Dot => ResolverConfig::cloudflare_tls(),
                DnsMode::Custom => ResolverConfig::cloudflare(),
                DnsMode::Doh | DnsMode::System => ResolverConfig::cloudflare_https(),
            };
            warn!(
                mode = net.dns_mode.label(),
                "DNS config incomplete — using Cloudflare preset (not system DNS)"
            );
            fallback
        }
    };
    let mut opts = ResolverOpts::default();
    opts.timeout = Duration::from_secs(5);
    opts.attempts = 2;
    let resolver = TokioResolver::builder_with_config(config, TokioConnectionProvider::default())
        .with_options(opts)
        .build();
    Some(Arc::new(HickoryResolve {
        resolver: Arc::new(resolver),
    }))
}

fn resolver_config(net: &NetworkSettings) -> Option<ResolverConfig> {
    match net.dns_mode {
        DnsMode::System => None,
        DnsMode::Custom => {
            let group = custom_servers(&net.dns_servers)?;
            Some(ResolverConfig::from_parts(None, vec![], group))
        }
        DnsMode::Doh => Some(doh_config(&net.doh_url)),
        DnsMode::Dot => Some(dot_config(&net.dot_server)),
    }
}

fn parse_server_tokens(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn custom_servers(raw: &str) -> Option<NameServerConfigGroup> {
    let mut ips: Vec<IpAddr> = Vec::new();
    let mut port = 53u16;
    for token in parse_server_tokens(raw) {
        let (host, p) = split_host_port(&token, 53);
        let Ok(ip) = host.parse::<IpAddr>() else {
            warn!(server = %token, "skip non-IP DNS server (use DoH/DoT for hostnames)");
            continue;
        };
        if ips.is_empty() {
            port = p;
        }
        ips.push(ip);
    }
    if ips.is_empty() {
        None
    } else {
        Some(NameServerConfigGroup::from_ips_clear(&ips, port, true))
    }
}

fn split_host_port(token: &str, default_port: u16) -> (&str, u16) {
    if let Some((h, p)) = token.rsplit_once(':') {
        // Avoid treating IPv6 as host:port unless bracketed.
        if h.contains(':') && !token.starts_with('[') {
            return (token, default_port);
        }
        if let Ok(port) = p.parse::<u16>() {
            return (h.trim_matches(['[', ']']), port);
        }
    }
    (token.trim_matches(['[', ']']), default_port)
}

fn doh_config(url: &str) -> ResolverConfig {
    let u = url.trim().to_ascii_lowercase();
    if u.contains("cloudflare") || u.contains("1.1.1.1") || u.contains("mozilla") {
        return ResolverConfig::cloudflare_https();
    }
    if u.contains("dns.google") || u.contains("8.8.8.8") || u.contains("google") {
        return ResolverConfig::google_https();
    }
    if u.contains("quad9") || u.contains("9.9.9.9") {
        return ResolverConfig::quad9_https();
    }
    warn!(url = %url, "unknown DoH URL — using Cloudflare DoH preset");
    ResolverConfig::cloudflare_https()
}

fn dot_config(server: &str) -> ResolverConfig {
    let s = server.trim().to_ascii_lowercase();
    if s.contains("cloudflare") || s.starts_with("1.1.1.1") || s.starts_with("1.0.0.1") {
        return ResolverConfig::cloudflare_tls();
    }
    if s.contains("google")
        || s.starts_with("8.8.8.8")
        || s.starts_with("8.8.4.4")
        || s.contains("dns.google")
    {
        return ResolverConfig::google_tls();
    }
    if s.contains("quad9") || s.starts_with("9.9.9.9") {
        return ResolverConfig::quad9_tls();
    }
    let (host, port) = split_host_port(&s, 853);
    if let Ok(ip) = host.parse::<IpAddr>() {
        let group = NameServerConfigGroup::from_ips_tls(&[ip], port, host.to_string(), true);
        return ResolverConfig::from_parts(None, vec![], group);
    }
    warn!(server = %server, "unknown DoT server — using Cloudflare DoT");
    ResolverConfig::cloudflare_tls()
}

/// Bootstrap resolve (clearnet) using WG profile DNS — Endpoint only, before tunnel up.
pub async fn bootstrap_lookup_ip(bootstrap_dns: &str, host: &str) -> std::io::Result<Vec<IpAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    let group = custom_servers(bootstrap_dns).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "bootstrap DNS empty or invalid",
        )
    })?;
    let config = ResolverConfig::from_parts(None, vec![], group);
    let mut opts = ResolverOpts::default();
    opts.timeout = Duration::from_secs(5);
    opts.attempts = 2;
    let resolver = TokioResolver::builder_with_config(config, TokioConnectionProvider::default())
        .with_options(opts)
        .build();
    let lookup = resolver
        .lookup_ip(host)
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(lookup.iter().collect())
}

/// Outcome of a cheap `/` vs deep-path probe (WAF / sortie filtrée).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpPathPolicy {
    /// Both root and a deep path answered (or deep path gave HTTP, not RST).
    Open,
    /// TCP works for `/` but any deeper path is reset — WAF on filtered egress
    /// (home Wi‑Fi, phone hotspot, phone/system VPN exit, ISP DPI).
    RootOnly,
    /// Host unreachable / root also fails.
    Unreachable,
}

/// Detect “landing page OK, every IPTV path RST” (origine.one / drm-cloud class).
///
/// Used after a `connection reset` on `player_api` / media so the UI can say
/// path policy instead of a vague TCP error — DNSCrypt/`127.0.0.1` is fine.
pub async fn probe_http_path_policy(origin: &str) -> HttpPathPolicy {
    let origin = origin.trim_end_matches('/');
    if origin.is_empty() {
        return HttpPathPolicy::Unreachable;
    }
    let client = match shared_http() {
        Ok(c) => c,
        Err(_) => return HttpPathPolicy::Unreachable,
    };
    let root = format!("{origin}/");
    let deep = format!("{origin}/player_api.php");
    let root_ok = match client
        .get(&root)
        .timeout(Duration::from_secs(4))
        .send()
        .await
    {
        Ok(r) => r.status().as_u16() < 500,
        Err(_) => false,
    };
    if !root_ok {
        return HttpPathPolicy::Unreachable;
    }
    match client
        .get(&deep)
        .timeout(Duration::from_secs(4))
        .send()
        .await
    {
        Ok(_) => HttpPathPolicy::Open,
        Err(e) => {
            let blob = e.to_string().to_ascii_lowercase();
            if blob.contains("reset") || blob.contains("broken pipe") {
                warn!(
                    origin = %origin,
                    "HTTP path policy: root OK, deep path RST (filtered egress/WAF)"
                );
                HttpPathPolicy::RootOnly
            } else {
                // Timeout/refused on deep while root works — still treat as path policy.
                HttpPathPolicy::RootOnly
            }
        }
    }
}

/// Human + coded error when [`HttpPathPolicy::RootOnly`] is confirmed.
pub fn path_policy_blocked_msg(origin: &str) -> String {
    fluxplay_core::FpCode::NetClearnetBlocked.ensure_coded(format!(
        "Réseau filtré: {origin} répond sur `/` mais RST sur player_api/live/movie — hotspot téléphone, VPN téléphone/système (pas FluxPlay) ou FAI ; DNSCrypt OK. Autre réseau, coupe le VPN du téléphone, ou tunnel FluxPlay"
    ))
}

/// Best-effort public egress IP (via app HTTP client / SOCKS when tunnel is up).
pub async fn probe_egress_ip() -> Option<String> {
    let client = shared_http().ok()?;
    // Plain HTTP avoids TLS traps on locked-down nets; body is a single IPv4/IPv6.
    let resp = client
        .get("http://api.ipify.org/")
        .timeout(Duration::from_secs(4))
        .send()
        .await
        .ok()?;
    let body = resp.text().await.ok()?;
    let ip = body.trim();
    if ip.is_empty() || ip.len() > 64 || ip.contains(char::is_whitespace) {
        return None;
    }
    Some(ip.to_string())
}

/// Egress IP + optional portal `/` vs deep-path (no DNS line — caller prepends).
pub async fn probe_egress_and_path(portal_origin: Option<&str>) -> String {
    let mut parts = Vec::with_capacity(2);
    match probe_egress_ip().await {
        Some(ip) => parts.push(format!(
            "sortie {ip} (si ≠ IP mobile/FAI attendue → VPN téléphone/système probable)"
        )),
        None => parts.push("sortie publique: indisponible".into()),
    }
    if let Some(origin) = portal_origin.map(str::trim).filter(|s| !s.is_empty()) {
        let origin = origin.trim_end_matches('/');
        let path = match probe_http_path_policy(origin).await {
            HttpPathPolicy::Open => format!("{origin} chemins IPTV OK"),
            HttpPathPolicy::RootOnly => {
                format!("{origin} `/` OK mais chemins IPTV RST (réseau/VPN filtré)")
            }
            HttpPathPolicy::Unreachable => format!("{origin} injoignable"),
        };
        parts.push(path);
    }
    parts.join(" · ")
}

/// Settings “Tester DNS”: resolver + egress IP + optional portal `/` vs deep-path.
pub async fn probe_network_diag(dns_host: &str, portal_origin: Option<&str>) -> String {
    format!(
        "{} · {}",
        probe_dns(dns_host).await,
        probe_egress_and_path(portal_origin).await
    )
}

/// Quick connectivity probe for settings UI (does not throw).
pub async fn probe_dns(hostname: &str) -> String {
    let net = current_network_settings();
    let host = hostname.trim_end_matches('.').to_string();
    let Some(resolver) = build_dns_resolver(&net) else {
        return format!("DNS système — résolution déléguée à l’OS ({host})");
    };
    match resolver.resolver.lookup_ip(&host).await {
        Ok(lookup) => {
            let ips: Vec<_> = lookup.iter().map(|ip| ip.to_string()).collect();
            if ips.is_empty() {
                format!("résolveur OK mais aucune adresse pour {host}")
            } else {
                format!("{} → {}", host, ips.join(", "))
            }
        }
        Err(e) => format!("échec résolution {host}: {e}"),
    }
}
