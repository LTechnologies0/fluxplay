//! System media players for the "Lecteur système" backend.
//!
//! Order: the OS default video handler (freedesktop `xdg-mime` / `mimeapps.list`),
//! then installed players known to handle IPTV links (HLS, MPEG-TS over HTTP), native
//! binaries first and Flatpak apps second. `xdg-open` on an `http://` URL would open
//! the web browser, so it is never used for streams.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::{PlayerError, Result};

/// Command-line options a player understands for HTTP headers / title / cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dialect {
    Vlc,
    Mpv,
    /// Celluloid forwards `--mpv-<option>` to its mpv core.
    Celluloid,
    Ffplay,
    /// URL only (Haruna, SMPlayer, Totem…).
    Plain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Launch {
    Native(PathBuf),
    Flatpak(String),
    /// `Exec=` of a `.desktop` entry, `{url}` marks where the URL goes.
    Desktop(Vec<String>),
    /// macOS `open -a <App>`.
    MacApp(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalPlayer {
    /// Stable id stored in settings (`vlc`, `haruna`, `desktop:foo.desktop`).
    pub id: String,
    pub name: String,
    /// Default video player of the OS / desktop session.
    pub is_default: bool,
    pub flatpak: bool,
    dialect: Dialect,
    launch: Launch,
}

impl ExternalPlayer {
    /// `Haruna (Flatpak)`
    pub fn label(&self) -> String {
        if self.flatpak {
            format!("{} (Flatpak)", self.name)
        } else {
            self.name.clone()
        }
    }

    /// True when user agent / referer can be forwarded (panels often require them).
    pub fn forwards_headers(&self) -> bool {
        !matches!(self.dialect, Dialect::Plain)
    }
}

/// Per-launch stream details.
#[derive(Debug, Clone, Default)]
pub struct ExternalLaunch<'a> {
    pub user_agent: Option<&'a str>,
    pub referer: Option<&'a str>,
    pub title: Option<&'a str>,
    pub cache_ms: Option<u32>,
}

struct Known {
    id: &'static str,
    name: &'static str,
    bins: &'static [&'static str],
    flatpaks: &'static [&'static str],
    desktops: &'static [&'static str],
    dialect: Dialect,
}

/// Best IPTV support first (full HTTP header control, HLS, TS).
const KNOWN: &[Known] = &[
    Known {
        id: "vlc",
        name: "VLC",
        bins: &[
            "vlc",
            "/Applications/VLC.app/Contents/MacOS/VLC",
            r"C:\Program Files\VideoLAN\VLC\vlc.exe",
            r"C:\Program Files (x86)\VideoLAN\VLC\vlc.exe",
        ],
        flatpaks: &["org.videolan.VLC"],
        desktops: &["vlc.desktop", "org.videolan.VLC.desktop", "vlc_vlc.desktop"],
        dialect: Dialect::Vlc,
    },
    Known {
        id: "mpv",
        name: "mpv",
        bins: &["mpv", "/Applications/mpv.app/Contents/MacOS/mpv"],
        flatpaks: &["io.mpv.Mpv"],
        desktops: &["mpv.desktop", "io.mpv.Mpv.desktop"],
        dialect: Dialect::Mpv,
    },
    Known {
        id: "haruna",
        name: "Haruna",
        bins: &["haruna"],
        flatpaks: &["org.kde.haruna"],
        desktops: &["org.kde.haruna.desktop", "haruna.desktop"],
        dialect: Dialect::Plain,
    },
    Known {
        id: "celluloid",
        name: "Celluloid",
        bins: &["celluloid"],
        flatpaks: &["io.github.celluloid_player.Celluloid"],
        desktops: &["io.github.celluloid_player.Celluloid.desktop", "celluloid.desktop"],
        dialect: Dialect::Celluloid,
    },
    Known {
        id: "smplayer",
        name: "SMPlayer",
        bins: &["smplayer"],
        flatpaks: &["info.smplayer.SMPlayer"],
        desktops: &["smplayer.desktop", "info.smplayer.SMPlayer.desktop"],
        dialect: Dialect::Plain,
    },
    Known {
        id: "mpc-qt",
        name: "MPC-Qt",
        bins: &["mpc-qt"],
        flatpaks: &["io.github.mpc_qt.mpc-qt"],
        desktops: &["io.github.mpc_qt.mpc-qt.desktop", "mpc-qt.desktop"],
        dialect: Dialect::Plain,
    },
    Known {
        id: "mpc-hc",
        name: "MPC-HC",
        bins: &[
            r"C:\Program Files\MPC-HC\mpc-hc64.exe",
            r"C:\Program Files (x86)\MPC-HC\mpc-hc.exe",
        ],
        flatpaks: &[],
        desktops: &[],
        dialect: Dialect::Plain,
    },
    Known {
        id: "potplayer",
        name: "PotPlayer",
        bins: &[r"C:\Program Files\DAUM\PotPlayer\PotPlayerMini64.exe"],
        flatpaks: &[],
        desktops: &[],
        dialect: Dialect::Plain,
    },
    Known {
        id: "clapper",
        name: "Clapper",
        bins: &["clapper"],
        flatpaks: &["com.github.rafostar.Clapper"],
        desktops: &["com.github.rafostar.Clapper.desktop"],
        dialect: Dialect::Plain,
    },
    Known {
        id: "showtime",
        name: "Showtime",
        bins: &["showtime"],
        flatpaks: &["org.gnome.Showtime"],
        desktops: &["org.gnome.Showtime.desktop"],
        dialect: Dialect::Plain,
    },
    Known {
        id: "totem",
        name: "Vidéos (Totem)",
        bins: &["totem"],
        flatpaks: &["org.gnome.Totem"],
        desktops: &["org.gnome.Totem.desktop", "totem.desktop"],
        dialect: Dialect::Plain,
    },
    Known {
        id: "dragon",
        name: "Dragon Player",
        bins: &["dragon"],
        flatpaks: &["org.kde.dragonplayer"],
        desktops: &["org.kde.dragonplayer.desktop"],
        dialect: Dialect::Plain,
    },
    Known {
        id: "kaffeine",
        name: "Kaffeine",
        bins: &["kaffeine"],
        flatpaks: &["org.kde.kaffeine"],
        desktops: &["org.kde.kaffeine.desktop"],
        dialect: Dialect::Plain,
    },
    Known {
        id: "qmplay2",
        name: "QMPlay2",
        bins: &["qmplay2", "QMPlay2"],
        flatpaks: &["io.github.zaps166.QMPlay2"],
        desktops: &["io.github.zaps166.QMPlay2.desktop", "QMPlay2.desktop"],
        dialect: Dialect::Plain,
    },
    Known {
        id: "ffplay",
        name: "ffplay",
        bins: &["ffplay"],
        flatpaks: &[],
        desktops: &[],
        dialect: Dialect::Ffplay,
    },
];

/// MIME types checked for the OS default player (IPTV first).
const VIDEO_MIMES: &[&str] = &[
    "video/mp2t",
    "application/x-mpegurl",
    "video/x-mpegurl",
    "application/vnd.apple.mpegurl",
    "video/mp4",
    "video/x-matroska",
];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn find_bin(bin: &str) -> Option<PathBuf> {
    let p = Path::new(bin);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default();
    // GUI sessions often start with a reduced PATH.
    dirs.extend(
        [
            "/usr/bin",
            "/usr/local/bin",
            "/snap/bin",
            "/opt/homebrew/bin",
            "/home/linuxbrew/.linuxbrew/bin",
        ]
        .map(PathBuf::from),
    );
    if let Some(h) = home() {
        dirs.push(h.join(".local/bin"));
    }
    let exe = if cfg!(windows) { format!("{bin}.exe") } else { bin.to_string() };
    dirs.into_iter().map(|d| d.join(&exe)).find(|p| p.is_file())
}

fn flatpak_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/var/lib/flatpak")];
    if let Some(h) = home() {
        roots.insert(0, h.join(".local/share/flatpak"));
    }
    roots
}

fn flatpak_installed(app_id: &str) -> bool {
    cfg!(target_os = "linux")
        && flatpak_roots()
            .iter()
            .any(|r| r.join("app").join(app_id).join("current").exists())
}

fn applications_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".local/share")));
    if let Some(d) = data_home {
        dirs.push(d.join("applications"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    for d in data_dirs.split(':').filter(|d| !d.is_empty()) {
        dirs.push(Path::new(d).join("applications"));
    }
    for r in flatpak_roots() {
        dirs.push(r.join("exports/share/applications"));
    }
    dirs.dedup();
    dirs
}

fn find_desktop_file(desktop_id: &str) -> Option<PathBuf> {
    applications_dirs()
        .into_iter()
        .map(|d| d.join(desktop_id))
        .find(|p| p.is_file())
}

/// `[Desktop Entry]` keys we need.
#[derive(Debug, Default)]
struct DesktopEntry {
    name: Option<String>,
    exec: Option<String>,
    mime: Vec<String>,
    hidden: bool,
}

fn parse_desktop_entry(text: &str) -> DesktopEntry {
    let mut out = DesktopEntry::default();
    let mut in_main = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "Name" => out.name = Some(v.trim().to_string()),
            "Exec" => out.exec = Some(v.trim().to_string()),
            "MimeType" => {
                out.mime = v
                    .split(';')
                    .filter(|m| !m.is_empty())
                    .map(|m| m.trim().to_string())
                    .collect()
            }
            "Hidden" => out.hidden = v.trim() == "true",
            _ => {}
        }
    }
    out
}

/// Split `Exec=` per the Desktop Entry spec (double quotes, backslash escapes).
fn split_exec(exec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut has_token = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                has_token = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() && !quoted => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}

/// Exec template with `{url}` in place of `%u`/`%U`; `None` when the entry only takes files.
fn exec_template(exec: &str) -> Option<Vec<String>> {
    let mut takes_url = false;
    let mut out = Vec::new();
    for tok in split_exec(exec) {
        match tok.as_str() {
            "%u" | "%U" => {
                takes_url = true;
                out.push("{url}".to_string());
            }
            // Flatpak `--file-forwarding` markers.
            "@@u" | "@@" | "@@f" => {}
            t if t.len() == 2 && t.starts_with('%') => {}
            t => out.push(t.replace("%%", "%")),
        }
    }
    (takes_url && !out.is_empty()).then_some(out)
}

fn xdg_default(mime: &str) -> Option<String> {
    let out = Command::new("xdg-mime")
        .args(["query", "default", mime])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && id.ends_with(".desktop")).then_some(id)
}

/// `[Default Applications]` of the `mimeapps.list` files, when `xdg-mime` is missing.
fn mimeapps_default(mime: &str) -> Option<String> {
    let mut files = Vec::new();
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".config")));
    if let Some(c) = config_home {
        files.push(c.join("mimeapps.list"));
    }
    files.push(PathBuf::from("/etc/xdg/mimeapps.list"));
    files.extend(applications_dirs().into_iter().map(|d| d.join("mimeapps.list")));
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let mut in_defaults = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_defaults = line == "[Default Applications]";
                continue;
            }
            if !in_defaults {
                continue;
            }
            if let Some(v) = line.strip_prefix(mime).and_then(|r| r.strip_prefix('=')) {
                if let Some(id) = v.split(';').map(str::trim).find(|s| !s.is_empty()) {
                    return Some(id.to_string());
                }
            }
        }
    }
    None
}

fn os_default_desktop_id() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    VIDEO_MIMES
        .iter()
        .find_map(|m| xdg_default(m).or_else(|| mimeapps_default(m)))
}

fn known_player(k: &Known) -> Option<ExternalPlayer> {
    let base = |flatpak: bool, launch: Launch| ExternalPlayer {
        id: k.id.to_string(),
        name: k.name.to_string(),
        is_default: false,
        flatpak,
        dialect: k.dialect,
        launch,
    };
    if let Some(p) = k.bins.iter().find_map(|b| find_bin(b)) {
        return Some(base(false, Launch::Native(p)));
    }
    if let Some(app) = k.flatpaks.iter().find(|a| flatpak_installed(a)) {
        return Some(base(true, Launch::Flatpak((*app).to_string())));
    }
    if cfg!(target_os = "macos") {
        let app = match k.id {
            "vlc" => Some("VLC"),
            "mpv" => Some("mpv"),
            _ => None,
        }?;
        let bundle = PathBuf::from(format!("/Applications/{app}.app"));
        return bundle.is_dir().then(|| base(false, Launch::MacApp(app.into())));
    }
    None
}

fn is_browser(entry: &DesktopEntry) -> bool {
    entry
        .mime
        .iter()
        .any(|m| m == "text/html" || m.starts_with("x-scheme-handler/http"))
}

/// Default player of the desktop session that is not in [`KNOWN`].
fn desktop_player(desktop_id: &str) -> Option<ExternalPlayer> {
    let path = find_desktop_file(desktop_id)?;
    let entry = parse_desktop_entry(&std::fs::read_to_string(path).ok()?);
    if entry.hidden || is_browser(&entry) {
        return None;
    }
    let template = exec_template(entry.exec.as_deref()?)?;
    let flatpak = template.first().is_some_and(|p| p.ends_with("flatpak"));
    Some(ExternalPlayer {
        id: format!("desktop:{desktop_id}"),
        name: entry
            .name
            .unwrap_or_else(|| desktop_id.trim_end_matches(".desktop").to_string()),
        is_default: true,
        flatpak,
        dialect: Dialect::Plain,
        launch: Launch::Desktop(template),
    })
}

fn detect_uncached() -> Vec<ExternalPlayer> {
    let mut out: Vec<ExternalPlayer> = KNOWN.iter().filter_map(known_player).collect();
    if let Some(id) = os_default_desktop_id() {
        let known_idx = KNOWN.iter().position(|k| {
            k.desktops.contains(&id.as_str())
                || k.flatpaks.iter().any(|f| id.strip_suffix(".desktop") == Some(f))
        });
        let pos = known_idx.and_then(|i| out.iter().position(|p| p.id == KNOWN[i].id));
        match pos {
            Some(i) => {
                let mut p = out.remove(i);
                p.is_default = true;
                out.insert(0, p);
            }
            None => {
                if let Some(p) = desktop_player(&id) {
                    out.insert(0, p);
                }
            }
        }
    }
    info!(
        players = ?out.iter().map(|p| p.label()).collect::<Vec<_>>(),
        "external players detected"
    );
    out
}

static CACHE: Mutex<Option<(Instant, Vec<ExternalPlayer>)>> = Mutex::new(None);
const CACHE_TTL: Duration = Duration::from_secs(60);

/// Installed system players, OS default first. Empty on Android / iOS (Intent path).
pub fn detect_external_players() -> Vec<ExternalPlayer> {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        return Vec::new();
    }
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, list)) = guard.as_ref() {
        if at.elapsed() < CACHE_TTL {
            return list.clone();
        }
    }
    let list = detect_uncached();
    *guard = Some((Instant::now(), list.clone()));
    list
}

/// Player for `choice` (settings id); falls back to the OS default / best installed.
pub fn pick_external_player(choice: Option<&str>) -> Option<ExternalPlayer> {
    let players = detect_external_players();
    choice
        .filter(|c| !c.is_empty())
        .and_then(|c| players.iter().find(|p| p.id == c).cloned())
        .or_else(|| players.into_iter().next())
}

fn clean_field(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

fn dialect_args(d: Dialect, o: &ExternalLaunch<'_>) -> Vec<String> {
    let ua = o.user_agent.map(clean_field).filter(|s| !s.is_empty());
    let referer = o.referer.map(clean_field).filter(|s| !s.is_empty());
    let title = o.title.map(clean_field).filter(|s| !s.is_empty());
    let mut a = Vec::new();
    match d {
        Dialect::Vlc => {
            if let Some(v) = ua {
                a.push(format!("--http-user-agent={v}"));
            }
            if let Some(v) = referer {
                a.push(format!("--http-referrer={v}"));
            }
            if let Some(v) = title {
                a.push(format!("--meta-title={v}"));
            }
            if let Some(ms) = o.cache_ms {
                a.push(format!("--network-caching={ms}"));
            }
            a.push("--http-reconnect".into());
        }
        Dialect::Mpv | Dialect::Celluloid => {
            let p = if d == Dialect::Mpv { "--" } else { "--mpv-" };
            if let Some(v) = ua {
                a.push(format!("{p}user-agent={v}"));
            }
            if let Some(v) = referer {
                a.push(format!("{p}referrer={v}"));
            }
            if let Some(v) = title {
                a.push(format!("{p}force-media-title={v}"));
            }
        }
        Dialect::Ffplay => {
            if let Some(v) = ua {
                a.extend(["-user_agent".into(), v]);
            }
            if let Some(v) = referer {
                a.extend(["-headers".into(), format!("Referer: {v}\r\n")]);
            }
            if let Some(v) = title {
                a.extend(["-window_title".into(), v]);
            }
        }
        Dialect::Plain => {}
    }
    a
}

/// Program + arguments (URL included) for `player`.
fn command_line(player: &ExternalPlayer, url: &str, o: &ExternalLaunch<'_>) -> Result<Vec<String>> {
    let args = dialect_args(player.dialect, o);
    let mut cmd: Vec<String> = match &player.launch {
        Launch::Native(p) => {
            let mut v = vec![p.display().to_string()];
            v.extend(args);
            if player.dialect == Dialect::Mpv {
                v.push("--".into());
            }
            v.push(url.to_string());
            v
        }
        Launch::Flatpak(app) => {
            let flatpak = find_bin("flatpak")
                .ok_or_else(|| PlayerError::Backend("flatpak introuvable".into()))?;
            let mut v = vec![flatpak.display().to_string(), "run".into(), app.clone()];
            v.extend(args);
            v.push(url.to_string());
            v
        }
        Launch::Desktop(template) => template
            .iter()
            .map(|t| if t == "{url}" { url.to_string() } else { t.clone() })
            .collect(),
        Launch::MacApp(app) => {
            let mut v = vec!["open".into(), "-a".into(), app.clone(), url.to_string()];
            if !args.is_empty() {
                v.push("--args".into());
                v.extend(args);
            }
            v
        }
    };
    // FluxPlay itself sandboxed: players live on the host.
    if Path::new("/.flatpak-info").exists() {
        cmd.splice(0..0, ["flatpak-spawn".to_string(), "--host".to_string()]);
    }
    Ok(cmd)
}

/// Start `player` detached on `url`. The process is reaped on a background thread.
pub fn launch_external(player: &ExternalPlayer, url: &str, o: &ExternalLaunch<'_>) -> Result<()> {
    // Players read a plain path reliably; `file://` wants percent-encoding.
    let target = url.strip_prefix("file://").filter(|p| p.starts_with('/')).unwrap_or(url);
    let argv = command_line(player, target, o)?;
    let (prog, rest) = argv
        .split_first()
        .ok_or_else(|| PlayerError::Backend("commande vide".into()))?;
    let mut cmd = Command::new(prog);
    cmd.args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Our libmpv/FFmpeg search path must not leak into system players.
        .env_remove("LD_LIBRARY_PATH");
    let mut child = cmd.spawn().map_err(|e| {
        warn!(player = %player.label(), error = %e, "external player spawn failed");
        PlayerError::Backend(format!("{} : {e}", player.label()))
    })?;
    info!(
        player = %player.label(),
        endpoint = %crate::url_endpoint(url),
        headers = player.forwards_headers(),
        "external player launched"
    );
    std::thread::Builder::new()
        .name("fluxplay-ext-reap".into())
        .spawn(move || {
            let status = child.wait();
            debug!(?status, "external player exited");
        })
        .ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_parsing() {
        let t = exec_template(
            "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=haruna \
             --file-forwarding org.kde.haruna @@u %U @@",
        )
        .unwrap();
        assert_eq!(t.last().unwrap(), "{url}");
        assert!(!t.iter().any(|a| a.starts_with("@@")));
        assert_eq!(
            exec_template(r#""/opt/My Player/run" --x "a\"b" %U"#).unwrap(),
            vec!["/opt/My Player/run", "--x", "a\"b", "{url}"]
        );
        assert!(exec_template("gwenview %F").is_none());
        assert!(exec_template("vlc --started-from-file %U %i %c").unwrap().len() == 3);
    }

    #[test]
    fn desktop_entry_fields() {
        let e = parse_desktop_entry(
            "[Desktop Entry]\nName=Zen\nExec=zen %u\nMimeType=text/html;x-scheme-handler/http;\n\
             [Desktop Action new]\nName=New window\n",
        );
        assert_eq!(e.name.as_deref(), Some("Zen"));
        assert!(is_browser(&e));
    }

    #[test]
    fn header_args() {
        let o = ExternalLaunch {
            user_agent: Some("IPTVSmartersPlayer"),
            referer: Some("http://x/\r\nEvil: 1"),
            title: Some("TF1"),
            cache_ms: Some(4000),
        };
        let vlc = dialect_args(Dialect::Vlc, &o);
        assert!(vlc.contains(&"--http-user-agent=IPTVSmartersPlayer".to_string()));
        assert!(vlc.contains(&"--http-referrer=http://x/Evil: 1".to_string()));
        assert!(dialect_args(Dialect::Celluloid, &o).contains(&"--mpv-force-media-title=TF1".into()));
        assert!(dialect_args(Dialect::Plain, &o).is_empty());
    }
}
