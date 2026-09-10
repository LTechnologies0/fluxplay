//! Shared reqwest clients with optional custom DNS (UDP / DoH / DoT).
//!
//! When an app-scoped WireGuard SOCKS proxy is active:
//! - traffic uses `socks5://` (local resolve, connect by IP through the tunnel)
//! - hostname lookups use the user's Custom / DoH / DoT prefs **through the tunnel**
//! - System mode + tunnel falls back to Cloudflare DoH through the tunnel
//!   (profile `DNS=` is never used here — bootstrap only, see fluxplay `network` / `wg_tunnel`)

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use fluxplay_core::{DnsMode, NetworkSettings};
use hickory_resolver::config::{NameServerConfigGroup, ResolverConfig, ResolverOpts};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::proto::op::{Message, MessageType, OpCode, Query};
use hickory_resolver::proto::rr::{Name as DnsName, RData, RecordType};
use hickory_resolver::TokioResolver;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_socks::tcp::Socks5Stream;
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
        socks = socks_proxy().as_deref().unwrap_or("-"),
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
        .gzip(true)
        .pool_max_idle_per_host(4);

    let socks = socks_proxy();

    // App-scoped WireGuard SOCKS takes priority over "no env proxy".
    if let Some(ref socks) = socks {
        match reqwest::Proxy::all(socks) {
            Ok(p) => {
                b = b.proxy(p);
                debug!(%socks, "HTTP client routed via WireGuard SOCKS");
            }
            Err(e) => {
                warn!(error = %e, %socks, "invalid SOCKS proxy URL — building without");
                if no_proxy {
                    b = b.no_proxy();
                }
            }
        }
    } else if no_proxy {
        // Portals / CDNs often reject Tor exit IPs — never inherit HTTP(S)_PROXY.
        b = b.no_proxy();
    }

    if let Some(ref socks) = socks {
        // Local resolve with user DNS prefs; queries go through the tunnel.
        b = b.dns_resolver(Arc::new(TunneledResolve {
            net: net.clone(),
            socks: socks.clone(),
        }));
        debug!(
            mode = net.dns_mode.label(),
            "tunneled DNS resolver attached (socks5 + user DNS)"
        );
    } else if let Some(resolver) = build_dns_resolver(net) {
        b = b.dns_resolver(resolver);
        debug!(mode = net.dns_mode.label(), "custom DNS resolver attached");
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

/// Resolve hostnames using the user's DNS prefs over the WireGuard SOCKS proxy.
struct TunneledResolve {
    net: NetworkSettings,
    socks: String,
}

impl Resolve for TunneledResolve {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().trim_end_matches('.').to_string();
        let net = self.net.clone();
        let socks = self.socks.clone();
        Box::pin(async move {
            let ips = resolve_through_tunnel(&net, &socks, &host)
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
            if ips.is_empty() {
                return Err(format!("no addresses for {host}").into());
            }
            let addrs: Addrs = Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

fn parse_socks_addr(socks: &str) -> std::io::Result<SocketAddr> {
    let rest = socks
        .trim()
        .strip_prefix("socks5://")
        .or_else(|| socks.trim().strip_prefix("socks5h://"))
        .unwrap_or(socks.trim());
    rest.parse::<SocketAddr>().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("bad SOCKS addr {socks}: {e}"),
        )
    })
}

async fn resolve_through_tunnel(
    net: &NetworkSettings,
    socks: &str,
    host: &str,
) -> std::io::Result<Vec<IpAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    let proxy = parse_socks_addr(socks)?;
    match net.dns_mode {
        DnsMode::Custom => {
            let servers = custom_dns_socket_addrs(&net.dns_servers);
            if servers.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "no custom DNS servers configured",
                ));
            }
            let mut last_err = None;
            for dns in servers {
                match dns_tcp_lookup(proxy, dns, host).await {
                    Ok(ips) if !ips.is_empty() => return Ok(ips),
                    Ok(_) => last_err = Some(format!("empty answer from {dns}")),
                    Err(e) => last_err = Some(e.to_string()),
                }
            }
            Err(std::io::Error::other(
                last_err.unwrap_or_else(|| "custom DNS via tunnel failed".into()),
            ))
        }
        DnsMode::Doh => doh_lookup_via_socks(socks, &net.doh_url, host).await,
        DnsMode::Dot => {
            // Prefer TCP/53 to the DoT IP through the tunnel (same resolver IPs).
            if let Some(dns) = first_dot_tcp_target(&net.dot_server) {
                if let Ok(ips) = dns_tcp_lookup(proxy, dns, host).await {
                    if !ips.is_empty() {
                        return Ok(ips);
                    }
                }
            }
            // Fallback: provider DoH through SOCKS (still tunnelled, not profile DNS).
            doh_lookup_via_socks(socks, doh_fallback_for_dot(&net.dot_server), host).await
        }
        DnsMode::System => {
            // Not profile DNS=: Cloudflare DoH inside the tunnel.
            doh_lookup_via_socks(socks, "https://cloudflare-dns.com/dns-query", host).await
        }
    }
}

fn custom_dns_socket_addrs(raw: &str) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for token in parse_server_tokens(raw) {
        let (host, port) = split_host_port(&token, 53);
        if let Ok(ip) = host.parse::<IpAddr>() {
            out.push(SocketAddr::new(ip, port));
        }
    }
    out
}

fn first_dot_tcp_target(server: &str) -> Option<SocketAddr> {
    let (host, _) = split_host_port(server.trim(), 853);
    let ip = host.parse::<IpAddr>().ok()?;
    Some(SocketAddr::new(ip, 53))
}

fn doh_fallback_for_dot(server: &str) -> &'static str {
    let s = server.trim().to_ascii_lowercase();
    if s.contains("google") || s.starts_with("8.8.") || s.contains("dns.google") {
        "https://dns.google/dns-query"
    } else if s.contains("quad9") || s.starts_with("9.9.9.9") {
        "https://dns.quad9.net/dns-query"
    } else {
        "https://cloudflare-dns.com/dns-query"
    }
}

async fn dns_tcp_lookup(
    proxy: SocketAddr,
    dns: SocketAddr,
    host: &str,
) -> std::io::Result<Vec<IpAddr>> {
    let mut ips = Vec::new();
    for qtype in [RecordType::A, RecordType::AAAA] {
        match dns_tcp_query(proxy, dns, host, qtype).await {
            Ok(mut got) => ips.append(&mut got),
            Err(e) => {
                debug!(%dns, ?qtype, error = %e, "DNS-over-TCP via SOCKS query failed");
            }
        }
    }
    if ips.is_empty() {
        Err(std::io::Error::other(
            format!("no A/AAAA for {host} via {dns}"),
        ))
    } else {
        Ok(ips)
    }
}

async fn dns_tcp_query(
    proxy: SocketAddr,
    dns: SocketAddr,
    host: &str,
    qtype: RecordType,
) -> std::io::Result<Vec<IpAddr>> {
    let mut stream = Socks5Stream::connect(proxy, dns)
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let qname = DnsName::from_utf8(host)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
    let mut msg = Message::new();
    msg.set_id(0xF1A7)
        .set_message_type(MessageType::Query)
        .set_op_code(OpCode::Query)
        .set_recursion_desired(true)
        .add_query(Query::query(qname, qtype));
    let payload = msg
        .to_vec()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let len = (payload.len() as u16).to_be_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&payload).await?;
    let mut len_buf = [0u8; 2];
    stream.read_exact(&mut len_buf).await?;
    let n = u16::from_be_bytes(len_buf) as usize;
    if n == 0 || n > 65_535 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid DNS TCP length",
        ));
    }
    let mut buf = vec![0u8; n];
    stream.read_exact(&mut buf).await?;
    let response = Message::from_vec(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(extract_ips(&response))
}

fn extract_ips(msg: &Message) -> Vec<IpAddr> {
    let mut ips = Vec::new();
    for rec in msg.answers() {
        match rec.data() {
            RData::A(a) => ips.push(IpAddr::V4(**a)),
            RData::AAAA(a) => ips.push(IpAddr::V6(**a)),
            _ => {}
        }
    }
    ips
}

async fn doh_lookup_via_socks(
    socks: &str,
    doh_url: &str,
    host: &str,
) -> std::io::Result<Vec<IpAddr>> {
    let mut ips = Vec::new();
    for qtype in [RecordType::A, RecordType::AAAA] {
        match doh_query_via_socks(socks, doh_url, host, qtype).await {
            Ok(mut got) => ips.append(&mut got),
            Err(e) => debug!(?qtype, error = %e, "DoH via SOCKS failed"),
        }
    }
    if ips.is_empty() {
        Err(std::io::Error::other(
            format!("DoH via tunnel returned no addresses for {host}"),
        ))
    } else {
        Ok(ips)
    }
}

fn doh_resolve_override(doh_url: &str) -> Option<(String, SocketAddr)> {
    let u = doh_url.trim().to_ascii_lowercase();
    if u.contains("cloudflare") || u.contains("1.1.1.1") || u.contains("mozilla") {
        return Some((
            "cloudflare-dns.com".into(),
            "1.1.1.1:443".parse().ok()?,
        ));
    }
    if u.contains("dns.google") || u.contains("8.8.8.8") || u.contains("google") {
        return Some(("dns.google".into(), "8.8.8.8:443".parse().ok()?));
    }
    if u.contains("quad9") || u.contains("9.9.9.9") {
        return Some(("dns.quad9.net".into(), "9.9.9.9:443".parse().ok()?));
    }
    None
}

async fn doh_query_via_socks(
    socks: &str,
    doh_url: &str,
    host: &str,
    qtype: RecordType,
) -> std::io::Result<Vec<IpAddr>> {
    let qname = DnsName::from_utf8(host)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
    let mut msg = Message::new();
    msg.set_id(0xD011)
        .set_message_type(MessageType::Query)
        .set_op_code(OpCode::Query)
        .set_recursion_desired(true)
        .add_query(Query::query(qname, qtype));
    let payload = msg
        .to_vec()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let b64 = base64_url_nopad(&payload);
    let base = doh_url.trim().trim_end_matches('/');
    let url = format!("{base}?dns={b64}");

    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(8))
        .pool_max_idle_per_host(2);
    let proxy = reqwest::Proxy::all(socks)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    builder = builder.proxy(proxy);
    if let Some((name, addr)) = doh_resolve_override(doh_url) {
        builder = builder.resolve(&name, addr);
    }

    let client = builder
        .build()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let resp = client
        .get(&url)
        .header("Accept", "application/dns-message")
        .send()
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(std::io::Error::other(
            format!("DoH HTTP {}", resp.status()),
        ));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let response = Message::from_vec(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(extract_ips(&response))
}

fn base64_url_nopad(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
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
    if let Some(socks) = socks_proxy() {
        return match resolve_through_tunnel(&net, &socks, &host).await {
            Ok(ips) if !ips.is_empty() => {
                let list: Vec<_> = ips.iter().map(|ip| ip.to_string()).collect();
                format!(
                    "{} → {} (via tunnel, mode {})",
                    host,
                    list.join(", "),
                    net.dns_mode.label()
                )
            }
            Ok(_) => format!("tunnel DNS OK mais aucune adresse pour {host}"),
            Err(e) => format!("échec DNS tunnelisé {host}: {e}"),
        };
    }
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
