use chrono::{DateTime, Utc};
use fluxplay_core::models::ContentKind;
use fluxplay_core::Channel;
use fluxplay_core::Stopwatch;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, trace, warn};

use crate::backend::{BackendId, NativePlayer, PlayOptions, VideoRect};
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
    pub channel: Option<Channel>,
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
    pub bookmarks: Vec<Bookmark>,
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
            channel: None,
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
            bookmarks: Vec::new(),
        }
    }
}

impl StreamSession {
    pub fn with_options(opts: PlayOptions) -> Self {
        debug!(preferred = ?opts.preferred, volume = opts.volume, "StreamSession::with_options");
        let mut s = Self::default();
        s.volume = opts.volume;
        s.native = NativePlayer::new(opts);
        s
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

    pub fn pull_video_frame(&mut self, w: u32, h: u32) -> Option<(u32, u32, Vec<u8>)> {
        self.native.pull_video_frame(w, h)
    }

    pub fn frame_needs_redraw(&self) -> bool {
        self.native.frame_needs_redraw()
    }

    pub fn has_embedded_video(&self) -> bool {
        self.native.has_embedded_video()
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
        match route(&channel.stream_url) {
            Ok(routed) => {
                self.routed = Some(routed);
                let url = channel.stream_url.clone();
                match self.native.play(&url) {
                    Ok(backend) => {
                        self.backend = Some(backend);
                        self.channel = Some(channel);
                        self.started_at = Some(Utc::now());
                        self.state = PlaybackState::Playing;
                        let vol = if self.muted { 0.0 } else { self.volume };
                        let _ = self.native.set_volume(vol);
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

    pub fn pause(&mut self) {
        if self.state == PlaybackState::Playing {
            debug!("StreamSession::pause");
            let _ = self.native.pause(true);
            self.state = PlaybackState::Paused;
        }
    }

    pub fn resume(&mut self) {
        if self.state == PlaybackState::Paused {
            debug!("StreamSession::resume");
            let _ = self.native.pause(false);
            self.state = PlaybackState::Playing;
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
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        debug!(muted = self.muted, "StreamSession::toggle_mute");
        let _ = self.native.set_mute(self.muted);
        if !self.muted {
            let _ = self.native.set_volume(self.volume);
        }
    }

    pub fn set_volume(&mut self, vol: f32) {
        self.volume = vol.clamp(0.0, 1.0);
        debug!(volume = self.volume, "StreamSession::set_volume");
        if !self.muted {
            let _ = self.native.set_volume(self.volume);
        }
    }

    pub fn volume_delta(&mut self, delta: f32) {
        self.set_volume(self.volume + delta);
    }

    pub fn seek_relative(&mut self, secs: f64) {
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
                self.started_at = Some(Utc::now());
                self.backend = self.native.active_backend();
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

    pub fn cycle_audio(&mut self) {
        debug!("StreamSession::cycle_audio");
        let _ = self.native.cycle_audio();
    }

    pub fn cycle_subtitles(&mut self) {
        debug!("StreamSession::cycle_subtitles");
        let _ = self.native.cycle_subtitles();
    }

    pub fn frame_step(&mut self) {
        if self.state == PlaybackState::Paused {
            trace!("StreamSession::frame_step");
            let _ = self.native.frame_step();
        }
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(0.25, 3.0);
        let _ = self.native.set_speed(self.speed);
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
        self.loop_file = !self.loop_file;
        let _ = self.native.set_loop_file(self.loop_file);
    }

    pub fn mark_ab_a(&mut self) {
        self.refresh_times();
        self.ab_a = Some(self.position_secs);
        let _ = self.native.set_ab_loop(self.ab_a, self.ab_b);
    }

    pub fn mark_ab_b(&mut self) {
        self.refresh_times();
        self.ab_b = Some(self.position_secs);
        let _ = self.native.set_ab_loop(self.ab_a, self.ab_b);
    }

    pub fn clear_ab_loop(&mut self) {
        self.ab_a = None;
        self.ab_b = None;
        let _ = self.native.set_ab_loop(None, None);
    }

    pub fn seek_absolute(&mut self, secs: f64) {
        if self.is_live() {
            return;
        }
        let _ = self.native.seek_absolute(secs);
        self.refresh_times();
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
        self.sub_delay = (self.sub_delay + delta).clamp(-10.0, 10.0);
        let _ = self.native.set_sub_delay(self.sub_delay);
    }

    pub fn nudge_audio_delay(&mut self, delta: f64) {
        self.audio_delay = (self.audio_delay + delta).clamp(-10.0, 10.0);
        let _ = self.native.set_audio_delay(self.audio_delay);
    }

    pub fn cycle_audio_mode(&mut self) {
        self.audio_mode = self.audio_mode.cycle();
        let _ = self.native.set_audio_channels(self.audio_mode.mpv_value());
    }

    pub fn cycle_eq(&mut self) {
        self.eq_preset = self.eq_preset.cycle();
        self.apply_af();
    }

    pub fn toggle_loudnorm(&mut self) {
        self.loudnorm = !self.loudnorm;
        self.apply_af();
    }

    fn apply_af(&mut self) {
        let mut parts = Vec::new();
        let eq = self.eq_preset.af();
        if !eq.is_empty() {
            parts.push(eq.to_string());
        }
        if self.loudnorm {
            parts.push("lavfi=[dynaudnorm=f=150:g=15]".into());
        }
        let af = parts.join(",");
        let _ = self.native.set_af(&af);
    }

    pub fn toggle_deinterlace(&mut self) {
        self.deinterlace = self.deinterlace.cycle();
        info!(mode = self.deinterlace.label(), "StreamSession::deinterlace");
        if let Err(e) = self.native.set_deinterlace_mode(self.deinterlace.mpv_value()) {
            warn!(error = %e, "deinterlace failed");
        }
    }

    pub fn cycle_upscale(&mut self) {
        self.upscale = self.upscale.cycle();
        info!(mode = self.upscale.label(), "StreamSession::upscale");
        if let Err(e) = self.native.set_scale(self.upscale.mpv_scale()) {
            warn!(error = %e, "upscale/scale failed");
        }
    }

    pub fn cycle_rotate(&mut self) {
        self.rotate_deg = match self.rotate_deg {
            0 => 90,
            90 => 180,
            180 => 270,
            _ => 0,
        };
        info!(deg = self.rotate_deg, "StreamSession::rotate");
        if let Err(e) = self.native.set_video_rotate(self.rotate_deg) {
            warn!(error = %e, "rotate failed");
        }
    }

    pub fn nudge_zoom(&mut self, delta: f64) {
        self.zoom = (self.zoom + delta).clamp(-1.5, 1.5);
        info!(zoom = self.zoom, "StreamSession::zoom");
        if let Err(e) = self.native.set_video_zoom(self.zoom) {
            warn!(error = %e, "zoom failed");
        }
    }

    pub fn cycle_aspect(&mut self) {
        self.aspect = self.aspect.cycle();
        info!(aspect = self.aspect.label(), "StreamSession::aspect");
        if let Err(e) = self.native.set_aspect(self.aspect.mpv_value()) {
            warn!(error = %e, "aspect failed");
        }
    }

    pub fn toggle_ontop(&mut self) {
        self.ontop = !self.ontop;
        info!(ontop = self.ontop, "StreamSession::ontop (mpv prop; iced window handled by UI)");
        let _ = self.native.set_ontop(self.ontop);
    }

    pub fn toggle_night_vf(&mut self) {
        self.night_vf = !self.night_vf;
        info!(night = self.night_vf, "StreamSession::night_vf");
        self.apply_video_filters();
    }

    fn apply_video_filters(&mut self) {
        let mut parts = Vec::new();
        if self.night_vf {
            parts.push("eq=gamma=0.85:saturation=0.85:contrast=1.05");
        }
        let vf = parts.join(",");
        if let Err(e) = self.native.set_vf(&vf) {
            warn!(error = %e, vf = %vf, "set_vf failed");
        }
    }

    pub fn toggle_sub_visibility(&mut self) {
        info!("StreamSession::toggle_sub_visibility");
        if let Err(e) = self.native.toggle_sub_visibility() {
            warn!(error = %e, "sub-visibility failed");
        }
    }

    pub fn screenshot_to_path(&mut self, path: &std::path::Path) -> bool {
        match self.native.screenshot_to(&path.to_string_lossy()) {
            Ok(()) => true,
            Err(e) => {
                warn!(error = %e, "screenshot failed");
                false
            }
        }
    }

    pub fn refresh_times(&mut self) {
        if let Some((pos, dur)) = self.native.playback_times() {
            self.position_secs = pos;
            self.duration_secs = dur;
        }
    }

    pub fn elapsed_label(&self) -> String {
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
        let be = self.backend.map(|b| b.label()).unwrap_or("—");
        match (&self.state, &self.channel, &self.routed) {
            (PlaybackState::Idle, _, _) => "Prêt".into(),
            (PlaybackState::Opening, Some(ch), _) => format!("Ouverture — {}", ch.name),
            (PlaybackState::Playing, Some(ch), Some(r)) => format!(
                "{} · {} · {} · {}",
                ch.name,
                r.url.scheme.label(),
                r.url.delivery.label(),
                be
            ),
            (PlaybackState::Paused, Some(ch), _) => format!("Pause — {} · {}", ch.name, be),
            (PlaybackState::Buffering, Some(ch), _) => format!("Buffer — {}", ch.name),
            (PlaybackState::Error, _, _) => self
                .error
                .clone()
                .unwrap_or_else(|| "Erreur de lecture".into()),
            _ => "…".into(),
        }
    }
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
