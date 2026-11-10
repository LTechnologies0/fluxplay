//! Stable FluxPlay error codes (`FP-…`) for diagnosis across net / catalog / A/V / GPU / OS / UI.
//!
//! Display form: `FP-NET-DNS-TIMEOUT`. Prefer attaching a code to every user-visible failure
//! so support and logs can pinpoint the subsystem without parsing free text.

use std::fmt;

/// Domain of a [`FpCode`] (first segment after `FP-`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FpDomain {
    /// TCP / UDP / HTTP(S) / DNS / TLS / proxy / tunnel.
    Net,
    /// M3U / Xtream / Stalker / XMLTV / catalog parse.
    Cat,
    /// Audio / video / ffmpeg / libmpv / codecs / sync.
    Av,
    /// NVIDIA / AMD / Intel / topology / iced GPU present.
    Gpu,
    /// iced UI / theme / layout / ergonomics.
    Ui,
    /// Linux / Windows / macOS / Android Phone/TV specifics.
    Os,
    /// Generic I/O / internal / unknown.
    Sys,
}

impl FpDomain {
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Net => "NET",
            Self::Cat => "CAT",
            Self::Av => "AV",
            Self::Gpu => "GPU",
            Self::Ui => "UI",
            Self::Os => "OS",
            Self::Sys => "SYS",
        }
    }
}

/// Stable diagnostic code. Keep variants additive — never renumber meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FpCode {
    // ── NET ──────────────────────────────────────────────────────────────
    NetDnsTimeout,
    NetDnsNxdomain,
    NetDnsServfail,
    NetDnsConfig,
    NetTcpConnect,
    NetTcpTimeout,
    NetTcpReset,
    NetUdpSend,
    NetUdpRecv,
    NetHttpStatus,
    NetHttpTimeout,
    NetHttpBody,
    NetHttpRedirect,
    NetHttpsTls,
    NetHttpsCert,
    NetProxyDead,
    NetProxyRefuse,
    NetTunnelDown,
    NetClearnetBlocked,

    // ── CAT ──────────────────────────────────────────────────────────────
    CatM3uEmpty,
    CatM3uParse,
    CatM3uOversize,
    CatM3uLine,
    CatXtreamAuth,
    CatXtreamApi,
    CatXtreamJson,
    CatXtreamEmpty,
    CatStalkerAuth,
    CatStalkerApi,
    CatXmltvParse,
    CatXmltvFetch,
    CatXmltvOversize,
    CatUnsupported,
    CatCacheIo,

    // ── AV ───────────────────────────────────────────────────────────────
    AvUnsupportedScheme,
    AvInvalidUrl,
    AvOpenFailed,
    AvDemux,
    AvDecodeVideo,
    AvDecodeAudio,
    AvEncode,
    AvHwdec,
    AvHwdecFallback,
    AvMpvInit,
    AvMpvOption,
    AvMpvVo,
    AvMpvAo,
    AvMpvLoadfile,
    AvFfmpegInit,
    AvFfmpegOpen,
    AvFfmpegPump,
    AvFfplaySpawn,
    AvExternalSpawn,
    AvExternalHeaders,
    AvSurfaceMissing,
    AvSurfaceRebind,
    AvSoftRgba,
    AvBufferUnderrun,
    AvLipSync,
    AvMuteRace,
    AvTrackSelect,
    AvIntentRefuse,

    // ── GPU ──────────────────────────────────────────────────────────────
    GpuTopology,
    GpuNvidia,
    GpuAmd,
    GpuIntel,
    GpuAdapter,
    GpuSurfaceCompat,
    GpuForceCopy,
    GpuVaapiDevice,
    GpuD3d11Adapter,
    GpuMetalVt,

    // ── UI ───────────────────────────────────────────────────────────────
    UiLayout,
    UiTheme,
    UiIced,
    UiFocus,
    UiHitTarget,
    UiOverscan,
    UiImageDecode,

    // ── OS ───────────────────────────────────────────────────────────────
    OsLinux,
    OsWindows,
    OsMacos,
    OsAndroid,
    OsAndroidTv,
    OsAndroidSurface,
    OsAndroidIntent,
    OsAndroidAudioFocus,
    OsPermission,
    OsSleepWake,

    // ── SYS ──────────────────────────────────────────────────────────────
    SysIo,
    SysParse,
    SysValidation,
    SysInternal,
    SysUnknown,
}

impl FpCode {
    pub const fn domain(self) -> FpDomain {
        use FpCode::*;
        match self {
            NetDnsTimeout | NetDnsNxdomain | NetDnsServfail | NetDnsConfig | NetTcpConnect
            | NetTcpTimeout | NetTcpReset | NetUdpSend | NetUdpRecv | NetHttpStatus
            | NetHttpTimeout | NetHttpBody | NetHttpRedirect | NetHttpsTls | NetHttpsCert
            | NetProxyDead | NetProxyRefuse | NetTunnelDown | NetClearnetBlocked => FpDomain::Net,

            CatM3uEmpty | CatM3uParse | CatM3uOversize | CatM3uLine | CatXtreamAuth
            | CatXtreamApi | CatXtreamJson | CatXtreamEmpty | CatStalkerAuth | CatStalkerApi
            | CatXmltvParse | CatXmltvFetch | CatXmltvOversize | CatUnsupported | CatCacheIo => {
                FpDomain::Cat
            }

            AvUnsupportedScheme | AvInvalidUrl | AvOpenFailed | AvDemux | AvDecodeVideo
            | AvDecodeAudio | AvEncode | AvHwdec | AvHwdecFallback | AvMpvInit | AvMpvOption
            | AvMpvVo | AvMpvAo | AvMpvLoadfile | AvFfmpegInit | AvFfmpegOpen | AvFfmpegPump
            | AvFfplaySpawn | AvExternalSpawn | AvExternalHeaders | AvSurfaceMissing
            | AvSurfaceRebind | AvSoftRgba | AvBufferUnderrun | AvLipSync | AvMuteRace
            | AvTrackSelect | AvIntentRefuse => FpDomain::Av,

            GpuTopology | GpuNvidia | GpuAmd | GpuIntel | GpuAdapter | GpuSurfaceCompat
            | GpuForceCopy | GpuVaapiDevice | GpuD3d11Adapter | GpuMetalVt => FpDomain::Gpu,

            UiLayout | UiTheme | UiIced | UiFocus | UiHitTarget | UiOverscan | UiImageDecode => {
                FpDomain::Ui
            }

            OsLinux | OsWindows | OsMacos | OsAndroid | OsAndroidTv | OsAndroidSurface
            | OsAndroidIntent | OsAndroidAudioFocus | OsPermission | OsSleepWake => FpDomain::Os,

            SysIo | SysParse | SysValidation | SysInternal | SysUnknown => FpDomain::Sys,
        }
    }

    /// Machine token, e.g. `FP-NET-DNS-TIMEOUT`.
    pub const fn as_str(self) -> &'static str {
        use FpCode::*;
        match self {
            NetDnsTimeout => "FP-NET-DNS-TIMEOUT",
            NetDnsNxdomain => "FP-NET-DNS-NXDOMAIN",
            NetDnsServfail => "FP-NET-DNS-SERVFAIL",
            NetDnsConfig => "FP-NET-DNS-CONFIG",
            NetTcpConnect => "FP-NET-TCP-CONNECT",
            NetTcpTimeout => "FP-NET-TCP-TIMEOUT",
            NetTcpReset => "FP-NET-TCP-RESET",
            NetUdpSend => "FP-NET-UDP-SEND",
            NetUdpRecv => "FP-NET-UDP-RECV",
            NetHttpStatus => "FP-NET-HTTP-STATUS",
            NetHttpTimeout => "FP-NET-HTTP-TIMEOUT",
            NetHttpBody => "FP-NET-HTTP-BODY",
            NetHttpRedirect => "FP-NET-HTTP-REDIRECT",
            NetHttpsTls => "FP-NET-HTTPS-TLS",
            NetHttpsCert => "FP-NET-HTTPS-CERT",
            NetProxyDead => "FP-NET-PROXY-DEAD",
            NetProxyRefuse => "FP-NET-PROXY-REFUSE",
            NetTunnelDown => "FP-NET-TUNNEL-DOWN",
            NetClearnetBlocked => "FP-NET-CLEARNET-BLOCKED",

            CatM3uEmpty => "FP-CAT-M3U-EMPTY",
            CatM3uParse => "FP-CAT-M3U-PARSE",
            CatM3uOversize => "FP-CAT-M3U-OVERSIZE",
            CatM3uLine => "FP-CAT-M3U-LINE",
            CatXtreamAuth => "FP-CAT-XTREAM-AUTH",
            CatXtreamApi => "FP-CAT-XTREAM-API",
            CatXtreamJson => "FP-CAT-XTREAM-JSON",
            CatXtreamEmpty => "FP-CAT-XTREAM-EMPTY",
            CatStalkerAuth => "FP-CAT-STALKER-AUTH",
            CatStalkerApi => "FP-CAT-STALKER-API",
            CatXmltvParse => "FP-CAT-XMLTV-PARSE",
            CatXmltvFetch => "FP-CAT-XMLTV-FETCH",
            CatXmltvOversize => "FP-CAT-XMLTV-OVERSIZE",
            CatUnsupported => "FP-CAT-UNSUPPORTED",
            CatCacheIo => "FP-CAT-CACHE-IO",

            AvUnsupportedScheme => "FP-AV-SCHEME",
            AvInvalidUrl => "FP-AV-URL",
            AvOpenFailed => "FP-AV-OPEN",
            AvDemux => "FP-AV-DEMUX",
            AvDecodeVideo => "FP-AV-DECODE-V",
            AvDecodeAudio => "FP-AV-DECODE-A",
            AvEncode => "FP-AV-ENCODE",
            AvHwdec => "FP-AV-HWDEC",
            AvHwdecFallback => "FP-AV-HWDEC-FALLBACK",
            AvMpvInit => "FP-AV-MPV-INIT",
            AvMpvOption => "FP-AV-MPV-OPTION",
            AvMpvVo => "FP-AV-MPV-VO",
            AvMpvAo => "FP-AV-MPV-AO",
            AvMpvLoadfile => "FP-AV-MPV-LOADFILE",
            AvFfmpegInit => "FP-AV-FFMPEG-INIT",
            AvFfmpegOpen => "FP-AV-FFMPEG-OPEN",
            AvFfmpegPump => "FP-AV-FFMPEG-PUMP",
            AvFfplaySpawn => "FP-AV-FFPLAY-SPAWN",
            AvExternalSpawn => "FP-AV-EXTERNAL-SPAWN",
            AvExternalHeaders => "FP-AV-EXTERNAL-HEADERS",
            AvSurfaceMissing => "FP-AV-SURFACE-MISSING",
            AvSurfaceRebind => "FP-AV-SURFACE-REBIND",
            AvSoftRgba => "FP-AV-SOFT-RGBA",
            AvBufferUnderrun => "FP-AV-BUFFER-UNDERRUN",
            AvLipSync => "FP-AV-LIPSYNC",
            AvMuteRace => "FP-AV-MUTE",
            AvTrackSelect => "FP-AV-TRACK",
            AvIntentRefuse => "FP-AV-INTENT-REFUSE",

            GpuTopology => "FP-GPU-TOPOLOGY",
            GpuNvidia => "FP-GPU-NVIDIA",
            GpuAmd => "FP-GPU-AMD",
            GpuIntel => "FP-GPU-INTEL",
            GpuAdapter => "FP-GPU-ADAPTER",
            GpuSurfaceCompat => "FP-GPU-SURFACE",
            GpuForceCopy => "FP-GPU-FORCE-COPY",
            GpuVaapiDevice => "FP-GPU-VAAPI",
            GpuD3d11Adapter => "FP-GPU-D3D11",
            GpuMetalVt => "FP-GPU-METAL-VT",

            UiLayout => "FP-UI-LAYOUT",
            UiTheme => "FP-UI-THEME",
            UiIced => "FP-UI-ICED",
            UiFocus => "FP-UI-FOCUS",
            UiHitTarget => "FP-UI-HIT",
            UiOverscan => "FP-UI-OVERSCAN",
            UiImageDecode => "FP-UI-IMAGE",

            OsLinux => "FP-OS-LINUX",
            OsWindows => "FP-OS-WINDOWS",
            OsMacos => "FP-OS-MACOS",
            OsAndroid => "FP-OS-ANDROID",
            OsAndroidTv => "FP-OS-ANDROID-TV",
            OsAndroidSurface => "FP-OS-ANDROID-SURFACE",
            OsAndroidIntent => "FP-OS-ANDROID-INTENT",
            OsAndroidAudioFocus => "FP-OS-ANDROID-AUDIOFOCUS",
            OsPermission => "FP-OS-PERMISSION",
            OsSleepWake => "FP-OS-SLEEP-WAKE",

            SysIo => "FP-SYS-IO",
            SysParse => "FP-SYS-PARSE",
            SysValidation => "FP-SYS-VALIDATION",
            SysInternal => "FP-SYS-INTERNAL",
            SysUnknown => "FP-SYS-UNKNOWN",
        }
    }

    /// Short French hint for status / toasts (no secrets).
    pub const fn hint_fr(self) -> &'static str {
        use FpCode::*;
        match self {
            NetDnsTimeout => "DNS: délai dépassé",
            NetDnsNxdomain => "DNS: nom introuvable",
            NetDnsServfail => "DNS: serveur en échec",
            NetDnsConfig => "DNS: configuration invalide",
            NetTcpConnect => "TCP: connexion refusée / impossible",
            NetTcpTimeout => "TCP: délai dépassé",
            NetTcpReset => "TCP: connexion réinitialisée",
            NetUdpSend => "UDP: envoi impossible",
            NetUdpRecv => "UDP: réception impossible",
            NetHttpStatus => "HTTP: statut d'erreur",
            NetHttpTimeout => "HTTP: délai dépassé",
            NetHttpBody => "HTTP: corps trop grand / tronqué",
            NetHttpRedirect => "HTTP: trop de redirections",
            NetHttpsTls => "HTTPS: échec TLS",
            NetHttpsCert => "HTTPS: certificat invalide",
            NetProxyDead => "Proxy: tunnel mort (fail-closed)",
            NetProxyRefuse => "Proxy: connexion refusée",
            NetTunnelDown => "Tunnel WG/SOCKS inactif",
            NetClearnetBlocked => "Réseau: chemins IPTV coupés (Wi‑Fi/FAI/WAF)",

            CatM3uEmpty => "M3U: liste vide",
            CatM3uParse => "M3U: parse échoué",
            CatM3uOversize => "M3U: fichier trop volumineux",
            CatM3uLine => "M3U: ligne invalide / trop longue",
            CatXtreamAuth => "Xtream: authentification refusée",
            CatXtreamApi => "Xtream: API en erreur",
            CatXtreamJson => "Xtream: JSON invalide / trop gros",
            CatXtreamEmpty => "Xtream: catalogue vide",
            CatStalkerAuth => "Stalker: auth / MAC refusée",
            CatStalkerApi => "Stalker: API en erreur",
            CatXmltvParse => "XMLTV: parse échoué",
            CatXmltvFetch => "XMLTV: téléchargement échoué",
            CatXmltvOversize => "XMLTV: guide trop volumineux",
            CatUnsupported => "Source non supportée",
            CatCacheIo => "Cache catalogue: I/O",

            AvUnsupportedScheme => "Protocole non supporté",
            AvInvalidUrl => "URL invalide",
            AvOpenFailed => "Ouverture flux échouée",
            AvDemux => "Demux / conteneur",
            AvDecodeVideo => "Décodage vidéo",
            AvDecodeAudio => "Décodage audio",
            AvEncode => "Encodage",
            AvHwdec => "Décodage matériel",
            AvHwdecFallback => "Repli soft après hwdec",
            AvMpvInit => "libmpv: init",
            AvMpvOption => "libmpv: option refusée",
            AvMpvVo => "libmpv: sortie vidéo",
            AvMpvAo => "libmpv: sortie audio",
            AvMpvLoadfile => "libmpv: loadfile",
            AvFfmpegInit => "FFmpeg: init",
            AvFfmpegOpen => "FFmpeg: open",
            AvFfmpegPump => "FFmpeg: soft pump",
            AvFfplaySpawn => "ffplay: démarrage",
            AvExternalSpawn => "Lecteur externe: démarrage",
            AvExternalHeaders => "Lecteur externe: headers requis",
            AvSurfaceMissing => "Android Surface absente",
            AvSurfaceRebind => "Android Surface rebind",
            AvSoftRgba => "Présentation soft RGBA",
            AvBufferUnderrun => "Sous-débit / underrun",
            AvLipSync => "Désync lèvres / audio",
            AvMuteRace => "Mute / volume",
            AvTrackSelect => "Piste audio / sous-titres",
            AvIntentRefuse => "ACTION_VIEW refusée (headers/tunnel)",

            GpuTopology => "Topologie GPU",
            GpuNvidia => "GPU NVIDIA",
            GpuAmd => "GPU AMD",
            GpuIntel => "GPU Intel",
            GpuAdapter => "Adaptateur iced/wgpu",
            GpuSurfaceCompat => "Surface GPU incompatible",
            GpuForceCopy => "Chemin *-copy forcé",
            GpuVaapiDevice => "vaapi-device",
            GpuD3d11Adapter => "d3d11-adapter",
            GpuMetalVt => "VideoToolbox / Metal",

            UiLayout => "Layout / breakpoints",
            UiTheme => "Thème / contraste",
            UiIced => "iced runtime",
            UiFocus => "Focus D-pad / leanback",
            UiHitTarget => "Cible tactile",
            UiOverscan => "Overscan TV",
            UiImageDecode => "Décodage image UI",

            OsLinux => "Linux",
            OsWindows => "Windows",
            OsMacos => "macOS",
            OsAndroid => "Android Phone",
            OsAndroidTv => "Android TV",
            OsAndroidSurface => "Android SurfaceView",
            OsAndroidIntent => "Android Intent",
            OsAndroidAudioFocus => "Focus audio Android",
            OsPermission => "Permission OS",
            OsSleepWake => "Veille / réveil",

            SysIo => "I/O système",
            SysParse => "Parse",
            SysValidation => "Validation",
            SysInternal => "Erreur interne",
            SysUnknown => "Erreur inconnue",
        }
    }

    /// Prefix a free-form message: `[FP-…] message`.
    pub fn format_msg(self, msg: impl fmt::Display) -> String {
        format!("[{}] {msg}", self.as_str())
    }

    /// Like [`Self::format_msg`], but skip if `msg` already carries an `[FP-…]` token.
    pub fn ensure_coded(self, msg: impl fmt::Display) -> String {
        let s = msg.to_string();
        if s.contains("[FP-") {
            s
        } else {
            self.format_msg(s)
        }
    }

    /// Every stable code (for token extract / uniqueness tests). Additive only.
    pub const ALL: &'static [FpCode] = &[
        Self::NetDnsTimeout,
        Self::NetDnsNxdomain,
        Self::NetDnsServfail,
        Self::NetDnsConfig,
        Self::NetTcpConnect,
        Self::NetTcpTimeout,
        Self::NetTcpReset,
        Self::NetUdpSend,
        Self::NetUdpRecv,
        Self::NetHttpStatus,
        Self::NetHttpTimeout,
        Self::NetHttpBody,
        Self::NetHttpRedirect,
        Self::NetHttpsTls,
        Self::NetHttpsCert,
        Self::NetProxyDead,
        Self::NetProxyRefuse,
        Self::NetTunnelDown,
        Self::NetClearnetBlocked,
        Self::CatM3uEmpty,
        Self::CatM3uParse,
        Self::CatM3uOversize,
        Self::CatM3uLine,
        Self::CatXtreamAuth,
        Self::CatXtreamApi,
        Self::CatXtreamJson,
        Self::CatXtreamEmpty,
        Self::CatStalkerAuth,
        Self::CatStalkerApi,
        Self::CatXmltvParse,
        Self::CatXmltvFetch,
        Self::CatXmltvOversize,
        Self::CatUnsupported,
        Self::CatCacheIo,
        Self::AvUnsupportedScheme,
        Self::AvInvalidUrl,
        Self::AvOpenFailed,
        Self::AvDemux,
        Self::AvDecodeVideo,
        Self::AvDecodeAudio,
        Self::AvEncode,
        Self::AvHwdec,
        Self::AvHwdecFallback,
        Self::AvMpvInit,
        Self::AvMpvOption,
        Self::AvMpvVo,
        Self::AvMpvAo,
        Self::AvMpvLoadfile,
        Self::AvFfmpegInit,
        Self::AvFfmpegOpen,
        Self::AvFfmpegPump,
        Self::AvFfplaySpawn,
        Self::AvExternalSpawn,
        Self::AvExternalHeaders,
        Self::AvSurfaceMissing,
        Self::AvSurfaceRebind,
        Self::AvSoftRgba,
        Self::AvBufferUnderrun,
        Self::AvLipSync,
        Self::AvMuteRace,
        Self::AvTrackSelect,
        Self::AvIntentRefuse,
        Self::GpuTopology,
        Self::GpuNvidia,
        Self::GpuAmd,
        Self::GpuIntel,
        Self::GpuAdapter,
        Self::GpuSurfaceCompat,
        Self::GpuForceCopy,
        Self::GpuVaapiDevice,
        Self::GpuD3d11Adapter,
        Self::GpuMetalVt,
        Self::UiLayout,
        Self::UiTheme,
        Self::UiIced,
        Self::UiFocus,
        Self::UiHitTarget,
        Self::UiOverscan,
        Self::UiImageDecode,
        Self::OsLinux,
        Self::OsWindows,
        Self::OsMacos,
        Self::OsAndroid,
        Self::OsAndroidTv,
        Self::OsAndroidSurface,
        Self::OsAndroidIntent,
        Self::OsAndroidAudioFocus,
        Self::OsPermission,
        Self::OsSleepWake,
        Self::SysIo,
        Self::SysParse,
        Self::SysValidation,
        Self::SysInternal,
        Self::SysUnknown,
    ];

    /// Parse `FP-…` or `[FP-…]` (exact token match).
    pub fn from_token(token: &str) -> Option<FpCode> {
        let t = token.trim().trim_start_matches('[').trim_end_matches(']');
        Self::ALL
            .iter()
            .copied()
            .find(|c| c.as_str().eq_ignore_ascii_case(t))
    }

    /// First `[FP-…]` embedded in free-form text (W4-14 / #19 gap fill).
    pub fn extract_from_message(msg: &str) -> Option<FpCode> {
        let bytes = msg.as_bytes();
        let mut i = 0;
        while i + 4 < bytes.len() {
            if bytes[i] == b'['
                && (bytes[i + 1] == b'F' || bytes[i + 1] == b'f')
                && (bytes[i + 2] == b'P' || bytes[i + 2] == b'p')
                && bytes[i + 3] == b'-'
            {
                if let Some(rel) = msg[i + 1..].find(']') {
                    return Self::from_token(&msg[i + 1..i + 1 + rel]);
                }
                return None;
            }
            i += 1;
        }
        None
    }
}

impl fmt::Display for FpCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Types that carry a stable [`FpCode`].
pub trait HasFpCode {
    fn fp_code(&self) -> FpCode;

    fn format_coded(&self) -> String
    where
        Self: fmt::Display,
    {
        self.fp_code().format_msg(self)
    }
}

/// Classify `std::io::Error` into a network/sys code.
pub fn classify_io(err: &std::io::Error) -> FpCode {
    use std::io::ErrorKind::*;
    match err.kind() {
        TimedOut => FpCode::NetTcpTimeout,
        ConnectionRefused => FpCode::NetTcpConnect,
        ConnectionReset | BrokenPipe => FpCode::NetTcpReset,
        NotFound | PermissionDenied => FpCode::SysIo,
        UnexpectedEof | WriteZero | InvalidData => FpCode::SysIo,
        AddrInUse | AddrNotAvailable | NetworkUnreachable | HostUnreachable => {
            FpCode::NetTcpConnect
        }
        _ => {
            let s = err.to_string().to_ascii_lowercase();
            if s.contains("dns") || s.contains("name or service") || s.contains("nodename") {
                FpCode::NetDnsServfail
            } else if s.contains("tls") || s.contains("ssl") || s.contains("certificate") {
                FpCode::NetHttpsTls
            } else {
                FpCode::SysIo
            }
        }
    }
}

/// Best-effort classification of free-form backend / provider messages.
pub fn classify_message(msg: &str) -> FpCode {
    // Prefer an already-embedded stable token (call sites that coded first).
    if let Some(code) = FpCode::extract_from_message(msg) {
        return code;
    }
    let s = msg.to_ascii_lowercase();
    let has = |p: &str| s.contains(p);

    if has("external_needs_intent") || (has("intent") && has("header")) {
        return FpCode::AvIntentRefuse;
    }
    if has("soft_rgba") || (has("soft") && has("present") && has("rgba")) {
        return FpCode::AvSoftRgba;
    }
    if has("surface") && (has("rebind") || has("reattach")) {
        return FpCode::AvSurfaceRebind;
    }
    if has("surface") && (has("wid") || has("missing") || has("dead") || has("absente")) {
        return FpCode::AvSurfaceMissing;
    }
    if has("mediacodec") || has("vo=gpu") || has("vo=libmpv") || has(" vo ") {
        return FpCode::AvMpvVo;
    }
    if has("ao-") || has("audio device") || has("pipewire") || has("wasapi") || has("opensles")
    {
        return FpCode::AvMpvAo;
    }
    if has("hwdec") && has("fallback") {
        return FpCode::AvHwdecFallback;
    }
    // Prefer GPU-device codes before generic hwdec bucket.
    if has("vaapi-device") || has("vaapi device") {
        return FpCode::GpuVaapiDevice;
    }
    if has("d3d11-adapter") || has("d3d11 adapter") {
        return FpCode::GpuD3d11Adapter;
    }
    if has("hwdec") || has("vaapi") || has("cuda") || has("d3d11") || has("videotoolbox") {
        return FpCode::AvHwdec;
    }
    if has("metal") && (has("vt") || has("zero-copy")) {
        return FpCode::GpuMetalVt;
    }
    if has("nvidia") {
        return FpCode::GpuNvidia;
    }
    if has("amd") || has("radeon") {
        return FpCode::GpuAmd;
    }
    if has("intel") || has("iris") || has("uhd") {
        return FpCode::GpuIntel;
    }
    if has("gpu") && has("topology") {
        return FpCode::GpuTopology;
    }
    if has("gpu") && has("adapter") {
        return FpCode::GpuAdapter;
    }
    if has("force-copy") || has("force_copy") {
        return FpCode::GpuForceCopy;
    }
    if has("mpv") && (has("option") || has("set_property")) {
        return FpCode::AvMpvOption;
    }
    if has("mpv") && (has("spawn") || has("init")) {
        return FpCode::AvMpvInit;
    }
    if has("ffplay") {
        return FpCode::AvFfplaySpawn;
    }
    if has("ffmpeg") && has("pump") {
        return FpCode::AvFfmpegPump;
    }
    if has("ffmpeg") && has("init") {
        return FpCode::AvFfmpegInit;
    }
    if has("ffmpeg") || has("libffmpeg") {
        return FpCode::AvFfmpegOpen;
    }
    if has("loadfile") {
        return FpCode::AvMpvLoadfile;
    }
    if has("external") && has("header") {
        return FpCode::AvExternalHeaders;
    }
    if has("external") && (has("spawn") || has("launch") || has("démarrage")) {
        return FpCode::AvExternalSpawn;
    }
    if has("mute") || has("volume race") {
        return FpCode::AvMuteRace;
    }
    // FR UI ("Piste audio…", "Sous-titres…") and EN track-select failures.
    if has("piste")
        || has("sous-titre")
        || has("sous titres")
        || (has("track") && (has("audio") || has("subtitle") || has("select")))
        || has("audio track")
        || has("subtitle")
    {
        return FpCode::AvTrackSelect;
    }
    // Transport before catalog: reqwest "error sending request for url (…player_api…)"
    // must not become FP-CAT-XTREAM-API just because the path contains player_api.
    // RST on IPTV paths (player_api/live/movie) = Wi‑Fi/FAI/WAF, not a random TCP blip.
    if (has("connection reset") || has("broken pipe") || (has("reset") && has("error sending")))
        && (has("player_api")
            || has("/movie/")
            || has("/live/")
            || has("/series/")
            || has("get.php")
            || has("xtream")
            || has("chemins iptv")
            || has("coupe player_api"))
    {
        return FpCode::NetClearnetBlocked;
    }
    if has("error sending request")
        || has("connection reset")
        || has("connection refused")
        || has("broken pipe")
        || has("network unreachable")
        || has("host unreachable")
    {
        if has("reset") {
            return FpCode::NetTcpReset;
        }
        if has("timed out") || has("timeout") {
            return FpCode::NetTcpTimeout;
        }
        return FpCode::NetTcpConnect;
    }
    if has("xtream")
        && (has("json")
            || has("oversize")
            || has("trop volumineuse")
            || (has("trop gros") && has("json")))
    {
        return FpCode::CatXtreamJson;
    }
    if has("xtream")
        && (has("auth") || has("password") || has("username") || has("identifiant"))
    {
        return FpCode::CatXtreamAuth;
    }
    if has("xtream") && (has("vide") || has("empty")) {
        return FpCode::CatXtreamEmpty;
    }
    // API/panel failures — not bare transport. Require xtream/panel context beyond URL path.
    if has("xtream")
        && (has("api") || has("panel") || has("user_info") || has("server_info") || has("http "))
    {
        return FpCode::CatXtreamApi;
    }
    if has("player_api")
        && (has("auth") || has("status") || has("401") || has("403") || has("json") || has("parse"))
    {
        return FpCode::CatXtreamApi;
    }
    // Auth: MAC / handshake / token. API: other stalker/portal.php failures.
    if has("stalker")
        && (has("auth") || has("mac") || has("handshake") || has("token") || has("bearer"))
    {
        return FpCode::CatStalkerAuth;
    }
    if has("stalker") || has("portal.php") {
        return FpCode::CatStalkerApi;
    }
    // M3U oversize: English keys, French "trop volumineuse", or explicit token.
    let m3u_ctx = has("m3u") || has("playlist") || has("get.php");
    let oversize_hint = has("oversize")
        || has("too large")
        || has("body cap")
        || has("volumineu")
        || has("trop volumineuse");
    if has("fp-cat-m3u-oversize") || (m3u_ctx && oversize_hint) {
        return FpCode::CatM3uOversize;
    }
    if has("fp-cat-m3u-empty") || (m3u_ctx && (has("empty") || has("vide"))) {
        return FpCode::CatM3uEmpty;
    }
    if has("fp-cat-m3u-line") || (has("m3u") && (has("line") || has("overlong"))) {
        return FpCode::CatM3uLine;
    }
    if has("m3u") || has("extinf") || has("extvlcopt") {
        return FpCode::CatM3uParse;
    }
    // XMLTV / EPG (W4-17): prefer OVERSIZE when body/inflate caps fire.
    let xmltv_ctx = has("xmltv") || has("epg") || has("fp-cat-xmltv");
    let xmltv_oversize = oversize_hint
        || has("expands past")
        || has("inflate")
        || has("gzip") && has("past");
    if has("fp-cat-xmltv-oversize") || (xmltv_ctx && xmltv_oversize) {
        return FpCode::CatXmltvOversize;
    }
    if has("fp-cat-xmltv-parse") || (xmltv_ctx && has("parse")) {
        return FpCode::CatXmltvParse;
    }
    if xmltv_ctx {
        return FpCode::CatXmltvFetch;
    }
    // Shared `read_body_capped` French string without catalog context (W4-13).
    if has("trop volumineuse") || has("trop volumineux") || has("body too large") {
        return FpCode::NetHttpBody;
    }
    if has("nxdomain") {
        return FpCode::NetDnsNxdomain;
    }
    if has("dns") && (has("timeout") || has("timed out")) {
        return FpCode::NetDnsTimeout;
    }
    if has("dns") || has("resolve") {
        return FpCode::NetDnsServfail;
    }
    // WG / SOCKS before TLS: boringtun "aucun handshake" must not become HTTPS-TLS.
    if has("wireguard") || (has("socks") && has("down")) || (has("tunnel") && has("échec")) {
        return FpCode::NetTunnelDown;
    }
    if has("proxy") && (has("dead") || has("127.0.0.1:9") || has("tunnel")) {
        return FpCode::NetProxyDead;
    }
    if has("proxy") {
        return FpCode::NetProxyRefuse;
    }
    if has("certificate") || has("cert") {
        return FpCode::NetHttpsCert;
    }
    // Require tls/ssl — bare "handshake" is also used by WireGuard probe errors.
    if has("tls") || has("ssl") {
        return FpCode::NetHttpsTls;
    }
    if has("clearnet")
        || has("path policy")
        || has("chemins iptv")
        || has("coupe player_api")
        || has("root only")
        || (has("wifi") && (has("fai") || has("waf") || has("rst")))
        || (has("leak") && (has("tunnel") || has("proxy") || has("http-proxy") || has("clearnet")))
    {
        return FpCode::NetClearnetBlocked;
    }
    // RST on Xtream/media paths is almost always DPI/WAF, not a random TCP blip.
    if has("reset")
        && (has("player_api")
            || has("/movie/")
            || has("/live/")
            || has("/series/")
            || has("xtream")
            || has("get.php"))
    {
        return FpCode::NetClearnetBlocked;
    }
    if has("udp") && has("send") {
        return FpCode::NetUdpSend;
    }
    if has("udp") && has("recv") {
        return FpCode::NetUdpRecv;
    }
    if (has("body") && (has("cap") || has("too large") || has("oversize") || has("tronqué")))
        || has("http body")
    {
        return FpCode::NetHttpBody;
    }
    if has("redirect") {
        return FpCode::NetHttpRedirect;
    }
    if has("timeout") || has("timed out") || has("délai") {
        return FpCode::NetHttpTimeout;
    }
    // "reconnexion" contains "connexion" — keep that as AvOpenFailed (below).
    if has("connect") || (has("connexion") && !has("reconnexion")) {
        return FpCode::NetTcpConnect;
    }
    if has("http")
        && (has("status")
            || has("401")
            || has("403")
            || has("404")
            || has("429")
            || has("500"))
    {
        return FpCode::NetHttpStatus;
    }
    if has("unsupported") && has("scheme") {
        return FpCode::AvUnsupportedScheme;
    }
    if has("invalid url") || (has("url") && has("invalid")) {
        return FpCode::AvInvalidUrl;
    }
    if has("iced") || has("wgpu") || has("swapchain") {
        return FpCode::UiIced;
    }
    if has("theme") || has("palette") {
        return FpCode::UiTheme;
    }
    if has("overscan") {
        return FpCode::UiOverscan;
    }
    if has("hit target") || has("cible tactile") {
        return FpCode::UiHitTarget;
    }
    if has("layout") && has("ui") {
        return FpCode::UiLayout;
    }
    if has("focus") && (has("ui") || has("d-pad") || has("dpad")) {
        return FpCode::UiFocus;
    }
    if has("image") && (has("decode") || has("poster") || has("art")) {
        return FpCode::UiImageDecode;
    }
    if has("android tv") || has("leanback") {
        return FpCode::OsAndroidTv;
    }
    if has("android") && has("surface") {
        return FpCode::OsAndroidSurface;
    }
    if has("android") && has("intent") {
        return FpCode::OsAndroidIntent;
    }
    if has("android") && (has("audiofocus") || has("audio focus")) {
        return FpCode::OsAndroidAudioFocus;
    }
    if has("android") {
        return FpCode::OsAndroid;
    }
    if has("wayland") || has("x11") || (has("pipewire") && has("linux")) {
        return FpCode::OsLinux;
    }
    if has("wasapi") || has("win32") || (has("d3d11") && has("windows")) {
        return FpCode::OsWindows;
    }
    if has("coreaudio") || has("videotoolbox") || has("macos") || has("darwin") {
        return FpCode::OsMacos;
    }
    if has("permission") || has("access denied") {
        return FpCode::OsPermission;
    }
    if has("sleep") || has("wake") || has("veille") || has("réveil") {
        return FpCode::OsSleepWake;
    }
    if has("decode") && (has("h264") || has("hevc") || has("av1") || has("vp9") || has("video"))
    {
        return FpCode::AvDecodeVideo;
    }
    if has("decode") && (has("aac") || has("ac3") || has("eac3") || has("opus") || has("audio"))
    {
        return FpCode::AvDecodeAudio;
    }
    if has("demux") || has("container") || has("mpegts") {
        return FpCode::AvDemux;
    }
    if has("encode") || has("transcode") {
        return FpCode::AvEncode;
    }
    if has("underrun") || (has("buffering") && has("stall")) {
        return FpCode::AvBufferUnderrun;
    }
    if has("lipsync") || has("a/v sync") || has("audio delay") {
        return FpCode::AvLipSync;
    }
    if has("flux inaccessible")
        || has("open failed")
        || has("échec lecture")
        || has("reconnexion")
    {
        return FpCode::AvOpenFailed;
    }
    if has("introuvable") || has("not found") {
        return FpCode::SysIo;
    }
    if has("catalogue") || has("catalog") || has("cache") {
        return FpCode::CatCacheIo;
    }
    FpCode::SysUnknown
}

/// Ensure a free-form status / log line carries an `[FP-…]` prefix.
///
/// Also scrubs `http(s)://…` tokens so accidental credential URLs never reach the UI.
pub fn ensure_status(msg: impl fmt::Display) -> String {
    let s = crate::redact_urls(&msg.to_string());
    if s.contains("[FP-") {
        s
    } else {
        classify_message(&s).format_msg(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_prefixed() {
        let mut seen = std::collections::BTreeSet::new();
        for c in FpCode::ALL {
            assert!(c.as_str().starts_with("FP-"));
            assert!(c.as_str().contains(c.domain().slug()));
            assert!(!c.hint_fr().is_empty());
            assert!(seen.insert(c.as_str()), "duplicate {}", c.as_str());
            assert_eq!(FpCode::from_token(c.as_str()), Some(*c));
            assert_eq!(
                FpCode::extract_from_message(&format!("x [{}] y", c.as_str())),
                Some(*c)
            );
        }
        assert_eq!(FpCode::ALL.len(), seen.len());
    }

    #[test]
    fn classify_message_hits_major_paths() {
        assert_eq!(
            classify_message("EXTERNAL_NEEDS_INTENT"),
            FpCode::AvIntentRefuse
        );
        assert_eq!(
            classify_message("vo=mediacodec_embed indisponible"),
            FpCode::AvMpvVo
        );
        assert_eq!(
            classify_message("xtream auth failed"),
            FpCode::CatXtreamAuth
        );
        // Transport must win over player_api in the URL path.
        assert_eq!(
            classify_message(
                "error sending request for url (http://exemple/player_api.php?username=x)"
            ),
            FpCode::NetTcpConnect
        );
        assert_eq!(
            classify_message("Connection reset by peer"),
            FpCode::NetTcpReset
        );
        assert_eq!(
            classify_message("error sending request for url (http://x/player_api.php): connection reset"),
            FpCode::NetClearnetBlocked
        );
        assert_eq!(
            classify_message("Stalker handshake: no token"),
            FpCode::CatStalkerAuth
        );
        assert_eq!(
            classify_message("stalker JSON parse: eof"),
            FpCode::CatStalkerApi
        );
        assert_eq!(
            classify_message("certificate verify failed"),
            FpCode::NetHttpsCert
        );
        assert_eq!(
            classify_message("DNS timeout resolving portal"),
            FpCode::NetDnsTimeout
        );
        assert_eq!(
            classify_message("nxdomain for portal.host"),
            FpCode::NetDnsNxdomain
        );
        assert_eq!(
            classify_message("réponse trop volumineuse (max 1024 o)"),
            FpCode::NetHttpBody
        );
        assert_eq!(
            classify_message("Échec tunnel WireGuard: aucun handshake en 12 s"),
            FpCode::NetTunnelDown
        );
        assert_eq!(
            classify_message("M3U playlist oversize (max 32)"),
            FpCode::CatM3uOversize
        );
        assert_eq!(
            classify_message("playlist vide (serveur a renvoyé un corps vide)"),
            FpCode::CatM3uEmpty
        );
        assert_eq!(
            classify_message("[FP-CAT-M3U-OVERSIZE] réponse trop volumineuse"),
            FpCode::CatM3uOversize
        );
        assert_eq!(
            classify_message("xmltv réponse trop volumineuse (max 8000000 o)"),
            FpCode::CatXmltvOversize
        );
        assert_eq!(
            classify_message("epg gzip oversize expands past 64 MB"),
            FpCode::CatXmltvOversize
        );
    }

    #[test]
    fn format_msg_brackets_code() {
        let s = FpCode::NetTcpConnect.format_msg("connexion refusée");
        assert!(s.starts_with("[FP-NET-TCP-CONNECT]"));
        assert!(s.contains("connexion"));
    }

    #[test]
    fn ensure_coded_is_idempotent() {
        let once = FpCode::CatXtreamAuth.format_msg("auth failed");
        let twice = FpCode::CatXtreamAuth.ensure_coded(&once);
        assert_eq!(once, twice);
        assert_eq!(once.matches("[FP-").count(), 1);
    }

    #[test]
    fn ensure_status_classifies_free_text() {
        let s = ensure_status("certificate verify failed");
        assert!(s.starts_with("[FP-NET-HTTPS-CERT]"));
    }

    #[test]
    fn classify_platform_and_codec_paths() {
        // "surface"+"dead" is an A/V surface fault before the OS bucket.
        assert_eq!(classify_message("android surface dead"), FpCode::AvSurfaceMissing);
        assert_eq!(classify_message("android intent refused"), FpCode::OsAndroidIntent);
        assert_eq!(classify_message("hevc decode failed"), FpCode::AvDecodeVideo);
        assert_eq!(classify_message("aac decode failed"), FpCode::AvDecodeAudio);
        assert_eq!(
            classify_message("Piste audio non supportée"),
            FpCode::AvTrackSelect
        );
        assert_eq!(
            classify_message("Sous-titres non supportés"),
            FpCode::AvTrackSelect
        );
        assert_eq!(
            classify_message("android soft vf rejected — hwdec-fallback to software decode"),
            FpCode::AvHwdecFallback
        );
        assert_eq!(classify_message("iced wgpu swapchain lost"), FpCode::UiIced);
    }

    #[test]
    fn classify_ffi_and_gap_paths() {
        assert_eq!(
            classify_message("android surface rebind failed"),
            FpCode::AvSurfaceRebind
        );
        assert_eq!(classify_message("ffmpeg pump spawn failed"), FpCode::AvFfmpegPump);
        assert_eq!(classify_message("mpv option refused"), FpCode::AvMpvOption);
        assert_eq!(classify_message("external headers required"), FpCode::AvExternalHeaders);
        assert_eq!(classify_message("vaapi-device missing"), FpCode::GpuVaapiDevice);
        assert_eq!(
            classify_message("Toujours rien — reconnexion…"),
            FpCode::AvOpenFailed
        );
        assert_eq!(classify_message("Chaîne introuvable"), FpCode::SysIo);
        assert_eq!(
            classify_message("xmltv parse error at line 2"),
            FpCode::CatXmltvParse
        );
    }

    #[test]
    fn ensure_status_redacts_urls() {
        let s = ensure_status(
            "fetch failed https://host/get.php?username=u&password=s3cret",
        );
        assert!(s.starts_with("[FP-"), "{s}");
        assert!(s.contains("<url>"), "{s}");
        assert!(!s.contains("s3cret"), "{s}");
    }
}
