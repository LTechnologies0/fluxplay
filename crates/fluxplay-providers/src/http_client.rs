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
static GENERIC: OnceLock<Mutex<Option<reqwest::Client>>> = OnceLock::new();
static SOCKS: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn net_lock() -> &'static Mutex<NetworkSettings> {
    NET.get_or_init(|| Mutex::new(NetworkSettings::default()))
}

fn portal_lock() -> &'static Mutex<Option<reqwest::Client>> {
    PORTAL.get_or_init(|| Mutex::new(None))
}

fn generic_lock() -> &'static Mutex<Option<reqwest::Client>> {
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
pub fn app_http(_user_agent: &str, timeout_secs: u64) -> Result<reqwest::Client> {
    let mut slot = generic_lock()
        .lock()
        .map_err(|_| ProviderError::Message("http client lock poisoned".into()))?;
    if let Some(c) = slot.as_ref() {
        return Ok(c.clone());
    }
    let net = current_network_settings();
    let client = build_client(
        "FluxPlay/0.2 (+catalog; images; metadata)",
        Duration::from_secs(timeout_secs.max(14)),
        Duration::from_secs(8),
        true,
        &net,
    )?;
    debug!("shared app HTTP client initialized");
    *slot = Some(client.clone());
    Ok(client)
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
        let proxy = reqwest::Proxy::all(socks)
            .map_err(|e| ProviderError::Message(format!("tunnel proxy URL: {e}")))?;
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

    b.build()
        .map_err(|e| ProviderError::Message(format!("http client build failed: {e}")))
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
    let config = match resolver_config(net) {
        Some(c) => c,
        None => {
            warn!(
                mode = net.dns_mode.label(),
                "DNS config incomplete — falling back to system"
            );
            return None;
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
