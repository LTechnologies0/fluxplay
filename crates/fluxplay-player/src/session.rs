use chrono::{DateTime, Utc};
use fluxplay_core::models::ContentKind;
use fluxplay_core::Channel;
use serde::{Deserialize, Serialize};

use crate::backend::{BackendId, NativePlayer, PlayOptions};
use crate::{route, RoutedStream};

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
        }
    }
}

impl StreamSession {
    pub fn with_options(opts: PlayOptions) -> Self {
        let mut s = Self::default();
        s.volume = opts.volume;
        s.native = NativePlayer::new(opts);
        s
    }

    pub fn is_live(&self) -> bool {
        self.channel
            .as_ref()
            .map(|c| c.kind == ContentKind::Live)
            .unwrap_or(false)
    }

    pub fn open_channel(&mut self, channel: Channel) -> crate::Result<()> {
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
                        Ok(())
                    }
                    Err(e) => {
                        // Still record channel; allow external open.
                        self.channel = Some(channel);
                        self.started_at = Some(Utc::now());
                        self.state = PlaybackState::Error;
                        self.error = Some(e.to_string());
                        Err(e)
                    }
                }
            }
            Err(e) => {
                self.state = PlaybackState::Error;
                self.error = Some(e.to_string());
                Err(e)
            }
        }
    }

    pub fn pause(&mut self) {
        if self.state == PlaybackState::Playing {
            let _ = self.native.pause(true);
            self.state = PlaybackState::Paused;
        }
    }

    pub fn resume(&mut self) {
        if self.state == PlaybackState::Paused {
            let _ = self.native.pause(false);
            self.state = PlaybackState::Playing;
        }
    }

    pub fn stop(&mut self) {
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
        let _ = self.native.set_mute(self.muted);
        if !self.muted {
            let _ = self.native.set_volume(self.volume);
        }
    }

    pub fn set_volume(&mut self, vol: f32) {
        self.volume = vol.clamp(0.0, 1.0);
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
            let _ = self.native.seek_relative(secs);
            self.refresh_times();
        }
    }

    pub fn seek_percent(&mut self, pct: f64) {
        if self.is_live() {
            return;
        }
        if matches!(
            self.state,
            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
        ) {
            let _ = self.native.seek_percent(pct);
            self.refresh_times();
        }
    }

    pub fn restart(&mut self) {
        let Some(url) = self.channel.as_ref().map(|c| c.stream_url.clone()) else {
            return;
        };
        match self.native.restart(&url) {
            Ok(()) => {
                self.state = PlaybackState::Playing;
                self.error = None;
                self.started_at = Some(Utc::now());
                self.backend = self.native.active_backend();
            }
            Err(e) => {
                self.state = PlaybackState::Error;
                self.error = Some(e.to_string());
            }
        }
    }

    pub fn toggle_fullscreen(&mut self) {
        let _ = self.native.toggle_fullscreen();
    }

    pub fn cycle_audio(&mut self) {
        let _ = self.native.cycle_audio();
    }

    pub fn cycle_subtitles(&mut self) {
        let _ = self.native.cycle_subtitles();
    }

    pub fn frame_step(&mut self) {
        if self.state == PlaybackState::Paused {
            let _ = self.native.frame_step();
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
