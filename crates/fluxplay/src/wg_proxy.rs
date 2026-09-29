//! In-process WireGuard datapath exposed on one authenticated loopback port.
//!
//! The port speaks both proxy dialects FluxPlay needs:
//! - SOCKS5 (`socks5h://`, RFC 1929 user/pass) for reqwest clients;
//! - HTTP proxy (`CONNECT` + absolute-form requests, Basic auth) for FFmpeg / mpv,
//!   whose `http_proxy` option ignores SOCKS URLs.
//!
//! Hostnames are always resolved by [`TunnelDns`] (DNS over TCP inside the
//! tunnel), never by the OS resolver.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use fluxplay_core::DnsMode;
use ipnet::IpNet;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};
use tokio::time::timeout;
use tokio_wireguard::config::{Address, Config, Interface as InterfaceConfig, Peer};
use tokio_wireguard::x25519::{PublicKey, StaticSecret};
use tokio_wireguard::Interface;
use tracing::{debug, info, warn};

/// wg-quick default when the profile has no `MTU =`.
const DEFAULT_MTU: usize = 1420;
/// Per-connection smoltcp buffers. Receive window bounds throughput (≈ window / RTT).
const TCP_RECV_BUFFER: usize = 512 * 1024;
const TCP_SEND_BUFFER: usize = 128 * 1024;
/// Bounds tunnel memory: each live connection owns both buffers above.
const MAX_CLIENTS: usize = 192;
/// boringtun retries the handshake every 5 s; allow two retries.
const HANDSHAKE_PROBE: Duration = Duration::from_secs(12);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DNS_TIMEOUT: Duration = Duration::from_secs(4);
const HEAD_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HEAD: usize = 16 * 1024;
const DNS_CACHE_MAX: usize = 1024;
const SPLICE_BUF: usize = 64 * 1024;

// ── Profile ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Profile {
    private_key: [u8; 32],
    address: Address,
    dns: Vec<IpAddr>,
    mtu: Option<usize>,
    listen_port: Option<u16>,
    peers: Vec<PeerProfile>,
}

#[derive(Debug, Clone)]
struct PeerProfile {
    public_key: [u8; 32],
    preshared_key: Option<[u8; 32]>,
    endpoint: SocketAddr,
    allowed_ips: Vec<IpNet>,
    keepalive: Option<u16>,
}

impl Profile {
    pub fn mtu(&self) -> usize {
        self.mtu.unwrap_or(DEFAULT_MTU)
    }

    pub fn has_preshared_key(&self) -> bool {
        self.peers.iter().any(|p| p.preshared_key.is_some())
    }
}

fn decode_key(value: &str, what: &str) -> Result<[u8; 32], String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .map_err(|_| format!("{what} : base64 invalide"))?;
    bytes
        .try_into()
        .map_err(|_| format!("{what} : 32 octets attendus"))
}

fn parse_net(token: &str) -> Option<IpNet> {
    let token = token.trim();
    token
        .parse::<IpNet>()
        .ok()
        .or_else(|| token.parse::<IpAddr>().ok().map(IpNet::from))
}

/// Parse a wg-quick profile whose `Endpoint`s are already IP literals.
pub fn parse_profile(conf: &str) -> Result<Profile, String> {
    #[derive(PartialEq)]
    enum Section {
        None,
        Interface,
        Peer,
    }
    let mut section = Section::None;
    let mut private_key = None;
    let mut v4 = None;
    let mut v6 = None;
    let mut dns = Vec::new();
    let mut mtu = None;
    let mut listen_port = None;
    let mut peers: Vec<PeerProfile> = Vec::new();
    let mut peer: Option<(Option<[u8; 32]>, Option<[u8; 32]>, Option<SocketAddr>, Vec<IpNet>, Option<u16>)> =
        None;

    fn finish_peer(
        peer: Option<(Option<[u8; 32]>, Option<[u8; 32]>, Option<SocketAddr>, Vec<IpNet>, Option<u16>)>,
        peers: &mut Vec<PeerProfile>,
    ) -> Result<(), String> {
        let Some((public_key, preshared_key, endpoint, allowed_ips, keepalive)) = peer else {
            return Ok(());
        };
        let public_key = public_key.ok_or("[Peer] sans PublicKey")?;
        let endpoint = endpoint.ok_or("[Peer] sans Endpoint")?;
        if allowed_ips.is_empty() {
            return Err("[Peer] sans AllowedIPs".into());
        }
        peers.push(PeerProfile {
            public_key,
            preshared_key,
            endpoint,
            allowed_ips,
            keepalive,
        });
        Ok(())
    }

    for raw in conf.lines() {
        let line = raw.split(['#', ';']).next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            let name = line.trim_matches(['[', ']']).trim();
            if name.eq_ignore_ascii_case("interface") {
                section = Section::Interface;
            } else if name.eq_ignore_ascii_case("peer") {
                finish_peer(peer.take(), &mut peers)?;
                peer = Some((None, None, None, Vec::new(), None));
                section = Section::Peer;
            } else {
                section = Section::None;
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match section {
            Section::Interface => match key.as_str() {
                "privatekey" => private_key = Some(decode_key(value, "PrivateKey")?),
                "address" => {
                    for token in value.split(',') {
                        match parse_net(token) {
                            Some(IpNet::V4(n)) if v4.is_none() => v4 = Some(n),
                            Some(IpNet::V6(n)) if v6.is_none() => v6 = Some(n),
                            Some(n) => warn!(%n, "extra WireGuard Address ignored (one per family)"),
                            None => return Err(format!("Address invalide : {}", token.trim())),
                        }
                    }
                }
                // Non-IP entries are search domains (wg-quick semantics).
                "dns" => dns.extend(value.split(',').filter_map(|t| t.trim().parse::<IpAddr>().ok())),
                "mtu" => {
                    let m: usize = value.parse().map_err(|_| format!("MTU invalide : {value}"))?;
                    if !(576..=9000).contains(&m) {
                        return Err(format!("MTU hors limites : {m}"));
                    }
                    mtu = Some(m);
                }
                "listenport" => listen_port = value.parse().ok(),
                _ => debug!(key, "WireGuard [Interface] key ignored"),
            },
            Section::Peer => {
                let Some(p) = peer.as_mut() else { continue };
                match key.as_str() {
                    "publickey" => p.0 = Some(decode_key(value, "PublicKey")?),
                    "presharedkey" => p.1 = Some(decode_key(value, "PresharedKey")?),
                    "endpoint" => {
                        p.2 = Some(value.parse().map_err(|_| {
                            format!("Endpoint doit être IP:port après résolution : {value}")
                        })?)
                    }
                    "allowedips" => {
                        for token in value.split(',').filter(|t| !t.trim().is_empty()) {
                            p.3.push(
                                parse_net(token)
                                    .ok_or_else(|| format!("AllowedIPs invalide : {}", token.trim()))?,
                            );
                        }
                    }
                    "persistentkeepalive" => {
                        p.4 = value.parse::<u16>().ok().filter(|&k| k > 0);
                    }
                    _ => debug!(key, "WireGuard [Peer] key ignored"),
                }
            }
            Section::None => {}
        }
    }
    finish_peer(peer.take(), &mut peers)?;

    let private_key = private_key.ok_or("[Interface] sans PrivateKey")?;
    let address = match (v4, v6) {
        (Some(a), Some(b)) => Address::Dual(a, b),
        (Some(a), None) => Address::V4(a),
        (None, Some(b)) => Address::V6(b),
        (None, None) => return Err("[Interface] sans Address".into()),
    };
    if peers.is_empty() {
        return Err("aucun [Peer] dans le profil".into());
    }
    Ok(Profile {
        private_key,
        address,
        dns,
        mtu,
        listen_port,
        peers,
    })
}

fn build_interface(profile: &Profile) -> io::Result<Interface> {
    let config = Config {
        interface: InterfaceConfig {
            address: profile.address,
            listen_port: profile.listen_port,
            private_key: StaticSecret::from(profile.private_key),
            mtu: Some(profile.mtu()),
        },
        peers: profile
            .peers
            .iter()
            .map(|p| Peer {
                endpoint: Some(p.endpoint),
                allowed_ips: p.allowed_ips.clone(),
                public_key: PublicKey::from(p.public_key),
                preshared_key: p.preshared_key,
                persistent_keepalive: p.keepalive,
            })
            .collect(),
    };
    let mut options = tokio_wireguard::interface::Options::default();
    options.tcp.recv_buffer_size = TCP_RECV_BUFFER;
    options.tcp.send_buffer_size = TCP_SEND_BUFFER;
    options.tcp.connect_timeout = CONNECT_TIMEOUT;
    Interface::new_with(config, options)
}

// ── DNS inside the tunnel ───────────────────────────────────────────────────

/// DNS over TCP to resolvers reached through the tunnel, with a small TTL cache.
pub struct TunnelDns {
    iface: Interface,
    profile_dns: Vec<IpAddr>,
    has_v4: bool,
    has_v6: bool,
    cache: Mutex<HashMap<String, (Vec<IpAddr>, Instant)>>,
}

/// Plain-DNS address of a DoH/DoT provider (queries still travel encrypted in the tunnel).
fn provider_ip(spec: &str) -> Option<IpAddr> {
    let s = spec.trim().to_ascii_lowercase();
    let host = s
        .trim_start_matches("https://")
        .trim_start_matches("tls://")
        .split(['/', '?'])
        .next()
        .unwrap_or("");
    let host = if let Some(bracketed) = host.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else if host.matches(':').count() == 1 {
        host.split(':').next().unwrap_or("")
    } else {
        host
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(ip);
    }
    if s.contains("cloudflare") || s.contains("mozilla") {
        Some(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)))
    } else if s.contains("google") {
        Some(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)))
    } else if s.contains("quad9") {
        Some(IpAddr::V4(Ipv4Addr::new(9, 9, 9, 9)))
    } else {
        None
    }
}

fn custom_server_addrs(raw: &str) -> Vec<SocketAddr> {
    raw.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .filter_map(|t| {
            t.parse::<SocketAddr>()
                .ok()
                .or_else(|| t.trim_matches(['[', ']']).parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, 53)))
        })
        .collect()
}

impl TunnelDns {
    fn new(iface: Interface, profile: &Profile) -> Self {
        Self {
            iface,
            profile_dns: profile.dns.clone(),
            has_v4: profile.address.is_v4(),
            has_v6: profile.address.is_v6(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn reachable(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(_) => self.has_v4,
            IpAddr::V6(_) => self.has_v6,
        }
    }

    /// User DNS preference first, then profile `DNS=`, then public resolvers.
    pub fn servers(&self) -> Vec<SocketAddr> {
        let net = fluxplay_providers::current_network_settings();
        let mut out: Vec<SocketAddr> = match net.dns_mode {
            DnsMode::Custom => custom_server_addrs(&net.dns_servers),
            DnsMode::Doh => provider_ip(&net.doh_url).map(|ip| SocketAddr::new(ip, 53)).into_iter().collect(),
            DnsMode::Dot => provider_ip(&net.dot_server).map(|ip| SocketAddr::new(ip, 53)).into_iter().collect(),
            DnsMode::System => Vec::new(),
        };
        out.extend(self.profile_dns.iter().map(|ip| SocketAddr::new(*ip, 53)));
        out.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 53));
        out.push(SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111)), 53));
        out.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(9, 9, 9, 9)), 53));
        let mut seen = Vec::new();
        out.retain(|s| self.reachable(s.ip()) && !seen.contains(s) && {
            seen.push(*s);
            true
        });
        out
    }

    pub async fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
            return Ok(vec![ip]);
        }
        if host.is_empty() || host.len() > 253 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "nom d’hôte invalide"));
        }
        if let Some((ips, until)) = self.cache.lock().ok().and_then(|c| c.get(&host).cloned()) {
            if until > Instant::now() {
                return Ok(ips);
            }
        }
        let mut last_err = io::Error::other("aucun serveur DNS joignable dans le tunnel");
        for server in self.servers() {
            match timeout(DNS_TIMEOUT * 2, self.lookup(server, &host)).await {
                Ok(Ok((ips, ttl))) if !ips.is_empty() => {
                    self.remember(&host, &ips, ttl);
                    return Ok(ips);
                }
                Ok(Ok(_)) => {
                    last_err = io::Error::new(io::ErrorKind::NotFound, format!("aucune adresse pour {host}"))
                }
                Ok(Err(e)) if e.kind() == io::ErrorKind::NotFound => return Err(e),
                Ok(Err(e)) => last_err = e,
                Err(_) => last_err = io::Error::new(io::ErrorKind::TimedOut, format!("DNS {server} : délai dépassé")),
            }
            debug!(%server, error = %last_err, "tunnel DNS server failed");
        }
        Err(last_err)
    }

    fn remember(&self, host: &str, ips: &[IpAddr], ttl: u32) {
        let Ok(mut cache) = self.cache.lock() else { return };
        let now = Instant::now();
        if cache.len() >= DNS_CACHE_MAX {
            cache.retain(|_, (_, until)| *until > now);
            if cache.len() >= DNS_CACHE_MAX {
                cache.clear();
            }
        }
        let ttl = Duration::from_secs(u64::from(ttl.clamp(30, 600)));
        cache.insert(host.to_string(), (ips.to_vec(), now + ttl));
    }

    async fn lookup(&self, server: SocketAddr, host: &str) -> io::Result<(Vec<IpAddr>, u32)> {
        let mut stream = timeout(DNS_TIMEOUT, tokio_wireguard::TcpStream::connect(server, &self.iface))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connexion DNS"))??;
        let mut ips = Vec::new();
        let mut ttl = u32::MAX;
        // IPv4 first: most IPTV CDNs and WireGuard exits are v4-only.
        let mut qtypes = Vec::with_capacity(2);
        if self.has_v4 {
            qtypes.push(1u16);
        }
        if self.has_v6 {
            qtypes.push(28u16);
        }
        for qtype in qtypes {
            let rnd = uuid::Uuid::new_v4();
            let id = u16::from_be_bytes([rnd.as_bytes()[0], rnd.as_bytes()[1]]);
            let query = build_dns_query(id, host, qtype)?;
            stream.write_all(&(query.len() as u16).to_be_bytes()).await?;
            stream.write_all(&query).await?;
            let mut len = [0u8; 2];
            timeout(DNS_TIMEOUT, stream.read_exact(&mut len))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "réponse DNS"))??;
            let mut buf = vec![0u8; u16::from_be_bytes(len) as usize];
            timeout(DNS_TIMEOUT, stream.read_exact(&mut buf))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "réponse DNS"))??;
            let (mut got, t) = parse_dns_answer(&buf, id)?;
            if !got.is_empty() {
                ttl = ttl.min(t);
            }
            ips.append(&mut got);
            if !ips.is_empty() {
                break;
            }
        }
        Ok((ips, if ttl == u32::MAX { 60 } else { ttl }))
    }
}

fn build_dns_query(id: u16, host: &str, qtype: u16) -> io::Result<Vec<u8>> {
    let mut q = Vec::with_capacity(18 + host.len());
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "nom d’hôte invalide"));
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&[0, 1]);
    Ok(q)
}

fn skip_dns_name(buf: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let len = *buf.get(pos)? as usize;
        if len == 0 {
            return Some(pos + 1);
        }
        if len & 0xC0 == 0xC0 {
            return Some(pos + 2);
        }
        pos += 1 + len;
    }
}

/// A/AAAA records and the smallest TTL. NXDOMAIN maps to `NotFound`.
fn parse_dns_answer(buf: &[u8], id: u16) -> io::Result<(Vec<IpAddr>, u32)> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "réponse DNS malformée");
    if buf.len() < 12 || u16::from_be_bytes([buf[0], buf[1]]) != id {
        return Err(bad());
    }
    match buf[3] & 0x0F {
        0 => {}
        3 => return Err(io::Error::new(io::ErrorKind::NotFound, "domaine inexistant (NXDOMAIN)")),
        rcode => return Err(io::Error::other(format!("erreur DNS rcode {rcode}"))),
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut pos = 12;
    for _ in 0..qd {
        pos = skip_dns_name(buf, pos).ok_or_else(bad)? + 4;
    }
    let mut ips = Vec::new();
    let mut ttl = u32::MAX;
    for _ in 0..an {
        pos = skip_dns_name(buf, pos).ok_or_else(bad)?;
        let rr = buf.get(pos..pos + 10).ok_or_else(bad)?;
        let rtype = u16::from_be_bytes([rr[0], rr[1]]);
        let rttl = u32::from_be_bytes([rr[4], rr[5], rr[6], rr[7]]);
        let rdlen = u16::from_be_bytes([rr[8], rr[9]]) as usize;
        pos += 10;
        let data = buf.get(pos..pos + rdlen).ok_or_else(bad)?;
        pos += rdlen;
        match (rtype, rdlen) {
            (1, 4) => ips.push(IpAddr::V4(Ipv4Addr::new(data[0], data[1], data[2], data[3]))),
            (28, 16) => {
                let mut o = [0u8; 16];
                o.copy_from_slice(data);
                ips.push(IpAddr::V6(Ipv6Addr::from(o)));
            }
            _ => continue,
        }
        ttl = ttl.min(rttl);
    }
    Ok((ips, ttl))
}

// ── Proxy ───────────────────────────────────────────────────────────────────

struct Ctx {
    iface: Interface,
    dns: Arc<TunnelDns>,
    user: String,
    pass: String,
    basic: String,
    slots: Arc<Semaphore>,
}

pub struct WgProxy {
    addr: SocketAddr,
    ctx: Arc<Ctx>,
    stop: watch::Sender<bool>,
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl WgProxy {
    /// Bring the tunnel up and verify the handshake before exposing the port.
    pub async fn start(profile: &Profile) -> Result<Self, String> {
        let iface = build_interface(profile).map_err(|e| format!("interface WireGuard : {e}"))?;
        let dns = Arc::new(TunnelDns::new(iface.clone(), profile));

        let t0 = Instant::now();
        if let Err(e) = probe(&iface, &dns, profile).await {
            iface.close();
            return Err(e);
        }
        info!(
            elapsed_ms = t0.elapsed().as_millis() as u64,
            mtu = profile.mtu(),
            psk = profile.has_preshared_key(),
            "WireGuard handshake verified"
        );

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|e| format!("port proxy local : {e}"))?;
        let addr = listener.local_addr().map_err(|e| format!("port proxy local : {e}"))?;
        let user = "fluxplay".to_string();
        let pass = uuid::Uuid::new_v4().simple().to_string();
        let basic = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
        );
        let ctx = Arc::new(Ctx {
            iface,
            dns,
            user,
            pass,
            basic,
            slots: Arc::new(Semaphore::new(MAX_CLIENTS)),
        });
        let (stop, stop_rx) = watch::channel(false);
        tokio::spawn(accept_loop(listener, ctx.clone(), stop_rx));
        Ok(Self { addr, ctx, stop })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// For reqwest (`socks5h` = hostname resolved by the proxy, inside the tunnel).
    pub fn socks_url(&self) -> String {
        format!("socks5h://{}:{}@{}", self.ctx.user, self.ctx.pass, self.addr)
    }

    /// For FFmpeg / mpv `http_proxy`.
    pub fn http_url(&self) -> String {
        format!("http://{}:{}@{}", self.ctx.user, self.ctx.pass, self.addr)
    }

    pub fn resolver(&self) -> Arc<TunnelDns> {
        self.ctx.dns.clone()
    }

    pub fn dns_servers(&self) -> Vec<SocketAddr> {
        self.ctx.dns.servers()
    }

    /// Stop accepting, drop live connections and close the WireGuard interface.
    pub fn shutdown(&self) {
        self.stop.send_replace(true);
        self.ctx.iface.close();
    }
}

impl Drop for WgProxy {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Wait for a completed handshake (proof both sides hold the right keys and PSK).
/// Traffic to an address the peer routes makes boringtun initiate it.
async fn probe(iface: &Interface, dns: &TunnelDns, profile: &Profile) -> Result<(), String> {
    let routed = |ip: IpAddr| profile.peers.iter().any(|p| p.allowed_ips.iter().any(|n| n.contains(&ip)));
    let mut targets: Vec<SocketAddr> = dns.servers().into_iter().filter(|s| routed(s.ip())).collect();
    targets.extend(
        profile
            .peers
            .iter()
            .flat_map(|p| p.allowed_ips.iter())
            .filter_map(|n| n.hosts().next())
            .filter(|ip| !ip.is_unspecified() && dns.reachable(*ip))
            .map(|ip| SocketAddr::new(ip, 443)),
    );
    targets.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 443));
    let target = targets.into_iter().find(|t| routed(t.ip()) && dns.reachable(t.ip()));
    let Some(target) = target else {
        return Err("AllowedIPs ne couvre aucune adresse joignable par l’interface".into());
    };

    let kick = {
        let iface = iface.clone();
        tokio::spawn(async move {
            let _ = timeout(HANDSHAKE_PROBE, tokio_wireguard::TcpStream::connect(target, &iface)).await;
        })
    };
    let deadline = Instant::now() + HANDSHAKE_PROBE;
    let result = loop {
        match iface.handshakes().await {
            Ok(ages) if ages.iter().any(Option::is_some) => break Ok(()),
            Ok(_) => {}
            Err(e) => break Err(format!("interface WireGuard fermée : {e}")),
        }
        if Instant::now() >= deadline {
            break Err(format!(
                "aucun handshake en {} s — vérifiez les clés (PrivateKey, PublicKey, PresharedKey), l’Endpoint et que l’UDP sortant n’est pas bloqué",
                HANDSHAKE_PROBE.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    kick.abort();
    result
}

async fn wait_stop(mut stop: watch::Receiver<bool>) {
    while !*stop.borrow_and_update() {
        if stop.changed().await.is_err() {
            return;
        }
    }
}

async fn accept_loop(listener: TcpListener, ctx: Arc<Ctx>, stop: watch::Receiver<bool>) {
    loop {
        let accepted = tokio::select! {
            _ = wait_stop(stop.clone()) => break,
            r = listener.accept() => r,
        };
        let sock = match accepted {
            Ok((sock, _)) => sock,
            Err(e) => {
                warn!(error = %e, "WireGuard proxy accept failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(permit) = ctx.slots.clone().try_acquire_owned() else {
            warn!("WireGuard proxy: too many connections, refusing");
            continue;
        };
        let ctx = ctx.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            let _permit = permit;
            tokio::select! {
                r = serve_client(sock, &ctx) => {
                    if let Err(e) = r {
                        debug!(error = %e, "WireGuard proxy client ended");
                    }
                }
                _ = wait_stop(stop) => {}
            }
        });
    }
    debug!("WireGuard proxy listener closed");
}

async fn serve_client(sock: TcpStream, ctx: &Ctx) -> io::Result<()> {
    sock.set_nodelay(true).ok();
    let mut first = [0u8; 1];
    let n = timeout(HEAD_TIMEOUT, sock.peek(&mut first))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "client muet"))??;
    if n == 0 {
        return Ok(());
    }
    if first[0] == 0x05 {
        serve_socks5(sock, ctx).await
    } else {
        serve_http(sock, ctx).await
    }
}

async fn connect_upstream(ctx: &Ctx, host: &str, port: u16) -> io::Result<tokio_wireguard::TcpStream> {
    let ips = ctx.dns.resolve(host).await?;
    let mut last = io::Error::other(format!("aucune adresse utilisable pour {host}"));
    for ip in ips.into_iter().filter(|ip| ctx.dns.reachable(*ip)) {
        match timeout(CONNECT_TIMEOUT, tokio_wireguard::TcpStream::connect(SocketAddr::new(ip, port), &ctx.iface)).await {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(e)) => last = e,
            Err(_) => last = io::Error::new(io::ErrorKind::TimedOut, format!("{ip}:{port} : délai dépassé")),
        }
    }
    Err(last)
}

async fn splice(mut client: TcpStream, mut upstream: tokio_wireguard::TcpStream) -> io::Result<()> {
    tokio::io::copy_bidirectional_with_sizes(&mut client, &mut upstream, SPLICE_BUF, SPLICE_BUF)
        .await
        .map(drop)
}

async fn serve_socks5(mut sock: TcpStream, ctx: &Ctx) -> io::Result<()> {
    let mut hdr = [0u8; 2];
    sock.read_exact(&mut hdr).await?;
    let mut methods = vec![0u8; hdr[1] as usize];
    sock.read_exact(&mut methods).await?;
    if !methods.contains(&0x02) {
        sock.write_all(&[5, 0xFF]).await?;
        return Ok(());
    }
    sock.write_all(&[5, 0x02]).await?;

    let mut ver_ulen = [0u8; 2];
    sock.read_exact(&mut ver_ulen).await?;
    let mut uname = vec![0u8; ver_ulen[1] as usize];
    sock.read_exact(&mut uname).await?;
    let mut plen = [0u8; 1];
    sock.read_exact(&mut plen).await?;
    let mut passwd = vec![0u8; plen[0] as usize];
    sock.read_exact(&mut passwd).await?;
    let ok = ct_eq(&uname, ctx.user.as_bytes()) & ct_eq(&passwd, ctx.pass.as_bytes());
    sock.write_all(&[1, if ok { 0 } else { 1 }]).await?;
    if !ok {
        return Ok(());
    }

    let mut req = [0u8; 4];
    sock.read_exact(&mut req).await?;
    let host = match req[3] {
        1 => {
            let mut a = [0u8; 4];
            sock.read_exact(&mut a).await?;
            Ipv4Addr::from(a).to_string()
        }
        3 => {
            let mut l = [0u8; 1];
            sock.read_exact(&mut l).await?;
            let mut name = vec![0u8; l[0] as usize];
            sock.read_exact(&mut name).await?;
            String::from_utf8(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "nom SOCKS"))?
        }
        4 => {
            let mut a = [0u8; 16];
            sock.read_exact(&mut a).await?;
            Ipv6Addr::from(a).to_string()
        }
        _ => {
            socks_reply(&mut sock, 0x08).await?;
            return Ok(());
        }
    };
    let mut port = [0u8; 2];
    sock.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);
    if req[1] != 0x01 {
        socks_reply(&mut sock, 0x07).await?;
        return Ok(());
    }
    match connect_upstream(ctx, &host, port).await {
        Ok(upstream) => {
            socks_reply(&mut sock, 0x00).await?;
            splice(sock, upstream).await
        }
        Err(e) => {
            let code = match e.kind() {
                io::ErrorKind::NotFound => 0x04,
                io::ErrorKind::ConnectionRefused => 0x05,
                io::ErrorKind::TimedOut => 0x06,
                _ => 0x01,
            };
            debug!(%host, port, error = %e, "SOCKS connect through tunnel failed");
            socks_reply(&mut sock, code).await
        }
    }
}

async fn socks_reply(sock: &mut TcpStream, code: u8) -> io::Result<()> {
    sock.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await
}

fn split_authority(authority: &str, default_port: u16) -> Option<(String, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        let port = match rest.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None => default_port,
        };
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) => Some((h.to_string(), p.parse().ok()?)),
        None => Some((authority.to_string(), default_port)),
    }
}

async fn http_error(sock: &mut TcpStream, status: &str, extra: &str) -> io::Result<()> {
    let msg = format!("HTTP/1.1 {status}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n");
    sock.write_all(msg.as_bytes()).await
}

async fn serve_http(mut sock: TcpStream, ctx: &Ctx) -> io::Result<()> {
    let mut buf = Vec::with_capacity(2048);
    let head_end = timeout(HEAD_TIMEOUT, async {
        let mut chunk = [0u8; 2048];
        loop {
            let n = sock.read(&mut chunk).await?;
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                return Ok(i + 4);
            }
            if buf.len() > MAX_HEAD {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "en-têtes trop longs"));
            }
        }
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "en-têtes HTTP"))??;

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let rest = buf[head_end..].to_vec();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next()) else {
        return http_error(&mut sock, "400 Bad Request", "").await;
    };
    let headers: Vec<&str> = lines.filter(|l| !l.is_empty()).collect();
    let header = |name: &str| {
        headers.iter().find_map(|h| {
            let (k, v) = h.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
    };

    let authorized = header("proxy-authorization").is_some_and(|v| ct_eq(v.as_bytes(), ctx.basic.as_bytes()));
    if !authorized {
        return http_error(
            &mut sock,
            "407 Proxy Authentication Required",
            "Proxy-Authenticate: Basic realm=\"fluxplay\"\r\n",
        )
        .await;
    }

    if method.eq_ignore_ascii_case("CONNECT") {
        let Some((host, port)) = split_authority(target, 443) else {
            return http_error(&mut sock, "400 Bad Request", "").await;
        };
        return match connect_upstream(ctx, &host, port).await {
            Ok(mut upstream) => {
                sock.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await?;
                if !rest.is_empty() {
                    upstream.write_all(&rest).await?;
                }
                splice(sock, upstream).await
            }
            Err(e) => {
                debug!(%host, port, error = %e, "CONNECT through tunnel failed");
                let status = if e.kind() == io::ErrorKind::TimedOut { "504 Gateway Timeout" } else { "502 Bad Gateway" };
                http_error(&mut sock, status, "").await
            }
        };
    }

    // Absolute-form (`GET http://host/path`): only plain http; https goes through CONNECT.
    let Some(after_scheme) = target.strip_prefix("http://") else {
        return http_error(&mut sock, "400 Bad Request", "").await;
    };
    let split = after_scheme.find(['/', '?']).unwrap_or(after_scheme.len());
    let (authority, path) = after_scheme.split_at(split);
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let Some((host, port)) = split_authority(authority, 80) else {
        return http_error(&mut sock, "400 Bad Request", "").await;
    };
    let path = if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('?') {
        format!("/{path}")
    } else {
        path.to_string()
    };

    let mut out = format!("{method} {path} {version}\r\n");
    let mut has_host = false;
    for h in &headers {
        let name = h.split(':').next().unwrap_or("").trim();
        if name.eq_ignore_ascii_case("proxy-authorization")
            || name.eq_ignore_ascii_case("proxy-connection")
            || name.eq_ignore_ascii_case("connection")
        {
            continue;
        }
        has_host |= name.eq_ignore_ascii_case("host");
        out.push_str(h);
        out.push_str("\r\n");
    }
    if !has_host {
        out.push_str(&format!("Host: {authority}\r\n"));
    }
    // One origin per client connection: the next request may target another host.
    out.push_str("Connection: close\r\n\r\n");

    match connect_upstream(ctx, &host, port).await {
        Ok(mut upstream) => {
            upstream.write_all(out.as_bytes()).await?;
            if !rest.is_empty() {
                upstream.write_all(&rest).await?;
            }
            splice(sock, upstream).await
        }
        Err(e) => {
            debug!(%host, port, error = %e, "HTTP request through tunnel failed");
            let status = if e.kind() == io::ErrorKind::TimedOut { "504 Gateway Timeout" } else { "502 Bad Gateway" };
            http_error(&mut sock, status, "").await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = "\
[Interface]
PrivateKey = yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=
Address = 172.16.0.2/32, 2606:4700:110:8a36::2/128, 10.9.9.9/32
DNS = 1.1.1.1, 2606:4700:4700::1111, corp.example
MTU = 1280 # WARP

[Peer]
PublicKey = bmXOC+F1FxEMF9dyiK2H5/1SUtzH0JuVo51h2wPfgyo=
PresharedKey = FpCyhws9cxwWoV4xELtfJvjJN+zQVRPISllRWgeopVE=
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint = 162.159.192.1:2408
PersistentKeepalive = 25
";

    #[test]
    fn parses_wg_quick_profile() {
        let p = parse_profile(PROFILE).unwrap();
        assert!(p.address.is_dual());
        assert_eq!(p.mtu(), 1280);
        assert_eq!(p.dns.len(), 2);
        assert!(p.has_preshared_key());
        assert_eq!(p.peers[0].keepalive, Some(25));
        assert_eq!(p.peers[0].allowed_ips.len(), 2);
    }

    #[test]
    fn rejects_incomplete_profiles() {
        assert!(parse_profile(&PROFILE.replace("Endpoint = 162.159.192.1:2408", "")).is_err());
        assert!(parse_profile(&PROFILE.replace("Endpoint = 162.159.192.1:2408", "Endpoint = host:1")).is_err());
        assert!(parse_profile(&PROFILE.replace("PublicKey = bmXOC", "PublicKey = AAA")).is_err());
        assert!(parse_profile("[Interface]\nPrivateKey = yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=\nAddress = 10.0.0.2/32\n").is_err());
    }

    #[test]
    fn dns_wire_roundtrip() {
        let q = build_dns_query(0x1234, "example.com", 1).unwrap();
        assert_eq!(&q[12..], b"\x07example\x03com\x00\x00\x01\x00\x01");
        let mut r = q.clone();
        r[2] = 0x81;
        r[3] = 0x80;
        r[7] = 2;
        for (ttl, ip) in [(300u32, [93, 184, 216, 34]), (60, [93, 184, 216, 35])] {
            r.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1]);
            r.extend_from_slice(&ttl.to_be_bytes());
            r.extend_from_slice(&[0, 4]);
            r.extend_from_slice(&ip);
        }
        let (ips, ttl) = parse_dns_answer(&r, 0x1234).unwrap();
        assert_eq!(ips.len(), 2);
        assert_eq!(ttl, 60);
        r[3] = 0x83;
        assert_eq!(parse_dns_answer(&r, 0x1234).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(parse_dns_answer(&r[..20], 0x1234).is_err());
    }

    #[test]
    fn provider_ips() {
        assert_eq!(provider_ip("https://cloudflare-dns.com/dns-query"), Some(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert_eq!(provider_ip("https://9.9.9.11/dns-query"), Some(IpAddr::V4(Ipv4Addr::new(9, 9, 9, 11))));
        assert_eq!(provider_ip("tls://[2620:fe::fe]:853"), "2620:fe::fe".parse().ok());
        assert_eq!(provider_ip("dns.example.net"), None);
        assert_eq!(split_authority("[::1]:8080", 80), Some(("::1".into(), 8080)));
        assert_eq!(split_authority("host", 80), Some(("host".into(), 80)));
    }
}
