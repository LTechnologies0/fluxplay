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
    /// Soft present scale vf kept when rebuilding eq/tonemap chains.
    pub soft_vf_prefix: Option<String>,
    /// LED/AMOLED eq fragment (`eq=...`) — soft path only.
    pub panel_eq: Option<String>,
    /// Panel color base (brightness, contrast, saturation, gamma) without night.
    pub color_base: (i32, i32, i32, i32),
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
            soft_vf_prefix: None,
            panel_eq: None,
            color_base: (0, 0, 0, 0),
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

    pub fn recycle_soft_rgba(&mut self, buf: Vec<u8>) {
        self.native.recycle_soft_rgba(buf);
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
            return;
        }
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
        self.native
            .last_soft_rgba()
            .map(|(w, h, b)| (w, h, b.to_vec()))
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
