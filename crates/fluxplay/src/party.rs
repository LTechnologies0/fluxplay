//! Phone remote and cinema room.
//!
//! One small HTTP listener on every interface (LAN and VPN). A Tor onion is
//! published when the local control port answers. The phone is the same
//! FluxPlay app: it pastes the `fp1|…` text (or a QR of that text) and sends
//! play, pause, seek and volume. The room owner runs one ffmpeg on the portal
//! URL and fans the MPEG-TS out; members open that local URL and can ask for
//! 720p or 480p, which is transcoded from the fan-out, not from the portal.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};

const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Clone, Copy, Debug)]
pub enum RemoteOp {
    Toggle,
    Seek(i32),
    Volume(i32),
    Mute,
    Next,
    Prev,
}

#[derive(Clone, Debug)]
pub struct HostInfo {
    pub port: u16,
    pub token: String,
    pub addrs: Vec<String>,
    pub onion: Option<String>,
}

impl HostInfo {
    pub fn payload(&self, room: &str) -> String {
        format!(
            "fp1|{}|{}|{}|{}|{}",
            self.port,
            self.token,
            self.addrs.join(","),
            self.onion.as_deref().unwrap_or(""),
            room
        )
    }

    pub fn relay(&self, room: &str, height: u16) -> String {
        format!(
            "http://127.0.0.1:{}/v1/cinema/live.ts?k={}&c={}&h={}",
            self.port, self.token, room, height
        )
    }
}

#[derive(Clone, Debug)]
pub struct ParsedLink {
    pub port: u16,
    pub token: String,
    pub addrs: Vec<String>,
    pub onion: Option<String>,
    pub room: String,
}

pub fn parse_link(text: &str) -> Option<ParsedLink> {
    let text = text.trim();
    let rest = text.strip_prefix("fp1|")?;
    let mut p = rest.split('|');
    let port = p.next()?.parse().ok()?;
    let token = p.next()?.trim().to_string();
    if token.len() < 8 {
        return None;
    }
    let addrs = p
        .next()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let onion = p.next().map(str::trim).filter(|s| s.ends_with(".onion")).map(str::to_string);
    let room = p.next().unwrap_or("").trim().to_string();
    Some(ParsedLink {
        port,
        token,
        addrs,
        onion,
        room,
    })
}

#[derive(Clone, Debug)]
pub enum PartyEvent {
    Info(HostInfo),
    Status(String),
    Room {
        code: String,
        people: usize,
        broadcasting: bool,
    },
    Remote(RemoteOp),
}

pub enum Order {
    Room(String),
    CloseRoom,
    Broadcast(String),
    StopBroadcast,
    Pause(bool),
}

pub struct Party {
    pub info: Option<HostInfo>,
    pub qr: Option<(u32, Vec<u8>)>,
    pub draft: String,
    pub join_draft: String,
    pub paired: Option<ParsedLink>,
    pub room_code: String,
    pub people: usize,
    pub owner: bool,
    pub broadcasting: bool,
    pub quality: u16,
    pub status: String,
    pub guest_play: Option<String>,
    pub want_broadcast: bool,
    events: Option<Receiver<PartyEvent>>,
    orders: Option<mpsc::Sender<Order>>,
}

impl Party {
    pub fn start() -> Self {
        let (ev_tx, ev_rx) = mpsc::channel();
        let (ord_tx, ord_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("fp-party".into())
            .spawn(move || serve(ev_tx, ord_rx))
            .ok();
        Self {
            info: None,
            qr: None,
            draft: String::new(),
            join_draft: String::new(),
            paired: None,
            room_code: String::new(),
            people: 0,
            owner: false,
            broadcasting: false,
            quality: 0,
            status: "Démarrage du lien…".into(),
            guest_play: None,
            want_broadcast: false,
            events: Some(ev_rx),
            orders: Some(ord_tx),
        }
    }

    pub fn order(&self, order: Order) {
        if let Some(tx) = &self.orders {
            let _ = tx.send(order);
        }
    }

    pub fn poll(&mut self) -> Vec<PartyEvent> {
        let Some(rx) = &self.events else {
            return Vec::new();
        };
        let mut out = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(ev) => out.push(ev),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }

    pub fn relay_url(&self) -> Option<String> {
        let info = self.info.as_ref()?;
        if self.room_code.is_empty() {
            return None;
        }
        Some(info.relay(&self.room_code, self.quality))
    }

    pub fn refresh_qr(&mut self) {
        let Some(info) = self.info.clone() else {
            return;
        };
        let payload = info.payload(&self.room_code);
        self.qr = crate::qr::rgba(&payload);
    }

    pub fn payload(&self) -> String {
        self.info
            .as_ref()
            .map(|i| i.payload(&self.room_code))
            .unwrap_or_default()
    }
}

impl Default for Party {
    fn default() -> Self {
        Self::start()
    }
}

fn serve(events: mpsc::Sender<PartyEvent>, orders: Receiver<Order>) {
    let listener = match TcpListener::bind(("0.0.0.0", 0)) {
        Ok(l) => l,
        Err(e) => {
            let _ = events.send(PartyEvent::Status(format!("Lien impossible: {e}")));
            return;
        }
    };
    let _ = listener.set_nonblocking(true);
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let token = fresh_token();
    let addrs = local_ipv4();
    let state = Arc::new(Shared {
        token: token.clone(),
        port,
        code: Mutex::new(String::new()),
        people: Mutex::new(0),
        broadcasting: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        gen: AtomicU64::new(0),
        clients: Mutex::new(Vec::new()),
        tiers: Mutex::new(HashMap::new()),
        child: Mutex::new(None),
        events: events.clone(),
        onion: Mutex::new(None),
        addrs: Mutex::new(addrs.clone()),
    });
    let _ = events.send(PartyEvent::Info(HostInfo {
        port,
        token: token.clone(),
        addrs: addrs.clone(),
        onion: None,
    }));
    let _ = events.send(PartyEvent::Status(
        "Télécommande prête sur ce réseau. Ouverture du port pour l'extérieur…".into(),
    ));

    let onion_state = Arc::clone(&state);
    std::thread::Builder::new()
        .name("fp-onion".into())
        .spawn(move || {
            if let Some(id) = publish_onion(port) {
                if let Ok(mut slot) = onion_state.onion.lock() {
                    *slot = Some(id);
                }
                let addrs = onion_state
                    .addrs
                    .lock()
                    .map(|a| a.clone())
                    .unwrap_or_default();
                let onion = onion_state.onion.lock().ok().and_then(|o| o.clone());
                let _ = onion_state.events.send(PartyEvent::Info(HostInfo {
                    port: onion_state.port,
                    token: onion_state.token.clone(),
                    addrs,
                    onion,
                }));
            }
        })
        .ok();

    let wan_state = Arc::clone(&state);
    let lan_ip = addrs.iter().find(|ip| is_private(ip)).cloned().unwrap_or_default();
    std::thread::Builder::new()
        .name("fp-wan".into())
        .spawn(move || {
            let Some(ip) = map_outside(wan_state.port, &lan_ip) else {
                let _ = wan_state.events.send(PartyEvent::Status(
                    "Télécommande prête sur ce réseau. La box n'ouvre pas le port : un appareil hors de ce réseau ne peut pas joindre cet ordinateur.".into(),
                ));
                return;
            };
            let addrs = {
                let Ok(mut slot) = wan_state.addrs.lock() else {
                    return;
                };
                if !slot.iter().any(|a| a == &ip) {
                    slot.insert(0, ip.clone());
                }
                slot.clone()
            };
            let onion = wan_state.onion.lock().ok().and_then(|o| o.clone());
            let _ = wan_state.events.send(PartyEvent::Info(HostInfo {
                port: wan_state.port,
                token: wan_state.token.clone(),
                addrs,
                onion,
            }));
            let opened = allow_firewall(wan_state.port);
            let _ = wan_state.events.send(PartyEvent::Status(if opened {
                format!(
                    "Télécommande joignable hors du réseau sur {ip}:{}.",
                    wan_state.port
                )
            } else {
                format!(
                    "Port ouvert sur la box ({ip}:{}). Autorisez FluxPlay dans le pare-feu Windows si le téléphone ne répond pas.",
                    wan_state.port
                )
            }));
        })
        .ok();

    loop {
        loop {
            match orders.try_recv() {
                Ok(order) => apply_order(&Arc::clone(&state), order),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        match listener.accept() {
            Ok((sock, peer)) => {
                let st = Arc::clone(&state);
                std::thread::Builder::new()
                    .name("fp-http".into())
                    .spawn(move || {
                        if let Err(e) = handle_client(sock, peer, &st) {
                            tracing::debug!(error = %e, "party http");
                        }
                    })
                    .ok();
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => {
                let _ = events.send(PartyEvent::Status(format!("Lien coupé: {e}")));
                break;
            }
        }
    }
}

struct Shared {
    token: String,
    port: u16,
    code: Mutex<String>,
    people: Mutex<usize>,
    broadcasting: AtomicBool,
    paused: AtomicBool,
    gen: AtomicU64,
    clients: Mutex<Vec<SyncSender<Arc<Vec<u8>>>>>,
    tiers: Mutex<HashMap<u16, Tier>>,
    child: Mutex<Option<Child>>,
    events: mpsc::Sender<PartyEvent>,
    onion: Mutex<Option<String>>,
    addrs: Mutex<Vec<String>>,
}

struct Tier {
    stdin_tx: SyncSender<Arc<Vec<u8>>>,
    clients: Arc<Mutex<Vec<SyncSender<Arc<Vec<u8>>>>>>,
    child: Child,
}

fn apply_order(state: &Arc<Shared>, order: Order) {
    match order {
        Order::Room(code) => {
            if let Ok(mut c) = state.code.lock() {
                *c = code.clone();
            }
            if let Ok(mut n) = state.people.lock() {
                if *n == 0 {
                    *n = 1;
                }
            }
            emit_room(state);
            let _ = state.events.send(PartyEvent::Status(format!(
                "Salle {code} — 10 places. Diffusez le film en cours, les autres collent le lien."
            )));
        }
        Order::CloseRoom => {
            stop_broadcast(state);
            if let Ok(mut c) = state.code.lock() {
                c.clear();
            }
            if let Ok(mut n) = state.people.lock() {
                *n = 0;
            }
            emit_room(state);
        }
        Order::Broadcast(url) => start_broadcast(state, url),
        Order::StopBroadcast => {
            stop_broadcast(state);
            let _ = state
                .events
                .send(PartyEvent::Status("Diffusion arrêtée.".into()));
            emit_room(state);
        }
        Order::Pause(on) => state.paused.store(on, Ordering::Relaxed),
    }
}

fn emit_room(state: &Shared) {
    let code = state.code.lock().ok().map(|c| c.clone()).unwrap_or_default();
    let people = state.people.lock().map(|n| *n).unwrap_or(0);
    let _ = state.events.send(PartyEvent::Room {
        code,
        people,
        broadcasting: state.broadcasting.load(Ordering::Relaxed),
    });
}

fn start_broadcast(state: &Arc<Shared>, url: String) {
    stop_broadcast(state);
    let bin = match ffmpeg_bin() {
        Some(b) => b,
        None => {
            state.broadcasting.store(false, Ordering::Relaxed);
            let _ = state.events.send(PartyEvent::Status(
                "ffmpeg est introuvable. La télécommande fonctionne, la salle a besoin de ffmpeg à côté de FluxPlay ou dans le PATH.".into(),
            ));
            emit_room(state);
            return;
        }
    };
    let gen = state.gen.fetch_add(1, Ordering::Relaxed) + 1;
    state.broadcasting.store(true, Ordering::Relaxed);
    state.paused.store(false, Ordering::Relaxed);
    let st = Arc::clone(state);
    std::thread::Builder::new()
        .name("fp-relay".into())
        .spawn(move || relay_loop(st, bin, url, gen))
        .ok();
    emit_room(state);
}

fn relay_loop(state: Arc<Shared>, bin: PathBuf, url: String, gen: u64) {
    // Copy the source when the codecs already fit MPEG-TS. If that exits
    // without producing a stream, encode 1080p (never below the owner's floor).
    for encode in [false, true] {
        if state.gen.load(Ordering::Relaxed) != gen {
            return;
        }
        let mut cmd = Command::new(&bin);
        cmd.arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-nostdin")
            .arg("-fflags")
            .arg("nobuffer")
            .arg("-flags")
            .arg("low_delay")
            .arg("-probesize")
            .arg("32768")
            .arg("-analyzeduration")
            .arg("0")
            .arg("-user_agent")
            .arg("IPTVSmartersPlayer")
            .arg("-reconnect")
            .arg("1")
            .arg("-reconnect_streamed")
            .arg("1")
            .arg("-reconnect_delay_max")
            .arg("2")
            .arg("-i")
            .arg(&url);
        if encode {
            cmd.arg("-vf")
                .arg("scale=-2:1080")
                .arg("-c:v")
                .arg("libx264")
                .arg("-preset")
                .arg("ultrafast")
                .arg("-tune")
                .arg("zerolatency")
                .arg("-profile:v")
                .arg("high")
                .arg("-g")
                .arg("48")
                .arg("-bf")
                .arg("0")
                .arg("-b:v")
                .arg("6000k")
                .arg("-maxrate")
                .arg("8000k")
                .arg("-bufsize")
                .arg("3000k")
                .arg("-c:a")
                .arg("aac")
                .arg("-b:a")
                .arg("160k")
                .arg("-ac")
                .arg("2");
        } else {
            cmd.arg("-c").arg("copy");
        }
        cmd.arg("-f")
            .arg("mpegts")
            .arg("-muxdelay")
            .arg("0")
            .arg("-muxpreload")
            .arg("0")
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = state.events.send(PartyEvent::Status(format!("ffmpeg: {e}")));
                state.broadcasting.store(false, Ordering::Relaxed);
                return;
            }
        };
        let mut stdout = match child.stdout.take() {
            Some(o) => o,
            None => return,
        };
        if let Ok(mut slot) = state.child.lock() {
            *slot = Some(child);
        }
        let mode = if encode { "1080p" } else { "qualité source" };
        let _ = state.events.send(PartyEvent::Status(format!(
            "Salle en diffusion ({mode}). Les invités peuvent descendre en 720p ou 480p."
        )));
        let mut buf = vec![0u8; 188 * 16];
        let mut produced = false;
        loop {
            if state.gen.load(Ordering::Relaxed) != gen {
                return;
            }
            while state.paused.load(Ordering::Relaxed) {
                if state.gen.load(Ordering::Relaxed) != gen {
                    return;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    produced = true;
                    let chunk: Arc<Vec<u8>> = Arc::new(buf[..n].to_vec());
                    fanout(&state.clients, &chunk);
                    if let Ok(tiers) = state.tiers.lock() {
                        for tier in tiers.values() {
                            let _ = tier.stdin_tx.try_send(Arc::clone(&chunk));
                        }
                    }
                }
            }
        }
        if produced || encode {
            if state.gen.load(Ordering::Relaxed) == gen {
                state.broadcasting.store(false, Ordering::Relaxed);
                let _ = state.events.send(PartyEvent::Status(
                    "La diffusion s'est arrêtée.".into(),
                ));
                emit_room(&state);
            }
            return;
        }
        let _ = state.events.send(PartyEvent::Status(
            "Copie directe impossible, passage en 1080p.".into(),
        ));
    }
}

fn fanout(clients: &Mutex<Vec<SyncSender<Arc<Vec<u8>>>>>, chunk: &Arc<Vec<u8>>) {
    let Ok(mut list) = clients.lock() else {
        return;
    };
    list.retain(|tx| match tx.try_send(Arc::clone(chunk)) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => true,
        Err(TrySendError::Disconnected(_)) => false,
    });
}

fn stop_broadcast(state: &Shared) {
    state.gen.fetch_add(1, Ordering::Relaxed);
    state.broadcasting.store(false, Ordering::Relaxed);
    state.paused.store(false, Ordering::Relaxed);
    if let Ok(mut child) = state.child.lock() {
        if let Some(mut c) = child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
    if let Ok(mut tiers) = state.tiers.lock() {
        for (_, mut tier) in tiers.drain() {
            let _ = tier.child.kill();
            let _ = tier.child.wait();
        }
    }
    if let Ok(mut clients) = state.clients.lock() {
        clients.clear();
    }
}

fn ensure_tier(state: &Arc<Shared>, height: u16) -> bool {
    if height != 720 && height != 480 {
        return true;
    }
    if let Ok(tiers) = state.tiers.lock() {
        if tiers.contains_key(&height) {
            return true;
        }
    }
    let Some(bin) = ffmpeg_bin() else {
        return false;
    };
    let (tx, rx) = mpsc::sync_channel::<Arc<Vec<u8>>>(24);
    let clients = Arc::new(Mutex::new(Vec::new()));
    let mut cmd = Command::new(bin);
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-fflags")
        .arg("nobuffer")
        .arg("-f")
        .arg("mpegts")
        .arg("-i")
        .arg("pipe:0")
        .arg("-vf")
        .arg(format!("scale=-2:{height}"))
        .arg("-c:v")
        .arg("libx264")
        .arg("-preset")
        .arg("ultrafast")
        .arg("-tune")
        .arg("zerolatency")
        .arg("-g")
        .arg("48")
        .arg("-bf")
        .arg("0")
        .arg("-b:v")
        .arg(if height == 720 { "2800k" } else { "1200k" })
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("128k")
        .arg("-f")
        .arg("mpegts")
        .arg("-muxdelay")
        .arg("0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut stdin = match child.stdin.take() {
        Some(s) => s,
        None => return false,
    };
    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => return false,
    };
    let gen = state.gen.load(Ordering::Relaxed);
    let st = Arc::clone(state);
    std::thread::spawn(move || {
        while let Ok(chunk) = rx.recv() {
            if st.gen.load(Ordering::Relaxed) != gen {
                break;
            }
            if stdin.write_all(&chunk).is_err() {
                break;
            }
        }
    });
    let clients_r = Arc::clone(&clients);
    let st = Arc::clone(state);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 188 * 16];
        loop {
            if st.gen.load(Ordering::Relaxed) != gen {
                break;
            }
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk: Arc<Vec<u8>> = Arc::new(buf[..n].to_vec());
                    fanout(&clients_r, &chunk);
                }
            }
        }
    });
    if let Ok(mut tiers) = state.tiers.lock() {
        tiers.insert(
            height,
            Tier {
                stdin_tx: tx,
                clients,
                child,
            },
        );
    }
    true
}

fn handle_client(mut sock: TcpStream, _peer: SocketAddr, state: &Arc<Shared>) -> std::io::Result<()> {
    sock.set_read_timeout(Some(Duration::from_secs(8)))?;
    let mut buf = vec![0u8; 8192];
    let mut got = 0usize;
    while got < buf.len() {
        match sock.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => {
                got += n;
                if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                break;
            }
            Err(e) => return Err(e),
        }
    }
    let text = String::from_utf8_lossy(&buf[..got]);
    let mut lines = text.split("\r\n");
    let req = lines.next().unwrap_or("");
    let mut parts = req.split_whitespace();
    let _method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let q = query_map(query);
    if q.get("k").map(String::as_str) != Some(state.token.as_str()) && path != "/health" {
        return write_text(&mut sock, 401, "clef refusee");
    }
    match path {
        "/health" => write_text(&mut sock, 200, "ok"),
        "/v1/status" => {
            let code = state.code.lock().ok().map(|c| c.clone()).unwrap_or_default();
            let people = state.people.lock().map(|n| *n).unwrap_or(0);
            let on = state.broadcasting.load(Ordering::Relaxed);
            write_text(
                &mut sock,
                200,
                &format!("ok\ncode={code}\npeople={people}\nlive={on}\n"),
            )
        }
        "/v1/cmd" => {
            let op = q.get("op").map(String::as_str).unwrap_or("");
            let remote = match op {
                "toggle" => Some(RemoteOp::Toggle),
                "mute" => Some(RemoteOp::Mute),
                "next" => Some(RemoteOp::Next),
                "prev" => Some(RemoteOp::Prev),
                "seek" => q.get("secs").and_then(|s| s.parse().ok()).map(RemoteOp::Seek),
                "volume" => q
                    .get("delta")
                    .and_then(|s| s.parse().ok())
                    .map(RemoteOp::Volume),
                _ => None,
            };
            if let Some(op) = remote {
                let _ = state.events.send(PartyEvent::Remote(op));
            }
            write_text(&mut sock, 200, "ok")
        }
        "/v1/cinema/create" => {
            let code = make_code();
            if let Ok(mut c) = state.code.lock() {
                *c = code.clone();
            }
            if let Ok(mut n) = state.people.lock() {
                *n = 1;
            }
            emit_room(state);
            write_text(&mut sock, 200, &format!("ok\ncode={code}\npeople=1\n"))
        }
        "/v1/cinema/join" => {
            let want = q.get("c").map(String::as_str).unwrap_or("");
            let code = state.code.lock().ok().map(|c| c.clone()).unwrap_or_default();
            if code.is_empty() || want != code {
                return write_text(&mut sock, 404, "salle inconnue");
            }
            let people = {
                let Ok(mut n) = state.people.lock() else {
                    return write_text(&mut sock, 500, "salle indisponible");
                };
                if *n >= 10 {
                    return write_text(&mut sock, 403, "salle pleine (10)");
                }
                *n += 1;
                *n
            };
            emit_room(state);
            let onion = state.onion.lock().ok().and_then(|o| o.clone());
            let play = format!(
                "http://{{host}}:{}/v1/cinema/live.ts?k={}&c={}&h=0",
                state.port, state.token, code
            );
            let onion_line = onion
                .map(|id| format!("onion=http://{id}/v1/cinema/live.ts?k={}&c={}&h=0\n", state.token, code))
                .unwrap_or_default();
            write_text(
                &mut sock,
                200,
                &format!("ok\ncode={code}\npeople={people}\nplay={play}\n{onion_line}"),
            )
        }
        "/v1/cinema/leave" => {
            if let Ok(mut n) = state.people.lock() {
                if *n > 1 {
                    *n -= 1;
                }
            }
            emit_room(state);
            write_text(&mut sock, 200, "ok")
        }
        "/v1/cinema/live.ts" => {
            let want = q.get("c").map(String::as_str).unwrap_or("");
            let code = state.code.lock().ok().map(|c| c.clone()).unwrap_or_default();
            if code.is_empty() || want != code {
                return write_text(&mut sock, 404, "salle inconnue");
            }
            if !state.broadcasting.load(Ordering::Relaxed) {
                return write_text(&mut sock, 503, "pas encore en diffusion");
            }
            let height: u16 = q.get("h").and_then(|s| s.parse().ok()).unwrap_or(0);
            let (tx, rx) = mpsc::sync_channel(16);
            if height == 720 || height == 480 {
                if !ensure_tier(state, height) {
                    return write_text(&mut sock, 500, "transcodage impossible");
                }
                let Ok(tiers) = state.tiers.lock() else {
                    return write_text(&mut sock, 500, "salle indisponible");
                };
                let Some(tier) = tiers.get(&height) else {
                    return write_text(&mut sock, 500, "qualite indisponible");
                };
                let Ok(mut clients) = tier.clients.lock() else {
                    return write_text(&mut sock, 500, "salle indisponible");
                };
                if clients.len() >= 10 {
                    return write_text(&mut sock, 403, "salle pleine (10)");
                }
                clients.push(tx);
            } else {
                let Ok(mut clients) = state.clients.lock() else {
                    return write_text(&mut sock, 500, "salle indisponible");
                };
                if clients.len() >= 10 {
                    return write_text(&mut sock, 403, "salle pleine (10)");
                }
                clients.push(tx);
            }
            sock.set_write_timeout(Some(Duration::from_secs(8)))?;
            sock.set_nodelay(true).ok();
            write!(
                sock,
                "HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
            )?;
            while let Ok(chunk) = rx.recv() {
                if sock.write_all(&chunk).is_err() {
                    break;
                }
            }
            Ok(())
        }
        _ => write_text(&mut sock, 404, "inconnu"),
    }
}

fn write_text(sock: &mut TcpStream, code: u16, body: &str) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        503 => "Unavailable",
        _ => "Error",
    };
    write!(
        sock,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn query_map(q: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in q.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        out.insert(k.to_string(), percent_decode(v));
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if b[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(b[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn make_code() -> String {
    let mut n = mix_seed();
    let mut s = String::with_capacity(6);
    for _ in 0..6 {
        s.push(CODE_ALPHABET[(n % CODE_ALPHABET.len() as u64) as usize] as char);
        n /= CODE_ALPHABET.len() as u64;
        n ^= n << 7;
    }
    s
}

fn fresh_token() -> String {
    hex_of(&mix_seed().to_le_bytes())
}

fn hex_of(extra: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(extra);
    h.update(std::process::id().to_le_bytes());
    h.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().to_le_bytes())
            .unwrap_or([0; 16]),
    );
    let dig = h.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(32);
    for b in dig.iter().take(8) {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

fn mix_seed() -> u64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let boxed = Box::new(0u8);
    let addr = &*boxed as *const u8 as u64;
    t ^ addr.rotate_left(17) ^ (std::process::id() as u64).rotate_left(3)
}

fn local_ipv4() -> Vec<String> {
    let mut out = Vec::new();
    let text = Command::new("ipconfig")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    if text.is_empty() {
        if let Ok(outp) = Command::new("ip").args(["-4", "-o", "addr", "show"]).output() {
            if let Ok(text) = String::from_utf8(outp.stdout) {
                for line in text.lines() {
                    for word in line.split_whitespace() {
                        if let Some(ip) = word.split('/').next() {
                            if looks_like_ipv4(ip) && !ip.starts_with("127.") {
                                out.push(ip.to_string());
                            }
                        }
                    }
                }
            }
        }
        return out;
    }
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if !lower.contains("ipv4") {
            continue;
        }
        let Some((_, rest)) = line.split_once(':') else {
            continue;
        };
        let ip = rest.trim().split('%').next().unwrap_or("").trim();
        if looks_like_ipv4(ip) && !ip.starts_with("127.") {
            out.push(ip.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn allow_firewall(port: u16) -> bool {
    let _ = Command::new("netsh")
        .args(["advfirewall", "firewall", "delete", "rule", "name=FluxPlay"])
        .status();
    let port_arg = format!("localport={port}");
    Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "add",
            "rule",
            "name=FluxPlay",
            "dir=in",
            "action=allow",
            "protocol=TCP",
            port_arg.as_str(),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn looks_like_ipv4(s: &str) -> bool {
    let p: Vec<&str> = s.split('.').collect();
    p.len() == 4 && p.iter().all(|n| n.parse::<u8>().is_ok())
}

fn is_private(ip: &str) -> bool {
    if ip.starts_with("10.")
        || ip.starts_with("192.168.")
        || ip.starts_with("127.")
        || ip.starts_with("169.254.")
        || ip.starts_with("100.")
    {
        return true;
    }
    let mut parts = ip.split('.');
    let Some(a) = parts.next().and_then(|n| n.parse::<u8>().ok()) else {
        return false;
    };
    let Some(b) = parts.next().and_then(|n| n.parse::<u8>().ok()) else {
        return false;
    };
    a == 172 && (16..=31).contains(&b)
}

/// Ask the box to open `port` (NAT-PMP, then UPnP). Returns the public address
/// only when the mapping is accepted, so a phone off this network can connect.
fn map_outside(port: u16, lan_ip: &str) -> Option<String> {
    if let Some(gw) = default_gateway() {
        if let Some(ip) = nat_pmp(&gw, port) {
            return Some(ip);
        }
    }
    if !lan_ip.is_empty() {
        if let Some(ip) = upnp_map(port, lan_ip) {
            return Some(ip);
        }
    }
    None
}

fn default_gateway() -> Option<String> {
    let text = Command::new("ipconfig")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let mut pending = false;
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("gateway") || lower.contains("passerelle") {
            pending = true;
            if let Some((_, rest)) = line.split_once(':') {
                let ip = rest.trim().split('%').next().unwrap_or("").trim();
                if looks_like_ipv4(ip) && ip != "0.0.0.0" {
                    return Some(ip.to_string());
                }
            }
            continue;
        }
        if pending {
            let ip = line.trim().split('%').next().unwrap_or("").trim();
            if looks_like_ipv4(ip) && ip != "0.0.0.0" {
                return Some(ip.to_string());
            }
            if !line.trim().is_empty() {
                pending = false;
            }
        }
    }
    None
}

fn nat_pmp(gateway: &str, port: u16) -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.set_read_timeout(Some(Duration::from_millis(800))).ok()?;
    let addr = format!("{gateway}:5351");
    sock.send_to(&[0, 0], &addr).ok()?;
    let mut buf = [0u8; 32];
    let (n, _) = sock.recv_from(&mut buf).ok()?;
    if n < 12 || buf[0] != 0 || buf[1] != 128 || u16::from_be_bytes([buf[2], buf[3]]) != 0 {
        return None;
    }
    let ip = format!("{}.{}.{}.{}", buf[8], buf[9], buf[10], buf[11]);
    let mut req = [0u8; 12];
    req[1] = 2;
    req[4] = (port >> 8) as u8;
    req[5] = (port & 0xff) as u8;
    req[6] = req[4];
    req[7] = req[5];
    req[8..12].copy_from_slice(&7200u32.to_be_bytes());
    sock.send_to(&req, &addr).ok()?;
    let (n, _) = sock.recv_from(&mut buf).ok()?;
    if n < 16 || buf[1] != 130 || u16::from_be_bytes([buf[2], buf[3]]) != 0 {
        return None;
    }
    Some(ip)
}

fn upnp_map(port: u16, lan_ip: &str) -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.set_read_timeout(Some(Duration::from_millis(1200))).ok()?;
    let _ = sock.set_broadcast(true);
    let search = b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n";
    sock.send_to(search, "239.255.255.250:1900").ok()?;
    let mut buf = [0u8; 2048];
    let (n, _) = sock.recv_from(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]);
    let location = text.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        if k.trim().eq_ignore_ascii_case("location") {
            Some(v.trim().to_string())
        } else {
            None
        }
    })?;
    let xml = http_exchange(&location, "GET", "", "")?;
    let (urn, control) = wan_control(&xml, &location)?;
    let public_ip = soap_call(&control, &urn, "GetExternalIPAddress", "")?;
    let ip = between(
        &public_ip,
        "<NewExternalIPAddress>",
        "</NewExternalIPAddress>",
    )?
    .to_string();
    if !looks_like_ipv4(&ip) {
        return None;
    }
    let body = format!(
        "<NewRemoteHost></NewRemoteHost><NewExternalPort>{port}</NewExternalPort><NewProtocol>TCP</NewProtocol><NewInternalPort>{port}</NewInternalPort><NewInternalClient>{lan_ip}</NewInternalClient><NewEnabled>1</NewEnabled><NewPortMappingDescription>FluxPlay</NewPortMappingDescription><NewLeaseDuration>7200</NewLeaseDuration>"
    );
    let answer = soap_call(&control, &urn, "AddPortMapping", &body)?;
    if answer.to_ascii_lowercase().contains("fault") {
        return None;
    }
    Some(ip)
}

fn wan_control(xml: &str, location: &str) -> Option<(String, String)> {
    for kind in ["WANIPConnection:1", "WANPPPConnection:1"] {
        let urn = format!("urn:schemas-upnp-org:service:{kind}");
        let Some(idx) = xml.find(&urn) else {
            continue;
        };
        let window = &xml[idx..(idx + 900).min(xml.len())];
        let Some(path) = between(window, "<controlURL>", "</controlURL>") else {
            continue;
        };
        let path = path.trim();
        let control = if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            let base = location.split("://").nth(1).unwrap_or("");
            let host = base.split('/').next().unwrap_or("");
            let scheme = location.split("://").next().unwrap_or("http");
            if path.starts_with('/') {
                format!("{scheme}://{host}{path}")
            } else {
                format!("{scheme}://{host}/{path}")
            }
        };
        return Some((urn, control));
    }
    None
}

fn soap_call(control: &str, urn: &str, action: &str, inner: &str) -> Option<String> {
    let body = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{urn}\">{inner}</u:{action}></s:Body></s:Envelope>"
    );
    let extra = format!("SOAPAction: \"{urn}#{action}\"\r\nContent-Type: text/xml\r\n");
    http_exchange(control, "POST", &extra, &body)
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = text.find(start)? + start.len();
    let rest = &text[i..];
    let j = rest.find(end)?;
    Some(rest[..j].trim())
}

fn http_exchange(url: &str, method: &str, extra: &str, body: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = format!("/{path}");
    let (host, port) = if let Some((h, p)) = hostport.split_once(':') {
        (h, p.parse().unwrap_or(80))
    } else {
        (hostport, 80)
    };
    let addr = format!("{host}:{port}");
    let sock_addr: SocketAddr = addr.parse().ok()?;
    let mut sock = TcpStream::connect_timeout(&sock_addr, Duration::from_millis(1200)).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    sock.set_write_timeout(Some(Duration::from_secs(3))).ok()?;
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {hostport}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(req.as_bytes()).ok()?;
    read_http(&mut sock)
}

fn ffmpeg_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FLUXPLAY_FFMPEG") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in ["ffmpeg.exe", "ffmpeg"] {
                let c = dir.join(name);
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }
    if let Some(path) = which("ffmpeg") {
        return Some(path);
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        let c = PathBuf::from(home).join(r"AppData\Local\Programs\Stremio\ffmpeg.exe");
        if c.is_file() {
            return Some(c);
        }
    }
    let c = PathBuf::from(r"C:\ffmpeg\bin\ffmpeg.exe");
    if c.is_file() {
        return Some(c);
    }
    None
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in [bin.to_string(), format!("{bin}.exe")] {
            let c = dir.join(name);
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

fn publish_onion(port: u16) -> Option<String> {
    for control in ["127.0.0.1:9051", "127.0.0.1:9151"] {
        if let Some(id) = add_onion(control, port) {
            return Some(id);
        }
    }
    None
}

fn add_onion(control: &str, port: u16) -> Option<String> {
    let mut sock = TcpStream::connect_timeout(&control.parse().ok()?, Duration::from_millis(400)).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    sock.set_write_timeout(Some(Duration::from_secs(3))).ok()?;
    let cookie = tor_cookie();
    let auth = if let Some(c) = &cookie {
        format!("AUTHENTICATE {c}\r\n")
    } else {
        "AUTHENTICATE\r\n".into()
    };
    sock.write_all(auth.as_bytes()).ok()?;
    let banner = read_tor(&mut sock)?;
    if !banner.contains("250") {
        return None;
    }
    let cmd = format!("ADD_ONION NEW:BEST Port=80,127.0.0.1:{port}\r\n");
    sock.write_all(cmd.as_bytes()).ok()?;
    let reply = read_tor(&mut sock)?;
    for line in reply.lines() {
        if let Some(rest) = line.split("ServiceID=").nth(1) {
            let id = rest.split_whitespace().next().unwrap_or("").trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn read_tor(sock: &mut TcpStream) -> Option<String> {
    let mut buf = [0u8; 2048];
    let mut got = Vec::new();
    for _ in 0..8 {
        match sock.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got);
                if text.lines().any(|l| l.starts_with("250 ") || l.starts_with("515") || l.starts_with("551"))
                {
                    return Some(text.into_owned());
                }
            }
            Err(_) => break,
        }
    }
    if got.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(&got).into_owned())
    }
}

fn tor_cookie() -> Option<String> {
    let mut paths = Vec::new();
    if let Ok(app) = std::env::var("APPDATA") {
        paths.push(PathBuf::from(&app).join(r"tor\control_auth_cookie"));
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        paths.push(PathBuf::from(&local).join(r"tor\control_auth_cookie"));
    }
    if let Ok(home) = std::env::var("HOME") {
        paths.push(PathBuf::from(&home).join(".tor/control_auth_cookie"));
    }
    for path in paths {
        if let Ok(bytes) = std::fs::read(&path) {
            if !bytes.is_empty() {
                let mut s = String::with_capacity(bytes.len() * 2);
                for b in bytes {
                    s.push_str(&format!("{b:02x}"));
                }
                return Some(s);
            }
        }
    }
    None
}

/// Blocking call used from a worker: remote buttons and "rejoindre".
pub fn http_get(link: &ParsedLink, path_and_query: &str) -> Result<(String, String), String> {
    let req = format!(
        "GET {path_and_query} HTTP/1.1\r\nHost: fluxplay\r\nConnection: close\r\n\r\n"
    );
    if let Some((body, via)) = try_clear(link, req.as_bytes()) {
        return Ok((body, via));
    }
    if let Some(onion) = &link.onion {
        if let Some(body) = try_onion(onion, req.as_bytes()) {
            return Ok((body, format!("onion:{onion}")));
        }
    }
    Err("FluxPlay injoignable.".into())
}

fn try_clear(link: &ParsedLink, req: &[u8]) -> Option<(String, String)> {
    let mut hosts = link.addrs.clone();
    hosts.push("127.0.0.1".into());
    hosts.sort_by_key(|h| if is_private(h) || h.starts_with("127.") { 0 } else { 1 });
    hosts.dedup();
    for host in hosts {
        let wait = if is_private(&host) || host.starts_with("127.") {
            Duration::from_millis(400)
        } else {
            Duration::from_millis(1500)
        };
        let addr = format!("{host}:{}", link.port);
        let Ok(sock_addr) = addr.parse::<SocketAddr>() else {
            continue;
        };
        let Ok(mut sock) = TcpStream::connect_timeout(&sock_addr, wait) else {
            continue;
        };
        sock.set_read_timeout(Some(Duration::from_secs(6))).ok()?;
        sock.set_write_timeout(Some(Duration::from_secs(3))).ok()?;
        sock.write_all(req).ok()?;
        let body = read_http(&mut sock)?;
        if body.starts_with("ok") || body.contains("code=") {
            return Some((body, host));
        }
        // A reached server that refused is still the right host.
        return Some((body, host));
    }
    None
}

fn try_onion(onion: &str, req: &[u8]) -> Option<String> {
    for proxy in ["127.0.0.1:9050", "127.0.0.1:9150"] {
        if let Some(mut sock) = socks5_connect(proxy, onion, 80) {
            sock.set_read_timeout(Some(Duration::from_secs(20))).ok()?;
            sock.write_all(req).ok()?;
            if let Some(body) = read_http(&mut sock) {
                return Some(body);
            }
        }
    }
    None
}

fn read_http(sock: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    for _ in 0..40 {
        match sock.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(_) => break,
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() > 32 {
            // Short control replies fit in one read; stop once the header is in
            // and the socket has nothing further queued. A second short read
            // already happened above when the body arrived with the header.
            if buf.len() > 128 || buf.ends_with(b"\n") {
                break;
            }
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or(text.as_ref());
    Some(body.trim().to_string())
}

fn socks5_connect(proxy: &str, host: &str, port: u16) -> Option<TcpStream> {
    let addr: SocketAddr = proxy.parse().ok()?;
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(20))).ok()?;
    sock.set_write_timeout(Some(Duration::from_secs(10))).ok()?;
    sock.write_all(&[0x05, 0x01, 0x00]).ok()?;
    let mut gre = [0u8; 2];
    sock.read_exact(&mut gre).ok()?;
    if gre[0] != 0x05 || gre[1] != 0x00 {
        return None;
    }
    let hb = host.as_bytes();
    if hb.len() > 255 {
        return None;
    }
    let mut req = Vec::with_capacity(7 + hb.len());
    req.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, hb.len() as u8]);
    req.extend_from_slice(hb);
    req.push((port >> 8) as u8);
    req.push((port & 0xff) as u8);
    sock.write_all(&req).ok()?;
    let mut hdr = [0u8; 4];
    sock.read_exact(&mut hdr).ok()?;
    if hdr[1] != 0x00 {
        return None;
    }
    let skip = match hdr[3] {
        0x01 => 4,
        0x03 => {
            let mut n = [0u8; 1];
            sock.read_exact(&mut n).ok()?;
            n[0] as usize
        }
        0x04 => 16,
        _ => return None,
    };
    let mut dump = vec![0u8; skip + 2];
    sock.read_exact(&mut dump).ok()?;
    Some(sock)
}

/// Playable URL for a member. An onion target is forwarded through the local
/// Tor SOCKS port so the player itself does not have to speak SOCKS.
pub fn playable(url: &str) -> String {
    let Some(rest) = url.strip_prefix("http://") else {
        return url.to_string();
    };
    let Some((host, path)) = rest.split_once('/') else {
        return url.to_string();
    };
    if !host.ends_with(".onion") {
        return url.to_string();
    }
    let Ok(listener) = TcpListener::bind(("127.0.0.1", 0)) else {
        return url.to_string();
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let onion = host.to_string();
    let path = format!("/{path}");
    let serve_path = path.clone();
    std::thread::spawn(move || {
        let path = serve_path;
        for _ in 0..8 {
            let Ok((mut client, _)) = listener.accept() else {
                break;
            };
            let onion = onion.clone();
            let path = path.clone();
            std::thread::spawn(move || {
                let req_path = path.clone();
                let Some(mut remote) = socks5_connect("127.0.0.1:9050", &onion, 80)
                    .or_else(|| socks5_connect("127.0.0.1:9150", &onion, 80))
                else {
                    return;
                };
                // The player speaks HTTP to us. Read its request and forward a
                // clean one so a Host: 127.0.0.1 header is not required upstream.
                let _ = client.set_read_timeout(Some(Duration::from_secs(5)));
                let mut buf = [0u8; 2048];
                let _ = client.read(&mut buf);
                let req = format!("GET {req_path} HTTP/1.1\r\nHost: {onion}\r\nConnection: close\r\n\r\n");
                if remote.write_all(req.as_bytes()).is_err() {
                    return;
                }
                let _ = client.set_read_timeout(None);
                let _ = remote.set_read_timeout(None);
                let mut a = client.try_clone().ok();
                let mut b = remote.try_clone().ok();
                if let (Some(mut src), Some(_)) = (b.as_mut(), a.as_mut()) {
                    let mut dst = client;
                    let _ = std::io::copy(&mut src, &mut dst);
                }
            });
        }
    });
    format!("http://127.0.0.1:{port}{path}")
}

/// Replace `{host}` in a join reply with the address that actually answered.
pub fn materialize_play(body: &str, via: &str, link: &ParsedLink) -> Option<String> {
    let play = body.lines().find_map(|l| l.strip_prefix("play="))?;
    if via.starts_with("onion:") {
        let onion = body.lines().find_map(|l| l.strip_prefix("onion="))?;
        return Some(playable(onion));
    }
    let host = if via == "127.0.0.1" {
        link.addrs.first().map(String::as_str).unwrap_or("127.0.0.1")
    } else {
        via
    };
    Some(play.replace("{host}", host))
}
