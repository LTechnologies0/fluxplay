//! App network prefs: WireGuard profile import + DNS apply for HTTP clients.

use std::path::{Path, PathBuf};

use fluxplay_core::{DnsMode, NetworkSettings};
use tracing::{info, warn};

use crate::storage;

/// Directory holding the imported WireGuard profile.
pub fn wireguard_dir() -> PathBuf {
    storage::config_dir().join("wireguard")
}

pub fn wireguard_active_path() -> PathBuf {
    wireguard_dir().join("fluxplay.conf")
}

/// Import a `.conf` into the FluxPlay config dir and return updated network settings.
///
/// Profile `DNS=` is stored as **bootstrap only** (Endpoint resolution before tunnel).
/// User Custom / DoH / DoT prefs are left untouched.
pub fn import_wireguard_profile(
    net: &NetworkSettings,
    source: &Path,
) -> Result<NetworkSettings, String> {
    let bytes = std::fs::read(source).map_err(|e| format!("lecture profil: {e}"))?;
    let text = String::from_utf8_lossy(&bytes);
    if !text.contains("[Interface]") {
        return Err("fichier invalide — section [Interface] manquante".into());
    }
    let dir = wireguard_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("création dossier: {e}"))?;
    let dest = wireguard_active_path();
    std::fs::write(&dest, bytes.as_slice()).map_err(|e| format!("écriture profil: {e}"))?;

    let name = source
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("fluxplay.conf")
        .to_string();

    let mut out = net.clone();
    out.wireguard_profile_path = dest.display().to_string();
    out.wireguard_profile_name = name;
    if let Some(dns) = parse_wg_dns(&text) {
        out.wireguard_bootstrap_dns = dns;
    }
    info!(
        path = %out.wireguard_profile_path,
        name = %out.wireguard_profile_name,
        bootstrap = %out.wireguard_bootstrap_dns,
        "WireGuard profile imported (DNS= bootstrap only)"
    );
    Ok(out)
}

/// Import from pasted conf text.
pub fn import_wireguard_text(
    net: &NetworkSettings,
    text: &str,
    display_name: &str,
) -> Result<NetworkSettings, String> {
    let text = text.trim();
    if !text.contains("[Interface]") {
        return Err("profil invalide — section [Interface] manquante".into());
    }
    let dir = wireguard_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("création dossier: {e}"))?;
    let dest = wireguard_active_path();
    std::fs::write(&dest, text.as_bytes()).map_err(|e| format!("écriture profil: {e}"))?;

    let mut out = net.clone();
    out.wireguard_profile_path = dest.display().to_string();
    out.wireguard_profile_name = if display_name.trim().is_empty() {
        "collé.conf".into()
    } else {
        display_name.trim().to_string()
    };
    if let Some(dns) = parse_wg_dns(text) {
        out.wireguard_bootstrap_dns = dns;
    }
    Ok(out)
}

pub fn clear_wireguard_profile(net: &NetworkSettings) -> NetworkSettings {
    crate::wg_tunnel::stop_tunnel();
    let path = wireguard_active_path();
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            warn!(error = %e, "failed to remove WireGuard profile");
        }
    }
    let mut out = net.clone();
    out.wireguard_enabled = false;
    out.wireguard_profile_path.clear();
    out.wireguard_profile_name.clear();
    out.wireguard_bootstrap_dns.clear();
    out
}

/// Refresh bootstrap DNS from the on-disk profile without changing app DNS prefs.
pub fn refresh_bootstrap_dns(net: &NetworkSettings) -> Result<NetworkSettings, String> {
    let path = wireguard_active_path();
    let text = std::fs::read_to_string(&path).map_err(|_| "Profil WireGuard introuvable".to_string())?;
    let mut out = net.clone();
    match parse_wg_dns(&text) {
        Some(dns) if !dns.is_empty() => {
            out.wireguard_bootstrap_dns = dns;
            Ok(out)
        }
        _ => Err("Aucune ligne DNS= dans le profil".into()),
    }
}

/// `DNS = 1.1.1.1, 1.0.0.1` from a WireGuard conf.
pub fn parse_wg_dns(conf: &str) -> Option<String> {
    for line in conf.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, vals)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("dns") {
            continue;
        }
        let vals = vals.trim();
        if !vals.is_empty() {
            return Some(vals.to_string());
        }
    }
    None
}

pub fn apply_to_http(net: &NetworkSettings) {
    fluxplay_providers::apply_network_settings(net);
}

pub fn wireguard_status_line(net: &NetworkSettings) -> String {
    if net.wireguard_profile_path.is_empty() {
        return "Aucun profil WireGuard importé.".into();
    }
    let name = if net.wireguard_profile_name.is_empty() {
        net.wireguard_profile_path.as_str()
    } else {
        net.wireguard_profile_name.as_str()
    };
    let bootstrap = if net.wireguard_bootstrap_dns.is_empty() {
        "bootstrap DNS : (aucun — Endpoint IP ou DNS système)".to_string()
    } else {
        format!("bootstrap DNS : {}", net.wireguard_bootstrap_dns)
    };
    if net.wireguard_enabled {
        if let Some(url) = crate::wg_tunnel::socks_proxy_url() {
            format!(
                "Tunnel app SOCKS actif (« {name} ») — {url}. DNS app via tunnel ({bootstrap})."
            )
        } else if crate::wg_tunnel::tunnel_start_in_flight() {
            format!("Profil « {name} » — démarrage tunnel app SOCKS… ({bootstrap})")
        } else {
            format!("Profil « {name} » activé ({bootstrap}) — tunnel app SOCKS non joignable.")
        }
    } else {
        format!("Profil « {name} » importé, tunnel off ({bootstrap}).")
    }
}

pub fn dns_status_line(net: &NetworkSettings) -> String {
    let base = match net.dns_mode {
        DnsMode::System => "Résolution DNS : système (OS).".to_string(),
        DnsMode::Custom => format!("DNS classique : {}", net.dns_servers),
        DnsMode::Doh => format!("DNS over HTTPS : {}", net.doh_url),
        DnsMode::Dot => format!("DNS over TLS : {}", net.dot_server),
    };
    if crate::wg_tunnel::tunnel_is_up() {
        match net.dns_mode {
            DnsMode::System => {
                format!(
                    "{base} Tunnel SOCKS ON → DoH Cloudflare dans le tunnel (pas le DNS= profil)."
                )
            }
            _ => format!("{base} Tunnel SOCKS ON → DNS app via le tunnel."),
        }
    } else {
        base
    }
}
