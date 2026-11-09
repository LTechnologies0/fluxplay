use chrono::{DateTime, Utc};
use fluxplay_core::models::ContentKind;
use fluxplay_core::Channel;
use fluxplay_core::Stopwatch;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, trace, warn};

use crate::backend::{BackendId, MediaTrack, NativePlayer, PlayOptions, VideoRect};
use crate::{route, url_endpoint, RoutedStream};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackState {
    Idle,
    Opening,
    Playing,
    Paused,
    Buffering,
    Error,
}

pub struct StreamSession {
    pub state: PlaybackState,
    /// Embedded FFmpeg read-ahead, refreshed with the buffering state.
    pub buffer: Option<crate::BufferState>,
    pub channel: Option<Channel>,
    /// When set, the player reads this URL while `channel.stream_url` stays the
    /// catalog address (cinema relay: one portal connection, in ffmpeg).
    pub playback_url: Option<String>,
    pub routed: Option<RoutedStream>,
    pub started_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub volume: f32,
    pub muted: bool,
    pub backend: Option<BackendId>,
    pub native: NativePlayer,
    /// Last known position / duration from mpv (seconds).
    pub position_secs: f64,
    pub duration_secs: f64,
    // ── Extended player state (UI + mpv mirrors) ──────────────────────────
    pub speed: f64,
    pub loop_file: bool,
    pub ab_a: Option<f64>,
    pub ab_b: Option<f64>,
    pub sub_delay: f64,
    pub audio_delay: f64,
    pub audio_mode: AudioChannelMode,
    pub eq_preset: EqPreset,
    pub loudnorm: bool,
    pub deinterlace: DeinterlaceMode,
    pub upscale: UpscaleMode,
    pub rotate_deg: u32,
    pub zoom: f64,
    pub aspect: AspectMode,
    pub ontop: bool,
    pub night_vf: bool,
    /// Soft present scale vf kept when rebuilding eq/tonemap chains.
    pub soft_vf_prefix: Option<String>,
    /// LED/AMOLED eq fragment (`eq=...`) — soft path only.
    pub panel_eq: Option<String>,
    /// Panel color base (brightness, contrast, saturation, gamma) without night.
    pub color_base: (i32, i32, i32, i32),
    pub bookmarks: Vec<Bookmark>,
    /// Cache for `elapsed_label` — the text changes once per second, not per frame.
    elapsed_label_cache: std::cell::RefCell<(i64, String)>,
    /// Last tracks read while the player lock was free. The view must not
    /// query mpv itself: a busy lock used to blank the options card every frame.
    audio_tracks_cache: Vec<crate::MediaTrack>,
    subtitle_tracks_cache: Vec<crate::MediaTrack>,
    /// New streams start with subtitles off. Cleared when the viewer picks a track.
    subs_hold_off: bool,
    /// Auto-skip already jumped this opening, so a later seek back into it stays put.
    intro_skipped: bool,
    credits_skipped: bool,
    /// Opening credits, once we know where they start and where to land after them.
    opening: Option<OpeningSpan>,
    opening_watch: OpeningWatch,
    /// OS language (`fr`, `en`…). Empty means leave the demuxer's default track.
    preferred_audio_lang: String,
    /// The preferred track was applied, or the viewer picked one.
    audio_lang_applied: bool,
    /// Polls spent waiting for a matching track to show up.
    audio_lang_wait: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioChannelMode {
    #[default]
    Auto,
    Stereo,
    Mono,
    Left,
    Right,
}

impl AudioChannelMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Stereo => "Stéréo",
            Self::Mono => "Mono",
            Self::Left => "Gauche",
            Self::Right => "Droite",
        }
    }

    pub fn mpv_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Stereo => "stereo",
            Self::Mono => "mono",
            Self::Left => "1",
            Self::Right => "2",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::Stereo,
            Self::Stereo => Self::Mono,
            Self::Mono => Self::Left,
            Self::Left => Self::Right,
            Self::Right => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EqPreset {
    #[default]
    Off,
    Voice,
    Bass,
    Treble,
}

impl EqPreset {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Égaliseur off",
            Self::Voice => "Égaliseur voix",
            Self::Bass => "Égaliseur basses",
            Self::Treble => "Égaliseur aigus",
        }
    }

    pub fn af(self) -> &'static str {
        match self {
            Self::Off => "",
            Self::Voice => "lavfi=[highpass=f=180,lowpass=f=3500]",
            Self::Bass => "lavfi=[bass=g=6]",
            Self::Treble => "lavfi=[treble=g=5]",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::Voice,
            Self::Voice => Self::Bass,
            Self::Bass => Self::Treble,
            Self::Treble => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AspectMode {
    #[default]
    Auto,
    R16x9,
    R4x3,
    R235,
}

impl AspectMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Format auto",
            Self::R16x9 => "Format 16:9",
            Self::R4x3 => "Format 4:3",
            Self::R235 => "Format 2.35:1",
        }
    }

    /// Forced display aspect ratio, `None` for the stream's own.
    pub fn ratio(self) -> Option<f32> {
        match self {
            Self::Auto => None,
            Self::R16x9 => Some(16.0 / 9.0),
            Self::R4x3 => Some(4.0 / 3.0),
            Self::R235 => Some(2.35),
        }
    }

    pub fn mpv_value(self) -> &'static str {
        match self {
            Self::Auto => "-1",
            Self::R16x9 => "16:9",
            Self::R4x3 => "4:3",
            Self::R235 => "2.35",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::R16x9,
            Self::R16x9 => Self::R4x3,
            Self::R4x3 => Self::R235,
            Self::R235 => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpscaleMode {
    #[default]
    Auto,
    Bilinear,
    Lanczos,
    EwaLanczos,
    Nearest,
}

impl UpscaleMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Upscale auto",
            Self::Bilinear => "Upscale bilinéaire",
            Self::Lanczos => "Upscale Lanczos",
            Self::EwaLanczos => "Upscale EWA Lanczos (HQ)",
            Self::Nearest => "Upscale nearest (pixel)",
        }
    }

    pub fn mpv_scale(self) -> &'static str {
        match self {
            Self::Auto => "bilinear",
            Self::Bilinear => "bilinear",
            Self::Lanczos => "lanczos",
            Self::EwaLanczos => "ewa_lanczossharp",
            Self::Nearest => "nearest",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::Bilinear,
            Self::Bilinear => Self::Lanczos,
            Self::Lanczos => Self::EwaLanczos,
            Self::EwaLanczos => Self::Nearest,
            Self::Nearest => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeinterlaceMode {
    #[default]
    Off,
    Yes,
    Auto,
}

impl DeinterlaceMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Désentrelacement off",
            Self::Yes => "Désentrelacement ON",
            Self::Auto => "Désentrelacement auto",
        }
    }

    pub fn mpv_value(self) -> &'static str {
        match self {
            Self::Off => "no",
            Self::Yes => "yes",
            Self::Auto => "auto",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::Yes,
            Self::Yes => Self::Auto,
            Self::Auto => Self::Off,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Bookmark {
    pub label: String,
    pub secs: f64,
}

impl Default for StreamSession {
    fn default() -> Self {
        Self {
            state: PlaybackState::Idle,
            buffer: None,
            channel: None,
            playback_url: None,
            routed: None,
            started_at: None,
            error: None,
            volume: 0.85,
            muted: false,
            backend: None,
            native: NativePlayer::default(),
            position_secs: 0.0,
            duration_secs: 0.0,
            speed: 1.0,
            loop_file: false,
            ab_a: None,
            ab_b: None,
            sub_delay: 0.0,
            audio_delay: 0.0,
            audio_mode: AudioChannelMode::Auto,
            eq_preset: EqPreset::Off,
            loudnorm: false,
            deinterlace: DeinterlaceMode::Off,
            upscale: UpscaleMode::Auto,
            rotate_deg: 0,
            zoom: 0.0,
            aspect: AspectMode::Auto,
            ontop: false,
            night_vf: false,
            soft_vf_prefix: None,
            panel_eq: None,
            color_base: (0, 0, 0, 0),
            bookmarks: Vec::new(),
            elapsed_label_cache: std::cell::RefCell::new((-1, String::new())),
            audio_tracks_cache: Vec::new(),
            subtitle_tracks_cache: Vec::new(),
            subs_hold_off: true,
            intro_skipped: false,
            credits_skipped: false,
            opening: None,
            opening_watch: OpeningWatch::new(),
            preferred_audio_lang: String::new(),
            audio_lang_applied: false,
            audio_lang_wait: 0,
        }
    }
}

fn track_is_lang(track: &crate::MediaTrack, lang: &str) -> bool {
    let want = lang.split(['-', '_']).next().unwrap_or(lang).trim();
    if want.is_empty() {
        return false;
    }
    let code = track
        .lang
        .split(['-', '_'])
        .next()
        .unwrap_or(track.lang.as_str())
        .trim();
    if !code.is_empty() && audio_codes_match(want, code) {
        return true;
    }
    let label = track.label.to_ascii_lowercase();
    // VOSTFR is a subtitle convention, not a French audio dub.
    if label.contains("vostfr") || label.contains("vost") {
        return false;
    }
    audio_names(want).iter().any(|name| {
        label
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| word == *name)
    })
}

fn audio_codes_match(want: &str, have: &str) -> bool {
    if want == have {
        return true;
    }
    const GROUPS: &[&[&str]] = &[
        &["fr", "fre", "fra"],
        &["en", "eng"],
        &["ja", "jpn"],
        &["es", "spa", "esp"],
        &["de", "ger", "deu"],
        &["pt", "por", "pob"],
        &["ko", "kor"],
        &["zh", "chi", "zho", "cmn"],
        &["it", "ita"],
        &["ar", "ara"],
        &["ru", "rus"],
        &["nl", "dut", "nld"],
    ];
    GROUPS
        .iter()
        .any(|group| group.contains(&want) && group.contains(&have))
}

fn audio_names(lang: &str) -> &'static [&'static str] {
    match lang {
        "fr" | "fre" | "fra" => &["français", "francais", "french", "vf", "vff", "truefrench", "fr", "fre", "fra"],
        "en" | "eng" => &["anglais", "english", "en", "eng"],
        "ja" | "jpn" => &["japonais", "japanese", "ja", "jpn"],
        "es" | "spa" | "esp" => &["espagnol", "spanish", "es", "spa"],
        "de" | "ger" | "deu" => &["allemand", "german", "de", "ger", "deu"],
        "ko" | "kor" => &["coréen", "coreen", "korean", "ko", "kor"],
        "zh" | "chi" | "zho" => &["chinois", "chinese", "zh", "chi"],
        _ => &[],
    }
}

impl StreamSession {
    pub fn with_options(opts: PlayOptions) -> Self {
        debug!(preferred = ?opts.preferred, volume = opts.volume, "StreamSession::with_options");
        Self {
            volume: opts.volume,
            native: NativePlayer::new(opts),
            ..Default::default()
        }
    }

    /// Align video stage size for embedded rendering (or CLI window geometry).
    pub fn set_video_rect(&mut self, rect: VideoRect) {
        self.native.set_video_rect(rect);
    }

    pub fn video_anchored(&self) -> bool {
        true // embedded path always "in" the iced window
    }

    pub fn raise_video_window(&mut self) {
        // No-op when video is embedded; CLI fallback may still have a window.
        let _ = self.native.raise_video_window();
    }

    pub fn set_present_hz(&mut self, hz: u32) {
        self.native.set_present_hz(hz);
    }

    pub fn pull_video_frame(&mut self, w: u32, h: u32) -> Option<(u32, u32, Vec<u8>)> {
        self.native.pull_video_frame(w, h)
    }

    pub fn recycle_soft_rgba(&mut self, buf: Vec<u8>) {
        self.native.recycle_soft_rgba(buf);
    }

    /// Next frame for the GPU video stage; see [`crate::NativePlayer::pull_frame`].
    pub fn pull_frame(&mut self, w: u32, h: u32) -> Option<crate::VideoFrame> {
        self.native.pull_frame(w, h)
    }

    pub fn recycle_video_frame(&mut self, frame: crate::VideoFrame) {
        self.native.recycle_video_frame(frame);
    }

    pub fn frame_needs_redraw(&self) -> bool {
        self.native.frame_needs_redraw()
    }

    pub fn has_embedded_video(&self) -> bool {
        self.native.has_embedded_video()
    }

    pub fn caps(&self) -> crate::BackendCaps {
        self.native.caps()
    }

    pub fn backend_display_label(&self) -> &'static str {
        self.native.display_label()
    }

    /// Sync Playing ↔ Buffering from mpv cache-pause (no-op for other backends).
    pub fn refresh_buffering_state(&mut self) {
        if !matches!(
            self.state,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.buffer = None;
            return;
        }
        self.buffer = self.native.buffer_state();
        if self.native.paused_for_cache() {
            self.state = PlaybackState::Buffering;
        } else if self.state == PlaybackState::Buffering {
            self.state = PlaybackState::Playing;
        }
    }

    pub fn is_live(&self) -> bool {
        self.channel
            .as_ref()
            .map(|c| c.kind == ContentKind::Live)
            .unwrap_or(false)
    }

    pub fn open_channel(&mut self, channel: Channel) -> crate::Result<()> {
        let _prof = Stopwatch::start("session_open_channel");
        let endpoint = url_endpoint(&channel.stream_url);
        info!(
            channel_id = %channel.id,
            channel_name = %channel.name,
            %endpoint,
            "StreamSession::open_channel"
        );
        self.state = PlaybackState::Opening;
        self.error = None;
        self.position_secs = 0.0;
        self.duration_secs = 0.0;
        self.audio_tracks_cache.clear();
        self.subtitle_tracks_cache.clear();
        self.subs_hold_off = true;
        self.intro_skipped = false;
        self.credits_skipped = false;
        self.opening = None;
        self.opening_watch = OpeningWatch::new();
        self.audio_lang_applied = false;
        self.audio_lang_wait = 0;
        let url = self
            .playback_url
            .clone()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| channel.stream_url.clone());
        match route(&url) {
            Ok(routed) => {
                self.routed = Some(routed);
                self.native.options_mut().media_title = Some(channel.name.trim().to_string());
                match self.native.play(&url) {
                    Ok(backend) => {
                        self.backend = Some(backend);
                        self.channel = Some(channel);
                        self.started_at = Some(Utc::now());
                        self.state = PlaybackState::Playing;
                        if self.muted {
                            let _ = self.native.set_mute(true);
                        } else {
                            let _ = self.native.set_volume(self.volume);
                        }
                        info!(?backend, "StreamSession playing");
                        Ok(())
                    }
                    Err(e) => {
                        // Still record channel; allow external open.
                        warn!(
                            channel_id = %channel.id,
                            error = %e,
                            "StreamSession play failed"
                        );
                        self.channel = Some(channel);
                        self.started_at = Some(Utc::now());
                        self.state = PlaybackState::Error;
                        self.error = Some(e.to_string());
                        Err(e)
                    }
                }
            }
            Err(e) => {
                error!(
                    channel_id = %channel.id,
                    error = %e,
                    "StreamSession route failed"
                );
                self.state = PlaybackState::Error;
                self.error = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// Current stream in the system player (headers + title kept). Returns its label.
    #[cfg(not(target_os = "android"))]
    pub fn open_external(&mut self) -> crate::Result<String> {
        let channel = self
            .channel
            .as_ref()
            .ok_or_else(|| crate::PlayerError::Message("aucun flux en cours".into()))?;
        let url = channel.stream_url.clone();
        self.native.options_mut().media_title = Some(channel.name.trim().to_string());
        self.native.start_external(&url)
    }

    pub fn pause(&mut self) {
        if matches!(
            self.state,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            debug!("StreamSession::pause");
            match self.native.pause(true) {
                Ok(()) => self.state = PlaybackState::Paused,
                Err(e) => warn!(error = %e, "StreamSession::pause failed"),
            }
        }
    }

    pub fn resume(&mut self) {
        if self.state == PlaybackState::Paused {
            debug!("StreamSession::resume");
            match self.native.pause(false) {
                Ok(()) => self.state = PlaybackState::Playing,
                Err(e) => warn!(error = %e, "StreamSession::resume failed"),
            }
        }
    }

    pub fn stop(&mut self) {
        info!(
            channel_id = self.channel.as_ref().map(|c| c.id.as_str()),
            "StreamSession::stop"
        );
        self.native.stop();
        self.state = PlaybackState::Idle;
        self.channel = None;
        self.routed = None;
        self.started_at = None;
        self.error = None;
        self.backend = None;
        self.position_secs = 0.0;
        self.duration_secs = 0.0;
        self.audio_tracks_cache.clear();
        self.subtitle_tracks_cache.clear();
    }

    pub fn toggle_mute(&mut self) {
        let next = !self.muted;
        debug!(muted = next, "StreamSession::toggle_mute");
        match self.native.set_mute(next) {
            Ok(()) => {
                self.muted = next;
                if !next {
                    let _ = self.native.set_volume(self.volume);
                }
            }
            Err(e) => warn!(error = %e, "toggle_mute failed"),
        }
    }

    pub fn set_volume(&mut self, vol: f32) {
        let next = vol.clamp(0.0, 1.0);
        debug!(volume = next, "StreamSession::set_volume");
        if self.muted && next > 0.0 {
            match self.native.set_mute(false) {
                Ok(()) => self.muted = false,
                Err(e) => {
                    warn!(error = %e, "unmute via volume failed");
                    return;
                }
            }
        }
        // Always remember preferred volume for next open.
        self.volume = next;
        if !self.muted {
            if let Err(e) = self.native.set_volume(self.volume) {
                warn!(error = %e, "set_volume native failed");
            }
        }
    }

    pub fn volume_delta(&mut self, delta: f32) {
        self.set_volume(self.volume + delta);
    }

    pub fn seek_relative(&mut self, secs: f64) {
        if self.is_live() {
            trace!("StreamSession::seek_relative skipped (live)");
            return;
        }
        if matches!(
            self.state,
            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
        ) {
            debug!(secs, "StreamSession::seek_relative");
            let _ = self.native.seek_relative(secs);
            self.refresh_times();
        }
    }

    pub fn seek_percent(&mut self, pct: f64) {
        if self.is_live() {
            trace!("StreamSession::seek_percent skipped (live)");
            return;
        }
        if matches!(
            self.state,
            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
        ) {
            debug!(pct, "StreamSession::seek_percent");
            let _ = self.native.seek_percent(pct);
            self.refresh_times();
        }
    }

    /// Keep the file. Reload the audio device, and nudge a running playback
    /// so the picture does not stay on the pre-sleep frame.
    pub fn wake_output(&mut self, paused: bool) {
        if self.channel.is_none() {
            return;
        }
        info!(paused, "StreamSession::wake_output");
        if let Err(e) = self.native.wake_after_sleep(paused) {
            warn!(error = %e, "wake output failed");
        }
        self.refresh_times();
    }

    pub fn wake_audio(&mut self) {
        if self.channel.is_none() {
            return;
        }
        if let Err(e) = self.native.wake_audio() {
            warn!(error = %e, "wake audio failed");
        }
        self.refresh_times();
    }

    pub fn restart(&mut self) {
        let Some(url) = self.channel.as_ref().map(|c| c.stream_url.clone()) else {
            warn!("StreamSession::restart with no channel");
            return;
        };
        let endpoint = url_endpoint(&url);
        info!(%endpoint, "StreamSession::restart");
        match self.native.restart(&url) {
            Ok(()) => {
                self.state = PlaybackState::Playing;
                self.error = None;
                self.intro_skipped = false;
                self.credits_skipped = false;
                self.opening = None;
                self.opening_watch = OpeningWatch::new();
                self.audio_lang_applied = false;
                self.audio_lang_wait = 0;
                self.started_at = Some(Utc::now());
                self.backend = self.native.active_backend();
                // The engine was rebuilt at `opts.volume`: re-apply the user's mute.
                if self.muted {
                    if let Err(e) = self.native.set_mute(true) {
                        warn!(error = %e, "mute after restart failed");
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "StreamSession::restart failed");
                self.state = PlaybackState::Error;
                self.error = Some(e.to_string());
            }
        }
    }

    pub fn toggle_fullscreen(&mut self) {
        debug!("StreamSession::toggle_fullscreen");
        let _ = self.native.toggle_fullscreen();
    }

    /// Refresh the cached track lists. A busy player lock leaves the previous
    /// lists in place so the options card does not blink empty.
    /// Language the player should start on (`fr` for a French Windows).
    pub fn set_preferred_audio_lang(&mut self, lang: &str) {
        self.preferred_audio_lang = lang.trim().to_ascii_lowercase();
    }

    /// Switch to the system language once the demuxer lists it, and show that
    /// track first. A later manual choice is left alone.
    fn prefer_system_audio(&mut self) {
        if self.audio_lang_applied || self.preferred_audio_lang.is_empty() {
            return;
        }
        if self.audio_tracks_cache.is_empty() {
            return;
        }
        let want = self.preferred_audio_lang.clone();
        let Some(id) = self
            .audio_tracks_cache
            .iter()
            .find(|t| track_is_lang(t, &want))
            .map(|t| t.id)
        else {
            self.audio_lang_wait = self.audio_lang_wait.saturating_add(1);
            if self.audio_lang_wait >= 12 {
                self.audio_lang_applied = true;
            }
            return;
        };
        self.audio_tracks_cache.sort_by_key(|t| t.id != id);
        let already = self
            .audio_tracks_cache
            .iter()
            .any(|t| t.selected && t.id == id);
        if !already {
            if self.native.set_audio_track(id).is_ok() {
                for track in &mut self.audio_tracks_cache {
                    track.selected = track.id == id;
                }
            }
        }
        self.audio_lang_applied = true;
    }

    pub fn refresh_track_cache(&mut self) {
        if let Some(tracks) = self.native.tracks_polled("audio") {
            self.audio_tracks_cache = tracks;
            self.prefer_system_audio();
        }
        if let Some(tracks) = self.native.tracks_polled("sub") {
            if !tracks.is_empty() || self.subtitle_tracks_cache.is_empty() {
                self.subtitle_tracks_cache = tracks;
            }
        }
        // Demuxers often arm the first subtitle a moment after open.
        if self.subs_hold_off && self.subtitle_tracks_cache.iter().any(|t| t.selected) {
            if self.native.set_subtitle_track(None).is_ok() {
                for track in &mut self.subtitle_tracks_cache {
                    track.selected = false;
                }
            }
        }
    }

    pub fn audio_tracks(&self) -> Vec<MediaTrack> {
        self.audio_tracks_cache.clone()
    }

    pub fn subtitle_tracks(&self) -> Vec<MediaTrack> {
        self.subtitle_tracks_cache.clone()
    }

    pub fn set_audio_track(&mut self, id: i64) -> bool {
        // A manual pick wins over the automatic language choice.
        self.audio_lang_applied = true;
        match self.native.set_audio_track(id) {
            Ok(()) => {
                for track in &mut self.audio_tracks_cache {
                    track.selected = track.id == id;
                }
                true
            }
            Err(e) => {
                warn!(error = %e, id, "set_audio_track failed");
                false
            }
        }
    }

    pub fn set_subtitle_track(&mut self, id: Option<i64>) -> bool {
        self.subs_hold_off = id.is_none();
        match self.native.set_subtitle_track(id) {
            Ok(()) => {
                for track in &mut self.subtitle_tracks_cache {
                    track.selected = Some(track.id) == id;
                }
                true
            }
            Err(e) => {
                warn!(error = %e, ?id, "set_subtitle_track failed");
                false
            }
        }
    }

    pub fn cycle_audio(&mut self) -> bool {
        debug!("StreamSession::cycle_audio");
        match self.native.cycle_audio() {
            Ok(()) => true,
            Err(e) => {
                warn!(error = %e, "cycle_audio failed");
                false
            }
        }
    }

    pub fn cycle_subtitles(&mut self) -> bool {
        debug!("StreamSession::cycle_subtitles");
        match self.native.cycle_subtitles() {
            Ok(()) => true,
            Err(e) => {
                warn!(error = %e, "cycle_subtitles failed");
                false
            }
        }
    }

    pub fn frame_step(&mut self) -> bool {
        if self.state != PlaybackState::Paused {
            return false;
        }
        trace!("StreamSession::frame_step");
        match self.native.frame_step() {
            Ok(()) => true,
            Err(e) => {
                warn!(error = %e, "frame_step failed");
                false
            }
        }
    }

    pub fn set_speed(&mut self, speed: f64) {
        let next = speed.clamp(0.25, 3.0);
        match self.native.set_speed(next) {
            Ok(()) => self.speed = next,
            Err(e) => warn!(error = %e, speed = next, "set_speed unsupported"),
        }
    }

    pub fn cycle_speed(&mut self) {
        let next = match self.speed {
            s if s < 0.74 => 1.0,
            s if s < 1.24 => 1.5,
            s if s < 1.74 => 2.0,
            s if s < 2.49 => 3.0,
            _ => 0.5,
        };
        self.set_speed(next);
    }

    pub fn toggle_loop(&mut self) {
        let next = !self.loop_file;
        match self.native.set_loop_file(next) {
            Ok(()) => self.loop_file = next,
            Err(e) => warn!(error = %e, "toggle_loop unsupported"),
        }
    }

    pub fn mark_ab_a(&mut self) -> bool {
        self.refresh_times();
        let a = Some(self.position_secs);
        match self.native.set_ab_loop(a, self.ab_b) {
            Ok(()) => {
                self.ab_a = a;
                true
            }
            Err(e) => {
                warn!(error = %e, "mark_ab_a unsupported");
                false
            }
        }
    }

    pub fn mark_ab_b(&mut self) -> bool {
        self.refresh_times();
        let b = Some(self.position_secs);
        match self.native.set_ab_loop(self.ab_a, b) {
            Ok(()) => {
                self.ab_b = b;
                true
            }
            Err(e) => {
                warn!(error = %e, "mark_ab_b unsupported");
                false
            }
        }
    }

    pub fn clear_ab_loop(&mut self) -> bool {
        match self.native.set_ab_loop(None, None) {
            Ok(()) => {
                self.ab_a = None;
                self.ab_b = None;
                true
            }
            Err(e) => {
                warn!(error = %e, "clear_ab_loop unsupported");
                false
            }
        }
    }

    pub fn seek_absolute(&mut self, secs: f64) {
        if self.is_live() {
            return;
        }
        let _ = self.native.seek_absolute(secs);
        self.refresh_times();
    }

    /// « Passer le générique » during a detected opening, or for a series
    /// during the first 90s. Animated openings have no title card, so the
    /// picture scan never armed a button and nothing was drawn.
    pub fn skip_prompt(&self) -> Option<&'static str> {
        if self.intro_skipped || self.is_live() {
            return None;
        }
        if let Some(span) = &self.opening {
            if span.contains(self.position_secs) {
                return Some("Passer le générique");
            }
        }
        if self.series_opening_guess() {
            return Some("Passer le générique");
        }
        None
    }

    /// Series episodes usually open with the générique and no chapter.
    /// Hidden once a later opening has been found, so a cold open is not jumped.
    fn series_opening_guess(&self) -> bool {
        if self.intro_skipped || !self.playing_vod() {
            return false;
        }
        if self.channel.as_ref().map(|c| c.kind) != Some(ContentKind::Series) {
            return false;
        }
        if !(0.5..90.0).contains(&self.position_secs) {
            return false;
        }
        if self
            .opening
            .as_ref()
            .is_some_and(|s| s.start > self.position_secs + 8.0)
        {
            return false;
        }
        true
    }

    fn playing_vod(&self) -> bool {
        !self.is_live()
            && matches!(
                self.state,
                PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
            )
    }

    /// Read chapter titles once, then keep watching the picture until the
    /// opening actually starts (it is not always at 0:00).
    pub fn poll_opening(&mut self) {
        if self.intro_skipped || self.is_live() || !self.playing_vod() {
            return;
        }
        if self.opening.as_ref().is_some_and(|s| s.from_chapter) || self.opening_watch.gave_up {
            return;
        }
        if !self.opening_watch.chapters_read && self.position_secs >= 0.3 {
            match self.native.chapter_markers() {
                Some(chapters) => {
                    self.opening_watch.chapters_read = true;
                    if let Some(span) = opening_from_chapters(
                        &chapters,
                        self.channel.as_ref().map(|c| c.kind) == Some(ContentKind::Series),
                    ) {
                        tracing::info!(start = span.start, end = span.end, "opening from chapters");
                        self.opening = Some(span);
                    }
                }
                None if self.position_secs > 8.0 => self.opening_watch.chapters_read = true,
                None => {}
            }
        }
    }

    /// One already-decoded RGBA frame. Looks for a run of black or title cards,
    /// which is how an opening shows up when the file has no chapters.
    pub fn observe_opening_frame(&mut self, w: u32, h: u32, pitch: u32, rgba: &[u8]) {
        if self.intro_skipped
            || self.is_live()
            || !self.playing_vod()
            || self.opening.as_ref().is_some_and(|s| s.from_chapter)
            || self.opening_watch.gave_up
        {
            return;
        }
        let pos = self.position_secs;
        if !(0.4..200.0).contains(&pos) {
            if pos >= 200.0 {
                self.opening_watch.gave_up = true;
            }
            return;
        }
        if pos - self.opening_watch.last_pos > 2.5 {
            self.opening_watch.run_slate = false;
            self.opening_watch.seen_card = false;
            self.opening_watch.grid.clear();
        }
        if pos - self.opening_watch.last_pos < 0.35 && self.opening_watch.last_pos > 0.0 {
            return;
        }
        let grid = sample_luma_grid(w, h, pitch, rgba);
        if grid.is_empty() {
            return;
        }
        let motion = mean_abs_diff(&self.opening_watch.grid, &grid);
        let luma = mean(&grid);
        let flat = flatness(&grid, luma);
        self.opening_watch.grid = grid;
        self.opening_watch.last_pos = pos;
        // A logo or a black card is flat. A scene, even a still one, is not.
        let black = luma < 0.07;
        let card = !black && flat > 0.78 && motion < 0.035;
        let slate = black || card;
        if slate {
            if !self.opening_watch.run_slate {
                self.opening_watch.run_slate = true;
                self.opening_watch.seen_card = card;
                self.opening_watch.run_at = pos;
            } else if card {
                self.opening_watch.seen_card = true;
            }
            let start = self.opening_watch.run_at;
            // A fade from black is not a logo. Wait until a real card has been on screen.
            if self.opening_watch.seen_card && pos - start >= 1.8 && self.opening.is_none() {
                let guess = if self.channel.as_ref().map(|c| c.kind) == Some(ContentKind::Series) {
                    90.0
                } else {
                    20.0
                };
                let mut end = start + guess;
                if self.duration_secs > start + 10.0 {
                    end = end.min(self.duration_secs - 1.0);
                }
                let span = OpeningSpan {
                    start,
                    end,
                    from_chapter: false,
                };
                if span.end > span.start + 3.0 {
                    tracing::info!(start = span.start, end = span.end, "opening from picture");
                    self.opening = Some(span);
                }
            }
        } else if self.opening_watch.run_slate {
            let start = self.opening_watch.run_at;
            let held = pos - start;
            let saw_card = self.opening_watch.seen_card;
            self.opening_watch.run_slate = false;
            self.opening_watch.seen_card = false;
            // A short card is the start of the opening (logo, then the animation).
            // Closing the window here made the button vanish for the whole générique,
            // and a click sought to "now" so nothing moved.
            if saw_card && held >= 15.0 {
                if let Some(span) = self.opening.as_mut() {
                    if !span.from_chapter {
                        span.end = pos.max(span.start + 3.0);
                    }
                }
            }
        }
    }

    /// Seek past the opening. Same percent command as the progress bar — an
    /// absolute second seek is ignored on a lot of these series streams, which
    /// made the button look dead. The button stays if the seek did not land.
    pub fn skip_generic(&mut self) -> bool {
        if self.intro_skipped || self.is_live() {
            return false;
        }
        let target = if let Some(span) = self.opening.clone() {
            let in_span =
                self.position_secs + 1.5 >= span.start && self.position_secs <= span.end + 2.0;
            if in_span {
                span.end.max(self.position_secs + 2.0)
            } else if self.series_opening_guess() {
                90.0
            } else {
                return false;
            }
        } else if self.series_opening_guess() {
            90.0
        } else {
            return false;
        };
        let delta = target - self.position_secs;
        if delta < 1.5 {
            return false;
        }
        let result = if self.duration_secs > 1.0 {
            let pct = (target / self.duration_secs * 100.0).clamp(0.0, 99.0);
            match self.native.seek_percent(pct) {
                Ok(()) => Ok(()),
                Err(e) => {
                    warn!(error = %e, "skip percent failed, trying relative");
                    self.native.seek_relative(delta)
                }
            }
        } else {
            self.native.seek_relative(delta)
        };
        match result {
            Ok(()) => {
                self.intro_skipped = true;
                self.credits_skipped = true;
                self.refresh_times();
                true
            }
            Err(e) => {
                warn!(error = %e, target, "skip opening failed");
                false
            }
        }
    }

    /// Once per play, when the automatic option is on, as soon as the opening starts.
    pub fn auto_skip_generic(&mut self) -> bool {
        if self.intro_skipped {
            return false;
        }
        let Some(span) = self.opening.clone() else {
            return false;
        };
        if self.position_secs + 0.15 < span.start || self.position_secs >= span.end - 0.4 {
            return false;
        }
        self.skip_generic()
    }

    pub fn add_bookmark(&mut self) {
        self.refresh_times();
        let secs = self.position_secs;
        let label = format!("★ {}", format_hms(secs));
        self.bookmarks.push(Bookmark { label, secs });
        if self.bookmarks.len() > 24 {
            self.bookmarks.remove(0);
        }
    }

    pub fn jump_bookmark(&mut self, idx: usize) {
        if let Some(b) = self.bookmarks.get(idx).cloned() {
            self.seek_absolute(b.secs);
        }
    }

    pub fn chapter_step(&mut self, delta: i32) {
        let _ = self.native.chapter_step(delta);
        self.refresh_times();
    }

    pub fn nudge_sub_delay(&mut self, delta: f64) {
        let next = (self.sub_delay + delta).clamp(-10.0, 10.0);
        match self.native.set_sub_delay(next) {
            Ok(()) => self.sub_delay = next,
            Err(e) => warn!(error = %e, "sub_delay unsupported"),
        }
    }

    pub fn nudge_audio_delay(&mut self, delta: f64) {
        let next = (self.audio_delay + delta).clamp(-10.0, 10.0);
        match self.native.set_audio_delay(next) {
            Ok(()) => self.audio_delay = next,
            Err(e) => warn!(error = %e, "audio_delay unsupported"),
        }
    }

    pub fn cycle_audio_mode(&mut self) {
        let next = self.audio_mode.cycle();
        match self.native.set_audio_channels(next.mpv_value()) {
            Ok(()) => self.audio_mode = next,
            Err(e) => warn!(error = %e, "audio_mode unsupported"),
        }
    }

    pub fn cycle_eq(&mut self) {
        let prev = self.eq_preset;
        self.eq_preset = self.eq_preset.cycle();
        if self.apply_af().is_err() {
            self.eq_preset = prev;
        }
    }

    pub fn toggle_loudnorm(&mut self) {
        self.loudnorm = !self.loudnorm;
        if self.apply_af().is_err() {
            self.loudnorm = !self.loudnorm;
        }
    }

    fn apply_af(&mut self) -> Result<(), ()> {
        let mut parts = Vec::new();
        let eq = self.eq_preset.af();
        if !eq.is_empty() {
            parts.push(eq.to_string());
        }
        if self.loudnorm {
            parts.push("lavfi=[dynaudnorm=f=150:g=15]".into());
        }
        let af = parts.join(",");
        match self.native.set_af(&af) {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!(error = %e, "set_af unsupported");
                Err(())
            }
        }
    }

    pub fn toggle_deinterlace(&mut self) {
        if self.native.android_surface_present() {
            warn!("deinterlace skipped on MediaCodec Surface (would black video)");
            return;
        }
        let next = self.deinterlace.cycle();
        info!(mode = next.label(), "StreamSession::deinterlace");
        match self.native.set_deinterlace_mode(next.mpv_value()) {
            Ok(()) => self.deinterlace = next,
            Err(e) => warn!(error = %e, "deinterlace failed"),
        }
    }

    pub fn cycle_upscale(&mut self) {
        if self.native.android_surface_present() {
            warn!("upscale/scale skipped on MediaCodec Surface (would black video)");
            return;
        }
        let next = self.upscale.cycle();
        info!(mode = next.label(), "StreamSession::upscale");
        match self.native.set_scale(next.mpv_scale()) {
            Ok(()) => self.upscale = next,
            Err(e) => warn!(error = %e, "upscale/scale failed"),
        }
    }

    pub fn cycle_rotate(&mut self) {
        let next = match self.rotate_deg {
            0 => 90,
            90 => 180,
            180 => 270,
            _ => 0,
        };
        info!(deg = next, "StreamSession::rotate");
        match self.native.set_video_rotate(next) {
            Ok(()) => self.rotate_deg = next,
            Err(e) => warn!(error = %e, "rotate failed"),
        }
    }

    pub fn nudge_zoom(&mut self, delta: f64) {
        let next = (self.zoom + delta).clamp(-1.5, 1.5);
        info!(zoom = next, "StreamSession::zoom");
        match self.native.set_video_zoom(next) {
            Ok(()) => self.zoom = next,
            Err(e) => warn!(error = %e, "zoom failed"),
        }
    }

    pub fn cycle_aspect(&mut self) -> bool {
        let next = self.aspect.cycle();
        info!(aspect = next.label(), "StreamSession::aspect");
        match self.native.set_aspect(next.mpv_value()) {
            Ok(()) => {
                self.aspect = next;
                true
            }
            Err(e) => {
                warn!(error = %e, "aspect failed");
                false
            }
        }
    }

    pub fn toggle_ontop(&mut self) {
        let next = !self.ontop;
        info!(ontop = next, "StreamSession::ontop (mpv prop; iced window handled by UI)");
        match self.native.set_ontop(next) {
            Ok(()) => self.ontop = next,
            Err(e) => warn!(error = %e, "ontop unsupported"),
        }
    }

    pub fn toggle_night_vf(&mut self) {
        self.night_vf = !self.night_vf;
        info!(night = self.night_vf, "StreamSession::night_vf");
        if self.apply_color_and_filters().is_err() {
            self.night_vf = !self.night_vf;
        }
    }

    /// Apply persisted Settings defaults right after open (aspect / deint / scale / tone).
    /// Never injects a scale vf on Android Surface — that blacks MediaCodec embed.
    // Bundling these prefs into a struct would churn all call sites for no gain.
    pub fn apply_saved_video_prefs(
        &mut self,
        aspect: AspectMode,
        deinterlace: DeinterlaceMode,
        upscale: UpscaleMode,
        night: bool,
        panel_eq: Option<String>,
        soft_vf_prefix: Option<String>,
        color_base: (i32, i32, i32, i32),
    ) {
        let surface = self.native.android_surface_present();
        self.soft_vf_prefix = if surface { None } else { soft_vf_prefix };
        let _ = panel_eq; // tone via color props (safe on Surface)
        self.color_base = color_base;
        self.night_vf = night;
        if self.native.set_aspect(aspect.mpv_value()).is_ok() {
            self.aspect = aspect;
        }
        // Deinterlace / upscale inject filters that break mediacodec_embed zero-copy.
        if !surface {
            if self
                .native
                .set_deinterlace_mode(deinterlace.mpv_value())
                .is_ok()
            {
                self.deinterlace = deinterlace;
            }
            if self.native.set_scale(upscale.mpv_scale()).is_ok() {
                self.upscale = upscale;
            }
        }
        let _ = self.apply_color_and_filters();
    }

    fn apply_color_and_filters(&mut self) -> Result<(), ()> {
        let (mut b, mut c, mut s, mut g) = self.color_base;
        if self.night_vf {
            b -= 12;
            s -= 15;
            g -= 12;
            c += 5;
        }
        if self
            .native
            .set_color_adjust(
                b.clamp(-100, 100),
                c.clamp(-100, 100),
                s.clamp(-100, 100),
                g.clamp(-100, 100),
            )
            .is_err()
        {
            return Err(());
        }
        if self.native.android_surface_present() {
            return Ok(());
        }
        self.apply_video_filters()
    }

    /// Re-apply soft vf chain after layout/rotate (keeps aspect-safe scale in sync).
    // Unit error mirrors the internal filter-apply chain; callers only check is_err().
    pub fn refresh_soft_filters(&mut self) -> Result<(), ()> {
        self.apply_video_filters()
    }

    fn apply_video_filters(&mut self) -> Result<(), ()> {
        if self.native.android_surface_present() {
            return Ok(());
        }
        let mut parts = Vec::new();
        if let Some(soft) = &self.soft_vf_prefix {
            if !soft.is_empty() {
                parts.push(soft.clone());
            }
        }
        let vf = parts.join(",");
        // Always set (including empty) so leftover soft scale is cleared on soft path.
        match self.native.set_vf(&vf) {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!(error = %e, vf = %vf, "set_vf failed");
                Err(())
            }
        }
    }

    pub fn toggle_sub_visibility(&mut self) -> bool {
        info!("StreamSession::toggle_sub_visibility");
        match self.native.toggle_sub_visibility() {
            Ok(()) => true,
            Err(e) => {
                warn!(error = %e, "sub-visibility failed");
                false
            }
        }
    }

    pub fn screenshot_to_path(&mut self, path: &std::path::Path) -> bool {
        match self.native.screenshot_to(&path.to_string_lossy()) {
            Ok(()) => true,
            Err(e) if e.to_string().contains("SOFT_RGBA") => false, // app soft-writes PNG
            Err(e) => {
                warn!(error = %e, "screenshot failed");
                false
            }
        }
    }

    /// Soft RGBA bytes for PNG capture when mpv screenshot is unavailable.
    pub fn soft_rgba_snapshot(&self) -> Option<(u32, u32, Vec<u8>)> {
        self.native.soft_snapshot_rgba()
    }

    pub fn refresh_times(&mut self) {
        if let Some((pos, dur)) = self.native.playback_times() {
            self.position_secs = pos;
            self.duration_secs = dur;
        }
    }

    pub fn content_fps(&self) -> Option<f64> {
        self.native.content_fps()
    }

    pub fn elapsed_label(&self) -> String {
        // The label only changes when the displayed second flips — cache it
        // (called per view rebuild, up to 120×/s while chrome is visible).
        let key: i64 = if self.is_live() {
            self.started_at
                .map(|s| (Utc::now() - s).num_seconds().max(0))
                .unwrap_or(0)
        } else {
            self.position_secs as i64
        };
        if let Ok(mut c) = self.elapsed_label_cache.try_borrow_mut() {
            if c.0 == key && !c.1.is_empty() {
                return c.1.clone();
            }
            let label = self.elapsed_label_uncached();
            *c = (key, label.clone());
            return label;
        }
        self.elapsed_label_uncached()
    }

    fn elapsed_label_uncached(&self) -> String {
        if let (Some(start), true) = (self.started_at, self.is_live()) {
            let secs = (Utc::now() - start).num_seconds().max(0) as u64;
            return format!("EN DIRECT · {}", format_hms(secs as f64));
        }
        if self.duration_secs > 1.0 {
            return format!(
                "{} / {}",
                format_hms(self.position_secs),
                format_hms(self.duration_secs)
            );
        }
        if self.position_secs > 0.0 {
            return format_hms(self.position_secs);
        }
        "—:—".into()
    }

    pub fn progress_ratio(&self) -> f64 {
        if self.is_live() || self.duration_secs <= 1.0 {
            return 0.0;
        }
        (self.position_secs / self.duration_secs).clamp(0.0, 1.0)
    }

    pub fn status_line(&self) -> String {
        match (&self.state, &self.channel, &self.routed) {
            (PlaybackState::Idle, _, _) => "Prêt".into(),
            (PlaybackState::Opening, Some(ch), _) => format!("Ouverture — {}", ch.name),
            (PlaybackState::Playing, Some(ch), Some(r)) => format!(
                "{} · {} · {} · {}",
                ch.name,
                r.url.scheme.label(),
                r.url.delivery.label(),
                self.backend_display_label()
            ),
            (PlaybackState::Paused, Some(ch), _) => {
                format!("Pause — {} · {}", ch.name, self.backend_display_label())
            }
            (PlaybackState::Buffering, Some(ch), _) => match self.buffer {
                Some(b) if b.goal_secs > 0.0 => format!(
                    "Mise en mémoire tampon {} % — {}",
                    b.fill_percent(),
                    ch.name
                ),
                _ => format!("Buffer — {}", ch.name),
            },
            (PlaybackState::Error, _, _) => self
                .error
                .clone()
                .unwrap_or_else(|| "Erreur de lecture".into()),
            _ => "…".into(),
        }
    }
}

#[derive(Clone, Copy)]
struct OpeningSpan {
    start: f64,
    end: f64,
    from_chapter: bool,
}

impl OpeningSpan {
    fn contains(self, pos: f64) -> bool {
        pos + 0.15 >= self.start && pos < self.end - 0.25
    }
}

struct OpeningWatch {
    chapters_read: bool,
    gave_up: bool,
    run_slate: bool,
    seen_card: bool,
    run_at: f64,
    last_pos: f64,
    grid: Vec<f32>,
}

impl OpeningWatch {
    fn new() -> Self {
        Self {
            chapters_read: false,
            gave_up: false,
            run_slate: false,
            seen_card: false,
            run_at: 0.0,
            last_pos: 0.0,
            grid: Vec::new(),
        }
    }
}

fn opening_from_chapters(chapters: &[(f64, String)], series: bool) -> Option<OpeningSpan> {
    let guess = if series { 90.0 } else { 25.0 };
    for (i, (start, title)) in chapters.iter().enumerate() {
        if !looks_like_opening(title) {
            continue;
        }
        let next = chapters.get(i + 1).map(|(t, _)| *t);
        let end = next.unwrap_or(start + guess).min(start + 150.0);
        if end > start + 3.0 {
            return Some(OpeningSpan {
                start: *start,
                end,
                from_chapter: true,
            });
        }
    }
    None
}

fn looks_like_opening(title: &str) -> bool {
    let mut folded = String::new();
    for c in title.chars() {
        let c = match c {
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'à' | 'â' | 'ä' => 'a',
            'ù' | 'û' | 'ü' => 'u',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ç' => 'c',
            other => other.to_ascii_lowercase(),
        };
        folded.push(c);
    }
    let t = format!(" {folded} ");
    const BLOCK: &[&str] = &[
        "ending",
        "outro",
        "preview",
        "end credit",
        "credits de fin",
        "credit de fin",
        "generique de fin",
    ];
    if BLOCK.iter().any(|k| t.contains(k)) {
        return false;
    }
    const OPEN: &[&str] = &[
        " intro",
        "intro ",
        "opening",
        "generique",
        " op ",
        "o.p",
        "theme",
        "logo",
        "studio",
        "cold open",
    ];
    OPEN.iter().any(|k| t.contains(k)) || t.trim() == "op"
}

fn sample_luma_grid(w: u32, h: u32, pitch: u32, rgba: &[u8]) -> Vec<f32> {
    if w < 16 || h < 16 || pitch < w.saturating_mul(4) {
        return Vec::new();
    }
    const NX: u32 = 16;
    const NY: u32 = 9;
    let x0 = w / 5;
    let x1 = w.saturating_sub(x0).max(x0 + 1);
    let y0 = h / 5;
    let y1 = h.saturating_sub(y0).max(y0 + 1);
    let mut grid = Vec::with_capacity((NX * NY) as usize);
    for iy in 0..NY {
        let y = y0 + (y1 - y0) * iy / NY;
        let row = y as usize * pitch as usize;
        for ix in 0..NX {
            let x = x0 + (x1 - x0) * ix / NX;
            let off = row + x as usize * 4;
            if off + 2 >= rgba.len() {
                grid.push(0.0);
                continue;
            }
            let r = rgba[off] as f32;
            let g = rgba[off + 1] as f32;
            let b = rgba[off + 2] as f32;
            grid.push((0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0);
        }
    }
    grid
}

fn mean(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f32>() / values.len() as f32
}

fn flatness(values: &[f32], luma: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let close = values.iter().filter(|v| (**v - luma).abs() < 0.08).count();
    close as f32 / values.len() as f32
}

fn mean_abs_diff(prev: &[f32], next: &[f32]) -> f32 {
    if prev.len() != next.len() || next.is_empty() {
        return 1.0;
    }
    let sum: f32 = prev.iter().zip(next).map(|(a, b)| (a - b).abs()).sum();
    sum / next.len() as f32
}

fn format_hms(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}
