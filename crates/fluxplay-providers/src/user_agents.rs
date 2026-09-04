//! User-Agents commonly accepted by IPTV panels / CDN WAFs.

/// Rotate through these when a playlist URL returns HTTP 885 / empty / 403.
pub const IPTV_USER_AGENTS: &[&str] = &[
    "IPTVSmartersPlayer",
    "IPTV Smarters Pro",
    "VLC/3.0.21 LibVLC/3.0.21",
    "VLC/3.0.18 LibVLC/3.0.18",
    "Lavf/60.16.100",
    "Lavf/58.76.100",
    "TiviMate/4.7.0",
    "OTT Navigator/1.7.2.2",
    "GSE SMART IPTV",
    "XCIPTV",
    "Perfect Player",
    "Kodi/20.0 (Linux; Android 11)",
    "Mozilla/5.0 (QtEmbedded; U; Linux; C) AppleWebKit/533.3 (KHTML, like Gecko) MAG200 stbapp ver: 4 rev: 1812",
    "Mozilla/5.0 (QtEmbedded; U; Linux; C) AppleWebKit/533.3 (KHTML, like Gecko) MAG254",
    "okhttp/4.12.0",
    "Dalvik/2.1.0 (Linux; U; Android 13)",
    "ExoPlayerLib/2.18.0",
    "AppleCoreMedia/1.0.0.19E241 (iPhone; U; CPU OS 15_0 like Mac OS X)",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
];

pub fn agents_for(source_ua: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(ua) = source_ua {
        if !ua.trim().is_empty() {
            out.push(ua.trim().to_string());
        }
    }
    for ua in IPTV_USER_AGENTS {
        if !out.iter().any(|e| e == ua) {
            out.push((*ua).to_string());
        }
    }
    out
}
